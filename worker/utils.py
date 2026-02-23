"""
Utility functions for Hermes Media Worker
Path sanitization, formatting, and helpers
"""

import os
import re
import shutil
from pathlib import Path
from typing import Optional


def sanitize_filename(filename: str, max_length: int = 200) -> str:
    """
    Sanitize filename to prevent path traversal and OS issues.

    Args:
        filename: Raw filename from yt-dlp
        max_length: Maximum filename length (default 200)

    Returns:
        Safe filename
    """
    # Remove path traversal attempts
    filename = filename.replace('..', '').replace('/', '').replace('\\', '')

    # Remove or replace invalid characters for all OSes
    # Windows: < > : " / \ | ? *
    # Also remove control characters
    invalid_chars = r'[<>:"/\\|?*\x00-\x1f]'
    filename = re.sub(invalid_chars, '', filename)

    # Replace common problematic sequences
    filename = re.sub(r'\s+', ' ', filename).strip()

    # Limit length
    filename = filename[:max_length]

    # Ensure not empty
    if not filename:
        filename = 'untitled'

    return filename


def sanitize_folder_name(folder_name: str, max_length: int = 100) -> str:
    """
    Sanitize folder name (less restrictive than filenames).

    Args:
        folder_name: Raw folder name
        max_length: Maximum folder name length

    Returns:
        Safe folder name
    """
    # Remove path traversal
    folder_name = folder_name.replace('..', '').replace('/', '').replace('\\', '')

    # Remove invalid chars
    invalid_chars = r'[<>:"/\\|?*\x00-\x1f]'
    folder_name = re.sub(invalid_chars, '', folder_name)

    # Normalize whitespace
    folder_name = re.sub(r'\s+', ' ', folder_name).strip()

    # Limit length
    folder_name = folder_name[:max_length]

    if not folder_name:
        folder_name = 'playlist'

    return folder_name


def validate_youtube_url(url: str) -> bool:
    """
    Validate that URL is from YouTube domain.

    Args:
        url: URL string to validate

    Returns:
        True if valid YouTube URL
    """
    # Whitelist YouTube domains
    youtube_domains = [
        'youtube.com',
        'youtu.be',
        'www.youtube.com',
        'm.youtube.com',
        'youtube.co.uk',
    ]

    url_lower = url.lower()
    return any(domain in url_lower for domain in youtube_domains) and 'youtube' in url_lower


def validate_search_query(query: str, max_length: int = 100) -> bool:
    """
    Validate search query to prevent injection.

    Args:
        query: Search query string
        max_length: Maximum query length

    Returns:
        True if valid query
    """
    if not query or len(query) == 0:
        return False

    if len(query) > max_length:
        return False

    # Block shell/command injection characters
    dangerous_chars = [';', '|', '&', '$', '`', '\n', '\r', '$(', '`']
    for char in dangerous_chars:
        if char in query:
            return False

    return True


def safe_mkdir(path: str) -> bool:
    """
    Safely create directory.

    Args:
        path: Directory path

    Returns:
        True if created or exists
    """
    try:
        os.makedirs(path, exist_ok=True)
        return True
    except Exception:
        return False


def safe_rmtree(path: str) -> bool:
    """
    Safely remove directory tree.

    Args:
        path: Directory path

    Returns:
        True if removed successfully
    """
    try:
        if os.path.exists(path):
            shutil.rmtree(path)
        return True
    except Exception:
        return False


def safe_remove_file(path: str) -> bool:
    """
    Safely remove single file.

    Args:
        path: File path

    Returns:
        True if removed successfully
    """
    try:
        if os.path.exists(path):
            os.remove(path)
        return True
    except Exception:
        return False


def safe_output_path(base_dir: str, filename: str) -> Optional[str]:
    """
    Construct safe output path without traversal.

    Args:
        base_dir: Base directory (must exist)
        filename: Requested filename

    Returns:
        Safe absolute path within base_dir, or None if traversal detected
    """
    # Sanitize filename
    safe_name = sanitize_filename(filename)

    # Join paths
    full_path = os.path.normpath(os.path.join(base_dir, safe_name))

    # Ensure within base_dir (prevent traversal)
    try:
        base_abs = os.path.abspath(base_dir)
        full_abs = os.path.abspath(full_path)

        if not full_abs.startswith(base_abs):
            return None
    except Exception:
        return None

    return full_abs


def format_bytes(bytes_value: float) -> str:
    """
    Format bytes to human-readable size.

    Args:
        bytes_value: Size in bytes

    Returns:
        Formatted string (e.g., "1.2 MB")
    """
    units = ['B', 'KB', 'MB', 'GB', 'TB']

    for unit in units:
        if bytes_value < 1024:
            return f"{bytes_value:.1f} {unit}"
        bytes_value /= 1024

    return f"{bytes_value:.1f} PB"


def format_duration(seconds: int) -> str:
    """
    Format seconds to HH:MM:SS.

    Args:
        seconds: Duration in seconds

    Returns:
        Formatted string
    """
    if seconds < 0:
        return "Unknown"

    hours = seconds // 3600
    minutes = (seconds % 3600) // 60
    secs = seconds % 60

    if hours > 0:
        return f"{hours}:{minutes:02d}:{secs:02d}"
    elif minutes > 0:
        return f"{minutes}:{secs:02d}"
    else:
        return f"{secs}s"


def get_file_size(filepath: str) -> Optional[int]:
    """
    Get file size safely.

    Args:
        filepath: Path to file

    Returns:
        File size in bytes, or None if error
    """
    try:
        if os.path.exists(filepath):
            return os.path.getsize(filepath)
    except Exception:
        pass
    return None


def file_exists_and_valid(filepath: str, min_size_bytes: int = 1) -> bool:
    """
    Check if file exists and has minimum size.

    Args:
        filepath: Path to file
        min_size_bytes: Minimum required size

    Returns:
        True if valid file
    """
    try:
        if not os.path.exists(filepath):
            return False

        size = os.path.getsize(filepath)
        return size >= min_size_bytes
    except Exception:
        return False


def ensure_parent_dir(filepath: str) -> bool:
    """
    Ensure parent directory of file exists.

    Args:
        filepath: File path

    Returns:
        True if parent exists or was created
    """
    try:
        parent = os.path.dirname(filepath)
        if parent:
            os.makedirs(parent, exist_ok=True)
        return True
    except Exception:
        return False
