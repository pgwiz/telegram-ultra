import httpx
import asyncio
import subprocess
import os
import json
import re
import shutil
import sys
import time
import random
import threading
import uuid
import tempfile
import zipfile
import glob
from zipfile import ZipFile
from flask import Flask, request, jsonify, send_from_directory, render_template, url_for, Response
from collections import Counter
from datetime import datetime
from functools import lru_cache
import logging
import atexit

# --- Flask App Setup (WSGI Only) ---
app = Flask(__name__, template_folder='.')
app.config['SECRET_KEY'] = 'your-secret-key-here'

# --- Configuration ---
API_BASE_URL = 'https://spotipiapi3yts.vercel.app'
SPOTIFY_API_BASE = 'https://api2.spotify.pgwiz.uk'  # Your Spotify API
COOKIES_FILE_PATH = "cookies.txt"
DOWNLOADS_DIR = os.path.join("public", "downloads")
TEMP_LINK_EXPIRY_SECONDS = 600  # 10 minutes

# --- In-memory storage ---
temp_links = {}
stream_cache = {}
recent_downloads = []
recent_lock = threading.Lock()

# --- FIXED: Cookie Management ---
COOKIE_MANAGER = {
    'path': None,
    'loaded': False
}

def get_cookie_file_path():
    """Get or create cookie file path (reuse existing if available)."""
    global COOKIE_MANAGER
    
    if COOKIE_MANAGER['loaded'] and COOKIE_MANAGER['path'] and os.path.exists(COOKIE_MANAGER['path']):
        return COOKIE_MANAGER['path']
    
    cookie_data = None
    if os.path.exists(COOKIES_FILE_PATH):
        with open(COOKIES_FILE_PATH, "r", encoding='utf-8') as f:
            cookie_data = f.read()
    else:
        cookie_data = os.environ.get("YTDLP_COOKIES")

    if cookie_data:
        try:
            temp_dir = tempfile.gettempdir()
            cookie_path = os.path.join(temp_dir, "yt_cookies_reusable.txt")
            
            if not os.path.exists(cookie_path):
                with open(cookie_path, "w", encoding='utf-8') as f:
                    f.write(cookie_data)
            
            COOKIE_MANAGER['path'] = cookie_path
            COOKIE_MANAGER['loaded'] = True
            return cookie_path
        except Exception as e:
            return None
    return None

# --- FIXED: Safe File Operations ---
def safe_delete_files(folder_path, extension='.mp3'):
    """Safely delete files with given extension after archiving."""
    if not os.path.exists(folder_path):
        print(f"⚠️ Safe delete: folder {folder_path} does not exist")
        return
    
    deleted_count = 0
    for filename in os.listdir(folder_path):
        if filename.endswith(extension):
            filepath = os.path.join(folder_path, filename)
            try:
                os.remove(filepath)
                print(f"🗑️ Deleted original file: {filename}")
                deleted_count += 1
            except Exception as e:
                print(f"⚠️ Failed to delete {filepath}: {e}")
    
    print(f"🧹 Cleaned up {deleted_count} original audio files")

# --- ENHANCED: Split Archive Creation with File Verification ---
def create_split_archives(folder_path, archive_base_name, max_part_size_mb=100):
    """
    Creates multiple zip archive parts from MP3 files in folder_path.
    FIXED: Verifies files exist before archiving.
    """
    if not os.path.exists(folder_path):
        print(f"❌ Archive creation failed: folder {folder_path} doesn't exist")
        return []
    
    max_part_size = max_part_size_mb * 1024 * 1024  # Convert to bytes
    
    # Get all MP3 files and verify they exist
    mp3_files = []
    for filename in os.listdir(folder_path):
        if filename.lower().endswith('.mp3'):
            file_path = os.path.join(folder_path, filename)
            if os.path.exists(file_path) and os.path.getsize(file_path) > 0:
                mp3_files.append(file_path)
            else:
                print(f"⚠️ Skipping invalid/empty file: {filename}")
    
    if not mp3_files:
        print(f"❌ No valid MP3 files found in {folder_path}")
        return []
    
    print(f"📦 Creating split archives for {len(mp3_files)} MP3 files, max {max_part_size_mb}MB per part")
    for f in mp3_files:
        size_mb = os.path.getsize(f) / (1024 * 1024)
        print(f"   📄 {os.path.basename(f)} ({size_mb:.1f}MB)")
    
    archives_created = []
    part_number = 1
    current_part_size = 0
    current_files = []
    
    def create_archive_name(num):
        return f"{archive_base_name}-part{str(num).zfill(2)}.zip"
    
    for file_path in mp3_files:
        file_size = os.path.getsize(file_path)
        
        # If adding this file would exceed limit and we have files in current part
        if current_part_size + file_size > max_part_size and current_files:
            # Create current archive
            archive_name = create_archive_name(part_number)
            archive_path = os.path.join(folder_path, archive_name)
            
            try:
                print(f"📦 Creating {archive_name} with {len(current_files)} files ({current_part_size / (1024 * 1024):.1f}MB)")
                
                with zipfile.ZipFile(archive_path, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as zip_file:
                    for current_file in current_files:
                        if os.path.exists(current_file):  # Double-check file exists
                            arcname = os.path.basename(current_file)
                            zip_file.write(current_file, arcname=arcname)
                            print(f"   ✅ Added to archive: {arcname}")
                        else:
                            print(f"   ⚠️ File missing during archiving: {current_file}")
                
                # Verify archive was created successfully
                if os.path.exists(archive_path) and os.path.getsize(archive_path) > 0:
                    archives_created.append(archive_name)
                    print(f"   ✅ Archive created successfully: {archive_name}")
                else:
                    print(f"   ❌ Failed to create archive: {archive_name}")
                
            except Exception as e:
                print(f"❌ Error creating archive {archive_name}: {e}")
            
            # Reset for next part
            part_number += 1
            current_part_size = 0
            current_files = []
        
        # Add file to current part
        current_files.append(file_path)
        current_part_size += file_size
    
    # Create final archive if there are remaining files
    if current_files:
        archive_name = create_archive_name(part_number)
        archive_path = os.path.join(folder_path, archive_name)
        
        try:
            print(f"📦 Creating final {archive_name} with {len(current_files)} files ({current_part_size / (1024 * 1024):.1f}MB)")
            
            with zipfile.ZipFile(archive_path, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as zip_file:
                for current_file in current_files:
                    if os.path.exists(current_file):
                        arcname = os.path.basename(current_file)
                        zip_file.write(current_file, arcname=arcname)
                        print(f"   ✅ Added to final archive: {arcname}")
                    else:
                        print(f"   ⚠️ File missing during final archiving: {current_file}")
            
            # Verify final archive was created successfully
            if os.path.exists(archive_path) and os.path.getsize(archive_path) > 0:
                archives_created.append(archive_name)
                print(f"   ✅ Final archive created successfully: {archive_name}")
            else:
                print(f"   ❌ Failed to create final archive: {archive_name}")
                
        except Exception as e:
            print(f"❌ Error creating final archive {archive_name}: {e}")
    
    print(f"📦 Successfully created {len(archives_created)} archive parts")
    return archives_created

# --- Keep all your existing functions (fetch_spotify_tracks, etc.) ---
async def fetch_spotify_tracks(spotify_url):
    """Fetch track data from your Spotify API with YouTube video IDs."""
    try:
        print(f"🎵 Fetching Spotify data from: {spotify_url}")
        
        async with httpx.AsyncClient(timeout=60.0) as client:
            api_url = f"{SPOTIFY_API_BASE}/"
            params = {'spotifyUrl': spotify_url}
            
            response = await client.get(api_url, params=params)
            response.raise_for_status()
            data = response.json()
            
            if not data or 'tracks' not in data:
                raise ValueError("Invalid response from Spotify API")
            
            tracks = data.get('tracks', [])
            playlist_name = data.get('playName', None)
            is_playlist = data.get('isPlaylist', False)
            
            # Convert to format compatible with your download system
            converted_tracks = []
            for track in tracks:
                video_id = track.get('videoId')
                if video_id:
                    converted_tracks.append({
                        'videoId': video_id,
                        'name': track.get('name', 'Unknown Title'),
                        'artist': track.get('artist', 'Unknown Artist'),
                        'thumbnail': track.get('thumbnail', f"https://img.youtube.com/vi/{video_id}/mqdefault.jpg"),
                        'duration': 'Unknown',
                        'url': f"https://www.youtube.com/watch?v={video_id}",
                        'spotify_id': track.get('id', ''),
                        'source': 'spotify'
                    })
            
            return {
                'tracks': converted_tracks,
                'playlist_name': playlist_name,
                'is_playlist': is_playlist,
                'total_tracks': len(tracks),
                'valid_tracks': len(converted_tracks)
            }
            
    except Exception as e:
        print(f"❌ Spotify API Error: {e}")
        return None

def schedule_cleanup(path, delay):
    """Schedules a path (file or directory) for deletion after a delay."""
    def cleanup():
        time.sleep(delay)
        try:
            if os.path.isdir(path):
                shutil.rmtree(path)
            elif os.path.exists(path):
                os.remove(path)
        except Exception as e:
            pass
    
    threading.Thread(target=cleanup, daemon=True).start()

def add_to_recent_downloads(filename, download_url, file_size_mb):
    """Add a download to the recent downloads list (thread-safe)."""
    with recent_lock:
        global recent_downloads
        
        entry = {
            'filename': filename,
            'download_url': download_url,
            'size_mb': file_size_mb,
            'timestamp': time.time(),
            'date': datetime.now().strftime('%Y-%m-%d %H:%M:%S')
        }
        
        recent_downloads = [r for r in recent_downloads if r['filename'] != filename]
        recent_downloads.insert(0, entry)
        
        if len(recent_downloads) > 5:
            recent_downloads = recent_downloads[:5]
        
        return recent_downloads.copy()

def create_temp_link_for_file(file_path, folder_name, filename):
    """Create a temporary download link for a file."""
    try:
        if not os.path.exists(file_path):
            print(f"⚠️ Cannot create temp link: file doesn't exist: {file_path}")
            return None
        
        link_id = str(uuid.uuid4())
        temp_links[link_id] = file_path
        
        threading.Timer(TEMP_LINK_EXPIRY_SECONDS, lambda: temp_links.pop(link_id, None)).start()
        
        download_url = url_for('download_temp', link_id=link_id, _external=True)
        
        print(f"📎 Created temp link for {filename}: {download_url}")
        return download_url
    except Exception as e:
        print(f"Failed to create temp link for {filename}: {e}")
        return None

# --- Keep all your existing YouTube functions ---
def _execute_yt_dlp_command(youtube_url: str):
    command = [
        sys.executable, "-m", "yt_dlp",
        youtube_url,
        "--no-cache-dir",
        "--no-check-certificate", 
        "--dump-single-json",
        "--flat-playlist",
        "-f", "best[ext=mp4]/best"
    ]

    cookie_path = get_cookie_file_path()
    if cookie_path:
        command.extend(["--cookies", cookie_path])

    try:
        process = subprocess.run(command, capture_output=True, text=True, check=True)
        return json.loads(process.stdout)
    except subprocess.CalledProcessError as e:
        error_message = e.stderr.strip()
        if "confirm you're not a bot" in error_message:
            raise RuntimeError("YouTube's bot detection was triggered.")
        if "is unavailable" in error_message or "Private video" in error_message:
            raise RuntimeError("The requested video is private or unavailable.")
        raise RuntimeError("Failed to extract video information.")
    except Exception as e:
        raise RuntimeError("An internal server error occurred while processing the request.")

def _process_single_video_entry(entry: dict) -> dict:
    if not entry:
        return None

    return {
        'title': entry.get('title', 'Untitled'),
        'url': entry.get('url'),
        'thumbnail': entry.get('thumbnail'),
        'duration': entry.get('duration_string', 'N/A'),
        'uploader': entry.get('uploader', 'Unknown'),
        'videoId': entry.get('id', ''),
        'name': entry.get('title', 'Untitled'),
        'artist': entry.get('uploader', 'Unknown')
    }

@lru_cache(maxsize=128)
def extract_media_info(youtube_url: str) -> dict:
    info = _execute_yt_dlp_command(youtube_url)
    
    if 'entries' in info:
        tracks = [
            {
                'title': entry.get('title', 'Untitled'),
                'url': entry.get('url'),
                'id': entry.get('id'),
                'videoId': entry.get('id'),
                'duration_string': entry.get('duration_string', 'N/A'),
                'uploader': entry.get('uploader', 'Unknown'),
                'name': entry.get('title', 'Untitled'),
                'artist': entry.get('uploader', 'Unknown'),
                'duration': entry.get('duration_string', 'N/A'),
                'thumbnail': entry.get('thumbnail', f"https://img.youtube.com/vi/{entry.get('id', '')}/mqdefault.jpg")
            }
            for entry in info.get('entries', []) if entry
        ]
        
        return {
            'is_playlist': True,
            'playlist_title': info.get('title'),
            'tracks': tracks
        }
    else:
        track = _process_single_video_entry(info)
        return {
            'is_playlist': False,
            'tracks': [track] if track and track.get('url') else []
        }

# --- Keep your existing search functions ---
async def search_youtube(query, limit=10):
    try:
        command = [
            sys.executable, "-m", "yt_dlp",
            f"ytsearch{limit}:{query}",
            "--dump-single-json",
            "--flat-playlist",
            "--no-cache-dir"
        ]
        
        cookie_path = get_cookie_file_path()
        if cookie_path:
            command.extend(["--cookies", cookie_path])
        
        process = subprocess.run(command, capture_output=True, text=True, check=True)
        data = json.loads(process.stdout)
        
        results = []
        if 'entries' in data:
            for entry in data['entries'][:limit]:
                if entry:
                    results.append({
                        'videoId': entry.get('id', ''),
                        'name': entry.get('title', 'Unknown Title'),
                        'artist': entry.get('uploader', 'Unknown Artist'),
                        'duration': entry.get('duration_string', 'Unknown'),
                        'thumbnail': entry.get('thumbnail', f"https://img.youtube.com/vi/{entry.get('id', '')}/mqdefault.jpg"),
                        'url': f"https://www.youtube.com/watch?v={entry.get('id', '')}"
                    })
        
        return results
    except Exception as e:
        return []

# --- FIXED: Enhanced Download Function with Proper File Management ---
def download_youtube_tracks(tracks, save_dir, is_playlist=False, playlist_name=None):
    """Downloads tracks with enhanced file management for playlists."""
    downloaded_files = []
    download_info = []
    total_tracks = len(tracks)

    # FIXED: Ensure save directory exists and is consistent
    os.makedirs(save_dir, exist_ok=True)
    print(f"📁 Download directory confirmed: {save_dir}")

    for i, track in enumerate(tracks):
        print("-" * 50)
        print(f"Processing track {i+1}/{total_tracks}: {track.get('artist')} - {track.get('name')}")
        
        video_id = track.get('videoId')
        if not video_id:
            print(f"Skipping '{track.get('name')}' - No videoId found.")
            continue
            
        youtube_url = f"https://www.youtube.com/watch?v={video_id}"

        # Sanitize filename
        sanitized_name = re.sub(r'[\\/*?:"<>|]', "", track.get('name', 'Unknown Track'))
        if track.get('artist') and track.get('artist') != 'Unknown Artist':
            sanitized_artist = re.sub(r'[\\/*?:"<>|]', "", track.get('artist'))
            final_filename = f"{sanitized_artist} - {sanitized_name}.mp3"
        else:
            final_filename = f"{sanitized_name}.mp3"

        final_filepath = os.path.join(save_dir, final_filename)
        temp_output_template = os.path.join(save_dir, f"{video_id}.%(ext)s")

        cookie_path = get_cookie_file_path()
        
        command = [
            sys.executable, "-m", "yt_dlp",
            "-f", "bestaudio[filesize<15M]/bestaudio",
            "-x", "--audio-format", "mp3",
            "--output", temp_output_template,
            youtube_url,
        ]

        if cookie_path:
            command.extend(["--cookies", cookie_path])

        try:
            print(f"Executing command: {' '.join(command)}")
            process = subprocess.Popen(
                command, 
                stdout=subprocess.PIPE, 
                stderr=subprocess.STDOUT, 
                text=True, 
                encoding='utf-8', 
                errors='ignore', 
                bufsize=1
            )

            captured_destination = ""
            for line in iter(process.stdout.readline, ''):
                line = line.strip()
                if not line: 
                    continue
                print(f"  [yt-dlp]: {line}")
                
                if '[ExtractAudio] Destination:' in line:
                    captured_destination = line.split('Destination: ')[1].strip()

            process.wait()

            if process.returncode != 0:
                print(f"Download failed for {track.get('name')}. yt-dlp exited with code {process.returncode}")
                continue

            # Enhanced file detection with verification
            file_found = False
            
            if captured_destination and os.path.exists(captured_destination):
                if captured_destination != final_filepath:
                    print(f"Renaming '{captured_destination}' to '{final_filepath}'")
                    shutil.move(captured_destination, final_filepath)
                
                # Verify file exists and has content after move
                if os.path.exists(final_filepath) and os.path.getsize(final_filepath) > 0:
                    downloaded_files.append(final_filepath)
                    file_found = True
                    print(f"✅ Successfully processed: {final_filename}")
                else:
                    print(f"⚠️ File verification failed: {final_filename}")
            
            elif os.path.exists(final_filepath) and os.path.getsize(final_filepath) > 0:
                print(f"File already exists at final destination: {final_filepath}")
                downloaded_files.append(final_filepath)
                file_found = True
            
            # Additional search methods if file not found
            elif not file_found:
                for filename in os.listdir(save_dir):
                    if filename.startswith(video_id) and filename.endswith('.mp3'):
                        source_path = os.path.join(save_dir, filename)
                        if os.path.exists(source_path) and os.path.getsize(source_path) > 0:
                            print(f"Found downloaded file: {source_path}, moving to: {final_filepath}")
                            shutil.move(source_path, final_filepath)
                            downloaded_files.append(final_filepath)
                            file_found = True
                            break
            
            # For individual files (non-playlist), create temp links immediately
            if not is_playlist and file_found:
                folder_name = os.path.basename(save_dir)
                file_size_mb = round(os.path.getsize(final_filepath) / (1024 * 1024), 2)
                temp_download_url = create_temp_link_for_file(final_filepath, folder_name, final_filename)
                
                if temp_download_url:
                    download_info.append({
                        'name': final_filename,
                        'size': file_size_mb,
                        'download_url': temp_download_url,
                        'path': os.path.join(folder_name, final_filename)
                    })
                    
                    add_to_recent_downloads(final_filename, temp_download_url, file_size_mb)
            
            if not file_found:
                print(f"Warning: Could not find downloaded MP3 file for '{track.get('name')}'")

        except Exception as e:
            print(f"An unexpected error occurred while downloading {track.get('name')}.\nError: {e}")
            continue

        if i < total_tracks - 1:
            delay = random.uniform(1, 3)
            print(f"Waiting for {delay:.2f} seconds...")
            time.sleep(delay)
    
    # ENHANCED: Playlist Processing with Proper File Management
    if is_playlist and downloaded_files and playlist_name:
        print(f"🎵 Processing playlist: {playlist_name} ({len(downloaded_files)} files downloaded)")
        
        # Verify all files exist before archiving
        valid_files = []
        for file_path in downloaded_files:
            if os.path.exists(file_path) and os.path.getsize(file_path) > 0:
                valid_files.append(file_path)
                print(f"✅ Verified file: {os.path.basename(file_path)} ({os.path.getsize(file_path) / (1024*1024):.1f}MB)")
            else:
                print(f"⚠️ Missing/invalid file: {file_path}")
        
        if not valid_files:
            print("❌ No valid files found for archiving")
            return [], "No valid files found for playlist archiving.", []
        
        sanitized_playlist_name = re.sub(r'[\\/*?:"<>|]', "", playlist_name)
        
        # Create split archives from verified files
        archives = create_split_archives(save_dir, f"Playlist - {sanitized_playlist_name}")
        
        if not archives:
            print("❌ Failed to create any archives")
            return downloaded_files, f"Successfully processed {len(downloaded_files)} track(s) but archiving failed.", []
        
        # Create temp links for archives
        archive_info = []
        for archive in archives:
            archive_path = os.path.join(save_dir, archive)
            if os.path.exists(archive_path) and os.path.getsize(archive_path) > 0:
                archive_size_mb = round(os.path.getsize(archive_path) / (1024 * 1024), 2)
                temp_archive_url = create_temp_link_for_file(archive_path, os.path.basename(save_dir), archive)
                
                if temp_archive_url:
                    archive_info.append({
                        'filename': archive,
                        'download_url': temp_archive_url,
                        'size_mb': archive_size_mb,
                        'type': 'archive'
                    })
                    
                    # Add to recent downloads
                    add_to_recent_downloads(archive, temp_archive_url, archive_size_mb)
                    print(f"📦 Archive ready: {archive} ({archive_size_mb}MB)")
            else:
                print(f"⚠️ Archive creation failed or empty: {archive}")
        
        # FIXED: Only clean up original files AFTER successful archiving
        if archive_info:  # Only cleanup if archives were successfully created
            print(f"🧹 Cleaning up {len(valid_files)} original MP3 files...")
            safe_delete_files(save_dir, '.mp3')
            
            message = f"Successfully processed {len(downloaded_files)} track(s) and created {len(archives)} archive(s)."
            return downloaded_files, message, archive_info
        else:
            print("❌ No archives created successfully, keeping original files")
            message = f"Successfully processed {len(downloaded_files)} track(s) but archiving failed."
            return downloaded_files, message, []
    
    # Single track or non-playlist
    if not downloaded_files:
        return [], "No audio files were successfully downloaded.", []
        
    return downloaded_files, f"Successfully processed {len(downloaded_files)} track(s).", download_info

# --- Keep all your existing Flask routes ---
@app.route('/')
def index():
    return render_template('layout.html')

@app.route('/api/search/youtube', methods=['GET'])
def search_youtube_endpoint():
    query = request.args.get('query')
    limit = int(request.args.get('limit', 10))
    
    if not query:
        return jsonify({"error": "Query parameter is required."}), 400
    
    try:
        results = asyncio.run(search_youtube(query, limit))
        return jsonify({"results": results})
    except Exception as e:
        return jsonify({"error": str(e)}), 500

@app.route('/api/stream/<video_id>')
def get_stream_endpoint(video_id):
    try:
        if video_id.startswith('http'):
            youtube_url = video_id
        else:
            youtube_url = f"https://www.youtube.com/watch?v={video_id}"
        
        cache_key = video_id
        if cache_key in stream_cache:
            cached_data = stream_cache[cache_key]
            if time.time() - cached_data['timestamp'] < 3600:
                return jsonify({"streamUrl": cached_data['url']})
        
        info = extract_media_info(youtube_url)
        if info and info.get('tracks') and len(info['tracks']) > 0:
            stream_url = info['tracks'][0].get('url')
            if stream_url:
                stream_cache[cache_key] = {
                    'url': stream_url,
                    'timestamp': time.time()
                }
                return jsonify({"streamUrl": stream_url})
        
        return jsonify({"error": "Could not get stream URL"}), 404
    except Exception as e:
        return jsonify({"error": str(e)}), 500

@app.route('/api/recent-downloads', methods=['GET'])
def get_recent_downloads():
    """Get the list of recent downloads."""
    with recent_lock:
        return jsonify({"recent_downloads": recent_downloads.copy()})

# --- ENHANCED: Download Handler with Fixed File Management ---
@app.route('/download', methods=['POST'])
def handle_download():
    """Handle download request with enhanced playlist + archive support."""
    try:
        # Safe JSON parsing
        data = request.get_json(silent=True) or {}
        url = data.get('url', '').strip()
        track_data = data.get('track_data')
        
        print(f"📥 Received download request - URL: {repr(url)}, Track Data: {bool(track_data)}")
        
        if not url and not track_data:
            return jsonify({"error": "URL or track data is required."}), 400
        
        tracks = []
        playlist_name = None
        is_playlist = False
        session_id = str(uuid.uuid4())
        
        if track_data:
            tracks = [track_data]
            print(f"Single track download requested: {track_data.get('name', 'Unknown')}")
        elif url and "open.spotify.com" in url:
            # Handle Spotify URLs
            print(f"🎵 Spotify URL detected: {url}")
            spotify_data = asyncio.run(fetch_spotify_tracks(url))
            
            if not spotify_data or not spotify_data.get('tracks'):
                return jsonify({
                    "error": "Could not fetch tracks from Spotify URL. Make sure the URL is valid and accessible."
                }), 500
            
            tracks = spotify_data['tracks']
            playlist_name = spotify_data.get('playlist_name')
            is_playlist = spotify_data.get('is_playlist', len(tracks) > 1)
            
            print(f"✅ Got {len(tracks)} tracks from Spotify ({spotify_data['valid_tracks']}/{spotify_data['total_tracks']} with YouTube videos)")
            
        elif url and ("youtube.com" in url or "youtu.be" in url):
            # Handle YouTube URLs
            info = extract_media_info(url)
            if info and info.get('tracks'):
                tracks = info['tracks']
                is_playlist = info.get('is_playlist', False)
                if is_playlist:
                    playlist_name = info.get('playlist_title')
            else:
                return jsonify({"error": "Could not extract video information from YouTube URL."}), 500
        else:
            return jsonify({"error": "Please provide a valid Spotify or YouTube URL."}), 400
        
        if not tracks:
            return jsonify({"error": "No tracks found to download."}), 400
        
        # FIXED: Consistent directory creation
        is_playlist_download = len(tracks) > 1 or is_playlist
        
        if is_playlist_download and playlist_name:
            # Use playlist name for folder
            sanitized_folder_name = re.sub(r'[\\/*?:"<>|]', "", playlist_name)
            save_dir = os.path.join(DOWNLOADS_DIR, sanitized_folder_name)
        else:
            # Use session ID for single tracks
            save_dir = os.path.join(DOWNLOADS_DIR, session_id)
        
        os.makedirs(save_dir, exist_ok=True)
        print(f"📁 Created download directory: {save_dir}")
        
        # Download with enhanced file management
        downloaded_files, message, download_info = download_youtube_tracks(
            tracks, 
            save_dir, 
            is_playlist=is_playlist_download, 
            playlist_name=playlist_name
        )
        
        print(f"Download complete. Files: {len(downloaded_files)}")
        print(f"Message: {message}")
        
        if not downloaded_files:
            if os.path.exists(save_dir) and not os.listdir(save_dir):
                shutil.rmtree(save_dir)
            return jsonify({"error": message or "No files were downloaded."}), 500
        
        # Schedule cleanup for the directory
        schedule_cleanup(save_dir, TEMP_LINK_EXPIRY_SECONDS)
        
        # Get current recent downloads list
        with recent_lock:
            current_recent = recent_downloads.copy()
        
        print(f"Returning response with {len(download_info)} items and {len(current_recent)} recent downloads")
        
        # Enhanced response based on type
        source = "spotify" if url and "open.spotify.com" in url else "youtube"
        
        if is_playlist_download:
            # Playlist response with archives
            return jsonify({
                "message": message, 
                "playlist_name": playlist_name,
                "is_playlist": True,
                "archives": download_info,  # Contains archive info
                "recent_downloads": current_recent,
                "success": True,
                "total_files": len(downloaded_files),
                "total_archives": len(download_info),
                "source": source
            })
        else:
            # Single track response
            return jsonify({
                "message": message, 
                "files": download_info,
                "recent_downloads": current_recent,
                "success": True,
                "total_files": len(download_info),
                "source": source
            })
        
    except Exception as e:
        error_msg = f"Download error: {str(e)}"
        print(f"❌ {error_msg}")
        return jsonify({"error": error_msg}), 500

@app.route('/create_temp_link', methods=['POST'])
def create_temp_link():
    """Manual temp link creation (for backward compatibility)."""
    data = request.get_json()
    file_path_relative = data.get('path')
    
    if not file_path_relative:
        return jsonify({'error': 'File path required'}), 400
    
    absolute_path = os.path.join(DOWNLOADS_DIR, file_path_relative)
    if not os.path.exists(absolute_path):
        return jsonify({'error': 'File not found on server. It may have been cleaned up.'}), 404
    
    link_id = str(uuid.uuid4())
    temp_links[link_id] = absolute_path
    
    threading.Timer(TEMP_LINK_EXPIRY_SECONDS, lambda: temp_links.pop(link_id, None)).start()
    
    download_url = url_for('download_temp', link_id=link_id, _external=True)
    return jsonify({'download_url': download_url})

@app.route('/temp_download/<link_id>')
def download_temp(link_id):
    """Serve file from temporary link and invalidate the link."""
    file_path = temp_links.pop(link_id, None)  # One-time use
    
    if not file_path or not os.path.exists(file_path):
        return "Download link expired or invalid.", 404
    
    return send_from_directory(
        os.path.dirname(file_path), 
        os.path.basename(file_path), 
        as_attachment=True
    )

# WSGI Application Object
application = app

if __name__ == '__main__':
    if not os.path.exists(DOWNLOADS_DIR):
        os.makedirs(DOWNLOADS_DIR)
    app.run(debug=True, host='0.0.0.0', port=5000)
