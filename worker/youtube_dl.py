"""
YouTube single video download handler
Handles video downloads with progress tracking and error recovery
"""

import os
import subprocess
import sys
import logging
import asyncio
from typing import Optional
from worker.config import config
from worker.ipc import IPCHandler
from worker.cookies import get_yt_dlp_cookie_args
from worker.utils import sanitize_filename, safe_mkdir, file_exists_and_valid
from worker.error_handlers import categorize_error, get_error
from worker.progress_hooks import StreamProgressCollector


logger = logging.getLogger(__name__)


async def handle_youtube_download(ipc: IPCHandler, task_id: str, request: dict) -> None:
    """
    Download single YouTube video.

    IPC Request format:
    {
        "task_id": "uuid",
        "action": "youtube_dl",
        "url": "https://www.youtube.com/watch?v=...",
        "params": {
            "format": "best[ext=mp4]/best",
            "extract_audio": true,
            "audio_format": "mp3",
            "audio_quality": "192",
            "best_audio_limit_mb": 15,
            "output_dir": "/path/to/output"
        }
    }

    Args:
        ipc: IPC handler for responses
        task_id: Task identifier
        request: IPC request dictionary
    """
    try:
        # Extract parameters
        url = request.get('url', '').strip()
        params = request.get('params', {})

        if not url:
            ipc.send_error(task_id, "Missing 'url' parameter", 'INVALID_URL')
            return

        logger.info(f"[{task_id}] Starting download: {url[:50]}...")

        # Send initial progress
        ipc.send_progress(task_id, 0, status='preparing')

        # Build yt-dlp command
        command = [sys.executable, '-m', 'yt_dlp', url]

        # Audio extraction
        extract_audio = params.get('extract_audio', False)
        if extract_audio:
            audio_format = params.get('audio_format', 'mp3')
            audio_quality = params.get('audio_quality', '192')

            # Format: prefer bestaudio within size limit, fallback to bestaudio
            best_audio_limit = params.get('best_audio_limit_mb', config.BEST_AUDIO_LIMIT_MB)
            if best_audio_limit:
                command.extend(['-f', f'bestaudio[filesize<{best_audio_limit}M]/bestaudio'])
            else:
                command.extend(['-f', 'bestaudio'])

            command.extend(['-x', '--audio-format', audio_format])
            if audio_quality:
                command.extend(['--audio-quality', audio_quality])
        else:
            # Video format selection
            format_str = params.get('format', 'best[ext=mp4]/best')
            command.extend(['-f', format_str])
            # When merging separate video+audio streams, output as mp4
            if '+' in format_str:
                command.extend(['--merge-output-format', 'mp4'])

        # Output directory
        output_dir = params.get('output_dir', config.DOWNLOAD_DIR)
        safe_mkdir(output_dir)

        # Output template - use title with sanitization
        output_template = os.path.join(output_dir, '%(title)s.%(ext)s')
        command.extend(['-o', output_template])

        # Cookie handling
        cookie_args = get_yt_dlp_cookie_args()
        command.extend(cookie_args)

        # Other flags
        command.extend([
            '--no-cache-dir',
            '--no-check-certificate',
            # Use android + web player clients — android API bypasses VPS bot detection
            '--extractor-args', 'youtube:player_client=android,web',
            '--progress-template', '[download] %(progress._percent_str)s at %(progress._speed_str)s ETA %(progress._eta_str)s',
        ])

        logger.debug(f"[{task_id}] Command: {' '.join(command[:3])} ... (length: {len(command)})")

        # Execute download
        await _execute_download(ipc, task_id, command, output_dir, extract_audio)

    except Exception as e:
        error = categorize_error(e)
        logger.error(f"[{task_id}] Download failed: {error.user_message}", exc_info=True)
        ipc.send_error(task_id, error.user_message, error.code)


async def _execute_download(ipc: IPCHandler, task_id: str, command: list, output_dir: str,
                           extract_audio: bool) -> None:
    """
    Execute yt-dlp subprocess with progress tracking.

    Args:
        ipc: IPC handler
        task_id: Task ID
        command: yt-dlp command
        output_dir: Output directory
        extract_audio: Whether audio extraction is happening
    """
    try:
        # Start subprocess
        process = await asyncio.create_subprocess_exec(
            *command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )

        progress_collector = StreamProgressCollector()
        destination_file = None
        has_error = False
        error_message = None
        stderr_lines = []  # collect all yt-dlp output for error reporting

        # Read stderr for progress
        async def read_progress():
            nonlocal destination_file, has_error, error_message

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

                    # Parse progress
                    result = progress_collector.process_line(line)

                    if result:
                        if 'progress' in result:
                            data = result['progress']
                            ipc.send_progress(
                                task_id,
                                data['percent'],
                                speed=data['speed'],
                                eta_seconds=data['eta'],
                                status=data['status']
                            )

                        if 'destination' in result:
                            destination_file = result['destination']
                            logger.info(f"[{task_id}] Destination file: {destination_file}")

                        if 'error' in result:
                            has_error = True
                            error_message = result['error']
                            logger.error(f"[{task_id}] yt-dlp error: {error_message}")

                        if 'done' in result:
                            ipc.send_progress(task_id, 100, status='completed')

            except asyncio.TimeoutError:
                error = get_error('NETWORK_TIMEOUT')
                logger.error(f"[{task_id}] Timeout reading progress: {error.user_message}")
                has_error = True
                error_message = error.user_message

            except Exception as e:
                logger.error(f"[{task_id}] Error reading progress: {e}", exc_info=True)

        # Run progress reader
        try:
            await asyncio.wait_for(read_progress(), timeout=config.IPC_TIMEOUT)
        except asyncio.TimeoutError:
            error = get_error('NETWORK_TIMEOUT')
            logger.error(f"[{task_id}] Download timeout: {error.user_message}")
            process.kill()
            ipc.send_error(task_id, error.user_message, error.code)
            return

        # Wait for process to complete
        returncode = await process.wait()

        if returncode != 0:
            # Always log full yt-dlp output so the real error is visible
            if stderr_lines:
                logger.error(f"[{task_id}] yt-dlp exited with code {returncode}. Full output:")
                for l in stderr_lines:
                    logger.error(f"[{task_id}]   {l}")
            else:
                logger.error(f"[{task_id}] yt-dlp exited with code {returncode} (no output)")
            if has_error and error_message:
                ipc.send_error(task_id, error_message)
            else:
                # Check for known yt-dlp error patterns in the full output
                all_output = ' '.join(stderr_lines).lower()
                if 'sign in to confirm' in all_output or 'confirm you\'re not a bot' in all_output:
                    error = get_error('BOT_DETECTION')
                elif 'private video' in all_output or 'video is private' in all_output:
                    error = get_error('VIDEO_PRIVATE')
                elif 'video unavailable' in all_output or 'has been removed' in all_output:
                    error = get_error('VIDEO_REMOVED')
                elif 'no suitable format' in all_output:
                    error = get_error('NO_SUITABLE_FORMAT')
                else:
                    ytdlp_msg = next((l for l in reversed(stderr_lines) if 'ERROR' in l), None)
                    if ytdlp_msg:
                        logger.error(f"[{task_id}] yt-dlp error: {ytdlp_msg}")
                    error = get_error('UNKNOWN_ERROR')
                ipc.send_error(task_id, error.user_message, error.code)
            return

        # Find downloaded file
        if destination_file and os.path.exists(destination_file):
            file_size = os.path.getsize(destination_file)
            logger.info(f"[{task_id}] Download completed: {os.path.basename(destination_file)} ({file_size} bytes)")

            ipc.send_response(task_id, 'done', {
                'file_path': destination_file,
                'file_size': file_size,
                'filename': os.path.basename(destination_file),
            })
        else:
            # Fallback: scan output directory for the newest matching file
            found_file = _find_newest_media_file(output_dir)
            if found_file:
                file_size = os.path.getsize(found_file)
                logger.info(f"[{task_id}] Download completed (fallback): {os.path.basename(found_file)} ({file_size} bytes)")
                ipc.send_response(task_id, 'done', {
                    'file_path': found_file,
                    'file_size': file_size,
                    'filename': os.path.basename(found_file),
                })
            else:
                logger.error(f"[{task_id}] Downloaded file not found at {destination_file}")
                ipc.send_error(task_id, "Downloaded file not found", 'FILE_NOT_FOUND')

    except Exception as e:
        error = categorize_error(e)
        logger.error(f"[{task_id}] Execution error: {error.user_message}", exc_info=True)
        ipc.send_error(task_id, error.user_message, error.code)


def _find_newest_media_file(output_dir: str) -> Optional[str]:
    """Find the most recently modified media file in the output directory."""
    media_extensions = ('.mp3', '.m4a', '.mp4', '.webm', '.opus', '.ogg', '.wav', '.flac', '.mkv')
    newest_file = None
    newest_mtime = 0

    try:
        for filename in os.listdir(output_dir):
            if filename.lower().endswith(media_extensions):
                filepath = os.path.join(output_dir, filename)
                mtime = os.path.getmtime(filepath)
                if mtime > newest_mtime and os.path.getsize(filepath) > 0:
                    newest_mtime = mtime
                    newest_file = filepath
    except OSError as e:
        logger.error(f"Error scanning output directory: {e}")

    return newest_file
