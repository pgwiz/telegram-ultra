/// IPC Protocol types for Rust <-> Python worker communication.
///
/// Messages are newline-delimited JSON on stdin/stdout of the Python subprocess.
use serde::{Deserialize, Serialize};

// ====== REQUEST (Rust -> Python) ======

/// Request sent from Rust bot to Python worker via stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IPCRequest {
    pub task_id: String,
    pub action: IPCAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Supported IPC actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IPCAction {
    YoutubeDl,
    YoutubeSearch,
    GetVideoInfo,
    GetFormats,
    Playlist,
    CacheCleanup,
    CacheStats,
    HealthCheck,
}

impl std::fmt::Display for IPCAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}

/// Builder for constructing IPC requests.
impl IPCRequest {
    pub fn new(task_id: impl Into<String>, action: IPCAction) -> Self {
        Self {
            task_id: task_id.into(),
            action,
            url: None,
            params: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn with_params(mut self, params: serde_json::Value) -> Self {
        self.params = params;
        self
    }

    /// Serialize to a single JSON line (for stdin).
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

// ====== RESPONSE (Python -> Rust) ======

/// Response received from Python worker via stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IPCResponse {
    pub task_id: String,
    pub event: IPCEvent,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// Event types from the Python worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IPCEvent {
    Progress,
    Done,
    Error,
    SearchResults,
    VideoInfo,
    FormatList,
    HealthOk,
    CacheStats,
    CacheCleanupDone,
    Retry,
}

impl IPCResponse {
    /// Parse from a JSON line (from stdout).
    pub fn from_json_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }

    /// Check if this is an error event.
    pub fn is_error(&self) -> bool {
        self.event == IPCEvent::Error
    }

    /// Check if this is a completion event.
    pub fn is_done(&self) -> bool {
        self.event == IPCEvent::Done
    }

    /// Check if this is a progress event.
    pub fn is_progress(&self) -> bool {
        self.event == IPCEvent::Progress
    }

    /// Check if this is a format list event.
    pub fn is_format_list(&self) -> bool {
        self.event == IPCEvent::FormatList
    }

    /// Extract error message if this is an error event.
    pub fn error_message(&self) -> Option<String> {
        if self.is_error() {
            self.data.get("message").and_then(|v| v.as_str()).map(String::from)
        } else {
            None
        }
    }

    /// Extract error code if this is an error event.
    pub fn error_code(&self) -> Option<String> {
        if self.is_error() {
            self.data.get("error_code").and_then(|v| v.as_str()).map(String::from)
        } else {
            None
        }
    }

    /// Extract progress percentage.
    pub fn progress_percent(&self) -> Option<u8> {
        self.data.get("percent").and_then(|v| v.as_u64()).map(|v| v.min(100) as u8)
    }

    /// Extract download speed string.
    pub fn progress_speed(&self) -> Option<String> {
        self.data.get("speed").and_then(|v| v.as_str()).map(String::from)
    }
}

// ====== CONVENIENCE BUILDERS ======

/// Build a YouTube search request.
pub fn search_request(task_id: &str, query: &str, limit: u32) -> IPCRequest {
    IPCRequest::new(task_id, IPCAction::YoutubeSearch)
        .with_params(serde_json::json!({
            "query": query,
            "limit": limit,
        }))
}

/// Build a YouTube download request.
pub fn download_request(
    task_id: &str,
    url: &str,
    extract_audio: bool,
    output_dir: &str,
) -> IPCRequest {
    IPCRequest::new(task_id, IPCAction::YoutubeDl)
        .with_url(url)
        .with_params(serde_json::json!({
            "extract_audio": extract_audio,
            "audio_format": "mp3",
            "audio_quality": "192",
            "output_dir": output_dir,
        }))
}

/// Build a playlist download request.
pub fn playlist_request(task_id: &str, url: &str, output_dir: &str) -> IPCRequest {
    IPCRequest::new(task_id, IPCAction::Playlist)
        .with_url(url)
        .with_params(serde_json::json!({
            "extract_audio": true,
            "audio_format": "mp3",
            "output_dir": output_dir,
            "archive_max_size_mb": 100,
        }))
}

/// Build a health check request.
pub fn health_check_request(task_id: &str) -> IPCRequest {
    IPCRequest::new(task_id, IPCAction::HealthCheck)
}

/// Build a video info request.
pub fn video_info_request(task_id: &str, url: &str) -> IPCRequest {
    IPCRequest::new(task_id, IPCAction::GetVideoInfo)
        .with_url(url)
}

/// Build a get_formats request (for quality selection menus).
pub fn get_formats_request(task_id: &str, url: &str, mode: &str) -> IPCRequest {
    IPCRequest::new(task_id, IPCAction::GetFormats)
        .with_url(url)
        .with_params(serde_json::json!({
            "mode": mode,
        }))
}

/// Build a download request with a specific format selection.
pub fn download_request_with_format(
    task_id: &str,
    url: &str,
    format_id: &str,
    extract_audio: bool,
    audio_format: Option<&str>,
    audio_quality: Option<&str>,
    output_dir: &str,
) -> IPCRequest {
    let mut params = serde_json::json!({
        "format": format_id,
        "extract_audio": extract_audio,
        "output_dir": output_dir,
    });
    if let Some(af) = audio_format {
        params["audio_format"] = serde_json::json!(af);
    }
    if let Some(aq) = audio_quality {
        params["audio_quality"] = serde_json::json!(aq);
    }
    IPCRequest::new(task_id, IPCAction::YoutubeDl)
        .with_url(url)
        .with_params(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = search_request("task-1", "lo-fi beats", 5);
        let json = req.to_json_line().unwrap();
        assert!(json.contains("youtube_search"));
        assert!(json.contains("lo-fi beats"));
    }

    #[test]
    fn test_response_deserialization() {
        let json = r#"{"task_id":"t1","event":"progress","data":{"percent":42,"speed":"1.2MB/s"}}"#;
        let resp = IPCResponse::from_json_line(json).unwrap();
        assert_eq!(resp.task_id, "t1");
        assert_eq!(resp.event, IPCEvent::Progress);
        assert_eq!(resp.progress_percent(), Some(42));
    }

    #[test]
    fn test_error_response() {
        let json = r#"{"task_id":"t2","event":"error","data":{"message":"Video private","error_code":"VIDEO_PRIVATE"}}"#;
        let resp = IPCResponse::from_json_line(json).unwrap();
        assert!(resp.is_error());
        assert_eq!(resp.error_message(), Some("Video private".to_string()));
        assert_eq!(resp.error_code(), Some("VIDEO_PRIVATE".to_string()));
    }
}
