#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

function readFile(p) {
  return fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n').replace(/\r/g, '\n');
}

function writeFile(p, c) {
  const withCRLF = c.replace(/\n/g, '\r\n');
  fs.writeFileSync(p, withCRLF, 'utf8');
}

function replaceOnce(content, find, replace) {
  const idx = content.indexOf(find);
  if (idx === -1) {
    console.log(`NOT FOUND: ${find.substring(0, 50)}`);
    return content;
  }
  return content.substring(0, idx) + replace + content.substring(idx + find.length);
}

// Step 1: Add format constants after imports
const PLAYLIST_DL = "e:\Backup\pgwiz\bots\telegram-ultra\worker\playlist_dl.py";
let c = readFile(PLAYLIST_DL);

// Check if already applied
if (c.includes('AUDIO_FORMAT =')) {
  console.log('✓ Format constants already added');
} else {
  console.log('Adding format constants after imports...');
  const insertPoint = 'logger = logging.getLogger(__name__)';
  const formatConstants = `
# Format fallback chains for yt-dlp
AUDIO_FORMAT = "bestaudio[ext=m4a]/bestaudio[ext=webm]/bestaudio/best"
VIDEO_FORMAT = (
    "bestvideo[height<=1080][ext=mp4]+bestaudio[ext=m4a]"
    "/bestvideo[height<=1080]+bestaudio"
    "/best[height<=1080]/best"
)

`;
  c = replaceOnce(c, insertPoint, insertPoint + '\n' + formatConstants);
}

// Step 2: Update _download_playlist_tracks to use format constants and add archive support
if (c.includes('--download-archive')) {
  console.log('✓ Archive support already added');
} else {
  console.log('Adding archive support to _download_playlist_tracks...');
  // Find the format_str line
  const oldFormat = "        format_str = params.get('format', 'bestaudio[ext=m4a]/bestaudio')\n        command.extend(['-f', format_str])";
  const newFormat = `        # Determine format based on extract_audio flag
        if extract_audio:
            format_str = params.get('format', AUDIO_FORMAT)
        else:
            format_str = params.get('format', VIDEO_FORMAT)
        command.extend(['-f', format_str])

        # Archive file for deduplication (skip already-downloaded tracks)
        archive_path = params.get('archive_file')
        if archive_path:
            safe_mkdir(os.path.dirname(archive_path))
            command.extend(['--download-archive', archive_path])`;
  c = replaceOnce(c, oldFormat, newFormat);
}

writeFile(PLAYLIST_DL, c);
console.log('✓ playlist_dl.py updated');

// Step 3: Create worker/playlist_utils.py with utility functions
const PLAYLIST_UTILS = "e:\Backup\pgwiz\bots\telegram-ultra\worker\playlist_utils.py";
if (!fs.existsSync(PLAYLIST_UTILS)) {
  console.log('Creating playlist_utils.py...');
  const utilsContent = `"""Playlist utility functions for yt-dlp integration."""

import asyncio
import json
import logging
import re
import subprocess
import sys
from typing import Dict, Any, Optional, List
from worker.config import config
from worker.cookies import get_yt_dlp_cookie_args
from worker.utils import find_node_binary

logger = logging.getLogger(__name__)


def normalize_playlist_url(url: str) -> str:
    """
    Normalize YouTube playlist URLs for yt-dlp compatibility.

    Radio Mix URLs (list=RD...) expire when used as a plain playlist URL.
    They must include the seed video + start_radio=1 to work reliably.
    """
    radio_match = re.search(r'list=(RD([a-zA-Z0-9_-]+))', url)
    if radio_match:
        full_list_id = radio_match.group(1)  # e.g. RDEgBJmlPo8Xw
        video_id = radio_match.group(2)      # e.g. EgBJmlPo8Xw
        if 'start_radio=1' in url and f'v={video_id}' in url:
            return url
        return (
            f"https://www.youtube.com/watch?v={video_id}"
            f"&list={full_list_id}&start_radio=1"
        )
    return url


async def get_playlist_preview(url: str, preview_count: int = 5) -> Optional[Dict[str, Any]]:
    """
    Fetch first N tracks of a playlist without downloading.
    
    Returns dict with:
    - playlist_title: str
    - playlist_count: int (total tracks in playlist)
    - tracks: list of {'index': int, 'title': str}
    """
    try:
        # Normalize URL first
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
        
        # Parse title and count
        title_line = lines[0].split('|')
        playlist_title = title_line[0] if title_line else 'Playlist'
        playlist_count = int(title_line[1]) if len(title_line) > 1 else 0
        
        # Parse tracks
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
`;
  writeFile(PLAYLIST_UTILS, utilsContent);
  console.log('✓ playlist_utils.py created');
}

console.log('\n✅ All playlist enhancements applied!');
