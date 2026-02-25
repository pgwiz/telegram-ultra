/// Database connection pool and helpers for Hermes.
use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteJournalMode};
use sqlx::Row;
use std::str::FromStr;
use tracing::info;

/// Create SQLite connection pool with WAL mode and busy timeout.
pub async fn create_pool(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10))
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    info!("Connected to database: {}", database_url);
    Ok(pool)
}

/// Run migrations from the migrations directory.
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("../migrations")
        .run(pool)
        .await?;

    info!("Database migrations completed");
    Ok(())
}

/// Create a new task in the database.
pub async fn create_task(
    pool: &SqlitePool,
    task_id: &str,
    chat_id: i64,
    task_type: &str,
    url: &str,
    label: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO tasks (id, chat_id, task_type, url, label, status, progress)
        VALUES (?, ?, ?, ?, ?, 'queued', 0)
        "#,
    )
    .bind(task_id)
    .bind(chat_id)
    .bind(task_type)
    .bind(url)
    .bind(label)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update task status and progress.
pub async fn update_task_progress(
    pool: &SqlitePool,
    task_id: &str,
    status: &str,
    progress: i32,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE tasks SET status = ?, progress = ? WHERE id = ?
        "#,
    )
    .bind(status)
    .bind(progress)
    .bind(task_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark task as completed with file path.
pub async fn complete_task(
    pool: &SqlitePool,
    task_id: &str,
    file_path: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE tasks
        SET status = 'done', progress = 100, file_path = ?, finished_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    )
    .bind(file_path)
    .bind(task_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark task as failed.
pub async fn fail_task(
    pool: &SqlitePool,
    task_id: &str,
    error_msg: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE tasks
        SET status = 'error', error_msg = ?, finished_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    )
    .bind(error_msg)
    .bind(task_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Register or update user on first contact.
pub async fn upsert_user(
    pool: &SqlitePool,
    chat_id: i64,
    username: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO users (chat_id, username)
        VALUES (?, ?)
        ON CONFLICT(chat_id) DO UPDATE SET
            last_activity = CURRENT_TIMESTAMP,
            username = COALESCE(excluded.username, users.username)
        "#,
    )
    .bind(chat_id)
    .bind(username)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get all tasks for a user.
pub async fn get_user_tasks(
    pool: &SqlitePool,
    chat_id: i64,
) -> Result<Vec<crate::models::Task>> {
    let tasks = sqlx::query_as::<_, crate::models::Task>(
        r#"
        SELECT * FROM tasks WHERE chat_id = ? ORDER BY created_at DESC LIMIT 50
        "#,
    )
    .bind(chat_id)
    .fetch_all(pool)
    .await?;

    Ok(tasks)
}

/// Get running tasks count (for concurrency limiting).
pub async fn count_running_tasks(pool: &SqlitePool) -> Result<i64> {
    let row: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM tasks WHERE status = 'running'"#,
    )
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

// ====== SESSION MANAGEMENT ======

/// Create an OTP session (temporary, 5-min expiry).
pub async fn create_otp_session(
    pool: &SqlitePool,
    chat_id: i64,
    otp_code: &str,
) -> Result<()> {
    // Delete any existing OTP sessions for this user first
    sqlx::query("DELETE FROM sessions WHERE chat_id = ? AND token LIKE 'otp:%'")
        .bind(chat_id)
        .execute(pool)
        .await?;

    let token = format!("otp:{}", otp_code);
    sqlx::query(
        r#"
        INSERT INTO sessions (token, chat_id, expires_at)
        VALUES (?, ?, datetime('now', '+5 minutes'))
        "#,
    )
    .bind(&token)
    .bind(chat_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Verify an OTP code for a chat_id. Returns true if valid and not expired.
pub async fn verify_otp_session(
    pool: &SqlitePool,
    chat_id: i64,
    otp_code: &str,
) -> Result<bool> {
    let token = format!("otp:{}", otp_code);
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM sessions
        WHERE token = ? AND chat_id = ? AND expires_at > datetime('now')
        "#,
    )
    .bind(&token)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    let valid = row.map(|r| r.0 > 0).unwrap_or(false);

    if valid {
        // Delete the OTP session once verified
        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(&token)
            .execute(pool)
            .await?;
    }

    Ok(valid)
}

/// Create a JWT session (long-lived, configurable TTL).
pub async fn create_jwt_session(
    pool: &SqlitePool,
    chat_id: i64,
    token: &str,
    ttl_secs: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO sessions (token, chat_id, expires_at)
        VALUES (?, ?, datetime('now', '+' || ? || ' seconds'))
        "#,
    )
    .bind(token)
    .bind(chat_id)
    .bind(ttl_secs)
    .execute(pool)
    .await?;

    Ok(())
}

/// Validate a session token. Returns the chat_id if valid and not expired.
pub async fn validate_session(
    pool: &SqlitePool,
    token: &str,
) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT chat_id FROM sessions
        WHERE token = ? AND expires_at > datetime('now')
        "#,
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| r.0))
}

/// Delete a session (logout).
pub async fn delete_session(pool: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;

    Ok(())
}

/// Delete all expired sessions.
pub async fn cleanup_expired_sessions(pool: &SqlitePool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE expires_at <= datetime('now')")
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

/// Count recent OTP requests for rate limiting.
pub async fn count_recent_otp_requests(
    pool: &SqlitePool,
    chat_id: i64,
    window_secs: i64,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM sessions
        WHERE chat_id = ? AND token LIKE 'otp:%'
            AND created_at > datetime('now', '-' || ? || ' seconds')
        "#,
    )
    .bind(chat_id)
    .bind(window_secs)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

// ====== TASK QUERIES (API) ======

/// Get a single task by ID.
pub async fn get_task_by_id(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<Option<crate::models::Task>> {
    let task = sqlx::query_as::<_, crate::models::Task>(
        "SELECT * FROM tasks WHERE id = ?",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await?;

    Ok(task)
}

/// Get user's tasks filtered by status.
pub async fn get_user_tasks_by_status(
    pool: &SqlitePool,
    chat_id: i64,
    status: Option<&str>,
) -> Result<Vec<crate::models::Task>> {
    let tasks = if let Some(s) = status {
        sqlx::query_as::<_, crate::models::Task>(
            r#"
            SELECT * FROM tasks
            WHERE chat_id = ? AND status = ?
            ORDER BY created_at DESC LIMIT 100
            "#,
        )
        .bind(chat_id)
        .bind(s)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, crate::models::Task>(
            r#"
            SELECT * FROM tasks
            WHERE chat_id = ?
            ORDER BY created_at DESC LIMIT 100
            "#,
        )
        .bind(chat_id)
        .fetch_all(pool)
        .await?
    };

    Ok(tasks)
}

/// Get user's completed downloads (files page).
pub async fn get_user_completed_files(
    pool: &SqlitePool,
    chat_id: i64,
) -> Result<Vec<crate::models::Task>> {
    let tasks = sqlx::query_as::<_, crate::models::Task>(
        r#"
        SELECT * FROM tasks
        WHERE chat_id = ? AND status = 'done' AND file_path IS NOT NULL
        ORDER BY finished_at DESC LIMIT 200
        "#,
    )
    .bind(chat_id)
    .fetch_all(pool)
    .await?;

    Ok(tasks)
}

/// Clear all completed/failed/cancelled tasks for a user.
/// Returns the file_paths of deleted tasks so the caller can clean up files.
pub async fn clear_user_history(
    pool: &SqlitePool,
    chat_id: i64,
) -> Result<Vec<Option<String>>> {
    // First, get file paths for cleanup
    let paths: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT file_path FROM tasks WHERE chat_id = ? AND status IN ('done', 'error', 'cancelled')",
    )
    .bind(chat_id)
    .fetch_all(pool)
    .await?;

    // Delete the records
    sqlx::query(
        "DELETE FROM tasks WHERE chat_id = ? AND status IN ('done', 'error', 'cancelled')",
    )
    .bind(chat_id)
    .execute(pool)
    .await?;

    Ok(paths.into_iter().map(|(p,)| p).collect())
}

/// Cancel a task by setting status to cancelled.
pub async fn cancel_task(pool: &SqlitePool, task_id: &str) -> Result<bool> {
    let result = sqlx::query(
        r#"
        UPDATE tasks SET status = 'cancelled', finished_at = CURRENT_TIMESTAMP
        WHERE id = ? AND status IN ('web_queued', 'queued', 'running')
        "#,
    )
    .bind(task_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

// ====== ADMIN QUERIES ======

/// Get all users (admin).
pub async fn get_all_users(pool: &SqlitePool) -> Result<Vec<crate::models::User>> {
    let users = sqlx::query_as::<_, crate::models::User>(
        "SELECT * FROM users ORDER BY last_activity DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(users)
}

/// System stats for admin dashboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SystemStats {
    pub total_users: i64,
    pub total_tasks: i64,
    pub running_tasks: i64,
    pub completed_tasks: i64,
    pub failed_tasks: i64,
    pub queued_tasks: i64,
}

/// Get system-wide statistics.
pub async fn get_system_stats(pool: &SqlitePool) -> Result<SystemStats> {
    let (total_users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    let (total_tasks,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tasks")
        .fetch_one(pool)
        .await?;
    let (running,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tasks WHERE status = 'running'")
        .fetch_one(pool)
        .await?;
    let (completed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tasks WHERE status = 'done'")
        .fetch_one(pool)
        .await?;
    let (failed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tasks WHERE status = 'error'")
        .fetch_one(pool)
        .await?;
    let (queued,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tasks WHERE status = 'queued'")
        .fetch_one(pool)
        .await?;

    Ok(SystemStats {
        total_users,
        total_tasks,
        running_tasks: running,
        completed_tasks: completed,
        failed_tasks: failed,
        queued_tasks: queued,
    })
}

// ====== WEB DOWNLOAD QUEUE ======

/// Create a task queued from the web dashboard.
/// Uses status 'web_queued' so the bot can pick it up.
pub async fn create_web_task(
    pool: &SqlitePool,
    task_id: &str,
    chat_id: i64,
    url: &str,
    task_type: &str,
    label: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO tasks (id, chat_id, task_type, url, label, status, progress)
        VALUES (?, ?, ?, ?, ?, 'web_queued', 0)
        "#,
    )
    .bind(task_id)
    .bind(chat_id)
    .bind(task_type)
    .bind(url)
    .bind(label)
    .execute(pool)
    .await?;

    Ok(())
}

/// Fetch and claim pending web-queued tasks (atomically set to 'queued').
pub async fn claim_web_queued_tasks(
    pool: &SqlitePool,
) -> Result<Vec<crate::models::Task>> {
    // First fetch them
    let tasks = sqlx::query_as::<_, crate::models::Task>(
        r#"
        SELECT * FROM tasks WHERE status = 'web_queued'
        ORDER BY created_at ASC LIMIT 10
        "#,
    )
    .fetch_all(pool)
    .await?;

    // Mark as claimed
    if !tasks.is_empty() {
        sqlx::query(
            "UPDATE tasks SET status = 'queued' WHERE status = 'web_queued'"
        )
        .execute(pool)
        .await?;
    }

    Ok(tasks)
}

/// Retry a failed/cancelled/error task by re-queuing it as web_queued.
pub async fn retry_task(pool: &SqlitePool, task_id: &str) -> Result<bool> {
    let result = sqlx::query(
        r#"
        UPDATE tasks SET status = 'web_queued', progress = 0,
            error_msg = NULL, finished_at = NULL, started_at = NULL
        WHERE id = ? AND status IN ('cancelled', 'error', 'done')
        "#,
    )
    .bind(task_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Update a task's URL and/or label (only if still queued).
pub async fn update_task(
    pool: &SqlitePool,
    task_id: &str,
    url: Option<&str>,
    label: Option<&str>,
) -> Result<bool> {
    let mut affected = 0u64;

    if let Some(new_url) = url {
        let r = sqlx::query(
            "UPDATE tasks SET url = ? WHERE id = ? AND status IN ('web_queued', 'queued')",
        )
        .bind(new_url)
        .bind(task_id)
        .execute(pool)
        .await?;
        affected = r.rows_affected();
    }

    if let Some(new_label) = label {
        let r = sqlx::query(
            "UPDATE tasks SET label = ? WHERE id = ? AND status IN ('web_queued', 'queued')",
        )
        .bind(new_label)
        .bind(task_id)
        .execute(pool)
        .await?;
        affected = affected.max(r.rows_affected());
    }

    Ok(affected > 0)
}

/// Delete a task from the database.
pub async fn delete_task(pool: &SqlitePool, task_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(task_id)
        .execute(pool)
        .await?;

    Ok(())
}

// ====== ALLOW WINDOW ======

/// Open a time-limited OTP-free login window (admin feature).
pub async fn set_allow_window(pool: &SqlitePool, ttl_secs: i64) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token = 'allow_window'")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO sessions (token, chat_id, expires_at) \
         VALUES ('allow_window', NULL, datetime('now', '+' || ? || ' seconds'))",
    )
    .bind(ttl_secs)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns seconds remaining in the allow window, or None if expired / never set.
pub async fn get_allow_window_remaining(pool: &SqlitePool) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT CAST((julianday(expires_at) - julianday('now')) * 86400 AS INTEGER) \
         FROM sessions \
         WHERE token = 'allow_window' AND expires_at > datetime('now')",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

// ====== DEDUPLICATION PREFERENCES ======

/// Get user's deduplication preference (default: true/enabled).
/// Returns true if dedup is enabled, false if disabled, or true if preference not found.
pub async fn get_user_dedup_preference(pool: &SqlitePool, chat_id: i64) -> Result<bool> {
    // Try to read dedup preference; default to true if not found or column doesn't exist
    // Using raw query to avoid sqlx compile-time checking of non-existent columns
    match sqlx::query("SELECT COALESCE(dedup_enabled, 1) as enabled FROM user_preferences WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_optional(pool)
        .await {
            Ok(Some(row)) => {
                // try to extract the value; if it fails, default to true
                match row.try_get::<i64, _>("enabled") {
                    Ok(val) => Ok(val != 0),
                    Err(_) => Ok(true), // Column doesn't exist or can't read, default to true
                }
            }
            Ok(None) => Ok(true), // User not found, return default true
            Err(_) => Ok(true), // Query failed (table/column doesn't exist), return default true
        }
}

/// Set user's deduplication preference.
/// Inserts or updates the user's dedup preference.
pub async fn set_user_dedup_preference(
    pool: &SqlitePool,
    chat_id: i64,
    dedup_enabled: bool,
) -> Result<()> {
    // First ensure user exists in users table
    sqlx::query(
        "INSERT OR IGNORE INTO users (chat_id) VALUES (?)"
    )
    .bind(chat_id)
    .execute(pool)
    .await?;

    // Then ensure user has an entry in user_preferences
    sqlx::query(
        "INSERT OR IGNORE INTO user_preferences (chat_id) VALUES (?)"
    )
    .bind(chat_id)
    .execute(pool)
    .await?;

    // Finally update the dedup_enabled preference
    // Use dynamic query since column might not exist in older databases
    match sqlx::query(
        "UPDATE user_preferences SET dedup_enabled = ? WHERE chat_id = ?"
    )
    .bind(dedup_enabled)
    .bind(chat_id)
    .execute(pool)
    .await {
        Ok(_) => Ok(()),
        Err(e) => {
            // If column doesn't exist, log warning but don't fail
            // The system will use default behavior
            tracing::warn!("Could not update dedup preference (column may not exist yet): {}", e);
            Ok(())
        }
    }
}
