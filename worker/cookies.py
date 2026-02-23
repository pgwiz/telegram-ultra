"""
Cookie management for Hermes Media Worker
Handles Netscape format cookies and browser extraction
"""

import os
import tempfile
import logging
from typing import Optional
from datetime import datetime
from worker.config import config


logger = logging.getLogger(__name__)


class CookieManager:
    """Manages YouTube cookies from files and browser extraction."""

    def __init__(self):
        self.cookie_path: Optional[str] = None
        self.loaded: bool = False
        self.last_validated: Optional[datetime] = None

    def get_cookie_file(self) -> Optional[str]:
        """
        Get a TEMP COPY of the cookie file for yt-dlp.

        yt-dlp updates (overwrites) the cookies file in place with session
        cookies from YouTube responses. To protect the user's uploaded auth
        cookies, we always give yt-dlp a temp copy, not the real file.

        The copy is refreshed whenever the source file changes, so /upcook
        updates are always picked up on the next download.

        Returns temp cookie file path or None if unavailable.
        """
        import shutil
        import hashlib

        source_path = os.path.abspath(config.COOKIES_FILE)
        temp_dir = tempfile.gettempdir()
        temp_path = os.path.join(temp_dir, 'yt_cookies_working.txt')

        if os.path.exists(source_path):
            try:
                # Compute source hash to detect /upcook updates
                with open(source_path, 'rb') as f:
                    source_hash = hashlib.md5(f.read()).hexdigest()

                # Only copy if temp doesn't exist or source changed
                if not os.path.exists(temp_path) or self.cookie_path != temp_path \
                        or getattr(self, '_source_hash', None) != source_hash:
                    shutil.copy2(source_path, temp_path)
                    try:
                        os.chmod(temp_path, 0o600)
                    except Exception:
                        pass
                    self._source_hash = source_hash
                    logger.info(f"Cookie working copy updated from {source_path}")

                self.cookie_path = temp_path
                self.loaded = True
                self.last_validated = datetime.now()
                return temp_path
            except Exception as e:
                logger.error(f"Failed to create cookie working copy: {e}")
                return None

        # Fallback: YTDLP_COOKIES env var contains inline cookie content
        cookie_data = os.environ.get('YTDLP_COOKIES')
        if cookie_data:
            try:
                fallback_path = os.path.join(temp_dir, 'yt_cookies_working.txt')
                with open(fallback_path, 'w', encoding='utf-8') as f:
                    f.write(cookie_data)
                try:
                    os.chmod(fallback_path, 0o600)
                except Exception:
                    pass
                self.cookie_path = fallback_path
                self.loaded = True
                self.last_validated = datetime.now()
                logger.info(f"Cookie file from YTDLP_COOKIES env: {fallback_path}")
                return fallback_path
            except Exception as e:
                logger.error(f"Failed to write cookie file from env: {e}")
                return None

        return None

    def validate_cookie_file(self) -> bool:
        """
        Validate cookie file exists, is readable, and looks like cookies.

        Returns:
            True if valid
        """
        cookie_file = self.get_cookie_file()

        if not cookie_file or not os.path.exists(cookie_file):
            return False

        try:
            with open(cookie_file, 'r', encoding='utf-8') as f:
                content = f.read()
                if not content.strip():
                    return False
                return 'youtube.com' in content or '.google.com' in content
        except Exception as e:
            logger.warning(f"Cookie validation failed: {e}")
            return False

    def verify_on_startup(self):
        """
        Verify cookies on startup with detailed logging.
        Call this from the worker entry point.
        """
        cookie_file = self.get_cookie_file()
        if not cookie_file:
            logger.warning("No cookie file found. Downloads may fail for restricted content.")
            logger.warning(f"  Checked: {os.path.abspath(config.COOKIES_FILE)}")
            logger.warning("  Upload cookies via /upcook command or set YTDLP_COOKIES env var")
            return

        try:
            size = os.path.getsize(cookie_file)
            with open(cookie_file, 'r', encoding='utf-8') as f:
                lines = sum(1 for _ in f)
            has_youtube = self.validate_cookie_file()
            logger.info(f"Cookie file verified: {cookie_file}")
            logger.info(f"  Size: {size} bytes, {lines} lines")
            if has_youtube:
                logger.info("  Contains YouTube/Google cookies")
            else:
                logger.warning("  No YouTube/Google domains found in cookie file")
        except Exception as e:
            logger.error(f"Cookie verification error: {e}")

    def build_yt_dlp_args(self) -> list:
        """
        Build yt-dlp command arguments for cookie handling.

        Returns empty list if no cookie file is available.
        """
        cookie_file = self.get_cookie_file()
        if cookie_file:
            return ['--cookies', cookie_file]
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
        return age_hours > (30 * 24)

    def clear_cache(self):
        """Clear cached cookie path so next call re-checks the file."""
        self.cookie_path = None
        self.loaded = False
        self.last_validated = None


# Global cookie manager instance
cookie_manager = CookieManager()


def get_cookies_file() -> Optional[str]:
    """Convenience function to get cookies file path."""
    return cookie_manager.get_cookie_file()


def get_yt_dlp_cookie_args() -> list:
    """Convenience function to get yt-dlp cookie arguments."""
    return cookie_manager.build_yt_dlp_args()


def validate_cookies() -> bool:
    """Convenience function to validate cookies."""
    return cookie_manager.validate_cookie_file()


def should_refresh_cookies() -> bool:
    """Convenience function to check if cookies need refresh."""
    return cookie_manager.suggest_cookie_refresh()
