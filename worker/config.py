"""
Configuration module for Hermes Media Worker
Centralized environment variable management
"""

import os
from dataclasses import dataclass
from typing import Optional


@dataclass
class WorkerConfig:
    """Worker configuration from environment variables."""

    # Cookies
    COOKIES_FILE: str = os.getenv('YOUTUBE_COOKIE_FILE', './cookies.txt')

    # Download constraints
    BEST_AUDIO_LIMIT_MB: int = int(os.getenv('BEST_AUDIO_LIMIT_MB', '15'))
    # Path to node binary for yt-dlp JS challenges. Auto-detected if blank.
    NODE_BIN: str = os.getenv('NODE_BIN', '')
    MAX_RETRIES: int = int(os.getenv('MAX_RETRIES', '3'))
    RETRY_DELAY_SECONDS: int = int(os.getenv('RETRY_DELAY_SECONDS', '5'))

    # Timeouts
    YT_TIMEOUT: int = int(os.getenv('YT_TIMEOUT', '300'))  # 5 minutes
    IPC_TIMEOUT: int = int(os.getenv('IPC_TIMEOUT', '600'))  # 10 minutes

    # Output directories
    DOWNLOAD_DIR: str = os.getenv('DOWNLOAD_DIR', './downloads')
    TEMP_DIR: str = os.getenv('TEMP_DIR', './temp')

    # Search and caching
    ENABLE_SEARCH_CACHE: bool = os.getenv('ENABLE_SEARCH_CACHE', 'true').lower() == 'true'
    CACHE_EXPIRY_HOURS: int = int(os.getenv('CACHE_EXPIRY_HOURS', '24'))

    # Logging
    LOG_LEVEL: str = os.getenv('LOG_LEVEL', 'info').lower()
    LOG_FILE: Optional[str] = os.getenv('WORKER_LOG_FILE', None)

    # Archive settings
    ARCHIVE_MAX_SIZE_MB: int = int(os.getenv('ARCHIVE_MAX_SIZE_MB', '100'))
    ARCHIVE_COMPRESSION_LEVEL: int = int(os.getenv('ARCHIVE_COMPRESSION_LEVEL', '6'))

    # Playlist settings
    PLAYLIST_NAME_MAX_LENGTH: int = int(os.getenv('PLAYLIST_NAME_MAX_LENGTH', '100'))

    # Rate limiting
    RATE_LIMIT_SEARCHES_PER_HOUR: int = int(os.getenv('RATE_LIMIT_SEARCHES_PER_HOUR', '60'))

    # Database
    DATABASE_URL: str = os.getenv('DATABASE_URL', 'sqlite:///./hermes.db')

    def __post_init__(self):
        """Validate and create necessary directories."""
        os.makedirs(self.DOWNLOAD_DIR, exist_ok=True)
        os.makedirs(self.TEMP_DIR, exist_ok=True)

    def to_dict(self) -> dict:
        """Convert config to dictionary for logging/display."""
        return {
            'cookies_file': self.COOKIES_FILE,
            'best_audio_limit_mb': self.BEST_AUDIO_LIMIT_MB,
            'max_retries': self.MAX_RETRIES,
            'yt_timeout': self.YT_TIMEOUT,
            'download_dir': self.DOWNLOAD_DIR,
            'enable_search_cache': self.ENABLE_SEARCH_CACHE,
            'archive_max_size_mb': self.ARCHIVE_MAX_SIZE_MB,
        }


# Global config instance
config = WorkerConfig()
