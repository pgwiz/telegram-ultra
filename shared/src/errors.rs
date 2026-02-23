/// Unified error types for the Hermes system.
use thiserror::Error;

/// Top-level error type for the Hermes system.
#[derive(Debug, Error)]
pub enum HermesError {
    #[error("IPC error: {0}")]
    Ipc(#[from] IpcError),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Worker error: {0}")]
    Worker(#[from] WorkerError),

    #[error("Telegram error: {0}")]
    Telegram(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Errors related to IPC communication with the Python worker.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("Worker process not running")]
    NotRunning,

    #[error("Failed to spawn worker: {0}")]
    SpawnFailed(String),

    #[error("Failed to write to worker stdin: {0}")]
    WriteFailed(String),

    #[error("Failed to read from worker stdout: {0}")]
    ReadFailed(String),

    #[error("Worker returned invalid JSON: {0}")]
    InvalidJson(String),

    #[error("Request timed out after {0}s")]
    Timeout(u64),

    #[error("Worker exited with code {0}")]
    WorkerExited(i32),

    #[error("Worker crashed: {0}")]
    WorkerCrashed(String),
}

/// Errors returned by the Python worker in IPC responses.
#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("[{code}] {message}")]
    Remote {
        code: String,
        message: String,
        retriable: bool,
    },

    #[error("Network timeout")]
    NetworkTimeout,

    #[error("Rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("Authentication required")]
    AuthRequired,

    #[error("Video unavailable: {0}")]
    VideoUnavailable(String),

    #[error("Unknown worker error: {0}")]
    Unknown(String),
}

impl WorkerError {
    /// Create from IPC error response data.
    pub fn from_ipc_data(data: &serde_json::Value) -> Self {
        let code = data.get("error_code")
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN");
        let message = data.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        let retriable = data.get("retriable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match code {
            "NETWORK_TIMEOUT" | "SERVICE_UNAVAILABLE" => WorkerError::NetworkTimeout,
            "RATE_LIMITED" => WorkerError::RateLimited {
                retry_after_secs: data.get("retry_after")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(60),
            },
            "REQUIRE_AUTH" | "COOKIE_EXPIRED" => WorkerError::AuthRequired,
            "VIDEO_PRIVATE" | "VIDEO_DELETED" | "VIDEO_NOT_FOUND" | "GEO_RESTRICTED" => {
                WorkerError::VideoUnavailable(message.to_string())
            }
            _ => WorkerError::Remote {
                code: code.to_string(),
                message: message.to_string(),
                retriable,
            },
        }
    }

    /// Whether this error is retriable.
    pub fn is_retriable(&self) -> bool {
        matches!(self,
            WorkerError::NetworkTimeout
            | WorkerError::RateLimited { .. }
            | WorkerError::Remote { retriable: true, .. }
        )
    }
}

/// Result type alias for Hermes operations.
pub type HermesResult<T> = Result<T, HermesError>;
