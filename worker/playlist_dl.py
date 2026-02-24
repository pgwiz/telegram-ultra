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
    They must include the seed video + start_radio=1 to work reliably.
    """
    radio_match = re.search(r'list=(RD([a-zA-Z0-9_-]+))', url)
    if radio_match:
        full_list_id = radio_match.group(1)   # e.g. RDEgBJmlPo8Xw
        video_id     = radio_match.group(2)   # e.g. EgBJmlPo8Xw
        if 'start_radio=1' in url and f'v={video_id}' in url:
            return url
        return (
            f"https://www.youtube.com/watch?v={video_id}"
            f"&list={full_list_id}&start_radio=1"
        )
    return url


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
        total_tracks = playlist_info.get('entry_count', 0)

        logger.info(f"[{task_id}] Playlist: '{playlist_name}' ({total_tracks} tracks)")

        # Create output folder
        safe_folder_name = sanitize_folder_name(playlist_name)
        output_dir = os.path.join(params.get('output_dir', config.DOWNLOAD_DIR), safe_folder_name)
        safe_mkdir(output_dir)

        logger.info(f"[{task_id}] Output directory: {output_dir}")

        # Download all tracks
        downloaded_files = await _download_playlist_tracks(
            ipc, task_id, url, output_dir, params, total_tracks
        )

        if not downloaded_files:
            ipc.send_error(task_id, "No tracks were downloaded")
            return

        logger.info(f"[{task_id}] Downloaded {len(downloaded_files)} tracks")
        ipc.send_progress(task_id, 95, status='finalizing')

        # Return individual files instead of archives for easier sending to Telegram
        files = []
        for file_path in downloaded_files:
            try:
                file_size = os.path.getsize(file_path)
                files.append({
                    'path': file_path,
                    'name': os.path.basename(file_path),
                    'size_mb': round(file_size / (1024 * 1024), 2)
                })
            except OSError as e:
                logger.warning(f"Could not stat file {file_path}: {e}")

        if not files:
            logger.warning(f"[{task_id}] No files available to send")
            ipc.send_response(task_id, 'done', {
                'playlist_name': playlist_name,
                'total_tracks_downloaded': len(downloaded_files),
                'files': [],
                'folder_path': output_dir,
                'warning': 'Tracks downloaded but no files available'
            })
            return

        logger.info(f"[{task_id}] Prepared {len(files)} file(s) for sending")

        ipc.send_progress(task_id, 100, status='completed')

        ipc.send_response(task_id, 'done', {
            'playlist_name': playlist_name,
            'total_tracks_downloaded': len(downloaded_files),
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
                                   params: dict, total_tracks: int) -> List[str]:
    """Download all tracks in playlist."""
    try:
        extract_audio = params.get('extract_audio', False)
        audio_format = params.get('audio_format', 'mp3')

        # Build yt-dlp command for playlist
        command = [
            sys.executable, '-m', 'yt_dlp',
            url,
            '--no-cache-dir',
            '--ignore-errors',
            '--socket-timeout', '10',
            '--progress-template', '[download] %(progress._percent_str)s at %(progress._speed_str)s',
        ]

        # Limit tracks for Radio Mixes (infinite) and large playlists
        playlist_end = params.get('playlist_end', 50)
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

        # Archive file for deduplication (skip already-downloaded tracks on re-run)
        archive_path = params.get('archive_file')
        if archive_path:
            safe_mkdir(os.path.dirname(archive_path))
            command.extend(['--download-archive', archive_path])

        # Output template - use playlist_index to preserve original YouTube track numbers
        output_template = os.path.join(output_dir, '%(playlist_index)03d - %(title)s.%(ext)s')
        command.extend(['-o', output_template])

        # Cookies
        cookie_args = get_yt_dlp_cookie_args()
        command.extend(cookie_args)

        # android client doesn't support cookies — use web-only when cookies are present
        player_clients = 'web' if cookie_args else 'android,web'
        command.extend(['--extractor-args', f'youtube:player_client={player_clients}'])

        # JS runtime for signature/n-challenge solving
        node_bin = find_node_binary()
        if node_bin:
            command.extend(['--js-runtimes', f'node:{node_bin}'])
            command.extend(['--remote-components', 'ejs:github'])

        logger.debug(f"[{task_id}] Playlist download command: {command[0]} ... (length: {len(command)})")

        # Execute
        process = await asyncio.create_subprocess_exec(
            *command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )

        progress_collector = StreamProgressCollector()
        track_number = 0

        # Read progress
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

        await read_output()
        await process.wait()

        # Find downloaded files
        downloaded_files = []
        if os.path.exists(output_dir):
            for filename in os.listdir(output_dir):
                if filename.endswith(('.mp3', '.m4a', '.mp4', '.webm')):
                    filepath = os.path.join(output_dir, filename)
                    if os.path.getsize(filepath) > 0:
                        downloaded_files.append(filepath)

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
