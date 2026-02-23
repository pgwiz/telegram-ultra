-- Hermes Database Schema - Initial Migration
-- Creates all core tables for the Hermes download system.

-- Users table
CREATE TABLE IF NOT EXISTS users (
    chat_id INTEGER PRIMARY KEY,
    username TEXT,
    first_seen TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_admin BOOLEAN NOT NULL DEFAULT 0,
    last_activity TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Download tasks
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    chat_id INTEGER NOT NULL,
    task_type TEXT NOT NULL DEFAULT 'youtube',
    url TEXT NOT NULL,
    label TEXT,
    status TEXT NOT NULL DEFAULT 'queued',
    progress INTEGER NOT NULL DEFAULT 0,
    file_path TEXT,
    file_url TEXT,
    scheduled_at TIMESTAMP,
    started_at TIMESTAMP,
    finished_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    error_msg TEXT,
    FOREIGN KEY (chat_id) REFERENCES users(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_tasks_chat_id ON tasks(chat_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at);

-- Sessions for web dashboard auth
CREATE TABLE IF NOT EXISTS sessions (
    token TEXT PRIMARY KEY,
    chat_id INTEGER NOT NULL,
    expires_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (chat_id) REFERENCES users(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_sessions_chat_id ON sessions(chat_id);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

-- Media tasks (detailed tracking)
CREATE TABLE IF NOT EXISTS media_tasks (
    task_id TEXT PRIMARY KEY,
    user_chat_id INTEGER NOT NULL,
    task_type TEXT NOT NULL,
    url TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    progress_percent INTEGER NOT NULL DEFAULT 0,
    current_speed TEXT,
    eta_seconds INTEGER,
    result_file_path TEXT,
    file_size_bytes INTEGER,
    error_code TEXT,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    started_at TIMESTAMP,
    finished_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_chat_id) REFERENCES users(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_media_tasks_user ON media_tasks(user_chat_id);
CREATE INDEX IF NOT EXISTS idx_media_tasks_status ON media_tasks(status);

-- User preferences
CREATE TABLE IF NOT EXISTS user_preferences (
    chat_id INTEGER PRIMARY KEY,
    audio_format TEXT NOT NULL DEFAULT 'mp3',
    audio_quality TEXT NOT NULL DEFAULT '192',
    create_archives BOOLEAN NOT NULL DEFAULT 1,
    archive_max_size_mb INTEGER NOT NULL DEFAULT 100,
    auto_delete_original_files BOOLEAN NOT NULL DEFAULT 0,
    language TEXT NOT NULL DEFAULT 'en',
    timezone TEXT NOT NULL DEFAULT 'UTC',
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (chat_id) REFERENCES users(chat_id)
);

-- Download history
CREATE TABLE IF NOT EXISTS download_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_chat_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    file_path TEXT,
    file_size_bytes INTEGER,
    duration_seconds INTEGER,
    source TEXT NOT NULL DEFAULT 'youtube',
    is_favorite BOOLEAN NOT NULL DEFAULT 0,
    downloaded_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_chat_id) REFERENCES users(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_history_user ON download_history(user_chat_id);
CREATE INDEX IF NOT EXISTS idx_history_date ON download_history(downloaded_at);

-- Favorite playlists
CREATE TABLE IF NOT EXISTS favorite_playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_chat_id INTEGER NOT NULL,
    playlist_url TEXT NOT NULL,
    playlist_name TEXT NOT NULL,
    playlist_id TEXT,
    added_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_chat_id) REFERENCES users(chat_id),
    UNIQUE(user_chat_id, playlist_url)
);

-- YouTube metadata cache
CREATE TABLE IF NOT EXISTS youtube_metadata_cache (
    video_id TEXT PRIMARY KEY,
    metadata_json TEXT NOT NULL,
    fetched_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_metadata_cache_fetched ON youtube_metadata_cache(fetched_at);

-- Search cache
CREATE TABLE IF NOT EXISTS search_cache (
    query_hash TEXT PRIMARY KEY,
    query_text TEXT NOT NULL,
    results_json TEXT NOT NULL,
    result_count INTEGER NOT NULL DEFAULT 0,
    fetched_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_search_cache_fetched ON search_cache(fetched_at);

-- Rate limits
CREATE TABLE IF NOT EXISTS rate_limits (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_chat_id INTEGER NOT NULL,
    action TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    window_start TIMESTAMP NOT NULL,
    window_end TIMESTAMP NOT NULL,
    FOREIGN KEY (user_chat_id) REFERENCES users(chat_id),
    UNIQUE(user_chat_id, action)
);

-- API usage statistics
CREATE TABLE IF NOT EXISTS api_usage_stats (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_chat_id INTEGER NOT NULL,
    action TEXT NOT NULL,
    execution_time_ms INTEGER,
    success BOOLEAN NOT NULL DEFAULT 1,
    error_code TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_chat_id) REFERENCES users(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_usage_stats_user ON api_usage_stats(user_chat_id);
CREATE INDEX IF NOT EXISTS idx_usage_stats_date ON api_usage_stats(created_at);
