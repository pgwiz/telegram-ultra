"""
Playlist download handler
Handles YouTube playlist batch downloads with named folders and archives
"""

import os
import sys
import json
import subprocess
import logging
import asyncio
import zipfile
import re
from typing import List, Dict, Any, Optional
from worker.config import config
from worker.ipc import IPCHandler
from worker.cookies import get_yt_dlp_cookie_args
from worker.utils import sanitize_filename, sanitize_folder_name, safe_mkdir, safe_rmtree, find_node_binary
from worker.error_handlers import categorize_error, get_error
from worker.progress_hooks import StreamProgressCollector
from worker.storage import StorageManager


logger = logging.getLogger(__name__)


# Format fallback chains for yt-dlp (audio/video modes)
AUDIO_FORMAT = "bestaudio[ext=m4a]/bestaudio[ext=webm]/bestaudio/best"
VIDEO_FORMAT = (
    "bestvideo[height<=1080][ext=mp4]+bestaudio[ext=m4a]"
    "/bestvideo[height<=1080]+bestaudio"
    "/best[height<=1080]/best"
)


def normalize_playlist_url(url: str) -> str:
    """
    Normalize YouTube playlist URLs for yt-dlp compatibility.

    Radio Mix URLs (list=RD...) expire when used as a plain playlist URL.
    They MUST be in the form: watch?v={seed}&list=RD{seed}&start_radio=1

    Handles:
      watch?v=X&list=RDY           → watch?v=X&list=RDX&start_radio=1  (fix truncated list ID)
      playlist?list=RDX            → watch?v=X&list=RDX&start_radio=1  (fix broken playlist form)
      watch?v=X&list=RDX&start_radio=1  → unchanged (already correct)
    """
    radio_match = re.search(r'list=(RD([a-zA-Z0-9_-]+))', url)
    if not radio_match:
        return url

    list_id = radio_match.group(1)    # e.g. RDEgBJmlPo8Xw or RDEgBJmlPo
    list_suffix = radio_match.group(2)  # e.g. EgBJmlPo8Xw or EgBJmlPo

    # Preserve special Radio Mix types (My Mix, Artist Mix, Album Radio)
    if list_id.startswith(('RDMM', 'RDAM', 'RDCLAK')):
        return url

    # Determine the seed video ID:
    # 1. Prefer the v= parameter (always 11 chars, most reliable)
    # 2. Fall back to stripping RD prefix from list ID (only if 11 chars)
    video_match = re.search(r'v=([a-zA-Z0-9_-]{11})', url)
    if video_match:
        video_id = video_match.group(1)
    elif len(list_suffix) == 11:
        video_id = list_suffix
    else:
        return url  # Can't determine seed video, return as-is

    # Always reconstruct: full video ID in both v= and list=
    return (
        f"https://www.youtube.com/watch?v={video_id}"
        f"&list=RD{video_id}&start_radio=1"
    )


async def _validate_archive(archive_path: str, task_id: str) -> int:
    """Remove stale archive entries where the pool file no longer exists on disk.

    Also cleans the corresponding file_storage DB rows so the track can be
    re-downloaded and re-tracked correctly.

    Returns the number of entries removed.
    """
    if not os.path.exists(archive_path):
        return 0

    with open(archive_path, 'r') as f:
        lines = f.readlines()

    if not lines:
        return 0

    from worker.database import get_database
    db = await get_database()

    valid = []
    removed = 0

    for line in lines:
        parts = line.strip().split()
        if len(parts) < 2:
            valid.append(line)
            continue

        video_id = parts[1]

        # Check file_storage for a pool file matching this video ID
        cursor = await db.execute(
            'SELECT physical_path FROM file_storage WHERE youtube_url LIKE ?',
            (f'%{video_id}%',)
        )
        row = await cursor.fetchone()

        if row and row[0] and os.path.exists(row[0]):
            valid.append(line)  # DB record exists, pool file exists — keep
        elif row and row[0]:
            # DB record exists but pool file is missing — truly stale
            removed += 1
            logger.info(f"[{task_id}] Stale archive entry (file gone): {video_id}")
            # Clean DB record so re-download gets fresh tracking
            await db.execute(
                'DELETE FROM user_symlinks WHERE file_hash_sha1 IN '
                '(SELECT file_hash_sha1 FROM file_storage WHERE youtube_url LIKE ?)',
                (f'%{video_id}%',)
            )
            await db.execute(
                'DELETE FROM file_storage WHERE youtube_url LIKE ?',
                (f'%{video_id}%',)
            )
            await db.commit()
        else:
            # No DB match — can't verify, keep entry to be safe
            # (playlist tracks store the playlist URL, not individual video URLs,
            #  so the LIKE '%VIDEO_ID%' query may not match)
            valid.append(line)
            logger.debug(f"[{task_id}] Archive entry kept (no DB match): {video_id}")

    if removed:
        with open(archive_path, 'w') as f:
            f.writelines(valid)
        logger.info(f"[{task_id}] Archive cleaned: {removed} stale, {len(valid)} valid entries remain")

    return removed


async def handle_playlist_download(ipc: IPCHandler, task_id: str, request: dict) -> None:
    """
    Download YouTube playlist.

    IPC Request format:
    {
        "task_id": "uuid",
        "action": "playlist",
        "url": "https://www.youtube.com/playlist?list=...",
        "params": {
            "extract_audio": true,
            "audio_format": "mp3",
            "output_dir": "/path/to/output",
            "archive_max_size_mb": 100
        }
    }

    Response format:
    {
        "task_id": "uuid",
        "event": "done",
        "data": {
            "playlist_name": "My Playlist",
            "total_tracks_downloaded": 25,
            "archives": [
                {"name": "Playlist - My Playlist-part01.zip", "size_mb": 99.5},
                {"name": "Playlist - My Playlist-part02.zip", "size_mb": 87.3}
            ],
            "folder_path": "/downloads/playlists/My Playlist"
        }
    }

    Args:
        ipc: IPC handler for responses
        task_id: Task identifier
        request: IPC request dictionary
    """
    try:
        url = request.get('url', '').strip()
        params = request.get('params', {})

        if not url:
            ipc.send_error(task_id, "Missing 'url' parameter", 'INVALID_URL')
            return

        # Normalize Radio Mix URLs (list=RD...) before passing to yt-dlp
        url = normalize_playlist_url(url)
        logger.info(f"[{task_id}] Normalized URL: {url}")

        logger.info(f"[{task_id}] Starting playlist download: {url[:50]}...")
        ipc.send_progress(task_id, 0, status='preparing')

        # First, get playlist info
        try:
            playlist_info = await _get_playlist_info(task_id, url)
        except RuntimeError as e:
            ipc.send_error(task_id, str(e), 'PLAYLIST_ERROR')
            return

        if not playlist_info:
            error = get_error('UNKNOWN_ERROR')
            ipc.send_error(task_id, error.user_message, error.code)
            return

        playlist_name = playlist_info.get('title', 'Playlist')
        entries = playlist_info.get('entries', [])
        info_type = playlist_info.get('_type', 'unknown')
        # yt-dlp uses 'playlist_count' for total; fall back to len(entries)
        total_tracks = playlist_info.get('playlist_count') or len(entries) or playlist_info.get('entry_count', 0)

        logger.info(f"[{task_id}] Playlist: '{playlist_name}' (type={info_type}, {total_tracks} tracks, {len(entries)} entries)")

        # Pre-scan: check flat playlist entries against archive to report skipped/cached tracks
        archive_path = params.get('archive_file')
        skipped_count = 0
        cached_files = []  # Pool file paths for already-archived tracks

        # Validate archive: remove entries whose pool files were deleted
        if archive_path and os.path.exists(archive_path):
            cleaned = await _validate_archive(archive_path, task_id)
            if cleaned:
                logger.info(f"[{task_id}] Removed {cleaned} stale archive entries (files were deleted)")

        if archive_path and os.path.exists(archive_path) and entries:
            archived_ids = set()
            try:
                with open(archive_path, 'r') as f:
                    for line in f:
                        parts = line.strip().split()
                        if len(parts) >= 2:
                            archived_ids.add(parts[1])  # "youtube VIDEO_ID"
            except OSError as e:
                logger.warning(f"[{task_id}] Could not read archive: {e}")

            # Collect cached video IDs and look up their pool file paths
            # Only check the entries yt-dlp will process (respecting playlist_end limit)
            from worker.database import get_database
            db = await get_database()
            playlist_end = params.get('playlist_end')
            scan_entries = entries[:playlist_end] if playlist_end else entries
            unfindable_ids = set()  # Track IDs we can't locate — will remove from archive
            for entry in scan_entries:
                vid_id = entry.get('id', '')
                if vid_id in archived_ids:
                    skipped_count += 1
                    # Try to find existing pool file for this track
                    try:
                        cursor = await db.execute(
                            'SELECT physical_path, title FROM file_storage WHERE youtube_url LIKE ?',
                            (f'%{vid_id}%',)
                        )
                        row = await cursor.fetchone()
                        if row and row[0] and os.path.exists(row[0]):
                            cached_files.append({'path': row[0], 'title': row[1] or entry.get('title', '')})
                            logger.info(f"[{task_id}] Cached track found: {vid_id} -> {row[0]}")
                        else:
                            # Pool file not findable by video ID — remove from archive
                            # so yt-dlp re-downloads it (will get correct individual URL this time)
                            unfindable_ids.add(vid_id)
                            logger.info(f"[{task_id}] Cached track {vid_id}: not in DB, removing from archive for re-download")
                    except Exception as e:
                        logger.warning(f"[{task_id}] DB lookup failed for cached track {vid_id}: {e}")

            # Remove unfindable entries from archive so yt-dlp re-downloads them
            if unfindable_ids and archive_path:
                skipped_count -= len(unfindable_ids)  # These are no longer "skipped"
                try:
                    with open(archive_path, 'r') as f:
                        lines = f.readlines()
                    kept = []
                    for line in lines:
                        parts = line.strip().split()
                        if len(parts) >= 2 and parts[1] in unfindable_ids:
                            continue  # Drop this entry
                        kept.append(line)
                    with open(archive_path, 'w') as f:
                        f.writelines(kept)
                    logger.info(f"[{task_id}] Removed {len(unfindable_ids)} unfindable entries from archive for re-download")
                except OSError as e:
                    logger.warning(f"[{task_id}] Failed to clean archive: {e}")

            if skipped_count:
                logger.info(f"[{task_id}] Pre-scan: {skipped_count}/{len(scan_entries)} in archive, {len(cached_files)} pool files found")
                ipc.send_progress(task_id, 5, status=f'pre_scan:{skipped_count}_cached')

        # Short-circuit: all requested tracks are already downloaded AND we found all their files
        playlist_end = params.get('playlist_end')
        effective_count = min(len(entries), playlist_end) if (playlist_end and entries) else len(entries)
        if entries and skipped_count >= effective_count and len(cached_files) >= effective_count:
            logger.info(f"[{task_id}] All {skipped_count} tracks already cached with files, sending {len(cached_files)} existing files")
            files = []
            for cf in cached_files:
                try:
                    fp = cf['path']
                    title = cf.get('title', '')
                    file_ext = os.path.splitext(fp)[1] or '.mp3'
                    display_name = f"{title}{file_ext}" if title else os.path.basename(fp)
                    file_size = os.path.getsize(fp)
                    files.append({
                        'path': fp,
                        'name': display_name,
                        'size_mb': round(file_size / (1024 * 1024), 2),
                        'cached': True,
                    })
                except OSError:
                    pass
            ipc.send_response(task_id, 'done', {
                'playlist_name': playlist_name,
                'total_tracks_downloaded': 0,
                'already_cached': skipped_count,
                'files': files,
                'folder_path': '',
            })
            return

        # Create output folder
        safe_folder_name = sanitize_folder_name(playlist_name)
        output_dir = os.path.join(params.get('output_dir', config.DOWNLOAD_DIR), safe_folder_name)
        safe_mkdir(output_dir)

        logger.info(f"[{task_id}] Output directory: {output_dir}")

        # Download all tracks
        # Extract user_chat_id from request if available for deduplication
        user_chat_id = request.get('user_chat_id') or params.get('user_chat_id')
        database = params.get('database')  # Optional: database instance for dedup

        downloaded_files = await _download_playlist_tracks(
            ipc, task_id, url, output_dir, params, total_tracks, database, user_chat_id
        )

        # Merge cached files (from pre-scan) with newly downloaded files
        cached_count = len(cached_files)
        new_count = len(downloaded_files) if downloaded_files else 0

        if not cached_files and not downloaded_files:
            ipc.send_error(task_id, "No tracks were downloaded and no cached files found")
            return

        # Log path confirmation for all files
        logger.info(f"[{task_id}] File summary: {cached_count} cached + {new_count} new = {cached_count + new_count} total")
        for cf in cached_files:
            fp = cf['path']
            logger.info(f"[{task_id}]   [CACHED] {cf.get('title', '?')} -> {fp} (exists={os.path.exists(fp)})")
        if downloaded_files:
            for fp in downloaded_files:
                logger.info(f"[{task_id}]   [NEW] {os.path.basename(fp)} -> {fp} (exists={os.path.exists(fp)})")

        ipc.send_progress(task_id, 95, status='finalizing')

        # Return individual files instead of archives for easier sending to Telegram
        files = []
        # Add cached files with proper display names
        for cf in cached_files:
            try:
                fp = cf['path']
                if not os.path.exists(fp):
                    logger.warning(f"[{task_id}] Cached file missing, skipping: {fp}")
                    continue
                title = cf.get('title', '')
                file_ext = os.path.splitext(fp)[1] or '.mp3'
                display_name = f"{title}{file_ext}" if title else os.path.basename(fp)
                file_size = os.path.getsize(fp)
                files.append({
                    'path': fp,
                    'name': display_name,
                    'size_mb': round(file_size / (1024 * 1024), 2),
                    'cached': True,
                })
            except OSError as e:
                logger.warning(f"Could not stat cached file {cf.get('path', '?')}: {e}")
        # Add newly downloaded files
        if downloaded_files:
            for file_path in downloaded_files:
                try:
                    if not os.path.exists(file_path):
                        logger.warning(f"[{task_id}] File missing, skipping: {file_path}")
                        continue
                    file_size = os.path.getsize(file_path)
                    files.append({
                        'path': file_path,
                        'name': os.path.basename(file_path),
                        'size_mb': round(file_size / (1024 * 1024), 2),
                        'cached': False,
                    })
                except OSError as e:
                    logger.warning(f"Could not stat file {file_path}: {e}")

        if not files:
            logger.warning(f"[{task_id}] No files available to send")
            ipc.send_response(task_id, 'done', {
                'playlist_name': playlist_name,
                'total_tracks_downloaded': new_count,
                'already_cached': cached_count,
                'files': [],
                'folder_path': output_dir,
                'warning': 'Tracks downloaded but no files available'
            })
            return

        cached_in_response = sum(1 for f in files if f.get('cached'))
        new_in_response = len(files) - cached_in_response
        logger.info(f"[{task_id}] Prepared {len(files)} file(s) for sending ({cached_in_response} cached, {new_in_response} new)")

        ipc.send_progress(task_id, 100, status='completed')

        logger.info(f"[{task_id}] Sending Done event with {len(files)} files")
        logger.debug(f"[{task_id}] Files data: {files}")

        ipc.send_response(task_id, 'done', {
            'playlist_name': playlist_name,
            'total_tracks_downloaded': new_count,
            'already_cached': cached_in_response,
            'files': files,
            'folder_path': output_dir,
        })

    except Exception as e:
        error = categorize_error(e)
        logger.error(f"[{task_id}] Playlist download failed: {error.user_message}", exc_info=True)
        ipc.send_error(task_id, error.user_message, error.code)


async def _get_playlist_info(task_id: str, url: str) -> Optional[Dict[str, Any]]:
    """Get playlist metadata."""
    try:
        command = [
            sys.executable, '-m', 'yt_dlp',
            url,
            '--yes-playlist',   # Force playlist mode (needed for v=...&list=RD... Radio Mix URLs)
            '--dump-single-json',
            '--flat-playlist',
            '--no-cache-dir',
        ]

        cookie_args = get_yt_dlp_cookie_args()
        command.extend(cookie_args)

        # android client doesn't support cookies — use web-only when cookies are present
        player_clients = 'web' if cookie_args else 'android,web'
        command.extend(['--extractor-args', f'youtube:player_client={player_clients}'])

        node_bin = find_node_binary()
        if node_bin:
            command.extend(['--js-runtimes', f'node:{node_bin}'])
            command.extend(['--remote-components', 'ejs:github'])

        process = await asyncio.create_subprocess_exec(
            *command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )

        stdout_bytes, stderr_bytes = await asyncio.wait_for(
            process.communicate(),
            timeout=config.YT_TIMEOUT
        )
        stdout = stdout_bytes.decode('utf-8', errors='replace')
        stderr = stderr_bytes.decode('utf-8', errors='replace')

        if process.returncode != 0:
            logger.error(f"[{task_id}] Failed to get playlist info: {stderr[:300]}")
            # Extract the specific YouTube/yt-dlp error message for user display
            for line in reversed(stderr.splitlines()):
                line = line.strip()
                if 'ERROR:' in line:
                    msg = line.split('ERROR:', 1)[-1].strip()
                    if msg:
                        raise RuntimeError(msg)
            raise RuntimeError("Failed to fetch playlist info")

        data = json.loads(stdout)
        return data

    except RuntimeError:
        raise
    except Exception as e:
        logger.error(f"[{task_id}] Get playlist info failed: {e}")
        raise RuntimeError(f"Failed to fetch playlist info: {e}")


async def _download_playlist_tracks(ipc: IPCHandler, task_id: str, url: str, output_dir: str,
                                   params: dict, total_tracks: int, database=None, user_chat_id=None) -> List[str]:
    """Download all tracks in playlist."""
    try:
        extract_audio = params.get('extract_audio', False)
        audio_format = params.get('audio_format', 'mp3')

        # Build yt-dlp command for playlist
        command = [
            sys.executable, '-m', 'yt_dlp',
            url,
            '--yes-playlist',   # Force playlist mode even when v= and list= both present (Radio Mix)
            '--no-cache-dir',
            '--ignore-errors',
            '--socket-timeout', '10',
            '--progress-template', '[download] %(progress._percent_str)s at %(progress._speed_str)s',
        ]

        # Limit tracks for Radio Mixes (infinite) and large playlists
        playlist_end = params.get('playlist_end', 50)
        logger.info(f"[{task_id}] Playlist params: playlist_end={playlist_end}, extract_audio={extract_audio}, archive_file={params.get('archive_file')}")
        if playlist_end:
            command.extend(['--playlist-end', str(playlist_end)])

        # Format selector with fallback chain
        if extract_audio:
            format_str = params.get('format', AUDIO_FORMAT)
        else:
            format_str = params.get('format', VIDEO_FORMAT)
        command.extend(['-f', format_str])

        # Audio extraction
        if extract_audio:
            command.extend(['-x', '--audio-format', audio_format, '--audio-quality', '0'])

        # Output template
        # Radio Mixes may lack playlist_index; use title-only naming for them
        is_radio_mix = 'list=RD' in url
        if is_radio_mix:
            output_template = os.path.join(output_dir, '%(title)s.%(ext)s')
        else:
            output_template = os.path.join(output_dir, '%(playlist_index)03d - %(title)s.%(ext)s')
        command.extend(['-o', output_template])

        # Cookies
        cookie_args = get_yt_dlp_cookie_args()
        command.extend(cookie_args)

        # android client doesn't support cookies — use web-only when cookies are present
        player_clients = 'web' if cookie_args else 'android,web'
        command.extend(['--extractor-args', f'youtube:player_client={player_clients}'])

        # Archive file for deduplication (skip already-downloaded tracks on re-run)
        archive_path = params.get('archive_file')
        if archive_path:
            safe_mkdir(os.path.dirname(archive_path))
            command.extend(['--download-archive', archive_path])
            logger.info(f"[{task_id}] Using archive file: {archive_path}")

        # JS runtime for signature/n-challenge solving
        node_bin = find_node_binary()
        if node_bin:
            command.extend(['--js-runtimes', f'node:{node_bin}'])
            command.extend(['--remote-components', 'ejs:github'])

        # Print video ID to stdout after each track finishes (for per-track DB storage)
        command.extend(['--print', 'after_move:YTDLP_ID\t%(id)s\t%(filepath)s'])

        logger.info(f"[{task_id}] Playlist download command: {len(command)} args, url={url[:60]}")

        # Execute
        process = await asyncio.create_subprocess_exec(
            *command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )

        progress_collector = StreamProgressCollector()
        track_number = 0
        stderr_lines = []  # collect all yt-dlp output for diagnostics
        # Map filepath → video_id from yt-dlp --print output
        filepath_to_video_id: Dict[str, str] = {}

        # Read stdout for video ID mapping
        async def read_stdout():
            try:
                while True:
                    line_bytes = await process.stdout.readline()
                    if not line_bytes:
                        break
                    line = line_bytes.decode('utf-8', errors='replace').strip()
                    if line.startswith('YTDLP_ID\t'):
                        parts = line.split('\t', 2)
                        if len(parts) == 3:
                            vid_id = parts[1]
                            fpath = parts[2]
                            filepath_to_video_id[fpath] = vid_id
                            logger.debug(f"[{task_id}] Track ID mapping: {vid_id} -> {os.path.basename(fpath)}")
            except Exception as e:
                logger.debug(f"[{task_id}] Stdout reader ended: {e}")

        # Read progress from stderr
        async def read_output():
            nonlocal track_number

            try:
                while process.returncode is None:
                    line_bytes = await asyncio.wait_for(
                        process.stderr.readline(),
                        timeout=config.YT_TIMEOUT
                    )

                    if not line_bytes:
                        break

                    line = line_bytes.decode('utf-8', errors='replace').strip()
                    if not line:
                        continue

                    logger.debug(f"[{task_id}] yt-dlp: {line}")
                    stderr_lines.append(line)

                    if 'Downloading' in line and 'of' in line:
                        # Extract track progress
                        match = re.search(r'(\d+)/(\d+)', line)
                        if match:
                            track_number = int(match.group(1))

                    result = progress_collector.process_line(line)

                    if result and 'progress' in result:
                        # Calculate overall progress
                        overall_percent = (track_number / max(total_tracks, 1)) * 100
                        ipc.send_progress(
                            task_id,
                            int(overall_percent),
                            status='downloading_playlist'
                        )

            except asyncio.TimeoutError:
                logger.error(f"[{task_id}] Playlist download timeout")
            except Exception as e:
                logger.error(f"[{task_id}] Error reading output: {e}")

        # Read stderr (progress) and stdout (video IDs) concurrently
        await asyncio.gather(read_output(), read_stdout())
        await process.wait()

        # Log yt-dlp exit code and any errors
        if process.returncode != 0:
            logger.warning(f"[{task_id}] yt-dlp exited with code {process.returncode}")
            for l in stderr_lines:
                if 'ERROR' in l or 'WARNING' in l:
                    logger.warning(f"[{task_id}]   {l}")

        # Find downloaded files
        downloaded_files = []
        if os.path.exists(output_dir):
            for filename in os.listdir(output_dir):
                if filename.endswith(('.mp3', '.m4a', '.mp4', '.webm')):
                    filepath = os.path.join(output_dir, filename)
                    if os.path.getsize(filepath) > 0:
                        downloaded_files.append(filepath)

        # Process files through deduplication system
        if downloaded_files:
            try:
                from worker.database import get_database
                db = await get_database()
                user_cid = params.get('user_chat_id', 0)
                if user_cid:
                    logger.info(f"[{task_id}] Processing {len(downloaded_files)} files through dedup system (ID mappings: {len(filepath_to_video_id)})")
                    storage_manager = StorageManager(config.DOWNLOAD_DIR)
                    final_paths = []
                    for temp_file in downloaded_files:
                        try:
                            # Use individual video URL if available, fall back to playlist URL
                            video_id = filepath_to_video_id.get(temp_file)
                            if video_id:
                                track_url = f"https://www.youtube.com/watch?v={video_id}"
                            else:
                                # Try matching by basename (yt-dlp may report slightly different paths)
                                basename = os.path.basename(temp_file)
                                video_id = next(
                                    (vid for fp, vid in filepath_to_video_id.items()
                                     if os.path.basename(fp) == basename),
                                    None
                                )
                                track_url = f"https://www.youtube.com/watch?v={video_id}" if video_id else url

                            success, final_path = await storage_manager.store_or_link(
                                source_file=temp_file,
                                target_path=temp_file,
                                database=db,
                                user_chat_id=user_cid,
                                youtube_url=track_url,
                                use_symlink=True,
                            )
                            final_paths.append(final_path if success else temp_file)
                        except Exception as e:
                            logger.warning(f"[{task_id}] Dedup failed for {os.path.basename(temp_file)}: {e}")
                            final_paths.append(temp_file)
                    downloaded_files = final_paths
                    logger.info(f"[{task_id}] Dedup complete: {len(final_paths)} files processed")
            except Exception as e:
                logger.warning(f"[{task_id}] Dedup processing failed, using originals: {e}")

        return sorted(downloaded_files)

    except Exception as e:
        logger.error(f"[{task_id}] Download playlist tracks failed: {e}")
        return []


def _create_split_archives(folder_path: str, playlist_name: str, max_part_size_mb: int = 100) -> List[Dict[str, Any]]:
    """
    Create split archives from downloaded files.

    Args:
        folder_path: Folder containing downloaded files
        playlist_name: Playlist name for archive naming
        max_part_size_mb: Maximum size per archive in MB

    Returns:
        List of archive info dicts
    """
    try:
        max_part_size = max_part_size_mb * 1024 * 1024

        # Get all media files
        media_files = []
        for filename in os.listdir(folder_path):
            if filename.endswith(('.mp3', '.m4a', '.mp4', '.webm')):
                filepath = os.path.join(folder_path, filename)
                if os.path.getsize(filepath) > 0:
                    media_files.append(filepath)

        if not media_files:
            logger.warning(f"No media files found in {folder_path}")
            return []

        media_files.sort()
        logger.info(f"Creating archives for {len(media_files)} files")

        # Create archives
        archives = []
        part_number = 1
        current_part_size = 0
        current_files = []

        for filepath in media_files:
            file_size = os.path.getsize(filepath)

            # If this file would exceed limit, create archive
            if current_part_size + file_size > max_part_size and current_files:
                archive_info = _create_single_archive(
                    folder_path, playlist_name, part_number, current_files
                )
                if archive_info:
                    archives.append(archive_info)

                part_number += 1
                current_part_size = 0
                current_files = []

            current_files.append(filepath)
            current_part_size += file_size

        # Create final archive
        if current_files:
            archive_info = _create_single_archive(
                folder_path, playlist_name, part_number, current_files
            )
            if archive_info:
                archives.append(archive_info)

        logger.info(f"Created {len(archives)} archive(s)")
        return archives

    except Exception as e:
        logger.error(f"Create split archives failed: {e}")
        return []


def _create_single_archive(folder_path: str, playlist_name: str, part_number: int, files: List[str]) -> Optional[Dict[str, Any]]:
    """Create single archive from files."""
    try:
        archive_name = f"Playlist - {playlist_name}-part{str(part_number).zfill(2)}.zip"
        archive_path = os.path.join(folder_path, archive_name)

        with zipfile.ZipFile(archive_path, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as zf:
            for filepath in files:
                arcname = os.path.basename(filepath)
                zf.write(filepath, arcname=arcname)

        archive_size = os.path.getsize(archive_path)
        archive_size_mb = archive_size / (1024 * 1024)

        logger.info(f"Created archive: {archive_name} ({archive_size_mb:.1f}MB)")

        return {
            'name': archive_name,
            'size_mb': round(archive_size_mb, 2),
            'path': archive_path,
        }

    except Exception as e:
        logger.error(f"Create single archive failed: {e}")
        return None
