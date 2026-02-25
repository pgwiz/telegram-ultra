"""
MTProto upload IPC handler for Hermes.

Handles the 'mtproto_upload' action: uploads large files to the private
storage channel via Telethon, caches the channel message ID, and returns
it so the Rust bot can copy_message the file to the user.
"""
import os
import time
import asyncio
import hashlib
import logging
from typing import Optional

from worker.mtproto_client import mtproto, CHANNEL_ID
from worker.database import get_database

logger = logging.getLogger(__name__)

# File type detection
AUDIO_EXTS = {".mp3", ".m4a", ".opus", ".flac", ".ogg", ".wav"}
VIDEO_EXTS = {".mp4", ".mkv", ".webm", ".avi", ".mov"}


def _detect_file_type(file_path: str) -> str:
    ext = os.path.splitext(file_path)[1].lower()
    if ext in AUDIO_EXTS:
        return "audio"
    if ext in VIDEO_EXTS:
        return "video"
    return "document"


def _parse_audio_meta(filename: str) -> tuple:
    """Parse artist/title from 'Artist - Title.mp3' or '001 - Artist - Title.mp3'."""
    name = os.path.splitext(filename)[0]
    parts = name.split(" - ", 2)
    if len(parts) == 3:
        return parts[1].strip(), parts[2].strip()   # skip track number
    if len(parts) == 2:
        return parts[0].strip(), parts[1].strip()
    return "", name.strip()


def _build_attributes(file_path: str, file_type: str) -> list:
    from telethon.tl.types import (
        DocumentAttributeAudio,
        DocumentAttributeFilename,
        DocumentAttributeVideo,
    )
    filename   = os.path.basename(file_path)
    attributes = [DocumentAttributeFilename(filename)]

    if file_type == "audio":
        performer, title = _parse_audio_meta(filename)
        attributes.append(DocumentAttributeAudio(
            duration=0,
            title=title or filename,
            performer=performer,
            voice=False,
        ))
    elif file_type == "video":
        attributes.append(DocumentAttributeVideo(
            duration=0, w=0, h=0,
            supports_streaming=True,
        ))
    return attributes


def _sha256(file_path: str) -> str:
    h = hashlib.sha256()
    with open(file_path, "rb") as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()


async def _upload_with_retry(
    file_path:  str,
    caption:    str,
    attributes: list,
    file_type:  str,
    progress_cb,
    retries:    int = 3,
) -> int:
    """Upload to storage channel; return channel message ID."""
    from telethon.errors import FloodWaitError, FilePartMissingError

    for attempt in range(1, retries + 1):
        try:
            msg = await mtproto.client.send_file(
                CHANNEL_ID,
                file_path,
                caption=caption,
                attributes=attributes,
                force_document=(file_type == "document"),
                progress_callback=progress_cb,
            )
            return msg.id

        except FloodWaitError as e:
            wait = e.seconds + 5
            logger.warning(f"FloodWait: sleeping {wait}s (attempt {attempt})")
            await asyncio.sleep(wait)

        except FilePartMissingError:
            logger.warning(f"FilePartMissing on attempt {attempt} — retrying")
            await asyncio.sleep(2 * attempt)

        except Exception as e:
            logger.error(f"Upload error attempt {attempt}: {e}")
            if attempt == retries:
                raise
            await asyncio.sleep(2 ** attempt)

    raise RuntimeError(f"Upload failed after {retries} attempts")


async def handle_mtproto_upload(ipc, task_id: str, request: dict) -> None:
    """
    IPC handler for 'mtproto_upload' action.

    Request params:
        file_path: str  — absolute path to the file on disk
        chat_id:   int  — destination Telegram chat ID (for progress context)
        filename:  str  — display filename

    Response (done):
        channel_msg_id: int   — message ID in the storage channel
        cached:         bool  — True if served from cache (no upload)
    """
    params    = request.get("params", {})
    file_path = params.get("file_path", "")
    filename  = params.get("filename", "") or os.path.basename(file_path)

    # ── Validate ──────────────────────────────────────────────────────────────
    if not os.path.exists(file_path):
        ipc.send_error(task_id, f"File not found: {file_path}", "FILE_NOT_FOUND")
        return

    if CHANNEL_ID == 0:
        ipc.send_error(task_id, "STORAGE_CHANNEL_ID not set", "CONFIG_ERROR")
        return

    try:
        mtproto.client  # triggers RuntimeError if not connected
    except RuntimeError as e:
        ipc.send_error(task_id, str(e), "MTPROTO_NOT_CONNECTED")
        return

    # ── Cache check ───────────────────────────────────────────────────────────
    file_hash = _sha256(file_path)
    db        = await get_database()
    cached_id = await db.get_cached_channel_msg(file_hash)

    if cached_id is not None:
        logger.info(f"Cache hit for {filename} → channel_msg_id={cached_id}")
        ipc.send_response(task_id, "done", {"channel_msg_id": cached_id, "cached": True})
        return

    # ── Upload ────────────────────────────────────────────────────────────────
    file_size  = os.path.getsize(file_path)
    file_type  = _detect_file_type(file_path)
    attributes = _build_attributes(file_path, file_type)
    caption    = f"{filename}\n{file_size / 1024 / 1024:.1f} MB  |  task:{task_id}"

    ipc.send_progress(task_id, 0, status="Uploading to Telegram storage...")

    upload_start  = time.time()
    last_emit     = [0.0]   # mutable closure

    def progress_cb(sent: int, total: int) -> None:
        now = time.time()
        if now - last_emit[0] < 3:
            return
        last_emit[0] = now

        pct     = int(sent / total * 100) if total else 0
        elapsed = now - upload_start
        speed   = (sent / elapsed / 1024 / 1024) if elapsed > 0 else 0
        speed_s = f"{speed:.1f} MB/s"
        ipc.send_progress(task_id, pct, speed=speed_s, status="uploading")

    try:
        channel_msg_id = await _upload_with_retry(
            file_path, caption, attributes, file_type, progress_cb
        )
    except Exception as e:
        logger.error(f"MTProto upload failed for task {task_id}: {e}")
        ipc.send_error(task_id, f"Upload failed: {e}", "MTPROTO_UPLOAD_FAILED")
        return

    # ── Cache result ──────────────────────────────────────────────────────────
    await db.cache_channel_msg(file_hash, file_path, channel_msg_id, file_size)

    logger.info(f"Uploaded {filename} → channel_msg_id={channel_msg_id}")
    ipc.send_response(task_id, "done", {"channel_msg_id": channel_msg_id, "cached": False})
