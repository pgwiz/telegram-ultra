/// Database models shared across all Hermes crates.
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Telegram user who contacted the bot.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub chat_id: i64,
    pub username: Option<String>,
    pub first_seen: NaiveDateTime,
    pub is_admin: bool,
    pub last_activity: NaiveDateTime,
}

/// Download task status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Done,
    Error,
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Queued => write!(f, "queued"),
            TaskStatus::Running => write!(f, "running"),
            TaskStatus::Done => write!(f, "done"),
            TaskStatus::Error => write!(f, "error"),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Task type classification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Youtube,
    Playlist,
    Direct,
    TgFile,
    Search,
}

impl std::fmt::Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskType::Youtube => write!(f, "youtube"),
            TaskType::Playlist => write!(f, "playlist"),
            TaskType::Direct => write!(f, "direct"),
            TaskType::TgFile => write!(f, "tg_file"),
            TaskType::Search => write!(f, "search"),
        }
    }
}

/// Download task record.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Task {
    pub id: String,
    pub chat_id: i64,
    pub task_type: String,
    pub url: String,
    pub label: Option<String>,
    pub status: String,
    pub progress: i32,
    pub file_path: Option<String>,
    pub file_url: Option<String>,
    pub scheduled_at: Option<NaiveDateTime>,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub error_msg: Option<String>,
}

/// Media task record (enhanced).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct MediaTask {
    pub task_id: String,
    pub user_chat_id: i64,
    pub task_type: String,
    pub url: String,
    pub status: String,
    pub progress_percent: i32,
    pub current_speed: Option<String>,
    pub eta_seconds: Option<i32>,
    pub result_file_path: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub retry_count: i32,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

/// Session for dashboard authentication.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Session {
    pub token: String,
    pub chat_id: i64,
    pub expires_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
}

/// Progress update to send to Telegram user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressUpdate {
    pub task_id: String,
    pub chat_id: i64,
    pub percent: u8,
    pub speed: String,
    pub status: String,
    pub eta_seconds: u32,
}

/// Search result from Python worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    #[serde(rename = "videoId")]
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub duration: String,
    pub thumbnail: String,
    pub url: String,
}

/// Download completion result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub task_id: String,
    pub file_path: String,
    pub file_size: u64,
    pub filename: String,
}

/// Playlist completion result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistResult {
    pub task_id: String,
    pub playlist_name: String,
    pub total_tracks_downloaded: u32,
    pub archives: Vec<ArchiveInfo>,
    pub folder_path: String,
}

/// Archive info within a playlist result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveInfo {
    pub name: String,
    pub size_mb: f64,
    pub path: String,
}
