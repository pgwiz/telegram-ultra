"""
YouTube search handler
Searches YouTube and returns results via IPC
Includes caching to minimize API calls
"""

import sys
import json
import subprocess
import logging
import asyncio
from typing import List, Dict, Any
from worker.config import config
from worker.ipc import IPCHandler
from worker.cookies import get_yt_dlp_cookie_args
from worker.utils import validate_search_query, find_node_binary
from worker.error_handlers import categorize_error, get_error
from worker.cache import SearchCache, MetadataCache


logger = logging.getLogger(__name__)


async def handle_youtube_search(ipc: IPCHandler, task_id: str, request: dict) -> None:
    """
    Search YouTube and return results.

    IPC Request format:
    {
        "task_id": "uuid",
        "action": "youtube_search",
        "params": {
            "query": "lo-fi beats",
            "limit": 5
        }
    }

    Response format (sent once search completes):
    {
        "task_id": "uuid",
        "event": "search_results",
        "data": {
            "results": [
                {
                    "videoId": "dQw4w9WgXcQ",
                    "title": "Video Title",
                    "artist": "Channel Name",
                    "duration": "1:23:45",
                    "thumbnail": "https://...",
                    "url": "https://youtube.com/watch?v=..."
                }
            ],
            "query": "lo-fi beats",
            "total_results": 5
        }
    }

    Args:
        ipc: IPC handler for responses
        task_id: Task identifier
        request: IPC request dictionary
    """
    try:
        params = request.get('params', {})
        query = params.get('query', '').strip()
        limit = params.get('limit', 5)

        if not query:
            ipc.send_error(task_id, "Missing 'query' parameter")
            return

        # Validate query
        if not validate_search_query(query):
            ipc.send_error(task_id, "Invalid search query (too long or contains invalid characters)")
            return

        limit = min(limit, 20)  # Cap at 20 results
        limit = max(limit, 1)   # Min 1 result

        logger.info(f"[{task_id}] Searching YouTube: {query} (limit: {limit})")
        ipc.send_progress(task_id, 0, status='searching')

        # Check cache first
        cached_results = await SearchCache.get(query)
        if cached_results is not None:
            logger.info(f"[{task_id}] Using cached results for '{query}'")
            ipc.send_progress(task_id, 100, status='completed')
            ipc.send_response(task_id, 'search_results', {
                'results': cached_results[:limit],
                'query': query,
                'total_results': len(cached_results[:limit]),
                'from_cache': True,
            })
            return

        # Execute search
        results = await _search_youtube(task_id, query, limit)

        if results is None:
            error = get_error('UNKNOWN_ERROR')
            ipc.send_error(task_id, error.user_message, error.code)
            return

        logger.info(f"[{task_id}] Found {len(results)} results for '{query}'")

        # Cache the results
        await SearchCache.set(query, results)

        ipc.send_response(task_id, 'search_results', {
            'results': results,
            'query': query,
            'total_results': len(results),
            'from_cache': False,
        })

    except Exception as e:
        error = categorize_error(e)
        logger.error(f"[{task_id}] Search failed: {error.user_message}", exc_info=True)
        ipc.send_error(task_id, error.user_message, error.code)


async def _search_youtube(task_id: str, query: str, limit: int) -> List[Dict[str, Any]]:
    """
    Execute YouTube search via yt-dlp.

    Args:
        task_id: Task ID for logging
        query: Search query
        limit: Number of results

    Returns:
        List of result dictionaries, or None on error
    """
    try:
        # Build yt-dlp command
        search_query = f'ytsearch{limit}:{query}'

        command = [
            sys.executable, '-m', 'yt_dlp',
            search_query,
            '--dump-single-json',
            '--flat-playlist',
            '--no-cache-dir',
        ]

        # Add cookies
        cookie_args = get_yt_dlp_cookie_args()
        command.extend(cookie_args)

        logger.debug(f"[{task_id}] Search command: {command[0]} ... (length: {len(command)})")

        # Execute
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
            logger.error(f"[{task_id}] yt-dlp search failed: {stderr[:200]}")
            if 'confirm you' in stderr.lower() or 'bot' in stderr.lower():
                return None  # Bot detection
            return None

        # Parse results
        try:
            data = json.loads(stdout)
        except json.JSONDecodeError:
            logger.error(f"[{task_id}] Failed to parse yt-dlp JSON output")
            return None

        # Extract entries
        entries = data.get('entries', [])
        if not entries:
            logger.warning(f"[{task_id}] No entries in search results")
            return []

        # Format results
        results = []
        for entry in entries[:limit]:
            if not entry:
                continue

            try:
                video_id = entry.get('id', '')
                if not video_id:
                    continue

                result = {
                    'videoId': video_id,
                    'title': entry.get('title', 'Untitled'),
                    'artist': entry.get('uploader', 'Unknown'),
                    'duration': entry.get('duration_string', 'Unknown'),
                    'thumbnail': entry.get('thumbnail', _generate_thumbnail_url(video_id)),
                    'url': f"https://www.youtube.com/watch?v={video_id}",
                }

                results.append(result)

            except Exception as e:
                logger.warning(f"[{task_id}] Error formatting result entry: {e}")
                continue

        logger.debug(f"[{task_id}] Formatted {len(results)} results")
        return results

    except asyncio.TimeoutError:
        logger.error(f"[{task_id}] Search timeout after {config.YT_TIMEOUT}s")
        return None

    except Exception as e:
        logger.error(f"[{task_id}] Search exception: {e}", exc_info=True)
        return None


def _generate_thumbnail_url(video_id: str) -> str:
    """Generate YouTube thumbnail URL from video ID."""
    # Use medium quality thumbnail
    return f"https://img.youtube.com/vi/{video_id}/mqdefault.jpg"


async def handle_get_video_info(ipc: IPCHandler, task_id: str, request: dict) -> None:
    """
    Get video information (metadata).

    IPC Request format:
    {
        "task_id": "uuid",
        "action": "get_video_info",
        "url": "https://www.youtube.com/watch?v=..."
    }

    Uses caching to avoid repeated YouTube API calls.

    Args:
        ipc: IPC handler
        task_id: Task ID
        request: Request dictionary
    """
    try:
        url = request.get('url', '').strip()

        if not url:
            ipc.send_error(task_id, "Missing 'url' parameter", 'INVALID_URL')
            return

        # Extract video ID from URL
        video_id = None
        if 'watch?v=' in url:
            video_id = url.split('watch?v=')[1].split('&')[0]
        elif 'youtu.be/' in url:
            video_id = url.split('youtu.be/')[1].split('?')[0]

        # Check cache first
        if video_id:
            cached_info = await MetadataCache.get(video_id)
            if cached_info:
                logger.info(f"[{task_id}] Using cached info for video {video_id[:8]}...")
                ipc.send_response(task_id, 'video_info', {
                    'videoId': cached_info['video_id'],
                    'title': cached_info['title'],
                    'artist': cached_info['uploader'],
                    'duration': cached_info['duration_seconds'] or 0,
                    'duration_string': _format_duration(cached_info['duration_seconds'] or 0),
                    'thumbnail': cached_info['thumbnail_url'],
                    'description': '',
                    'is_age_restricted': cached_info['is_age_restricted'],
                    'is_private': cached_info['is_private'],
                    'from_cache': True,
                })
                return

        logger.info(f"[{task_id}] Fetching video info: {url[:50]}...")

        command = [
            sys.executable, '-m', 'yt_dlp',
            url,
            '--dump-single-json',
            '--no-cache-dir',
        ]

        # Add cookies
        cookie_args = get_yt_dlp_cookie_args()
        command.extend(cookie_args)

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
            logger.error(f"[{task_id}] Failed to get video info: {stderr[:200]}")
            ipc.send_error(task_id, "Failed to fetch video information")
            return

        data = json.loads(stdout)

        # Extract relevant info
        vid_id = data.get('id', '')
        info = {
            'videoId': vid_id,
            'title': data.get('title', 'Untitled'),
            'artist': data.get('uploader', 'Unknown'),
            'duration': data.get('duration', 0),
            'duration_string': _format_duration(data.get('duration', 0)),
            'thumbnail': data.get('thumbnail', ''),
            'description': data.get('description', ''),
            'is_age_restricted': data.get('age_limit', 0) > 0,
            'is_private': 'private' in data.get('availability', '').lower(),
            'from_cache': False,
        }

        # Cache the metadata
        await MetadataCache.set(vid_id, {
            'title': info['title'],
            'uploader': info['artist'],
            'duration_seconds': info['duration'],
            'thumbnail_url': info['thumbnail'],
            'is_age_restricted': info['is_age_restricted'],
            'is_private': info['is_private'],
        })

        ipc.send_response(task_id, 'video_info', info)
        logger.info(f"[{task_id}] Video info retrieved: {info['title']}")

    except asyncio.TimeoutError:
        error = get_error('NETWORK_TIMEOUT')
        ipc.send_error(task_id, error.user_message, error.code)

    except Exception as e:
        error = categorize_error(e)
        logger.error(f"[{task_id}] Get video info failed: {error.user_message}", exc_info=True)
        ipc.send_error(task_id, error.user_message, error.code)


async def handle_get_formats(ipc: IPCHandler, task_id: str, request: dict) -> None:
    """
    Get available download formats for a video.

    IPC Request format:
    {
        "task_id": "uuid",
        "action": "get_formats",
        "url": "https://www.youtube.com/watch?v=...",
        "params": {
            "mode": "video" | "audio"
        }
    }

    Response: grouped format tiers suitable for inline keyboard display.
    """
    try:
        url = request.get('url', '').strip()
        params = request.get('params', {})
        mode = params.get('mode', 'video')

        if not url:
            ipc.send_error(task_id, "Missing 'url' parameter", 'INVALID_URL')
            return

        logger.info(f"[{task_id}] Fetching formats for: {url[:50]}... (mode={mode})")

        command = [
            sys.executable, '-m', 'yt_dlp',
            url,
            '--dump-single-json',
            '--no-cache-dir',
        ]

        cookie_args = get_yt_dlp_cookie_args()
        command.extend(cookie_args)

        # android client bypasses bot detection but doesn't support cookies —
        # use web-only when cookies are present, android+web otherwise
        player_clients = 'web' if cookie_args else 'android,web'
        command.extend(['--extractor-args', f'youtube:player_client={player_clients}'])

        # JS runtime for signature/n-challenge solving
        node_bin = find_node_binary()
        if node_bin:
            command.extend(['--js-runtimes', f'node:{node_bin}'])

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
            logger.error(f"[{task_id}] Failed to get formats: {stderr[:200]}")
            ipc.send_error(task_id, "Failed to fetch video formats")
            return

        data = json.loads(stdout)
        raw_formats = data.get('formats', [])
        title = data.get('title', 'Untitled')
        duration = data.get('duration', 0)
        thumbnail = data.get('thumbnail', '')

        if mode == 'video':
            grouped = _group_video_formats(raw_formats)
        else:
            grouped = _group_audio_formats(raw_formats)

        ipc.send_response(task_id, 'format_list', {
            'title': title,
            'duration': duration,
            'duration_string': _format_duration(duration),
            'thumbnail': thumbnail,
            'mode': mode,
            'formats': grouped,
        })
        logger.info(f"[{task_id}] Returned {len(grouped)} format options ({mode} mode)")

    except asyncio.TimeoutError:
        error = get_error('NETWORK_TIMEOUT')
        ipc.send_error(task_id, error.user_message, error.code)

    except Exception as e:
        error = categorize_error(e)
        logger.error(f"[{task_id}] Get formats failed: {error.user_message}", exc_info=True)
        ipc.send_error(task_id, error.user_message, error.code)


def _group_video_formats(raw_formats: list) -> list:
    """Group raw yt-dlp formats into video quality tiers."""
    video_tiers = {
        '2160': {'label': '4K (2160p)', 'height': 2160},
        '1440': {'label': '2K (1440p)', 'height': 1440},
        '1080': {'label': 'Full HD (1080p)', 'height': 1080},
        '720': {'label': 'HD (720p)', 'height': 720},
        '480': {'label': 'SD (480p)', 'height': 480},
        '360': {'label': '360p', 'height': 360},
    }

    best_per_tier = {}

    for fmt in raw_formats:
        height = fmt.get('height')
        vcodec = fmt.get('vcodec', 'none')
        if not height or vcodec == 'none':
            continue

        # Map to nearest tier
        tier_key = None
        for key, info in video_tiers.items():
            if abs(height - info['height']) <= 30:
                tier_key = key
                break

        if not tier_key:
            continue

        filesize = fmt.get('filesize') or fmt.get('filesize_approx') or 0
        tbr = fmt.get('tbr') or 0

        existing = best_per_tier.get(tier_key)
        if not existing or tbr > (existing.get('tbr') or 0):
            acodec = fmt.get('acodec', 'none')
            has_audio = acodec != 'none'

            best_per_tier[tier_key] = {
                'format_id': fmt.get('format_id', ''),
                'label': video_tiers[tier_key]['label'],
                'ext': fmt.get('ext', 'mp4'),
                'filesize_approx': filesize,
                'type': 'video',
                'height': height,
                'has_audio': has_audio,
                'tbr': tbr,
            }

    # Sort by height descending and build result
    result = []
    for key in ['2160', '1440', '1080', '720', '480', '360']:
        if key in best_per_tier:
            entry = best_per_tier[key]
            size_str = _format_filesize(entry['filesize_approx'])
            if not entry['has_audio']:
                # Need to merge with best audio - use format like "video+bestaudio"
                entry['format_id'] = f"{entry['format_id']}+bestaudio"
                entry['needs_merge'] = True

            entry['label'] = f"{entry['label']} ({size_str})" if size_str else entry['label']
            del entry['tbr']
            del entry['has_audio']
            result.append(entry)

    return result


def _group_audio_formats(raw_formats: list) -> list:
    """Group raw yt-dlp formats into audio quality options."""
    # Find best native audio format
    best_audio = None
    best_abr = 0

    for fmt in raw_formats:
        vcodec = fmt.get('vcodec', 'none')
        acodec = fmt.get('acodec', 'none')
        if vcodec != 'none' or acodec == 'none':
            continue

        abr = fmt.get('abr') or fmt.get('tbr') or 0
        if abr > best_abr:
            best_abr = abr
            best_audio = fmt

    result = []

    # Best native audio
    if best_audio:
        filesize = best_audio.get('filesize') or best_audio.get('filesize_approx') or 0
        ext = best_audio.get('ext', 'webm')
        size_str = _format_filesize(filesize)
        label = f"Best Quality ({ext.upper()}, {int(best_abr)}kbps)"
        if size_str:
            label += f" ({size_str})"
        result.append({
            'format_id': best_audio.get('format_id', 'bestaudio'),
            'label': label,
            'ext': ext,
            'filesize_approx': filesize,
            'type': 'audio',
            'extract_audio': False,
        })

    # MP3 conversion options at different qualities
    for quality, kbps in [('0', '320'), ('2', '192'), ('5', '128')]:
        result.append({
            'format_id': 'bestaudio',
            'label': f'MP3 {kbps}kbps',
            'ext': 'mp3',
            'filesize_approx': 0,
            'type': 'audio',
            'extract_audio': True,
            'audio_format': 'mp3',
            'audio_quality': quality,
        })

    return result


def _format_filesize(size_bytes: int) -> str:
    """Format bytes to human-readable size."""
    if not size_bytes or size_bytes <= 0:
        return ''
    if size_bytes < 1024 * 1024:
        return f"{size_bytes / 1024:.0f}KB"
    elif size_bytes < 1024 * 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f}MB"
    else:
        return f"{size_bytes / (1024 * 1024 * 1024):.1f}GB"


def _format_duration(seconds: int) -> str:
    """Format seconds to HH:MM:SS."""
    if seconds <= 0:
        return "Unknown"

    hours = seconds // 3600
    minutes = (seconds % 3600) // 60
    secs = seconds % 60

    if hours > 0:
        return f"{hours}:{minutes:02d}:{secs:02d}"
    else:
        return f"{minutes}:{secs:02d}"
