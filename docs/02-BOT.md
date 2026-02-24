# Hermes Bot — Developer Guide

## Overview

The bot is a Rust binary (`bot/`) built on [teloxide](https://github.com/teloxide/teloxide). It:
- Receives Telegram messages and callback queries
- Detects media links (YouTube, Telegram)
- Manages multi-step confirmation dialogs via inline keyboards
- Dispatches download requests to the Python worker via IPC
- Sends downloaded files back to the user

**Entry point:** `bot/src/main.rs`
**Handler logic:** `bot/src/commands.rs`

---

## AppState

Defined in `commands.rs`. Shared via `Arc<AppState>` across all handler tasks.

```rust
pub struct AppState {
    pub dispatcher:      PythonDispatcher,    // manages Python worker subprocess
    pub task_queue:      TaskQueue,           // semaphore-based concurrency limiter
    pub download_dir:    String,              // base dir for all downloads
    pub callback_store:  CallbackStateStore,  // pending format-selection dialogs
    pub search_store:    SearchStateStore,    // pending search result sessions
    pub playlist_store:  PlaylistStateStore,  // pending playlist confirmation dialogs
    pub db_pool:         Option<SqlitePool>,  // task persistence (optional)
    pub admin_chat_id:   Option<i64>,         // Telegram chat ID of admin
}
```

---

## Bot Commands

| Command | Handler | Description |
|---------|---------|-------------|
| `/start` | `cmd_start` | Welcome message with user's chat ID |
| `/help` | `cmd_help` | Feature summary and command list |
| `/download <url>` | `cmd_download` | Download a YouTube video/audio |
| `/search <query>` | `cmd_search` | Search YouTube, show inline results |
| `/chatid` | `cmd_chatid` | Print the current chat's ID |
| `/allow <N>` | `handle_allow_command` | (Admin) Open OTP-free login window for N minutes |

---

## Link Detection (`bot/src/link_detector.rs`)

`detect_links(text) -> Vec<DetectedLink>` scans any message text with regex patterns.

### DetectedLink Variants

| Variant | Match Pattern | `ipc_action()` |
|---------|---------------|----------------|
| `YoutubeVideo` | `youtube.com/watch?v=ID` or `youtu.be/ID` (11-char ID) | `"youtube_dl"` |
| `YoutubePlaylist` | `youtube.com/playlist?list=ID` | `"playlist"` |
| `YoutubeShort` | `youtube.com/shorts/ID` | `"youtube_dl"` |
| `YoutubeMusic` | `music.youtube.com/watch?v=ID` | `"youtube_dl"` |
| `TelegramFile` | `t.me/c/{id}/{msg}` or `t.me/{user}/{msg}` | `"telegram_forward"` |
| `Unsupported` | Any other `https?://` URL | `"unsupported"` |

### Detection Priority
1. `YoutubePlaylist` (most specific, must check before video)
2. `YoutubeShort`
3. `YoutubeMusic`
4. `YoutubeVideo` (skip if video_id already captured)
5. Telegram private (`t.me/c/...`) — only if no YouTube found
6. Telegram public (`t.me/username/...`) — only if no YouTube found
7. Generic URL fallback → `Unsupported`

### Telegram URL Formats
- **Public:** `https://t.me/channelname/123` → `username = "channelname"`, `message_id = 123`
- **Private:** `https://t.me/c/1234567890/456` → `channel_id = -1001234567890`, `message_id = 456`
- **Viewer prefix:** `https://t.me/s/channelname/123` — `/s/` is stripped, treated as public

---

## Message Flow (`handle_message`)

```
handle_message(msg)
  │
  ├─ detect_links(text)
  │
  ├─ first link is YoutubePlaylist?
  │   └─ cmd_playlist_confirm() → show 3-button keyboard
  │
  ├─ first link is TelegramFile?
  │   └─ cmd_telegram_forward() → copy_message() via Bot API
  │
  ├─ first link is_supported() (video, short, music)?
  │   └─ cmd_download() → dispatch to worker
  │
  └─ Unsupported → "Link type not supported" message
```

---

## Download Flow (`cmd_download`)

```rust
cmd_download(bot, msg, url, state)
  1. detect_first_link(url) → determine type
  2. Redirect: is_telegram() → cmd_telegram_forward
  3. bot.send_message("Preparing download...") → status_msg
  4. task_id = Uuid::new_v4()
  5. out_dir = task_output_dir(download_dir, chat_id, task_id)
  6. request = download_request(task_id, url, extract_audio, out_dir)
     OR get_formats_request() if format chooser needed
  7. task_queue.enqueue(task_id, chat_id, "youtube_dl")
  8. db.create_task(...)
  9. dispatcher.send(request) → rx channel
 10. tokio::spawn → execute_download_and_send(rx, ...)
```

### `execute_download_and_send`
Drives the IPC response loop:
- `IPCResponse::progress` → edit status message with `▓▓▓░░ 45%`
- `IPCResponse::done` → upload files to Telegram, update DB task to `completed`
- `IPCResponse::error` → edit message with error, update DB task to `failed`

---

## Playlist Confirmation Flow

Triggered when a plain-message link is `YoutubePlaylist`.
The `/download <playlist-url>` command skips this dialog and downloads directly.

```
Step 1 — Scope choice (pc:KEY:choice)
  [🎵 Download Playlist]  [🎬 Single Video]  [✖ Cancel]
         │                        │
         ▼                        ▼
Step 2a — Limit (pl:KEY:N)   Step 2b — Format (pf:KEY:format)
  [10 tracks] [25 tracks]    (straight to format, is_single=true)
  [50 tracks] [All tracks]
         │
         ▼
Step 3 — Format (pf:KEY:format)
  [🎵 Audio (MP3)]  [🎬 Video (MP4)]
         │
         ▼
  Fire IPCRequest → Python worker
```

### Callback Data Formats

| Prefix | Format | Meaning |
|--------|--------|---------|
| `pc:` | `pc:KEY:p/s/x` | Playlist confirm: **p**laylist / **s**ingle / cancel |
| `pl:` | `pl:KEY:N` | Limit: 0=all, 10/25/50=cap at N |
| `pf:` | `pf:KEY:a/v` | Format: **a**udio MP3 / **v**ideo MP4 |

### PlaylistPending State (`callback_state.rs`)
```rust
pub struct PlaylistPending {
    pub url:        String,
    pub chat_id:    i64,
    pub message_id: MessageId,
    pub limit:      Option<u32>,   // None = all; Some(n) = cap at n
    pub is_single:  bool,          // true = download only this video
    pub created_at: Instant,
}
```
Stored in `PlaylistStateStore` (Arc<Mutex<HashMap>>). Cleaned up every 2 min, 10 min TTL.
Key = first 8 chars of a `Uuid::new_v4()`.
`take(key)` removes and returns the pending state when the final `pf:` callback fires.

---

## Search Flow

```
/search <query>
  → get_video_info requests → show first match with thumbnail
  → OR youtube_search IPC request → returns list of results
  → InlineKeyboard: [Result 1] [Result 2] ... [Result N] [Cancel]
  → User clicks result → sf:PREFIX:INDEX:a/v callback
  → decode_search_format_callback → show format buttons
  → User picks Audio/Video → dispatch download
```

Search results stored in `SearchStateStore` with `SearchPending`/`SearchResultItem`.

---

## In-Memory State Stores

All stores in `bot/src/callback_state.rs`:

| Store | Key | Value | TTL |
|-------|-----|-------|-----|
| `CallbackStateStore` | callback prefix (6 chars) | `PendingSelection` (URL + format choices) | 10 min |
| `SearchStateStore` | search prefix (6 chars) | `SearchPending` (query + result list) | 10 min |
| `PlaylistStateStore` | key (8 chars of UUID) | `PlaylistPending` (url, limit, is_single) | 10 min |

All three use the same pattern:
```rust
Arc<Mutex<HashMap<String, T>>>
cleanup_expired(ttl_secs) // called every 2 min from tokio::spawn loop
```

---

## Task Output Directory Layout

```
{DOWNLOAD_DIR}/
└── {chat_id}/
    └── {task_id}/
        ├── track01.mp3
        ├── track02.mp3
        └── playlist.zip   (for playlists)
```

`task_output_dir()` in `commands.rs` creates this path.

---

## PythonDispatcher (`bot/src/workers/python_dispatcher.rs`)

Spawns `python -m worker.application` as a child process.

```
┌────────────────────────────────────┐
│  PythonDispatcher                  │
│  ├── stdin_tx: mpsc::Sender        │ ─── write JSON lines → worker stdin
│  ├── pending: HashMap<task_id, tx> │ ◄── route stdout responses by task_id
│  └── running: Arc<Mutex<bool>>     │
└────────────────────────────────────┘
```

- `send(request) → UnboundedReceiver<IPCResponse>` — fire and get a channel
- `send_and_wait(request, timeout) → IPCResponse` — await final done/error
- PATH is augmented at startup: checks `FFMPEG_PATH`, scans winget packages, common install dirs
- Child process is monitored every 2s; logs exit code if it crashes
- Graceful shutdown: closes stdin (EOF signal to Python), waits 5s, then kills

---

## Telegram Forward (`cmd_telegram_forward`)

For `t.me` links. Uses `bot.copy_message()` — no Python worker involvement.

```rust
// Public channel (@username)
bot.copy_message(user_chat_id,
    Recipient::ChannelUsername("@channelname".into()),
    MessageId(msg_id))
// Private channel (numeric id)
bot.copy_message(user_chat_id,
    Recipient::Id(ChatId(channel_id)),
    MessageId(msg_id))
```

- Single link: "Forwarding..." → copy → "Done" (or error)
- Batch (multiple `t.me` links): progress updates every 3 copies
- Bot must be a member of the source channel/group
