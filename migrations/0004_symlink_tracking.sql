-- Smart Track Deduplication System with Symlinks
-- Phase 1: Database schema for file storage, symlink tracking, and user preferences
-- NOTE: This migration is shared by both Python and Rust systems.
-- Rust uses this for core dedup tables; Python runs its own logic in worker/database.py

-- Track physical files in global storage pool
CREATE TABLE IF NOT EXISTS file_storage (
    file_hash_sha1 TEXT PRIMARY KEY,
    physical_path TEXT NOT NULL UNIQUE,
    file_size_bytes BIGINT,
    file_extension TEXT,
    youtube_url TEXT,
    title TEXT,
    first_downloaded_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    access_count INTEGER DEFAULT 0,
    last_accessed_at TIMESTAMP,
    is_protected BOOLEAN DEFAULT 1
);

-- Track symlinks per user
CREATE TABLE IF NOT EXISTS user_symlinks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_chat_id INTEGER NOT NULL,
    file_hash_sha1 TEXT NOT NULL,
    symlink_path TEXT NOT NULL,
    is_protected BOOLEAN DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_chat_id) REFERENCES users(chat_id) ON DELETE CASCADE,
    FOREIGN KEY (file_hash_sha1) REFERENCES file_storage(file_hash_sha1) ON DELETE CASCADE,
    UNIQUE(symlink_path)
);

-- File metadata for corruption detection
CREATE TABLE IF NOT EXISTS file_metadata (
    file_hash_sha1 TEXT PRIMARY KEY,
    expected_size_bytes BIGINT,
    expected_duration_seconds INTEGER,
    corruption_checks INTEGER DEFAULT 0,
    last_checked_at TIMESTAMP,
    FOREIGN KEY (file_hash_sha1) REFERENCES file_storage(file_hash_sha1) ON DELETE CASCADE
);

-- Create indices for query performance
CREATE INDEX IF NOT EXISTS idx_file_storage_hash ON file_storage(file_hash_sha1);
CREATE INDEX IF NOT EXISTS idx_file_storage_url ON file_storage(youtube_url);
CREATE INDEX IF NOT EXISTS idx_user_symlinks_chat ON user_symlinks(user_chat_id);
CREATE INDEX IF NOT EXISTS idx_user_symlinks_hash ON user_symlinks(file_hash_sha1);
CREATE INDEX IF NOT EXISTS idx_user_symlinks_path ON user_symlinks(symlink_path);

