// fix_smart_playlist.js -- smart playlist confirmation flow
// Patches 4 files atomically via Node.js to avoid linter/CRLF corruption.
'use strict';
const fs = require('fs');

function toCRLF(t) { return t.replace(/\r\n/g, '\n').replace(/\r/g, '\n').replace(/\n/g, '\r\n'); }
// Normalize to LF on read so all search strings can use \n; toCRLF restores on write
function readFile(p)     { return fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n').replace(/\r/g, '\n'); }
function writeFile(p, c) { fs.writeFileSync(p, toCRLF(c), 'utf8'); }

function replaceOnce(content, find, repl, lbl) {
  const idx = content.indexOf(find);
  if (idx === -1) throw new Error('[' + lbl + '] NOT FOUND:\n' + find.slice(0, 120));
  const cnt = content.split(find).length - 1;
  if (cnt > 1) console.warn('  WARN [' + lbl + '] found ' + cnt + ' times, replacing first');
  return content.slice(0, idx) + repl + content.slice(idx + find.length);
}
function insertBefore(c, find, ins, lbl) { return replaceOnce(c, find, ins + find, lbl); }

let errors = 0;
function applyChange(label, filePath, transform) {
  try {
    const orig = readFile(filePath);
    const mod  = transform(orig);
    if (mod === orig) { console.log('  SKIP  ' + label + ' (no-op)'); }
    else              { writeFile(filePath, mod); console.log('  OK    ' + label); }
  } catch(e) { console.error('  ERROR ' + label + ': ' + e.message); errors++; }
}

const IPC_FILE  = 'e:/Backup/pgwiz/bots/telegram-ultra/shared/src/ipc_protocol.rs';
const CB_FILE   = 'e:/Backup/pgwiz/bots/telegram-ultra/bot/src/callback_state.rs';
const CMD_FILE  = 'e:/Backup/pgwiz/bots/telegram-ultra/bot/src/commands.rs';
const MAIN_FILE = 'e:/Backup/pgwiz/bots/telegram-ultra/bot/src/main.rs';

// ============================================================
// PATCH 1: ipc_protocol.rs — add playlist_request_opts()
// ============================================================
applyChange('ipc_protocol: add playlist_request_opts', IPC_FILE, c => {
  if (c.includes('pub fn playlist_request_opts(')) return c;
  return insertBefore(c,
    '/// Build a health check request.',
    `/// Build a playlist download request with user-chosen options.
/// \`max_items = None\` means all tracks; \`extract_audio = false\` means video.
pub fn playlist_request_opts(
    task_id: &str,
    url: &str,
    output_dir: &str,
    max_items: Option<u32>,
    extract_audio: bool,
) -> IPCRequest {
    let mut params = serde_json::json!({
        "extract_audio": extract_audio,
        "audio_format": if extract_audio { "mp3" } else { "mp4" },
        "output_dir": output_dir,
        "archive_max_size_mb": 100,
    });
    if let Some(n) = max_items {
        params["playlist_end"] = serde_json::json!(n);
    }
    IPCRequest::new(task_id, IPCAction::Playlist)
        .with_url(url)
        .with_params(params)
}

`,
    'ipc: insert playlist_request_opts'
  );
});

// ============================================================
// PATCH 2: callback_state.rs — add PlaylistPending, PlaylistStateStore, encode helpers
// ============================================================
applyChange('callback_state: add playlist types', CB_FILE, c => {
  if (c.includes('pub struct PlaylistPending {')) return c;
  return replaceOnce(c,
    '/// Encode search-format callback data.  Format: "sf:prefix:index:a" (audio) or ":v" (video)\npub fn encode_search_format_callback(prefix: &str, index: usize, is_audio: bool) -> String {\n    format!("sf:{}:{}:{}", prefix, index, if is_audio { "a" } else { "v" })\n}',
    `/// Encode search-format callback data.  Format: "sf:prefix:index:a" (audio) or ":v" (video)
pub fn encode_search_format_callback(prefix: &str, index: usize, is_audio: bool) -> String {
    format!("sf:{}:{}:{}", prefix, index, if is_audio { "a" } else { "v" })
}

/// Pending playlist download — awaiting user choice of scope, limit, and format.
#[derive(Debug, Clone)]
pub struct PlaylistPending {
    pub url:        String,
    pub chat_id:    i64,
    pub message_id: MessageId,
    pub limit:      Option<u32>,   // None = all tracks; Some(n) = cap at n
    pub is_single:  bool,          // true = download only this video, not the playlist
    pub created_at: std::time::Instant,
}

/// Thread-safe store for pending playlist confirmation dialogs.
#[derive(Clone)]
pub struct PlaylistStateStore {
    inner: Arc<Mutex<HashMap<String, PlaylistPending>>>,
}

impl PlaylistStateStore {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub async fn store(&self, key: String, pending: PlaylistPending) {
        self.inner.lock().await.insert(key, pending);
    }

    pub async fn get(&self, key: &str) -> Option<PlaylistPending> {
        self.inner.lock().await.get(key).cloned()
    }

    pub async fn set_single(&self, key: &str, is_single: bool) {
        if let Some(p) = self.inner.lock().await.get_mut(key) {
            p.is_single = is_single;
        }
    }

    pub async fn set_limit(&self, key: &str, limit: Option<u32>) {
        if let Some(p) = self.inner.lock().await.get_mut(key) {
            p.limit = limit;
        }
    }

    pub async fn take(&self, key: &str) -> Option<PlaylistPending> {
        self.inner.lock().await.remove(key)
    }

    pub async fn cleanup_expired(&self, ttl_secs: u64) {
        let now = std::time::Instant::now();
        let mut map = self.inner.lock().await;
        map.retain(|_, v| now.duration_since(v.created_at).as_secs() < ttl_secs);
    }
}

/// Encode playlist-confirm callback. choice: 'p'=full playlist, 's'=single video, 'x'=cancel
pub fn encode_playlist_confirm(key: &str, choice: char) -> String {
    format!("pc:{}:{}", key, choice)
}

/// Encode playlist-limit callback. limit: 0=all tracks, or specific count (10/25/50)
pub fn encode_playlist_limit(key: &str, limit: u32) -> String {
    format!("pl:{}:{}", key, limit)
}

/// Encode playlist-format callback. is_audio: true=Audio (MP3), false=Video (MP4)
pub fn encode_playlist_format(key: &str, is_audio: bool) -> String {
    format!("pf:{}:{}", key, if is_audio { "a" } else { "v" })
}`,
    'cb: replace tail with playlist additions'
  );
});

// ============================================================
// PATCH 3a: commands.rs — update callback_state imports
// ============================================================
applyChange('commands: update imports', CMD_FILE, c => {
  // Skip if already patched
  if (c.includes('PlaylistStateStore, PlaylistPending,')) return c;
  return replaceOnce(c,
    `use crate::callback_state::{\n    CallbackStateStore, SearchStateStore, SearchPending, SearchResultItem,\n    DownloadMode, FormatOption, PendingSelection,\n    decode_callback, encode_callback, encode_cancel, parse_format_options,\n    encode_search_callback, encode_search_format_callback,\n};`,
    `use crate::callback_state::{
    CallbackStateStore, SearchStateStore, SearchPending, SearchResultItem,
    PlaylistStateStore, PlaylistPending,
    DownloadMode, FormatOption, PendingSelection,
    decode_callback, encode_callback, encode_cancel, parse_format_options,
    encode_search_callback, encode_search_format_callback,
    encode_playlist_confirm, encode_playlist_limit, encode_playlist_format,
};`,
    'cmd: imports'
  );
});

// ============================================================
// PATCH 3b: commands.rs — add playlist_store field to AppState
// ============================================================
applyChange('commands: AppState playlist_store field', CMD_FILE, c => {
  if (c.includes('pub playlist_store: PlaylistStateStore,')) return c;
  return replaceOnce(c,
    `pub struct AppState {
    pub dispatcher: PythonDispatcher,
    pub task_queue: TaskQueue,
    pub download_dir: String,
    pub callback_store: CallbackStateStore,
    pub search_store: SearchStateStore,
    pub db_pool: Option<SqlitePool>,
    pub admin_chat_id: Option<i64>,
}`,
    `pub struct AppState {
    pub dispatcher: PythonDispatcher,
    pub task_queue: TaskQueue,
    pub download_dir: String,
    pub callback_store: CallbackStateStore,
    pub search_store: SearchStateStore,
    pub playlist_store: PlaylistStateStore,
    pub db_pool: Option<SqlitePool>,
    pub admin_chat_id: Option<i64>,
}`,
    'cmd: AppState field'
  );
});

// ============================================================
// PATCH 3c: commands.rs — handle_message intercept playlists
// ============================================================
applyChange('commands: handle_message intercept playlist', CMD_FILE, c => {
  if (c.includes('cmd_playlist_confirm(bot, msg, first.url()')) return c;
  return replaceOnce(c,
    `            } else if first.is_supported() {
                // YouTube links: download first one
                info!("Auto-detected link: {:?}", first);
                cmd_download(bot, msg, first.url().to_string(), state).await?;`,
    `            } else if first.is_supported() {
                info!("Auto-detected link: {:?}", first);
                if first.is_playlist() {
                    cmd_playlist_confirm(bot, msg, first.url().to_string(), state).await?;
                } else {
                    cmd_download(bot, msg, first.url().to_string(), state).await?;
                }`,
    'cmd: handle_message playlist intercept'
  );
});

// ============================================================
// PATCH 3d: commands.rs — insert cmd_playlist_confirm + helpers before handle_message
// ============================================================
const PLAYLIST_HELPERS = `/// Show playlist confirmation dialog — prompts user for playlist vs single video.
async fn cmd_playlist_confirm(
    bot: Bot,
    msg: Message,
    url: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id;
    let task_id = Uuid::new_v4().to_string();
    let key     = task_id[..8].to_string();

    let display_url = if url.len() > 60 {
        format!("{}\\u{2026}", &url[..59])
    } else {
        url.clone()
    };

    let buttons = vec![
        vec![
            InlineKeyboardButton::callback("🎵 Download Playlist", encode_playlist_confirm(&key, 'p')),
            InlineKeyboardButton::callback("🎬 Single Video",      encode_playlist_confirm(&key, 's')),
        ],
        vec![
            InlineKeyboardButton::callback("✖ Cancel", encode_playlist_confirm(&key, 'x')),
        ],
    ];

    let sent = bot.send_message(chat_id, format!(
        "Playlist detected!\\n{}\\n\\nDownload the full playlist or just this video?",
        display_url
    ))
    .reply_markup(InlineKeyboardMarkup::new(buttons))
    .await?;

    let pending = PlaylistPending {
        url,
        chat_id:    chat_id.0,
        message_id: sent.id,
        limit:      None,
        is_single:  false,
        created_at: std::time::Instant::now(),
    };
    state.playlist_store.store(key, pending).await;
    Ok(())
}

/// Strip list= and related params from a YouTube URL to return a single-video URL.
fn extract_single_video_url(url: &str) -> String {
    // Handle https://www.youtube.com/watch?v=VIDEO_ID&list=PL...
    if let Some(v_pos) = url.find("v=") {
        let after = &url[v_pos + 2..];
        let id_end = after.find('&').unwrap_or(after.len());
        let video_id = &after[..id_end];
        if video_id.len() == 11 {
            return format!("https://www.youtube.com/watch?v={}", video_id);
        }
    }
    // Handle https://youtu.be/VIDEO_ID?list=...  — strip query string
    if url.contains("youtu.be/") {
        if let Some(q_pos) = url.find('?') {
            return url[..q_pos].to_string();
        }
    }
    url.to_string()
}

`;

applyChange('commands: insert playlist helpers before handle_message', CMD_FILE, c => {
  if (c.includes('async fn cmd_playlist_confirm(')) return c;
  return insertBefore(c,
    '/// Handle plain messages (auto-detect links).',
    PLAYLIST_HELPERS,
    'cmd: playlist helpers'
  );
});

// ============================================================
// PATCH 3e: commands.rs — insert pc:/pl:/pf: callback handlers before decode_callback
// The anchor is the line immediately after the sf: handler's return
// ============================================================
const PLAYLIST_CALLBACKS = `    // Handle playlist confirm (pc:KEY:[p/s/x]) — before decode_callback
    if data.starts_with("pc:") {
        let _ = bot.answer_callback_query(&q.id).await;
        let parts: Vec<&str> = data.splitn(3, ':').collect();
        let pc_key    = parts.get(1).copied().unwrap_or("");
        let pc_choice = parts.get(2).copied().unwrap_or("x");

        let pending = match state.playlist_store.get(pc_key).await {
            Some(p) => p,
            None    => return Ok(()),
        };
        let chat_id = ChatId(pending.chat_id);
        let msg_id  = pending.message_id;

        if pc_choice == "x" {
            state.playlist_store.take(pc_key).await;
            let _ = bot.edit_message_text(chat_id, msg_id, "Cancelled.").await;
            return Ok(());
        }
        if pc_choice == "s" {
            state.playlist_store.set_single(pc_key, true).await;
            let buttons = vec![vec![
                InlineKeyboardButton::callback("🎵 Audio (MP3)", encode_playlist_format(pc_key, true)),
                InlineKeyboardButton::callback("🎬 Video (MP4)", encode_playlist_format(pc_key, false)),
            ]];
            let _ = bot.edit_message_text(chat_id, msg_id, "Choose format for this video:")
                .reply_markup(InlineKeyboardMarkup::new(buttons))
                .await;
            return Ok(());
        }
        // pc_choice == "p" — show limit selection
        state.playlist_store.set_single(pc_key, false).await;
        let buttons = vec![
            vec![
                InlineKeyboardButton::callback("10 tracks",  encode_playlist_limit(pc_key, 10)),
                InlineKeyboardButton::callback("25 tracks",  encode_playlist_limit(pc_key, 25)),
            ],
            vec![
                InlineKeyboardButton::callback("50 tracks",  encode_playlist_limit(pc_key, 50)),
                InlineKeyboardButton::callback("All tracks", encode_playlist_limit(pc_key, 0)),
            ],
        ];
        let _ = bot.edit_message_text(chat_id, msg_id, "How many tracks to download?")
            .reply_markup(InlineKeyboardMarkup::new(buttons))
            .await;
        return Ok(());
    }

    // Handle playlist limit (pl:KEY:N) — before decode_callback
    if data.starts_with("pl:") {
        let _ = bot.answer_callback_query(&q.id).await;
        let parts: Vec<&str> = data.splitn(3, ':').collect();
        let pl_key    = parts.get(1).copied().unwrap_or("");
        let pl_limit: u32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

        let limit_opt = if pl_limit == 0 { None } else { Some(pl_limit) };
        state.playlist_store.set_limit(pl_key, limit_opt).await;

        let pending = match state.playlist_store.get(pl_key).await {
            Some(p) => p,
            None    => return Ok(()),
        };
        let chat_id = ChatId(pending.chat_id);
        let msg_id  = pending.message_id;
        let limit_label = if pl_limit == 0 {
            "all tracks".to_string()
        } else {
            format!("up to {} tracks", pl_limit)
        };

        let buttons = vec![vec![
            InlineKeyboardButton::callback("🎵 Audio (MP3)", encode_playlist_format(pl_key, true)),
            InlineKeyboardButton::callback("🎬 Video (MP4)", encode_playlist_format(pl_key, false)),
        ]];
        let _ = bot.edit_message_text(chat_id, msg_id,
            format!("Downloading {} — choose format:", limit_label)
        )
        .reply_markup(InlineKeyboardMarkup::new(buttons))
        .await;
        return Ok(());
    }

    // Handle playlist format (pf:KEY:[a/v]) — before decode_callback
    if data.starts_with("pf:") {
        let _ = bot.answer_callback_query(&q.id).await;
        let parts: Vec<&str> = data.splitn(3, ':').collect();
        let pf_key      = parts.get(1).copied().unwrap_or("");
        let pf_is_audio = parts.get(2).copied().unwrap_or("a") == "a";

        let pending = match state.playlist_store.take(pf_key).await {
            Some(p) => p,
            None    => return Ok(()),
        };

        let chat_id    = ChatId(pending.chat_id);
        let msg_id     = pending.message_id;
        let task_id    = Uuid::new_v4().to_string();
        let short_id   = task_id[..8].to_string();
        let out_dir    = task_output_dir(&state.download_dir, pending.chat_id, &task_id);
        let mode_label = if pf_is_audio { "audio" } else { "video" };
        let is_single  = pending.is_single;

        let (url, ipc_action, request) = if is_single {
            let single_url = extract_single_video_url(&pending.url);
            let req = download_request(&task_id, &single_url, pf_is_audio, &out_dir);
            (single_url, "youtube_dl", req)
        } else {
            let req = playlist_request_opts(
                &task_id, &pending.url, &out_dir, pending.limit, pf_is_audio,
            );
            (pending.url.clone(), "playlist", req)
        };

        state.task_queue.enqueue(&task_id, pending.chat_id, ipc_action).await;

        if let Some(pool) = &state.db_pool {
            let db_kind = if is_single { "youtube_dl" } else { "playlist" };
            let _ = hermes_shared::db::create_task(
                pool, &task_id, pending.chat_id, db_kind, &url, Some(mode_label),
            ).await;
        }

        let dl_mode    = if pf_is_audio { DownloadMode::Audio } else { DownloadMode::Video };
        let kind_label = if is_single { "video" } else { "playlist" };

        let _ = bot.edit_message_text(chat_id, msg_id,
            format!("Queued {} [{}]", kind_label, short_id)
        ).reply_markup(InlineKeyboardMarkup::new(vec![])).await;

        let state2 = state.clone();
        tokio::spawn(async move {
            let _ = execute_download_and_send(
                &bot, chat_id, msg_id, &short_id,
                kind_label, &task_id, &request, dl_mode, &state2,
            ).await;
        });
        return Ok(());
    }

`;

applyChange('commands: insert pc/pl/pf callback handlers', CMD_FILE, c => {
  if (c.includes('data.starts_with("pc:")')) return c;
  return insertBefore(c,
    '    let (mode_prefix, key, index) = match decode_callback(&data) {',
    PLAYLIST_CALLBACKS,
    'cmd: playlist callbacks'
  );
});

// ============================================================
// PATCH 4a: main.rs — add PlaylistStateStore to imports
// ============================================================
applyChange('main: import PlaylistStateStore', MAIN_FILE, c => {
  if (c.includes('PlaylistStateStore')) return c;
  return replaceOnce(c,
    'use callback_state::{CallbackStateStore, SearchStateStore};',
    'use callback_state::{CallbackStateStore, SearchStateStore, PlaylistStateStore};',
    'main: imports'
  );
});

// ============================================================
// PATCH 4b: main.rs — initialize PlaylistStateStore after SearchStateStore
// ============================================================
applyChange('main: init playlist_store', MAIN_FILE, c => {
  if (c.includes('let playlist_store = PlaylistStateStore::new();')) return c;
  return replaceOnce(c,
    '    // Initialize search result store\n    let search_store = SearchStateStore::new();',
    `    // Initialize search result store
    let search_store = SearchStateStore::new();

    // Initialize playlist confirmation store
    let playlist_store = PlaylistStateStore::new();`,
    'main: init playlist_store'
  );
});

// ============================================================
// PATCH 4c: main.rs — add playlist_store to AppState construction
// ============================================================
applyChange('main: AppState playlist_store field', MAIN_FILE, c => {
  if (c.includes('playlist_store: playlist_store.clone(),')) return c;
  return replaceOnce(c,
    `        search_store: search_store.clone(),
        db_pool: db_pool.clone(),`,
    `        search_store: search_store.clone(),
        playlist_store: playlist_store.clone(),
        db_pool: db_pool.clone(),`,
    'main: AppState construction'
  );
});

// ============================================================
// PATCH 4d: main.rs — add cleanup task for playlist_store
// ============================================================
applyChange('main: playlist_store cleanup task', MAIN_FILE, c => {
  if (c.includes('cleanup_playlist.cleanup_expired(600)')) return c;
  return replaceOnce(c,
    `    let cleanup_search = search_store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            cleanup_search.cleanup_expired(600).await; // 10 min TTL
        }
    });`,
    `    let cleanup_search = search_store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            cleanup_search.cleanup_expired(600).await; // 10 min TTL
        }
    });

    let cleanup_playlist = playlist_store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            cleanup_playlist.cleanup_expired(600).await; // 10 min TTL
        }
    });`,
    'main: playlist cleanup task'
  );
});

// ============================================================
// Summary
// ============================================================
console.log('');
if (errors === 0) {
  console.log('All patches applied successfully.');
} else {
  console.error(errors + ' patch(es) FAILED. Fix errors above and re-run.');
  process.exit(1);
}
