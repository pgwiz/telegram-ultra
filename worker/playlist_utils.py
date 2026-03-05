"""Playlist utility functions for yt-dlp integration."""

import asyncio
import logging
import re
import sys
from typing import Dict, Any, Optional
from worker.config import config
from worker.cookies import get_yt_dlp_cookie_args
from worker.utils import find_node_binary

logger = logging.getLogger(__name__)


def normalize_playlist_url(url: str) -> str:
    """
    Normalize YouTube playlist URLs for yt-dlp compatibility.

    Radio Mix URLs (list=RD...) expire when used as a plain playlist URL.
    They must include the seed video + start_radio=1 to work reliably.

    Examples:
        youtube.com/playlist?list=RDEgBJmlPo8Xw
        → youtube.com/watch?v=EgBJmlPo8Xw&list=RDEgBJmlPo8Xw&start_radio=1

        youtube.com/watch?v=X&list=RDX&start_radio=1
        → unchanged (already correct)

        youtube.com/playlist?list=PLxxxxxxx
        → unchanged (regular playlist, no fix needed)
    """
    radio_match = re.search(r'list=(RD([a-zA-Z0-9_-]+))', url)
    if radio_match:
        full_list_id = radio_match.group(1)  # RDEgBJmlPo8Xw
        video_id = radio_match.group(2)      # EgBJmlPo8Xw

        # Already correct format?
        if 'start_radio=1' in url and f'v={video_id}' in url:
            return url

        # Reconstruct with seed video and start_radio param
        return (
            f"https://www.youtube.com/watch?v={video_id}"
            f"&list={full_list_id}&start_radio=1"
        )

    return url


async def get_playlist_preview(url: str, preview_count: int = 5) -> Optional[Dict[str, Any]]:
    """
    Fetch first N tracks of a playlist without downloading.

    Returns dict with:
    - playlist_title: str — name of the playlist
    - playlist_count: int — total tracks in playlist
    - tracks: list of {'index': int, 'title': str} — first N tracks

    Used by /playlist command to show preview before full download.
    """
    try:
        # Normalize URL first (fixes Radio Mix URLs)
        url = normalize_playlist_url(url)

        command = [
            sys.executable, '-m', 'yt_dlp',
            '--flat-playlist',
            '--print', '%(playlist_title)s|%(playlist_count)s',
            '--print', '%(playlist_index)s\t%(title)s',
            '--playlist-end', str(preview_count),
            '--no-cache-dir',
            url,
        ]

        # Add cookies if available
        cookie_args = get_yt_dlp_cookie_args()
        if cookie_args:
            command.extend(cookie_args)
            player_clients = 'web'
        else:
            player_clients = 'android,web'

        command.extend(['--extractor-args', f'youtube:player_client={player_clients}'])

        # Add Node.js for JS challenges
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
            error_msg = stderr.split('ERROR:')[-1].strip()[:200] if 'ERROR:' in stderr else 'Unknown error'
            logger.error(f"Playlist preview failed: {error_msg}")
            return None

        lines = stdout.strip().split('\n')
        if len(lines) < 1:
            return None

        # Parse title and count from first line
        title_line = lines[0].split('|')
        playlist_title = title_line[0] if title_line else 'Playlist'

        # Handle playlist_count which can be 'NA' if yt-dlp can't determine it
        playlist_count = 0
        if len(title_line) > 1:
            count_str = title_line[1].strip()
            try:
                playlist_count = int(count_str) if count_str and count_str != 'NA' else 0
            except ValueError:
                playlist_count = 0
                logger.warning(f"Could not parse playlist count: {count_str}")

        # Parse track list (index\ttitle format)
        tracks = []
        for line in lines[1:]:
            parts = line.split('\t')
            if len(parts) >= 2:
                try:
                    tracks.append({
                        'index': int(parts[0]),
                        'title': parts[1],
                    })
                except (ValueError, IndexError):
                    continue

        return {
            'playlist_title': playlist_title,
            'playlist_count': playlist_count,
            'tracks': tracks,
        }

    except asyncio.TimeoutError:
        logger.error("Playlist preview timeout")
        return None
    except Exception as e:
        logger.error(f"Playlist preview failed: {e}")
        return None
