"""
Database layer for Hermes Media Worker
Async SQLite wrapper with migration support
"""

import os
import sqlite3
import aiosqlite
import logging
from typing import Optional, List, Dict, Any, Tuple
from datetime import datetime, timedelta
from worker.config import config


logger = logging.getLogger(__name__)


class Database:
    """Async SQLite database wrapper."""

    def __init__(self, db_path: str = None):
        self.db_path = db_path or config.DATABASE_URL.replace('sqlite:///', '')
        self.connection: Optional[aiosqlite.Connection] = None

    async def connect(self) -> None:
        """Connect to database."""
        try:
            self.connection = await aiosqlite.connect(self.db_path, check_same_thread=False)
            # Enable WAL mode for concurrent access with bot/API
            await self.connection.execute('PRAGMA journal_mode = WAL')
            await self.connection.execute('PRAGMA busy_timeout = 10000')
            # Enable foreign keys
            await self.connection.execute('PRAGMA foreign_keys = ON')
            await self.connection.commit()
            logger.info(f"Connected to database: {self.db_path}")
        except Exception as e:
            logger.error(f"Failed to connect to database: {e}")
            raise

    async def disconnect(self) -> None:
        """Disconnect from database."""
        if self.connection:
            await self.connection.close()
            logger.info("Database disconnected")

    async def migrate(self) -> None:
        """Run database migrations.

        Each migration uses CREATE TABLE IF NOT EXISTS, so they are
        safe to re-run.  We run each one independently so that a
        failure in one (e.g. table already exists with a different
        schema created by the Rust side) does not block the others.
        """
        migration_names = [
            ("0001_initial", self._migration_0001_initial()),
            ("0002_media_tasks", self._migration_0002_media_tasks()),
            ("0003_user_preferences", self._migration_0003_user_preferences()),
            ("0004_cache_tables", self._migration_0004_cache_tables()),
            ("0005_rate_limiting", self._migration_0005_rate_limiting()),
        ]

        failures = 0
        for name, migration in migration_names:
            try:
                await self.connection.executescript(migration)
                await self.connection.commit()
            except Exception as e:
                failures += 1
                logger.warning(f"Migration {name} skipped (already applied by Rust?): {e}")

        if failures == 0:
            logger.info("✅ Database migrations completed")
        else:
            logger.info(f"✅ Database migrations completed ({failures} already applied, skipped)")

    async def execute(self, query: str, params: Tuple = ()) -> aiosqlite.Cursor:
        """Execute query."""
        if not self.connection:
            raise RuntimeError("Database not connected")
        return await self.connection.execute(query, params)

    async def fetch_one(self, query: str, params: Tuple = ()) -> Optional[Dict[str, Any]]:
        """Fetch single row."""
        cursor = await self.execute(query, params)
        cursor.row_factory = sqlite3.Row
        row = await cursor.fetchone()
        return dict(row) if row else None

    async def fetch_all(self, query: str, params: Tuple = ()) -> List[Dict[str, Any]]:
        """Fetch all rows."""
        cursor = await self.execute(query, params)
        cursor.row_factory = sqlite3.Row
        rows = await cursor.fetchall()
        return [dict(row) for row in rows]

    async def insert(self, query: str, params: Tuple = ()) -> int:
        """Insert row and return last insert rowid."""
        cursor = await self.execute(query, params)
        await self.connection.commit()
        return cursor.lastrowid

    async def update(self, query: str, params: Tuple = ()) -> int:
        """Update rows and return affected count."""
        cursor = await self.execute(query, params)
        await self.connection.commit()
        return cursor.rowcount

    async def delete(self, query: str, params: Tuple = ()) -> int:
        """Delete rows and return affected count."""
        cursor = await self.execute(query, params)
        await self.connection.commit()
        return cursor.rowcount

    async def commit(self) -> None:
        """Commit transaction."""
        if self.connection:
            await self.connection.commit()

    # ===== MIGRATION DEFINITIONS =====

    @staticmethod
    def _migration_0001_initial() -> str:
        """Initial schema with users and tasks."""
        return """
        CREATE TABLE IF NOT EXISTS users (
            chat_id INTEGER PRIMARY KEY,
            username TEXT,
            first_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
            is_admin BOOLEAN DEFAULT FALSE,
            last_activity DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY,
            chat_id INTEGER NOT NULL,
            task_type TEXT NOT NULL,
            url TEXT NOT NULL,
            label TEXT,
            status TEXT DEFAULT 'queued',
            progress INTEGER DEFAULT 0,
            file_path TEXT,
            file_url TEXT,
            scheduled_at DATETIME,
            started_at DATETIME,
            finished_at DATETIME,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            error_msg TEXT,
            FOREIGN KEY (chat_id) REFERENCES users(chat_id)
        );

        CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            chat_id INTEGER NOT NULL,
            expires_at DATETIME NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (chat_id) REFERENCES users(chat_id)
        );

        CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_tasks_chat_id ON tasks(chat_id);
        CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
        CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions(expires_at);
        """

    @staticmethod
    def _migration_0002_media_tasks() -> str:
        """Enhanced media task tracking."""
        return """
        CREATE TABLE IF NOT EXISTS media_tasks (
            task_id TEXT PRIMARY KEY,
            user_chat_id INTEGER NOT NULL,
            task_type TEXT NOT NULL,
            url TEXT NOT NULL,
            status TEXT DEFAULT 'pending',
            progress_percent INTEGER DEFAULT 0,
            current_speed TEXT,
            eta_seconds INTEGER,
            result_file_path TEXT,
            file_size_bytes BIGINT,
            error_code TEXT,
            error_message TEXT,
            retry_count INTEGER DEFAULT 0,
            started_at DATETIME,
            finished_at DATETIME,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (user_chat_id) REFERENCES users(chat_id)
        );

        CREATE TABLE IF NOT EXISTS task_progress_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL,
            timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
            percent INTEGER,
            speed_mbps REAL,
            eta_seconds INTEGER,
            FOREIGN KEY (task_id) REFERENCES media_tasks(task_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS playlists (
            playlist_id TEXT PRIMARY KEY,
            user_chat_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            description TEXT,
            url TEXT,
            total_tracks INTEGER,
            downloaded_tracks INTEGER DEFAULT 0,
            status TEXT DEFAULT 'pending',
            folder_path TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (user_chat_id) REFERENCES users(chat_id)
        );

        CREATE INDEX IF NOT EXISTS idx_media_tasks_chat_id ON media_tasks(user_chat_id);
        CREATE INDEX IF NOT EXISTS idx_media_tasks_status ON media_tasks(status);
        CREATE INDEX IF NOT EXISTS idx_playlists_chat_id ON playlists(user_chat_id);
        """

    @staticmethod
    def _migration_0003_user_preferences() -> str:
        """User preferences and download history."""
        return """
        CREATE TABLE IF NOT EXISTS user_preferences (
            chat_id INTEGER PRIMARY KEY,
            audio_format TEXT DEFAULT 'mp3',
            audio_quality TEXT DEFAULT '192',
            create_archives BOOLEAN DEFAULT TRUE,
            archive_max_size_mb INTEGER DEFAULT 100,
            auto_delete_original_files BOOLEAN DEFAULT FALSE,
            language TEXT DEFAULT 'en',
            timezone TEXT DEFAULT 'UTC',
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (chat_id) REFERENCES users(chat_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS download_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_chat_id INTEGER NOT NULL,
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            file_path TEXT,
            file_size_bytes BIGINT,
            duration_seconds INTEGER,
            source TEXT,
            downloaded_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            is_favorite BOOLEAN DEFAULT FALSE,
            FOREIGN KEY (user_chat_id) REFERENCES users(chat_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS favorite_playlists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_chat_id INTEGER NOT NULL,
            playlist_url TEXT NOT NULL,
            playlist_name TEXT NOT NULL,
            playlist_id TEXT,
            added_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(user_chat_id, playlist_url),
            FOREIGN KEY (user_chat_id) REFERENCES users(chat_id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_download_history_chat_id ON download_history(user_chat_id);
        CREATE INDEX IF NOT EXISTS idx_download_history_favorite ON download_history(is_favorite);
        CREATE INDEX IF NOT EXISTS idx_favorite_playlists_chat_id ON favorite_playlists(user_chat_id);
        """

    @staticmethod
    def _migration_0004_cache_tables() -> str:
        """Metadata caching to avoid repeated API calls."""
        return """
        CREATE TABLE IF NOT EXISTS youtube_metadata_cache (
            video_id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            uploader TEXT,
            duration_seconds INTEGER,
            thumbnail_url TEXT,
            is_age_restricted BOOLEAN DEFAULT FALSE,
            is_playlist BOOLEAN DEFAULT FALSE,
            is_private BOOLEAN DEFAULT FALSE,
            fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            expires_at DATETIME,
            access_count INTEGER DEFAULT 0,
            last_accessed DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS search_cache (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            query TEXT NOT NULL,
            query_hash TEXT NOT NULL UNIQUE,
            results_json TEXT NOT NULL,
            cached_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            expires_at DATETIME,
            access_count INTEGER DEFAULT 0,
            last_accessed DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS cookie_management (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            cookie_file_path TEXT NOT NULL,
            source TEXT,
            is_valid BOOLEAN DEFAULT TRUE,
            validation_error TEXT,
            expires_at DATETIME,
            last_validated DATETIME,
            validation_count INTEGER DEFAULT 0,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE INDEX IF NOT EXISTS idx_youtube_metadata_expires_at ON youtube_metadata_cache(expires_at);
        CREATE INDEX IF NOT EXISTS idx_search_cache_expires_at ON search_cache(expires_at);
        """

    @staticmethod
    def _migration_0005_rate_limiting() -> str:
        """Rate limiting per user and action."""
        return """
        CREATE TABLE IF NOT EXISTS rate_limits (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_chat_id INTEGER NOT NULL,
            action TEXT NOT NULL,
            attempt_count INTEGER DEFAULT 1,
            window_start DATETIME DEFAULT CURRENT_TIMESTAMP,
            window_end DATETIME,
            UNIQUE(user_chat_id, action),
            FOREIGN KEY (user_chat_id) REFERENCES users(chat_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS api_usage_stats (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_chat_id INTEGER NOT NULL,
            action TEXT NOT NULL,
            execution_time_ms INTEGER,
            success BOOLEAN DEFAULT TRUE,
            error_code TEXT,
            recorded_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (user_chat_id) REFERENCES users(chat_id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_rate_limits_chat_id ON rate_limits(user_chat_id);
        CREATE INDEX IF NOT EXISTS idx_api_usage_stats_chat_id ON api_usage_stats(user_chat_id);
        """


# Global database instance
_db_instance: Optional[Database] = None


async def get_database() -> Database:
    """Get or create global database instance."""
    global _db_instance

    if _db_instance is None:
        _db_instance = Database()
        await _db_instance.connect()
        await _db_instance.migrate()

    return _db_instance


async def close_database() -> None:
    """Close global database instance."""
    global _db_instance

    if _db_instance:
        await _db_instance.disconnect()
        _db_instance = None
