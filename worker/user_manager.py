"""
User management for Hermes Media Worker
Handles user preferences, history, and rate limiting
"""

import logging
from typing import Optional, Dict, Any, List
from datetime import datetime, timedelta
from worker.database import get_database


logger = logging.getLogger(__name__)


class UserManager:
    """Manage user data, preferences, and history."""

    @staticmethod
    async def get_or_create_user(chat_id: int, username: str = None) -> Dict[str, Any]:
        """
        Get or create user.

        Args:
            chat_id: Telegram chat ID
            username: Optional Telegram username

        Returns:
            User dictionary
        """
        try:
            db = await get_database()

            # Check if user exists
            user = await db.fetch_one(
                "SELECT * FROM users WHERE chat_id = ?",
                (chat_id,)
            )

            if user:
                # Update last activity
                await db.update(
                    "UPDATE users SET last_activity = ? WHERE chat_id = ?",
                    (datetime.now(), chat_id)
                )
                return user

            # Create new user
            await db.insert(
                """
                INSERT INTO users (chat_id, username, first_seen, last_activity)
                VALUES (?, ?, ?, ?)
                """,
                (chat_id, username or f'user_{chat_id}', datetime.now(), datetime.now())
            )

            # Create default preferences
            await db.insert(
                """
                INSERT INTO user_preferences (chat_id)
                VALUES (?)
                """,
                (chat_id,)
            )

            logger.info(f"Created new user: {chat_id}")

            return await db.fetch_one(
                "SELECT * FROM users WHERE chat_id = ?",
                (chat_id,)
            )

        except Exception as e:
            logger.error(f"Get or create user failed: {e}")
            return {}

    @staticmethod
    async def get_preferences(chat_id: int) -> Dict[str, Any]:
        """Get user preferences."""
        try:
            db = await get_database()

            prefs = await db.fetch_one(
                "SELECT * FROM user_preferences WHERE chat_id = ?",
                (chat_id,)
            )

            return prefs or {}

        except Exception as e:
            logger.error(f"Get preferences failed: {e}")
            return {}

    @staticmethod
    async def set_preferences(chat_id: int, preferences: Dict[str, Any]) -> bool:
        """Set user preferences."""
        try:
            db = await get_database()

            # Build UPDATE query dynamically
            updates = []
            params = []

            for key, value in preferences.items():
                if key in ('audio_format', 'audio_quality', 'language', 'timezone'):
                    updates.append(f"{key} = ?")
                    params.append(value)
                elif key in ('create_archives', 'auto_delete_original_files'):
                    updates.append(f"{key} = ?")
                    params.append(1 if value else 0)
                elif key == 'archive_max_size_mb':
                    updates.append(f"{key} = ?")
                    params.append(int(value))

            if not updates:
                return True

            updates.append("updated_at = ?")
            params.append(datetime.now())
            params.append(chat_id)

            query = f"UPDATE user_preferences SET {', '.join(updates)} WHERE chat_id = ?"

            await db.update(query, tuple(params))
            logger.info(f"Updated preferences for user {chat_id}")

            return True

        except Exception as e:
            logger.error(f"Set preferences failed: {e}")
            return False

    @staticmethod
    async def add_to_history(chat_id: int, title: str, url: str, file_path: str = None,
                            file_size_bytes: int = None, duration_seconds: int = None,
                            source: str = 'youtube') -> bool:
        """Add download to user history."""
        try:
            db = await get_database()

            await db.insert(
                """
                INSERT INTO download_history
                (user_chat_id, title, url, file_path, file_size_bytes, duration_seconds, source)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                (chat_id, title[:200], url[:500], file_path, file_size_bytes, duration_seconds, source)
            )

            logger.debug(f"Added to history for user {chat_id}: {title[:50]}")
            return True

        except Exception as e:
            logger.error(f"Add to history failed: {e}")
            return False

    @staticmethod
    async def get_history(chat_id: int, limit: int = 50) -> List[Dict[str, Any]]:
        """Get user download history."""
        try:
            db = await get_database()

            history = await db.fetch_all(
                """
                SELECT * FROM download_history
                WHERE user_chat_id = ?
                ORDER BY downloaded_at DESC
                LIMIT ?
                """,
                (chat_id, limit)
            )

            return history

        except Exception as e:
            logger.error(f"Get history failed: {e}")
            return []

    @staticmethod
    async def mark_favorite(chat_id: int, history_id: int) -> bool:
        """Mark download as favorite."""
        try:
            db = await get_database()

            await db.update(
                "UPDATE download_history SET is_favorite = 1 WHERE id = ? AND user_chat_id = ?",
                (history_id, chat_id)
            )

            logger.info(f"Marked as favorite for user {chat_id}")
            return True

        except Exception as e:
            logger.error(f"Mark favorite failed: {e}")
            return False

    @staticmethod
    async def get_favorites(chat_id: int, limit: int = 20) -> List[Dict[str, Any]]:
        """Get user's favorite downloads."""
        try:
            db = await get_database()

            favorites = await db.fetch_all(
                """
                SELECT * FROM download_history
                WHERE user_chat_id = ? AND is_favorite = 1
                ORDER BY downloaded_at DESC
                LIMIT ?
                """,
                (chat_id, limit)
            )

            return favorites

        except Exception as e:
            logger.error(f"Get favorites failed: {e}")
            return []

    @staticmethod
    async def add_playlist_favorite(chat_id: int, playlist_url: str, playlist_name: str,
                                   playlist_id: str = None) -> bool:
        """Add playlist to favorites."""
        try:
            db = await get_database()

            await db.insert(
                """
                INSERT OR IGNORE INTO favorite_playlists
                (user_chat_id, playlist_url, playlist_name, playlist_id)
                VALUES (?, ?, ?, ?)
                """,
                (chat_id, playlist_url, playlist_name[:200], playlist_id)
            )

            logger.info(f"Added playlist favorite for user {chat_id}: {playlist_name[:50]}")
            return True

        except Exception as e:
            logger.error(f"Add playlist favorite failed: {e}")
            return False

    @staticmethod
    async def get_playlist_favorites(chat_id: int) -> List[Dict[str, Any]]:
        """Get user's favorite playlists."""
        try:
            db = await get_database()

            playlists = await db.fetch_all(
                """
                SELECT * FROM favorite_playlists
                WHERE user_chat_id = ?
                ORDER BY added_at DESC
                """,
                (chat_id,)
            )

            return playlists

        except Exception as e:
            logger.error(f"Get playlist favorites failed: {e}")
            return []


class RateLimiter:
    """Rate limiting for user actions."""

    # Default limits (requests per hour)
    DEFAULT_LIMITS = {
        'search': 60,
        'download': 20,
        'playlist': 10,
    }

    @staticmethod
    async def check_limit(chat_id: int, action: str) -> tuple[bool, Optional[int]]:
        """
        Check if user exceeded rate limit.

        Args:
            chat_id: Telegram chat ID
            action: Action name (search, download, playlist)

        Returns:
            (allowed, seconds_until_reset) tuple
        """
        try:
            db = await get_database()
            limit = RateLimiter.DEFAULT_LIMITS.get(action, 60)

            # Get current window
            now = datetime.now()
            window_start = now - timedelta(hours=1)

            # Check and update
            rate_limit = await db.fetch_one(
                """
                SELECT * FROM rate_limits
                WHERE user_chat_id = ? AND action = ?
                """,
                (chat_id, action)
            )

            if not rate_limit:
                # First request in window
                await db.insert(
                    """
                    INSERT INTO rate_limits
                    (user_chat_id, action, attempt_count, window_start, window_end)
                    VALUES (?, ?, 1, ?, ?)
                    """,
                    (chat_id, action, now, now + timedelta(hours=1))
                )
                return True, None

            window_end = rate_limit['window_end']

            if datetime.fromisoformat(window_end) < now:
                # Window expired, reset
                await db.update(
                    """
                    UPDATE rate_limits
                    SET attempt_count = 1, window_start = ?, window_end = ?
                    WHERE user_chat_id = ? AND action = ?
                    """,
                    (now, now + timedelta(hours=1), chat_id, action)
                )
                return True, None

            # Check if limit exceeded
            attempt_count = rate_limit['attempt_count']

            if attempt_count >= limit:
                seconds_until_reset = int((
                    datetime.fromisoformat(window_end) - now
                ).total_seconds())
                logger.warning(f"Rate limit exceeded for user {chat_id} action {action}")
                return False, seconds_until_reset

            # Increment
            await db.update(
                """
                UPDATE rate_limits
                SET attempt_count = attempt_count + 1
                WHERE user_chat_id = ? AND action = ?
                """,
                (chat_id, action)
            )

            return True, None

        except Exception as e:
            logger.error(f"Rate limit check failed: {e}")
            # On error, allow the action (fail-open)
            return True, None

    @staticmethod
    async def record_usage(chat_id: int, action: str, execution_time_ms: int,
                          success: bool = True, error_code: str = None) -> None:
        """Record API usage statistics."""
        try:
            db = await get_database()

            await db.insert(
                """
                INSERT INTO api_usage_stats
                (user_chat_id, action, execution_time_ms, success, error_code)
                VALUES (?, ?, ?, ?, ?)
                """,
                (chat_id, action, execution_time_ms, 1 if success else 0, error_code)
            )

        except Exception as e:
            logger.warning(f"Record usage failed: {e}")
