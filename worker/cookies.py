"""
Cookie management for Hermes Media Worker
Handles Netscape format cookies and browser extraction
"""

import os
import tempfile
from typing import Optional
from datetime import datetime
from worker.config import config
from worker.utils import safe_output_path


class CookieManager:
    """Manages YouTube cookies from files and browser extraction."""

    def __init__(self):
        self.cookie_path: Optional[str] = None
        self.loaded: bool = False
        self.last_validated: Optional[datetime] = None

    def get_cookie_file(self) -> Optional[str]:
        """
        Get or create reusable cookie file.

        Returns cookie file path or None if unavailable.
        """
        # Return cached path if already loaded
        if self.loaded and self.cookie_path and os.path.exists(self.cookie_path):
            return self.cookie_path

        cookie_data = None

        # Try loading from configured path
        if os.path.exists(config.COOKIES_FILE):
            try:
                with open(config.COOKIES_FILE, 'r', encoding='utf-8') as f:
                    cookie_data = f.read()
            except Exception as e:
                print(f"⚠️ Failed to read cookie file {config.COOKIES_FILE}: {e}")
                return None

        # If not found, try environment variable
        if not cookie_data:
            cookie_data = os.environ.get('YTDLP_COOKIES')

        if not cookie_data:
            return None

        # Write to temp directory with reusable name
        try:
            temp_dir = tempfile.gettempdir()
            cookie_path = os.path.join(temp_dir, 'yt_cookies_reusable.txt')

            # Only write if doesn't exist to reuse
            if not os.path.exists(cookie_path):
                with open(cookie_path, 'w', encoding='utf-8') as f:
                    f.write(cookie_data)

            # Set file permissions to 0o600 (read/write for owner only)
            try:
                os.chmod(cookie_path, 0o600)
            except Exception:
                pass  # Windows may not support chmod

            self.cookie_path = cookie_path
            self.loaded = True
            self.last_validated = datetime.now()

            print(f"🍪 Cookie file loaded: {cookie_path}")
            return cookie_path

        except Exception as e:
            print(f"❌ Failed to setup cookie file: {e}")
            return None

        return None

    def validate_cookie_file(self) -> bool:
        """
        Validate cookie file exists and is readable.

        Returns:
            True if valid
        """
        cookie_file = self.get_cookie_file()

        if not cookie_file or not os.path.exists(cookie_file):
            return False

        try:
            with open(cookie_file, 'r', encoding='utf-8') as f:
                content = f.read()
                # Basic validation: should contain cookie lines
                return len(content) > 0 and ('\.youtube\.com' in content or 'youtube.com' in content or content.strip())

        except Exception as e:
            print(f"⚠️ Cookie validation failed: {e}")
            return False

    def build_yt_dlp_args(self) -> list:
        """
        Build yt-dlp command arguments for cookie handling.

        Only uses Netscape-format cookie files. Returns empty list
        if no cookie file is available (runs unauthenticated).

        Returns:
            List of arguments to append to yt-dlp command
        """
        cookie_file = self.get_cookie_file()
        if cookie_file:
            return ['--cookies', cookie_file]

        # No cookies available - run unauthenticated
        return []

    def suggest_cookie_refresh(self) -> bool:
        """
        Check if cookies might be stale.

        Returns:
            True if cookies should be refreshed
        """
        if not self.last_validated:
            return True

        age_hours = (datetime.now() - self.last_validated).total_seconds() / 3600

        # Suggest refresh after 30 days
        return age_hours > (30 * 24)

    def clear_cache(self):
        """Clear cached cookie path."""
        self.cookie_path = None
        self.loaded = False
        self.last_validated = None


# Global cookie manager instance
cookie_manager = CookieManager()


def get_cookies_file() -> Optional[str]:
    """
    Convenience function to get cookies file.

    Returns:
        Cookie file path or None
    """
    return cookie_manager.get_cookie_file()


def get_yt_dlp_cookie_args() -> list:
    """
    Convenience function to get yt-dlp cookie arguments.

    Returns:
        List of arguments for yt-dlp command
    """
    return cookie_manager.build_yt_dlp_args()


def validate_cookies() -> bool:
    """
    Convenience function to validate cookies.

    Returns:
        True if cookies are valid
    """
    return cookie_manager.validate_cookie_file()


def should_refresh_cookies() -> bool:
    """
    Convenience function to check if cookies need refresh.

    Returns:
        True if refresh recommended
    """
    return cookie_manager.suggest_cookie_refresh()
