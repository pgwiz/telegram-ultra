/// Callback state management for inline keyboard interactions.
///
/// Stores pending format selections so that when a user clicks a quality
/// button, we can retrieve the URL, mode, and format options.
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use teloxide::types::MessageId;
use tracing::debug;

/// Download mode: video or audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadMode {
    Video,
    Audio,
}

impl DownloadMode {
    pub fn as_str(&self) -> &str {
        match self {
            DownloadMode::Video => "video",
            DownloadMode::Audio => "audio",
        }
    }

    pub fn callback_prefix(&self) -> &str {
        match self {
            DownloadMode::Video => "dv",
            DownloadMode::Audio => "da",
        }
    }

    pub fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "dv" => Some(DownloadMode::Video),
            "da" => Some(DownloadMode::Audio),
            _ => None,
        }
    }
}

/// A single format option available for download.
#[derive(Debug, Clone)]
pub struct FormatOption {
    pub format_id: String,
    pub label: String,
    pub extract_audio: bool,
    pub audio_format: Option<String>,
    pub audio_quality: Option<String>,
}

/// Pending selection state stored while user views the quality keyboard.
#[derive(Debug, Clone)]
pub struct PendingSelection {
    pub chat_id: i64,
    pub url: String,
    pub message_id: MessageId,
    pub formats: Vec<FormatOption>,
    pub created_at: std::time::Instant,
    pub title: String,
}

/// Thread-safe store for pending callback selections.
#[derive(Clone)]
pub struct CallbackStateStore {
    inner: Arc<Mutex<HashMap<String, PendingSelection>>>,
}

impl CallbackStateStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Store a pending selection keyed by a 6-char prefix.
    pub async fn store(&self, key: String, selection: PendingSelection) {
        debug!("Storing callback state: key={}", key);
        self.inner.lock().await.insert(key, selection);
    }

    /// Take (remove and return) a pending selection.
    pub async fn take(&self, key: &str) -> Option<PendingSelection> {
        self.inner.lock().await.remove(key)
    }

    /// Remove expired entries (older than TTL).
    pub async fn cleanup_expired(&self, ttl_secs: u64) {
        let now = std::time::Instant::now();
        let mut map = self.inner.lock().await;
        let before = map.len();
        map.retain(|_, v| now.duration_since(v.created_at).as_secs() < ttl_secs);
        let removed = before - map.len();
        if removed > 0 {
            debug!("Cleaned up {} expired callback states", removed);
        }
    }
}

/// Encode callback data for an inline button.
/// Format: "mode:prefix:index" e.g. "dv:a3f2b1:2"
pub fn encode_callback(mode: &DownloadMode, prefix: &str, index: usize) -> String {
    format!("{}:{}:{}", mode.callback_prefix(), prefix, index)
}

/// Encode cancel callback data.
pub fn encode_cancel(prefix: &str) -> String {
    format!("cx:{}", prefix)
}

/// Decode callback data. Returns (mode_prefix, key, index).
pub fn decode_callback(data: &str) -> Option<(String, String, usize)> {
    let parts: Vec<&str> = data.split(':').collect();
    if parts.len() == 3 {
        let mode_prefix = parts[0].to_string();
        let key = parts[1].to_string();
        let index: usize = parts[2].parse().ok()?;
        Some((mode_prefix, key, index))
    } else if parts.len() == 2 && parts[0] == "cx" {
        // Cancel callback - return with special index
        Some(("cx".to_string(), parts[1].to_string(), usize::MAX))
    } else {
        None
    }
}

/// Parse format options from IPC response data.
pub fn parse_format_options(formats: &[serde_json::Value]) -> Vec<FormatOption> {
    formats
        .iter()
        .filter_map(|f| {
            Some(FormatOption {
                format_id: f.get("format_id")?.as_str()?.to_string(),
                label: f.get("label")?.as_str()?.to_string(),
                extract_audio: f.get("extract_audio").and_then(|v| v.as_bool()).unwrap_or(false),
                audio_format: f.get("audio_format").and_then(|v| v.as_str()).map(String::from),
                audio_quality: f.get("audio_quality").and_then(|v| v.as_str()).map(String::from),
            })
        })
        .collect()
}

/// A single search result item for inline keyboard selection.
#[derive(Debug, Clone)]
pub struct SearchResultItem {
    pub url:   String,
    pub title: String,
}

/// Pending search results waiting for user button-tap.
#[derive(Debug, Clone)]
pub struct SearchPending {
    pub results:    Vec<SearchResultItem>,
    pub created_at: std::time::Instant,
}

/// Thread-safe store for pending search result keyboards.
/// Uses peek (not take) so every button in the menu stays clickable.
#[derive(Clone)]
pub struct SearchStateStore {
    inner: Arc<Mutex<HashMap<String, SearchPending>>>,
}

impl SearchStateStore {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub async fn store(&self, key: String, pending: SearchPending) {
        self.inner.lock().await.insert(key, pending);
    }

    /// Return a clone without removing — all buttons stay active.
    pub async fn peek(&self, key: &str) -> Option<SearchPending> {
        self.inner.lock().await.get(key).cloned()
    }

    pub async fn cleanup_expired(&self, ttl_secs: u64) {
        let now = std::time::Instant::now();
        let mut map = self.inner.lock().await;
        map.retain(|_, v| now.duration_since(v.created_at).as_secs() < ttl_secs);
    }
}

/// Encode search-result callback data.  Format: "sr:prefix:index"
pub fn encode_search_callback(prefix: &str, index: usize) -> String {
    format!("sr:{}:{}", prefix, index)
}
