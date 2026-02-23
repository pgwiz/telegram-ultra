-- Fix cache table schemas to match Python worker expectations.
-- The initial migration created these tables with a Rust-centric schema
-- (metadata_json blob), but the Python worker needs individual columns.
-- Since no data worth preserving exists in cache tables, we drop and recreate.

-- Drop old cache tables and their indexes
DROP TABLE IF EXISTS youtube_metadata_cache;
DROP TABLE IF EXISTS search_cache;

-- Recreate youtube_metadata_cache with individual columns (Python-compatible)
CREATE TABLE IF NOT EXISTS youtube_metadata_cache (
    video_id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    uploader TEXT,
    duration_seconds INTEGER,
    thumbnail_url TEXT,
    is_age_restricted BOOLEAN DEFAULT 0,
    is_playlist BOOLEAN DEFAULT 0,
    is_private BOOLEAN DEFAULT 0,
    fetched_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMP,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_metadata_cache_fetched ON youtube_metadata_cache(fetched_at);
CREATE INDEX IF NOT EXISTS idx_youtube_metadata_expires_at ON youtube_metadata_cache(expires_at);

-- Recreate search_cache with Python-compatible schema
CREATE TABLE IF NOT EXISTS search_cache (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    query TEXT NOT NULL,
    query_hash TEXT NOT NULL UNIQUE,
    results_json TEXT NOT NULL,
    result_count INTEGER NOT NULL DEFAULT 0,
    cached_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    fetched_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMP,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_search_cache_fetched ON search_cache(fetched_at);
CREATE INDEX IF NOT EXISTS idx_search_cache_expires_at ON search_cache(expires_at);

-- Add cookie_management table (referenced by Python migration 0004)
CREATE TABLE IF NOT EXISTS cookie_management (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cookie_file_path TEXT NOT NULL,
    source TEXT,
    is_valid BOOLEAN DEFAULT 1,
    validation_error TEXT,
    expires_at TIMESTAMP,
    last_validated TIMESTAMP,
    validation_count INTEGER DEFAULT 0,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Add task_progress_history table (referenced by Python migration 0002)
CREATE TABLE IF NOT EXISTS task_progress_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    percent INTEGER,
    speed_mbps REAL,
    eta_seconds INTEGER,
    FOREIGN KEY (task_id) REFERENCES media_tasks(task_id) ON DELETE CASCADE
);

-- Add playlists table (referenced by Python migration 0002)
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
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_chat_id) REFERENCES users(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_playlists_chat_id ON playlists(user_chat_id);

-- Add config table (referenced by Python migration 0001)
CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
