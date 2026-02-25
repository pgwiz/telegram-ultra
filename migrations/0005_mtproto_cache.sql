-- MTProto upload tracking columns on tasks
-- Applied by sqlx::migrate! at bot startup.
ALTER TABLE tasks ADD COLUMN channel_msg_id INTEGER;
ALTER TABLE tasks ADD COLUMN uploaded_at    DATETIME;
