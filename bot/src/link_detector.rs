/// Smart link detection for incoming Telegram messages.
///
/// Detects YouTube URLs, Telegram links, and other URL patterns.
use regex::Regex;
use once_cell::sync::Lazy;

/// Detected link type from a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedLink {
    /// Single YouTube video.
    YoutubeVideo { url: String, video_id: String },
    /// YouTube playlist.
    YoutubePlaylist { url: String, playlist_id: String },
    /// YouTube short.
    YoutubeShort { url: String, video_id: String },
    /// YouTube Music link.
    YoutubeMusic { url: String, video_id: String },
    /// Telegram channel/group file link.
    TelegramFile {
        url: String,
        /// Channel username (for public links like t.me/channelname/123).
        username: Option<String>,
        /// Full chat ID (for private links like t.me/c/1234567890/123 → -1001234567890).
        channel_id: Option<i64>,
        /// Message ID within the channel.
        message_id: i32,
    },
    /// Unsupported URL (not YouTube or Telegram).
    Unsupported { url: String },
}

impl DetectedLink {
    /// Get the URL regardless of type.
    pub fn url(&self) -> &str {
        match self {
            DetectedLink::YoutubeVideo { url, .. } => url,
            DetectedLink::YoutubePlaylist { url, .. } => url,
            DetectedLink::YoutubeShort { url, .. } => url,
            DetectedLink::YoutubeMusic { url, .. } => url,
            DetectedLink::TelegramFile { url, .. } => url,
            DetectedLink::Unsupported { url } => url,
        }
    }

    /// Whether this is a playlist.
    pub fn is_playlist(&self) -> bool {
        matches!(self, DetectedLink::YoutubePlaylist { .. })
    }

    /// Whether this is a supported (downloadable) link.
    pub fn is_supported(&self) -> bool {
        !matches!(self, DetectedLink::Unsupported { .. })
    }

    /// Whether this is a Telegram link.
    pub fn is_telegram(&self) -> bool {
        matches!(self, DetectedLink::TelegramFile { .. })
    }

    /// Get the IPC action name for this link type.
    pub fn ipc_action(&self) -> &str {
        match self {
            DetectedLink::YoutubePlaylist { .. } => "playlist",
            DetectedLink::YoutubeVideo { .. }
            | DetectedLink::YoutubeShort { .. }
            | DetectedLink::YoutubeMusic { .. } => "youtube_dl",
            DetectedLink::TelegramFile { .. } => "telegram_forward",
            DetectedLink::Unsupported { .. } => "unsupported",
        }
    }
}

// ====== REGEX PATTERNS ======

static YOUTUBE_VIDEO_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:https?://)?(?:www\.)?(?:youtube\.com/watch\?v=|youtu\.be/)([a-zA-Z0-9_-]{11})"
    ).unwrap()
});

static YOUTUBE_PLAYLIST_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:https?://)?(?:www\.)?youtube\.com/playlist\?list=([a-zA-Z0-9_-]+)"
    ).unwrap()
});

static YOUTUBE_SHORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:https?://)?(?:www\.)?youtube\.com/shorts/([a-zA-Z0-9_-]{11})"
    ).unwrap()
});

static YOUTUBE_MUSIC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:https?://)?music\.youtube\.com/watch\?v=([a-zA-Z0-9_-]{11})"
    ).unwrap()
});

/// Generic URL pattern to catch any http/https link.
static GENERIC_URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"https?://[^\s<>\[\](){},"']+"#
    ).unwrap()
});

/// Telegram private channel link: t.me/c/{channel_id}/{message_id}
static TELEGRAM_PRIVATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"https?://t\.me/c/(\d+)/(\d+)"
    ).unwrap()
});

/// Telegram public channel link: t.me/{username}/{message_id} (optional /s/ prefix)
static TELEGRAM_PUBLIC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"https?://t\.me/(?:s/)?([a-zA-Z_]\w{4,31})/(\d+)"
    ).unwrap()
});

/// Detect all supported links in a message.
pub fn detect_links(text: &str) -> Vec<DetectedLink> {
    let mut links = Vec::new();

    // Check playlist first (more specific)
    for cap in YOUTUBE_PLAYLIST_RE.captures_iter(text) {
        links.push(DetectedLink::YoutubePlaylist {
            url: cap[0].to_string(),
            playlist_id: cap[1].to_string(),
        });
    }

    // YouTube Shorts
    for cap in YOUTUBE_SHORT_RE.captures_iter(text) {
        links.push(DetectedLink::YoutubeShort {
            url: cap[0].to_string(),
            video_id: cap[1].to_string(),
        });
    }

    // YouTube Music
    for cap in YOUTUBE_MUSIC_RE.captures_iter(text) {
        links.push(DetectedLink::YoutubeMusic {
            url: cap[0].to_string(),
            video_id: cap[1].to_string(),
        });
    }

    // Regular YouTube video (skip if already captured as playlist/short/music)
    for cap in YOUTUBE_VIDEO_RE.captures_iter(text) {
        let url = cap[0].to_string();
        let video_id = cap[1].to_string();

        // Skip if this URL was already captured
        let already = links.iter().any(|l| l.url().contains(&video_id));
        if !already {
            links.push(DetectedLink::YoutubeVideo { url, video_id });
        }
    }

    // If no YouTube links found, check for Telegram links
    if links.is_empty() {
        // Private channel links first (more specific: t.me/c/{id}/{msg})
        for cap in TELEGRAM_PRIVATE_RE.captures_iter(text) {
            let raw_id = &cap[1];
            let chat_id: i64 = format!("-100{}", raw_id).parse().unwrap_or(0);
            let message_id: i32 = cap[2].parse().unwrap_or(0);
            if chat_id != 0 && message_id > 0 {
                links.push(DetectedLink::TelegramFile {
                    url: cap[0].to_string(),
                    username: None,
                    channel_id: Some(chat_id),
                    message_id,
                });
            }
        }

        // Public channel links (t.me/{username}/{msg})
        for cap in TELEGRAM_PUBLIC_RE.captures_iter(text) {
            let url = cap[0].to_string();
            let username = cap[1].to_string();
            let message_id: i32 = cap[2].parse().unwrap_or(0);

            // Skip if already captured by private regex
            let already = links.iter().any(|l| l.url() == url);
            if !already && message_id > 0 {
                links.push(DetectedLink::TelegramFile {
                    url,
                    username: Some(username),
                    channel_id: None,
                    message_id,
                });
            }
        }
    }

    // If no YouTube or Telegram links found, check for any generic URL
    if links.is_empty() {
        if let Some(m) = GENERIC_URL_RE.find(text) {
            links.push(DetectedLink::Unsupported {
                url: m.as_str().to_string(),
            });
        }
    }

    links
}

/// Detect the first link in a message (most common case).
pub fn detect_first_link(text: &str) -> Option<DetectedLink> {
    detect_links(text).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_youtube_video() {
        let links = detect_links("Check this out: https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], DetectedLink::YoutubeVideo { video_id, .. } if video_id == "dQw4w9WgXcQ"));
    }

    #[test]
    fn test_youtu_be_short_url() {
        let links = detect_links("https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], DetectedLink::YoutubeVideo { .. }));
    }

    #[test]
    fn test_playlist() {
        let links = detect_links("https://www.youtube.com/playlist?list=PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf");
        assert_eq!(links.len(), 1);
        assert!(links[0].is_playlist());
    }

    #[test]
    fn test_youtube_short() {
        let links = detect_links("https://www.youtube.com/shorts/abc123def45");
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], DetectedLink::YoutubeShort { .. }));
    }

    #[test]
    fn test_youtube_music() {
        let links = detect_links("https://music.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], DetectedLink::YoutubeMusic { .. }));
    }

    #[test]
    fn test_no_links() {
        let links = detect_links("Just a regular message with no links");
        assert!(links.is_empty());
    }

    #[test]
    fn test_multiple_links() {
        let text = "Download https://youtu.be/abc12345678 and https://www.youtube.com/watch?v=xyz98765432";
        let links = detect_links(text);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn test_ipc_action() {
        let video = DetectedLink::YoutubeVideo { url: "test".into(), video_id: "id".into() };
        assert_eq!(video.ipc_action(), "youtube_dl");

        let playlist = DetectedLink::YoutubePlaylist { url: "test".into(), playlist_id: "id".into() };
        assert_eq!(playlist.ipc_action(), "playlist");

        let tg = DetectedLink::TelegramFile {
            url: "test".into(), username: Some("ch".into()), channel_id: None, message_id: 1,
        };
        assert_eq!(tg.ipc_action(), "telegram_forward");
    }

    #[test]
    fn test_telegram_public_link() {
        let links = detect_links("Check this https://t.me/somechannel/123");
        assert_eq!(links.len(), 1);
        assert!(links[0].is_supported());
        assert!(links[0].is_telegram());
        if let DetectedLink::TelegramFile { username, message_id, .. } = &links[0] {
            assert_eq!(username.as_deref(), Some("somechannel"));
            assert_eq!(*message_id, 123);
        } else {
            panic!("Expected TelegramFile");
        }
    }

    #[test]
    fn test_telegram_private_link() {
        let links = detect_links("https://t.me/c/1234567890/456");
        assert_eq!(links.len(), 1);
        assert!(links[0].is_telegram());
        if let DetectedLink::TelegramFile { channel_id, message_id, .. } = &links[0] {
            assert_eq!(*channel_id, Some(-1001234567890));
            assert_eq!(*message_id, 456);
        } else {
            panic!("Expected TelegramFile");
        }
    }

    #[test]
    fn test_telegram_s_prefix() {
        let links = detect_links("https://t.me/s/mychannel/789");
        assert_eq!(links.len(), 1);
        assert!(links[0].is_telegram());
        if let DetectedLink::TelegramFile { username, .. } = &links[0] {
            assert_eq!(username.as_deref(), Some("mychannel"));
        } else {
            panic!("Expected TelegramFile");
        }
    }

    #[test]
    fn test_telegram_batch_links() {
        let text = "Download these:\nhttps://t.me/somechannel/100\nhttps://t.me/somechannel/101\nhttps://t.me/somechannel/102";
        let links = detect_links(text);
        assert_eq!(links.len(), 3);
        assert!(links.iter().all(|l| l.is_telegram()));
    }

    #[test]
    fn test_generic_url_unsupported() {
        let links = detect_links("Download from https://example.com/file.mp4");
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], DetectedLink::Unsupported { .. }));
    }

    #[test]
    fn test_youtube_takes_priority_over_generic() {
        let links = detect_links("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(links.len(), 1);
        assert!(links[0].is_supported());
    }
}
