-- Extend user_preferences with additional columns for the Settings page.

ALTER TABLE user_preferences ADD COLUMN default_mode TEXT NOT NULL DEFAULT 'audio';
ALTER TABLE user_preferences ADD COLUMN dedup_enabled BOOLEAN NOT NULL DEFAULT 1;
ALTER TABLE user_preferences ADD COLUMN video_quality TEXT NOT NULL DEFAULT 'best';
