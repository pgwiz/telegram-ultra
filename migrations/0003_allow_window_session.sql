-- Fix allow_window session foreign key constraint
-- The allow_window is a special global session that doesn't belong to a specific user.
-- This migration recreates the sessions table with nullable chat_id to permit this.

-- Backup existing sessions (excluding expired ones)
CREATE TABLE sessions_backup AS
SELECT token, chat_id, expires_at, created_at
FROM sessions
WHERE expires_at > datetime('now');

-- Drop old sessions table and constraints
DROP TABLE IF EXISTS sessions;

-- Recreate sessions table with nullable chat_id
CREATE TABLE IF NOT EXISTS sessions (
    token TEXT PRIMARY KEY,
    chat_id INTEGER,
    expires_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (chat_id) REFERENCES users(chat_id)
);

CREATE INDEX IF NOT EXISTS idx_sessions_chat_id ON sessions(chat_id);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

-- Restore backed up sessions
INSERT INTO sessions (token, chat_id, expires_at, created_at)
SELECT token, chat_id, expires_at, created_at FROM sessions_backup;

-- Clean up
DROP TABLE sessions_backup;
