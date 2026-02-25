/// Telegram bot command handlers.
///
/// Handles /start, /help, /download, /dv, /da, /search, /status, /cancel, /ping, /upcook, /chatid.
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ParseMode, Recipient};
use teloxide::utils::command::BotCommands;
use tracing::{info, error, warn};
use uuid::Uuid;
use tokio::time::Instant;

use hermes_shared::ipc_protocol::*;
use hermes_shared::task_queue::TaskQueue;
use sqlx::SqlitePool;

use crate::workers::python_dispatcher::PythonDispatcher;
use crate::callback_state::{
    CallbackStateStore, SearchStateStore, SearchPending, SearchResultItem,
    PlaylistStateStore, PlaylistPending,
    DownloadMode, FormatOption, PendingSelection,
    decode_callback, encode_callback, encode_cancel, parse_format_options,
    encode_search_callback, encode_search_format_callback,
    encode_playlist_confirm, encode_playlist_limit, encode_playlist_format,
};
use crate::link_detector;
use crate::link_detector::DetectedLink;

/// Build the per-user, per-task output directory path.
/// Structure: <download_dir>/<chat_id>/<task_id>/
pub fn task_output_dir(base: &str, chat_id: i64, task_id: &str) -> String {
    let path = std::path::PathBuf::from(base)
        .join(chat_id.to_string())
        .join(task_id);
    path.to_string_lossy().to_string()
}

/// Bot command definitions.
#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Hermes Download Bot commands:")]
pub enum Command {
    #[command(description = "Start the bot")]
    Start,
    #[command(description = "Show help")]
    Help,
    #[command(description = "Download audio from a URL")]
    Download(String),
    #[command(description = "Download video (choose quality)")]
    Dv(String),
    #[command(description = "Download audio (choose quality)")]
    Da(String),
    #[command(description = "Preview and download playlist")]
    Playlist(String),
    #[command(description = "Search YouTube")]
    Search(String),
    #[command(description = "Check task status")]
    Status,
    #[command(description = "Cancel a download")]
    Cancel(String),
    #[command(description = "View download history")]
    History,
    #[command(description = "Health check")]
    Ping,
    #[command(description = "Update cookies (admin)")]
    Upcook(String),
    #[command(description = "Show your Telegram Chat ID")]
    Chatid,
    #[command(description = "Open OTP-free login window for N seconds (admin, max 300)")]
    Allow(String),
}

/// Shared application state passed to handlers.
pub struct AppState {
    pub dispatcher: PythonDispatcher,
    pub task_queue: TaskQueue,
    pub download_dir: String,
    pub callback_store: CallbackStateStore,
    pub search_store: SearchStateStore,
    pub playlist_store: PlaylistStateStore,
    pub db_pool: Option<SqlitePool>,
    pub admin_chat_id: Option<i64>,
}

/// Handle incoming commands.
pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    // Track user in DB (captures username from Telegram)
    if let Some(pool) = &state.db_pool {
        let username = msg.from()
            .and_then(|u| u.username.as_deref());
        let _ = hermes_shared::db::upsert_user(pool, msg.chat.id.0, username).await;
    }

    match cmd {
        Command::Start => cmd_start(bot, msg).await,
        Command::Help => cmd_help(bot, msg).await,
        Command::Download(url) => cmd_download(bot, msg, url, state).await,
        Command::Dv(url) => cmd_download_with_quality(bot, msg, url, DownloadMode::Video, state).await,
        Command::Da(url) => cmd_download_with_quality(bot, msg, url, DownloadMode::Audio, state).await,
        Command::Playlist(url) => cmd_playlist_preview(bot, msg, url, state).await,
        Command::Search(query) => cmd_search(bot, msg, query, state).await,
        Command::Status => cmd_status(bot, msg, state).await,
        Command::Cancel(task_id) => cmd_cancel(bot, msg, task_id, state).await,
        Command::History => cmd_history(bot, msg).await,
        Command::Ping => cmd_ping(bot, msg, state).await,
        Command::Upcook(content) => cmd_upcook(bot, msg, content, state).await,
        Command::Chatid => cmd_chatid(bot, msg).await,
        Command::Allow(secs_str) => cmd_allow(bot, msg, secs_str, state).await,
    }
}

/// /start - Welcome message
async fn cmd_start(bot: Bot, msg: Message) -> ResponseResult<()> {
    let chat_id = msg.chat.id.0;
    let help_text = "\
🎵 Hermes Download Bot

Download audio & video from YouTube, Telegram, and more!

📥 Quick Start:
Just paste a link and I'll download it for you!
Multiple links? I'll batch download them all.

🎬 Download Commands:
/download <url> — Download audio (default)
/dv <url> — Download video — choose resolution
/da <url> — Download audio — choose format
/playlist <url> — Preview & download playlists

🔍 Search & Browse:
/search <query> — Search YouTube (10 results)

📊 Manage Downloads:
/status — Show active & recent tasks
/cancel <id> — Cancel a download

ℹ️ Utilities:
/chatid — Show your Chat ID
/ping — Health check
/help — Show this message

💡 Telegram Forwarding:
Paste t.me links to forward files from channels.
Send multiple links for batch operations.

🌐 Web Dashboard:
https://tg-herms-bot.pgwiz.cloud/
Log in with your Chat ID to manage downloads";
    bot.send_message(msg.chat.id, help_text).await?;
    // Chat ID in monospace so the user can easily copy it
    bot.send_message(msg.chat.id, format!("🔐 Your Chat ID: `{}`", chat_id))
        .parse_mode(ParseMode::MarkdownV2)
        .await?;
    Ok(())
}

/// /help - Show help
async fn cmd_help(bot: Bot, msg: Message) -> ResponseResult<()> {
    cmd_start(bot, msg).await
}

/// /chatid - Send the user their Telegram Chat ID
async fn cmd_chatid(bot: Bot, msg: Message) -> ResponseResult<()> {
    let chat_id = msg.chat.id.0;
    bot.send_message(msg.chat.id, format!(
        "🔐 Your Chat ID\n\n{}\n\nAccess Dashboard:\nhttps://tg-herms-bot.pgwiz.cloud/\n\nPaste your Chat ID there to log in.",
        chat_id
    )).await?;
    Ok(())
}

/// /allow N - Open an OTP-free login window for N seconds (admin only, max 300)
async fn cmd_allow(
    bot: Bot,
    msg: Message,
    secs_str: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    // Admin-only
    if state.admin_chat_id != Some(msg.chat.id.0) {
        bot.send_message(msg.chat.id, "🔒 Access Denied\n\nYou are not authorized to use this command.")
            .await?;
        return Ok(());
    }

    let secs: i64 = match secs_str.trim().parse::<i64>() {
        Ok(n) if n > 0 && n <= 300 => n,
        Ok(_) => {
            bot.send_message(msg.chat.id, "⚠️ Invalid Duration\n\nSeconds must be between 1 and 300.")
                .await?;
            return Ok(());
        }
        Err(_) => {
            bot.send_message(msg.chat.id, "⚠️ Invalid Input\n\nUsage: /allow <seconds>\n\nExample: /allow 120")
                .await?;
            return Ok(());
        }
    };

    if let Some(pool) = &state.db_pool {
        match hermes_shared::db::set_allow_window(pool, secs).await {
            Ok(_) => {
                // Generate an auth token for quick access
                let token = format!("{:x}", uuid::Uuid::new_v4());
                let admin_id = state.admin_chat_id.unwrap_or(msg.chat.id.0);

                // Create a JWT session for the admin
                if let Ok(_) = hermes_shared::db::create_jwt_session(pool, admin_id, &token, secs).await {
                    let dashboard_url = format!("https://tg-herms-bot.pgwiz.cloud/?token={}", token);
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "✅ Quick Login Window Opened\n\n⏱️ Duration: {} seconds\n\n📋 Your Chat ID:\n{}\n\n🔗 Direct Access Link:\n{}\n\n📝 Steps:\n1. Copy your Chat ID above\n2. Click the dashboard link\n3. If prompted, paste your Chat ID\n\n⚠️ This is for emergency access only. Use with caution.",
                            secs, admin_id, dashboard_url
                        ),
                    ).await?;
                } else {
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "✅ Quick Login Window Opened\n\n⏱️ Duration: {} seconds\n\n📋 Your Chat ID:\n{}\n\n🔓 Anyone with this Chat ID can now log in without OTP.\n\n📝 Steps:\n1. Copy your Chat ID above\n2. Go to: https://tg-herms-bot.pgwiz.cloud/\n3. Paste Chat ID to log in\n\n⚠️ This is for emergency access only. Use with caution.",
                            secs, admin_id
                        ),
                    ).await?;
                }
            }
            Err(e) => {
                bot.send_message(msg.chat.id, format!("❌ Failed to Open Window\n\nError: {}", e))
                    .await?;
            }
        }
    } else {
        bot.send_message(msg.chat.id, "❌ Database unavailable")
            .await?;
    }

    Ok(())
}

/// /download <url> - Download audio from URL (default behavior)
async fn cmd_download(
    bot: Bot,
    msg: Message,
    url: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let url = url.trim().to_string();
    if url.is_empty() {
        bot.send_message(msg.chat.id, "⬇️ *Download Audio*\n\nUsage: `/download <url>`\n\nExample:\n`/download https://youtu.be/dQw4w9WgXcQ`")
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    // Detect link type
    let link = match link_detector::detect_first_link(&url) {
        Some(l) if l.is_telegram() => {
            // Delegate Telegram links to the forward handler
            return cmd_telegram_forward(bot, msg, vec![l], state).await;
        }
        Some(l) if l.is_supported() => l,
        Some(_) => {
            bot.send_message(msg.chat.id,
                "❌ Unsupported Link Type\n\n\
                 Currently supported:\n\
                 • YouTube videos & playlists\n\
                 • Telegram channels & groups\n\
                 • yt-dlp compatible sites\n\n\
                 Other link types coming soon!"
            ).await?;
            return Ok(());
        }
        None => {
            bot.send_message(msg.chat.id, "❌ Could not detect a valid URL. Please check and try again.").await?;
            return Ok(());
        }
    };

    let task_id = Uuid::new_v4().to_string();
    let short_id = task_id[..8].to_string();
    let chat_id = msg.chat.id;
    let is_playlist = link.is_playlist();

    // Enqueue
    state.task_queue.enqueue(&task_id, chat_id.0, link.ipc_action()).await;

    // Create DB record so the task shows in web dashboard
    if let Some(pool) = &state.db_pool {
        let _ = hermes_shared::db::create_task(pool, &task_id, chat_id.0, link.ipc_action(), link.url(), None).await;
    }

    // Send initial feedback
    let status_icon = if is_playlist { "📋" } else { "⏳" };
    let status_msg = bot.send_message(chat_id, format!(
        "{} Task Queued [{}]\n\nSource:\n{}",
        status_icon, short_id, link.url()
    ))
        .await?;
    let status_msg_id = status_msg.id;

    // Build IPC request
    let out_dir = task_output_dir(&state.download_dir, chat_id.0, &task_id);
    let request = if is_playlist {
        playlist_request(&task_id, link.url(), &out_dir)
    } else {
        download_request(&task_id, link.url(), true, &out_dir)
    };

    // Spawn download in background so the teloxide handler returns immediately.
    // This prevents blocking all other commands for this chat during the download.
    let task_kind = if is_playlist { "playlist" } else { "download" };
    tokio::spawn(async move {
        let _ = execute_download_and_send(
            &bot,
            chat_id,
            status_msg_id,
            &short_id,
            task_kind,
            &task_id,
            &request,
            DownloadMode::Audio,
            &state,
        ).await;
    });

    Ok(())
}

/// Forward/copy messages from Telegram channels to the user.
/// Handles both single links and batch (multiple links).
async fn cmd_telegram_forward(
    bot: Bot,
    msg: Message,
    links: Vec<DetectedLink>,
    _state: Arc<AppState>,
) -> ResponseResult<()> {
    // Filter to only Telegram links
    let tg_links: Vec<&DetectedLink> = links.iter()
        .filter(|l| l.is_telegram())
        .collect();

    if tg_links.is_empty() {
        bot.send_message(msg.chat.id, "No valid Telegram links found.").await?;
        return Ok(());
    }

    let chat_id = msg.chat.id;
    let total = tg_links.len();

    if total == 1 {
        // Single link - simple forward
        let link = tg_links[0];
        let status_msg = bot.send_message(chat_id, "Forwarding from channel...").await?;

        match copy_telegram_message(&bot, chat_id, link).await {
            Ok(()) => {
                // Status message served its purpose — remove it
                let _ = bot.delete_message(chat_id, status_msg.id).await;
            }
            Err(e) => {
                let err_text = telegram_error_message(&e);
                let _ = bot.edit_message_text(chat_id, status_msg.id, err_text).await;
            }
        }
    } else {
        // Batch - forward multiple
        let status_msg = bot.send_message(chat_id, format!(
            "Forwarding 0/{} files...", total
        )).await?;
        let status_id = status_msg.id;

        let mut success_count = 0usize;
        let mut failed = 0usize;
        let mut last_edit = Instant::now();

        for (i, link) in tg_links.iter().enumerate() {
            match copy_telegram_message(&bot, chat_id, link).await {
                Ok(()) => success_count += 1,
                Err(e) => {
                    failed += 1;
                    warn!("Telegram forward failed for {}: {}", link.url(), e);
                }
            }

            // Throttle progress edits (every 3 messages or every 2 seconds)
            let done = i + 1;
            if done == total || (done % 3 == 0 && last_edit.elapsed().as_secs() >= 2) {
                let _ = bot.edit_message_text(chat_id, status_id, format!(
                    "Forwarding {}/{}", done, total
                )).await;
                last_edit = Instant::now();
            }

            // Rate limit: 10s between copies (configurable via TELEGRAM_BATCH_DELAY_SECS)
            if done < total {
                let delay_secs: u64 = std::env::var("TELEGRAM_BATCH_DELAY_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(10);
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            }
        }

        // Final summary
        let summary = if failed == 0 {
            format!("Copied {} message{}", success_count, if success_count == 1 { "" } else { "s" })
        } else {
            format!("Copied {}/{} ({} failed)", success_count, total, failed)
        };
        let _ = bot.edit_message_text(chat_id, status_id, summary).await;
    }

    Ok(())
}

/// Copy a single message from a Telegram channel to the user via copy_message.
///
/// copy_message sends content without the "Forwarded from" header, regardless of
/// whether the original is media or text — the user just receives the content cleanly.
async fn copy_telegram_message(
    bot: &Bot,
    chat_id: ChatId,
    link: &DetectedLink,
) -> Result<(), teloxide::RequestError> {
    if let DetectedLink::TelegramFile { username, channel_id, message_id, .. } = link {
        let from_chat: Recipient = if let Some(uname) = username {
            Recipient::ChannelUsername(format!("@{}", uname))
        } else if let Some(cid) = channel_id {
            Recipient::Id(ChatId(*cid))
        } else {
            return Err(teloxide::RequestError::Api(
                teloxide::ApiError::Unknown("Invalid channel reference".to_string())
            ));
        };

        // copy_message delivers the content without any "Forwarded from" header
        bot.copy_message(chat_id, from_chat, MessageId(*message_id)).await?;
        Ok(())
    } else {
        Ok(())
    }
}

/// Convert a Telegram API error to a user-friendly message.
fn telegram_error_message(err: &teloxide::RequestError) -> String {
    let err_str = err.to_string();
    if err_str.contains("chat not found") {
        "I don't have access to that channel.\nAdd me to the channel first, or make sure the link is correct.".to_string()
    } else if err_str.contains("message to copy not found") || err_str.contains("message not found") {
        "Message not found. It may have been deleted.".to_string()
    } else if err_str.contains("bot was kicked") || err_str.contains("bot is not a member") {
        "I'm not a member of that channel. Add me first.".to_string()
    } else {
        format!("Failed to forward: {}", err_str)
    }
}

/// /dv or /da - Download with quality selection menu
async fn cmd_download_with_quality(
    bot: Bot,
    msg: Message,
    url: String,
    mode: DownloadMode,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let url = url.trim().to_string();
    if url.is_empty() {
        let cmd = if mode == DownloadMode::Video { "/dv" } else { "/da" };
        bot.send_message(msg.chat.id, format!("Usage: {} <youtube-url>", cmd)).await?;
        return Ok(());
    }

    // Detect link type
    let link = match link_detector::detect_first_link(&url) {
        Some(l) if l.is_supported() && !l.is_telegram() => l,
        Some(l) if l.is_telegram() => {
            bot.send_message(msg.chat.id, "Quality selection is not available for Telegram links. Just paste the link directly.").await?;
            return Ok(());
        }
        Some(_) => {
            bot.send_message(msg.chat.id,
                "This link type is not supported yet.\n\
                 Currently YouTube and Telegram links are supported.\n\
                 Other link support coming soon!"
            ).await?;
            return Ok(());
        }
        None => {
            bot.send_message(msg.chat.id, "Could not detect a valid YouTube URL.").await?;
            return Ok(());
        }
    };

    if link.is_playlist() {
        bot.send_message(msg.chat.id, "Quality selection is not available for playlists. Use /download instead.").await?;
        return Ok(());
    }

    let chat_id = msg.chat.id;
    let mode_label = mode.as_str();

    let fetching_msg = bot.send_message(chat_id, format!(
        "Fetching {} formats...", mode_label
    )).await?;

    // Fetch formats from Python worker
    let task_id = Uuid::new_v4().to_string();
    let request = get_formats_request(&task_id, link.url(), mode_label);

    match state.dispatcher.send_and_wait(&request, 30).await {
        Ok(response) => {
            if response.is_error() {
                let err = response.error_message().unwrap_or_else(|| "Failed to fetch formats".into());
                bot.edit_message_text(chat_id, fetching_msg.id, format!(
                    "Error: {}", err
                )).await?;
                return Ok(());
            }

            let title = response.data.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            let duration_str = response.data.get("duration_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let formats_data = response.data.get("formats")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            if formats_data.is_empty() {
                bot.edit_message_text(chat_id, fetching_msg.id,
                    "No formats available for this video."
                ).await?;
                return Ok(());
            }

            let format_options = parse_format_options(&formats_data);

            // Generate a short key for callback data
            let key = task_id[..6].to_string();

            // Build inline keyboard
            let keyboard = build_quality_keyboard(&format_options, &mode, &key);

            // Store state for callback
            let pending = PendingSelection {
                chat_id: chat_id.0,
                url: link.url().to_string(),
                message_id: fetching_msg.id,
                formats: format_options,
                created_at: std::time::Instant::now(),
                title: title.to_string(),
            };
            state.callback_store.store(key, pending).await;

            // Update message with keyboard
            let header = format!(
                "Select {} quality:\n{} [{}]",
                mode_label, title, duration_str
            );
            bot.edit_message_text(chat_id, fetching_msg.id, header)
                .reply_markup(keyboard)
                .await?;
        }
        Err(e) => {
            error!("Get formats IPC failed: {}", e);
            bot.edit_message_text(chat_id, fetching_msg.id, format!(
                "Error fetching formats: {}", e
            )).await?;
        }
    }

    Ok(())
}

/// Build inline keyboard for format selection.
fn build_quality_keyboard(
    formats: &[FormatOption],
    mode: &DownloadMode,
    key: &str,
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();

    if *mode == DownloadMode::Video {
        // Video: 2 buttons per row
        for chunk in formats.chunks(2) {
            let row: Vec<InlineKeyboardButton> = chunk
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let idx = formats.iter().position(|x| x.format_id == f.format_id && x.label == f.label).unwrap_or(i);
                    InlineKeyboardButton::callback(
                        &f.label,
                        encode_callback(mode, key, idx),
                    )
                })
                .collect();
            rows.push(row);
        }
    } else {
        // Audio: 1 button per row
        for (i, f) in formats.iter().enumerate() {
            rows.push(vec![
                InlineKeyboardButton::callback(
                    &f.label,
                    encode_callback(mode, key, i),
                )
            ]);
        }
    }

    // Cancel button
    rows.push(vec![
        InlineKeyboardButton::callback("Cancel", encode_cancel(key))
    ]);

    InlineKeyboardMarkup::new(rows)
}

/// Handle callback query from inline keyboard button press.
pub async fn handle_callback_query(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let data = match q.data {
        Some(ref d) => d.clone(),
        None => return Ok(()),
    };

    // Handle search format selection (4-part: sf:key:index:a/v) — must run before decode_callback
    if data.starts_with("sf:") {
        let _ = bot.answer_callback_query(&q.id).await;
        let parts: Vec<&str> = data.splitn(4, ':').collect();
        let sf_key   = parts.get(1).copied().unwrap_or("");
        let sf_idx: usize = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
        let is_audio = parts.get(3).copied().unwrap_or("a") == "a";

        let pending = match state.search_store.peek(sf_key).await {
            Some(p) => p,
            None    => return Ok(()),
        };
        if sf_idx >= pending.results.len() { return Ok(()); }

        let result   = &pending.results[sf_idx];
        let url      = result.url.clone();
        let chat_id  = match q.message { Some(ref m) => m.chat.id, None => return Ok(()) };
        let msg_id   = match q.message { Some(ref m) => m.id,      None => return Ok(()) };

        let task_id  = Uuid::new_v4().to_string();
        let short_id = task_id[..8].to_string();
        let mode_label = if is_audio { "audio" } else { "video" };

        state.task_queue.enqueue(&task_id, chat_id.0, "youtube_dl").await;

        if let Some(pool) = &state.db_pool {
            let _ = hermes_shared::db::create_task(
                pool, &task_id, chat_id.0, "youtube_dl", &url, Some(mode_label),
            ).await;
        }

        // Edit the format-choice message to show download status
        let _ = bot.edit_message_text(chat_id, msg_id,
            format!("Queued [{}] ({}) — {}", short_id, mode_label, url)
        ).reply_markup(InlineKeyboardMarkup::new(Vec::<Vec<InlineKeyboardButton>>::new())).await;

        let out_dir  = task_output_dir(&state.download_dir, chat_id.0, &task_id);
        let dl_mode  = if is_audio { DownloadMode::Audio } else { DownloadMode::Video };
        let request  = download_request(&task_id, &url, is_audio, &out_dir);

        let state2 = state.clone();
        tokio::spawn(async move {
            let _ = execute_download_and_send(
                &bot,
                chat_id,
                msg_id,
                &short_id,
                mode_label,
                &task_id,
                &request,
                dl_mode,
                &state2,
            ).await;
        });
        return Ok(());
    }

    // Handle playlist confirm (pc:KEY:[p/s/x]) — before decode_callback
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
            // For playlists, use archive file for deduplication
            let archive_path = format!("{}/playlist_archive.txt", state.download_dir);
            let req = playlist_request_opts(
                &task_id, &pending.url, &out_dir, pending.limit, pf_is_audio, Some(&archive_path),
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
        ).reply_markup(InlineKeyboardMarkup::new(Vec::<Vec<InlineKeyboardButton>>::new())).await;

        let state2 = state.clone();
        tokio::spawn(async move {
            let _ = execute_download_and_send(
                &bot, chat_id, msg_id, &short_id,
                kind_label, &task_id, &request, dl_mode, &state2,
            ).await;
        });
        return Ok(());
    }

    // Handle playlist preview download (pl_dl:URL) — triggered from preview
    if data.starts_with("pl_dl:") {
        let _ = bot.answer_callback_query(&q.id).await;
        let url = &data[6..]; // Extract URL after "pl_dl:"

        let chat_id = match q.message { Some(ref m) => m.chat.id, None => return Ok(()) };
        let msg_id  = match q.message { Some(ref m) => m.id,      None => return Ok(()) };

        // Create a new playlist store entry
        let key = format!("{:x}", chrono::Utc::now().timestamp_millis());
        state.playlist_store.store(key.clone(), PlaylistPending {
            url: url.to_string(),
            chat_id: chat_id.0,
            message_id: msg_id,
            is_single: false, // This is a playlist, not a single video
            limit: Some(10), // Default to 10 tracks
            created_at: std::time::Instant::now(),
        }).await;

        // Show track limit selection
        let buttons = vec![
            vec![
                InlineKeyboardButton::callback("10 tracks",  encode_playlist_limit(&key, 10)),
                InlineKeyboardButton::callback("25 tracks",  encode_playlist_limit(&key, 25)),
            ],
            vec![
                InlineKeyboardButton::callback("50 tracks",  encode_playlist_limit(&key, 50)),
                InlineKeyboardButton::callback("All tracks", encode_playlist_limit(&key, 0)),
            ],
        ];
        let _ = bot.edit_message_text(chat_id, msg_id, "How many tracks to download?")
            .reply_markup(InlineKeyboardMarkup::new(buttons))
            .await;
        return Ok(());
    }

    let (mode_prefix, key, index) = match decode_callback(&data) {
        Some(decoded) => decoded,
        None => {
            if let Some(id) = q.id.as_str().into() {
                let _ = bot.answer_callback_query(id).await;
            }
            return Ok(());
        }
    };

    // Answer the callback query immediately to stop the loading spinner
    let _ = bot.answer_callback_query(&q.id).await;

    // Handle cancel
    if mode_prefix == "cx" {
        if let Some(pending) = state.callback_store.take(&key).await {
            let chat_id = ChatId(pending.chat_id);
            let _ = bot.edit_message_text(chat_id, pending.message_id, "Cancelled.").await;
        }
        return Ok(());
    }

    // Handle search result selection — show audio/video format choice
    if mode_prefix == "sr" {
        let pending = match state.search_store.peek(&key).await {
            Some(p) => p,
            None    => return Ok(()),
        };
        if index >= pending.results.len() { return Ok(()); }

        let result = &pending.results[index];
        let title  = if result.title.chars().count() > 50 {
            format!("{}…", result.title.chars().take(49).collect::<String>())
        } else {
            result.title.clone()
        };
        let chat_id = match q.message { Some(ref m) => m.chat.id, None => return Ok(()) };

        // Send a new message with Audio / Video choice (search results message stays untouched)
        let buttons = vec![vec![
            InlineKeyboardButton::callback("🎵 Audio (MP3)", encode_search_format_callback(&key, index, true)),
            InlineKeyboardButton::callback("🎬 Video (MP4)", encode_search_format_callback(&key, index, false)),
        ]];
        let _ = bot.send_message(chat_id, format!("Choose format:\n{}", title))
            .reply_markup(InlineKeyboardMarkup::new(buttons))
            .await;

        return Ok(());
    }

    // Parse mode
    let mode = match DownloadMode::from_prefix(&mode_prefix) {
        Some(m) => m,
        None => return Ok(()),
    };

    // Get pending selection
    let pending = match state.callback_store.take(&key).await {
        Some(p) => p,
        None => {
            // Expired or already used
            if let Some(msg) = q.message {
                let chat_id = msg.chat.id;
                let _ = bot.edit_message_text(chat_id, msg.id, "Selection expired. Please try again.").await;
            }
            return Ok(());
        }
    };

    // Validate index
    if index >= pending.formats.len() {
        return Ok(());
    }

    let format = &pending.formats[index];
    let chat_id = ChatId(pending.chat_id);

    // Update message to show download started
    let short_label = &format.label;
    let _ = bot.edit_message_text(
        chat_id,
        pending.message_id,
        format!("Downloading: {} [{}]", pending.title, short_label),
    ).await;

    let status_msg_id = pending.message_id;
    let task_id = Uuid::new_v4().to_string();
    let short_id = task_id[..8].to_string();

    // Build IPC request based on format selection
    let out_dir = task_output_dir(&state.download_dir, pending.chat_id, &task_id);
    let request = download_request_with_format(
        &task_id,
        &pending.url,
        &format.format_id,
        format.extract_audio,
        format.audio_format.as_deref(),
        format.audio_quality.as_deref(),
        &out_dir,
    );

    // Enqueue task
    state.task_queue.enqueue(&task_id, pending.chat_id, "youtube_dl").await;

    // Create DB record so the task shows in web dashboard
    if let Some(pool) = &state.db_pool {
        let label = Some(mode.as_str());
        let _ = hermes_shared::db::create_task(pool, &task_id, pending.chat_id, "youtube_dl", &pending.url, label).await;
    }

    // Spawn download in background so the teloxide handler returns immediately.
    let mode_str = mode.as_str().to_string();
    tokio::spawn(async move {
        let _ = execute_download_and_send(
            &bot,
            chat_id,
            status_msg_id,
            &short_id,
            &mode_str,
            &task_id,
            &request,
            mode,
            &state,
        ).await;
    });

    Ok(())
}

/// Execute a download request, stream progress, and send the resulting file.
/// Shared by cmd_download and handle_callback_query.
pub async fn execute_download_and_send(
    bot: &Bot,
    chat_id: ChatId,
    status_msg_id: MessageId,
    short_id: &str,
    kind: &str,
    task_id: &str,
    request: &IPCRequest,
    mode: DownloadMode,
    state: &AppState,
) -> ResponseResult<()> {
    // Acquire concurrency slot
    if !state.task_queue.acquire(task_id).await {
        bot.edit_message_text(chat_id, status_msg_id, format!(
            "Failed to acquire download slot [{}]", short_id
        )).await?;
        return Ok(());
    }

    // Send to Python worker and process response stream
    let mut rx = match state.dispatcher.send(request).await {
        Ok(rx) => rx,
        Err(e) => {
            state.task_queue.fail(task_id).await;
            error!("Failed to send IPC request: {}", e);
            bot.edit_message_text(chat_id, status_msg_id, format!(
                "Worker error: {} [{}]", e, short_id
            )).await?;
            return Ok(());
        }
    };

    // Process response stream with throttled progress updates
    let mut last_edit = Instant::now();
    let mut last_percent: i32 = -1;
    let timeout = tokio::time::Duration::from_secs(600); // 10 min

    let result = tokio::time::timeout(timeout, async {
        while let Some(response) = rx.recv().await {
            if response.is_progress() {
                let pct = response.progress_percent().unwrap_or(0) as i32;
                let speed = response.progress_speed().unwrap_or_default();
                let status = response.data.get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("downloading");

                // Throttle edits: at least 3s apart and at least 5% change
                let elapsed = last_edit.elapsed().as_secs();
                if elapsed >= 3 && (pct - last_percent).abs() >= 5 {
                    let bar = progress_bar(pct as u8);
                    let text = format!(
                        "{} [{}]\n{} {}%\nSpeed: {}\nStatus: {}",
                        kind, short_id, bar, pct, speed, status
                    );
                    let _ = bot.edit_message_text(chat_id, status_msg_id, text).await;
                    last_edit = Instant::now();
                    last_percent = pct;
                }
                state.task_queue.update_progress(task_id, pct as u8, Some(speed)).await;
                continue;
            }

            // Non-progress event = final response
            return Some(response);
        }
        None
    }).await;

    // Handle result
    match result {
        Ok(Some(response)) => {
            if response.is_error() {
                let error_msg = response.error_message().unwrap_or_else(|| "Unknown error".into());
                state.task_queue.fail(task_id).await;
                // Persist failure to DB
                if let Some(pool) = &state.db_pool {
                    let _ = hermes_shared::db::fail_task(pool, task_id, &error_msg).await;
                }
                bot.edit_message_text(chat_id, status_msg_id, format!(
                    "Download failed [{}]\n{}", short_id, error_msg
                )).await?;
            } else {
                state.task_queue.complete(task_id).await;

                let file_path = response.data.get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let filename = response.data.get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("download");

                // Persist completion to DB
                if let Some(pool) = &state.db_pool {
                    let _ = hermes_shared::db::complete_task(pool, task_id, file_path).await;
                }

                bot.edit_message_text(chat_id, status_msg_id, format!(
                    "Download complete [{}]\nFile: {}", short_id, filename
                )).await?;

                // Send the file to user
                let path = std::path::PathBuf::from(file_path);
                if path.exists() {
                    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let input = teloxide::types::InputFile::file(&path);

                    if file_size > 50 * 1024 * 1024 {
                        // >50MB: send as document
                        if let Err(e) = bot.send_document(chat_id, input).await {
                            warn!("Failed to send document: {}", e);
                            bot.send_message(chat_id, format!(
                                "File too large for Telegram ({:.1}MB)",
                                file_size as f64 / 1024.0 / 1024.0
                            )).await?;
                        }
                    } else if mode == DownloadMode::Video {
                        // Send as video (shows inline player)
                        if let Err(e) = bot.send_video(chat_id, input).await {
                            warn!("Failed to send video, trying document: {}", e);
                            let input2 = teloxide::types::InputFile::file(&path);
                            let _ = bot.send_document(chat_id, input2).await;
                        }
                    } else {
                        // Send as audio
                        if let Err(e) = bot.send_audio(chat_id, input).await {
                            warn!("Failed to send audio, trying document: {}", e);
                            let input2 = teloxide::types::InputFile::file(&path);
                            let _ = bot.send_document(chat_id, input2).await;
                        }
                    }
                } else if !file_path.is_empty() {
                    warn!("Downloaded file not found at: {}", file_path);
                }

                // Handle playlist files - send each individually
                if let Some(files) = response.data.get("files").and_then(|v| v.as_array()) {
                    if !files.is_empty() {
                        let _ = bot.send_message(chat_id, format!(
                            "📤 Sending {} track(s)...",
                            files.len()
                        )).await;

                        for (idx, file_info) in files.iter().enumerate() {
                            let file_path = file_info.get("path").and_then(|v| v.as_str()).unwrap_or("");
                            let file_name = file_info.get("name").and_then(|v| v.as_str()).unwrap_or("track");

                            let fpath = std::path::PathBuf::from(file_path);
                            if fpath.exists() {
                                let input = teloxide::types::InputFile::file(&fpath);
                                if let Err(e) = bot.send_audio(chat_id, input).await {
                                    warn!("Failed to send audio {}: {}", file_name, e);
                                    // Try as document if audio fails
                                    let input2 = teloxide::types::InputFile::file(&fpath);
                                    let _ = bot.send_document(chat_id, input2).await;
                                }

                                // Add delay between sends to avoid rate limiting
                                if idx < files.len() - 1 {
                                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                                }
                            }
                        }

                        let _ = bot.send_message(chat_id, format!(
                            "✅ Sent all {} tracks", files.len()
                        )).await;
                    }
                } else if let Some(archives) = response.data.get("archives").and_then(|v| v.as_array()) {
                    // Fallback: handle archives if present (for backward compatibility)
                    for archive in archives {
                        let archive_path = archive.get("path").and_then(|v| v.as_str()).unwrap_or("");
                        let archive_name = archive.get("name").and_then(|v| v.as_str()).unwrap_or("archive.zip");

                        let apath = std::path::PathBuf::from(archive_path);
                        if apath.exists() {
                            let input = teloxide::types::InputFile::file(&apath);
                            if let Err(e) = bot.send_document(chat_id, input).await {
                                warn!("Failed to send archive {}: {}", archive_name, e);
                            }
                        }
                    }
                }
            }
        }
        Ok(None) => {
            state.task_queue.fail(task_id).await;
            if let Some(pool) = &state.db_pool {
                let _ = hermes_shared::db::fail_task(pool, task_id, "Worker connection lost").await;
            }
            bot.edit_message_text(chat_id, status_msg_id, format!(
                "Worker connection lost [{}]", short_id
            )).await?;
        }
        Err(_) => {
            state.task_queue.fail(task_id).await;
            if let Some(pool) = &state.db_pool {
                let _ = hermes_shared::db::fail_task(pool, task_id, "Download timed out").await;
            }
            bot.edit_message_text(chat_id, status_msg_id, format!(
                "Download timed out [{}]", short_id
            )).await?;
        }
    }

    // Cleanup
    state.dispatcher.remove_pending(task_id).await;
    Ok(())
}

/// /playlist <url> - Preview and download playlist
async fn cmd_playlist_preview(
    bot: Bot,
    msg: Message,
    url: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    use hermes_shared::ipc_protocol::{playlist_preview_request, IPCResponse};

    let url = url.trim().to_string();
    if url.is_empty() {
        bot.send_message(msg.chat.id, "🎵 *Download Playlist*\n\nUsage: `/playlist <url>`\n\nI'll show you a preview of the first few tracks, then you can choose:\n• How many tracks to download\n• Audio or video format\n\nExample:\n`/playlist https://www.youtube.com/playlist?list=...`")
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    // Detect link type
    if let Some(link) = crate::link_detector::detect_first_link(&url) {
        // Accept both playlists and single videos
        match link {
            crate::link_detector::DetectedLink::YoutubePlaylist { .. } => {
                // Proceed with playlist preview
            }
            crate::link_detector::DetectedLink::YoutubeVideo { .. }
            | crate::link_detector::DetectedLink::YoutubeShort { .. }
            | crate::link_detector::DetectedLink::YoutubeMusic { .. } => {
                // For single videos: treat as single-item playlist and download directly
                // Show format selection instead of preview
                return cmd_download(bot, msg, link.url().to_string(), state).await;
            }
            _ => {
                bot.send_message(msg.chat.id, "❌ This is not a supported YouTube link.\n\n✓ Playlists\n✓ Videos\n✓ Shorts\n\nPlease check the URL and try again.").await?;
                return Ok(());
            }
        }
    } else {
        bot.send_message(msg.chat.id, "❌ Could not detect a valid URL. Please check and try again.").await?;
        return Ok(());
    }

    // Check if this is a Radio Mix (list=RD pattern)
    // Radio Mixes are infinite and slow to preview, so skip to track selection
    // Match list=RD as a URL parameter (preceded by ? or &), not as part of a video ID
    let is_radio_mix = url.contains("?list=RD") || url.contains("&list=RD");
    if is_radio_mix {
        let key = format!("{:x}", chrono::Utc::now().timestamp_millis());
        state.playlist_store.store(key.clone(), PlaylistPending {
            url: url.to_string(),
            chat_id: msg.chat.id.0,
            message_id: msg.id,
            is_single: false,
            limit: Some(10),
            created_at: std::time::Instant::now(),
        }).await;

        // For Radio Mixes, go straight to track limit selection (skip preview)
        let buttons = vec![
            vec![
                InlineKeyboardButton::callback("🎵 10 tracks",  encode_playlist_limit(&key, 10)),
                InlineKeyboardButton::callback("🎵 25 tracks",  encode_playlist_limit(&key, 25)),
            ],
            vec![
                InlineKeyboardButton::callback("🎵 50 tracks",  encode_playlist_limit(&key, 50)),
                InlineKeyboardButton::callback("🎵 All tracks", encode_playlist_limit(&key, 0)),
            ],
        ];
        bot.send_message(msg.chat.id, "🎵 Radio Mix detected\n\n(Infinite playlist \\- skipping preview)\n\nHow many tracks to download?")
            .parse_mode(ParseMode::MarkdownV2)
            .reply_markup(InlineKeyboardMarkup::new(buttons))
            .await?;
        return Ok(());
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let status = bot.send_message(msg.chat.id, "🎵 Fetching playlist info...").await?;

    // Send preview request
    let req = playlist_preview_request(&task_id, &url, 5);
    let mut rx = match state.dispatcher.send(&req).await {
        Ok(rx) => rx,
        Err(e) => {
            bot.edit_message_text(msg.chat.id, status.id, format!("❌ Worker error: {}", e)).await?;
            return Ok(());
        }
    };

    // Wait for response (with timeout)
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await {
        Ok(Some(response)) => {
            let resp: IPCResponse = response;
            if resp.is_error() {
                let err_msg = resp.error_message().unwrap_or_else(|| "Unknown error".to_string());
                bot.edit_message_text(msg.chat.id, status.id, format!("❌ Error: {}", err_msg)).await?;
                return Ok(());
            }

            if resp.is_done() {
                // Parse response data
                if let Some(data) = resp.data.as_object() {
                    let title = data.get("playlist_title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Playlist");
                    let count = data.get("playlist_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let empty_vec = Vec::new();
                    let tracks = data.get("tracks")
                        .and_then(|v| v.as_array())
                        .unwrap_or(&empty_vec);

                    // Format message
                    let mut msg_text = format!("🎵 **{}**\n\n", title);

                    // Show track count or note if unknown (infinite playlists)
                    if count > 0 {
                        msg_text.push_str(&format!("📊 {} tracks total\n\n", count));
                    } else {
                        msg_text.push_str("📊 Total tracks: Unknown \\(infinite or uncountable playlist\\)\n\n");
                    }

                    // Show first few tracks
                    msg_text.push_str("**Preview \\(first tracks\\):**\n");
                    for track in tracks.iter().take(5) {
                        if let Some(track_obj) = track.as_object() {
                            if let (Some(idx), Some(track_title)) = (
                                track_obj.get("index").and_then(|v| v.as_u64()),
                                track_obj.get("title").and_then(|v| v.as_str()),
                            ) {
                                msg_text.push_str(&format!("{}\\. {}\n", idx, track_title));
                            }
                        }
                    }

                    if tracks.len() > 5 {
                        if count > 5 {
                            msg_text.push_str(&format!("\n\\.\\.\\. and {} more\n", count - 5));
                        } else {
                            msg_text.push_str("\n\\.\\.\\. and more available\n");
                        }
                    } else {
                        msg_text.push('\n');
                    }

                    msg_text.push_str("\n**Choose how many tracks to download:**");

                    // Update message with preview + button
                    let keyboard = InlineKeyboardMarkup::new(vec![
                        vec![InlineKeyboardButton::callback("⬇️ Download", format!("pl_dl:{}", url))],
                    ]);

                    bot.edit_message_text(msg.chat.id, status.id, msg_text)
                        .parse_mode(ParseMode::MarkdownV2)
                        .reply_markup(keyboard)
                        .await?;
                } else {
                    bot.edit_message_text(msg.chat.id, status.id, "Could not parse playlist info").await?;
                }
            }
        }
        Ok(None) => {
            bot.edit_message_text(msg.chat.id, status.id, "Worker disconnected unexpectedly").await?;
        }
        Err(_) => {
            bot.edit_message_text(msg.chat.id, status.id, "Request timed out").await?;
        }
    }

    Ok(())
}

/// /search <query> - Search YouTube
async fn cmd_search(
    bot: Bot,
    msg: Message,
    query: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let query = query.trim().to_string();
    if query.is_empty() {
        bot.send_message(msg.chat.id, "🔍 *Search YouTube*\n\nUsage: `/search <query>`\n\nExample:\n`/search billie eilish`")
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    let task_id = Uuid::new_v4().to_string();
    let request = search_request(&task_id, &query, 10);

    let searching_msg = bot.send_message(msg.chat.id, format!(
        "🔍 Searching for: {}\n⏳ Please wait...",
        query
    ))
        .await?;

    match state.dispatcher.send_and_wait(&request, 30).await {
        Ok(response) => {
            if response.is_error() {
                let err = response.error_message().unwrap_or_else(|| "Search failed".into());
                bot.edit_message_text(msg.chat.id, searching_msg.id, format!(
                    "❌ *Search Error*\n\n{}", err
                ))
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
            } else {
                let results = response.data.get("results")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                if results.is_empty() {
                    bot.edit_message_text(msg.chat.id, searching_msg.id,
                        format!("😕 No results found for \"{}\"", query)
                    ).await?;
                } else {
                    // Build (url, title) pairs
                    let items: Vec<(String, String)> = results.iter().map(|r| {
                        let url   = r.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                        (url, title)
                    }).collect();

                    // Store for callback retrieval (peek — buttons stay active)
                    let key: String = task_id[..6].to_string();
                    state.search_store.store(key.clone(), SearchPending {
                        results: items.iter().map(|(url, title)| SearchResultItem {
                            url:   url.clone(),
                            title: title.clone(),
                        }).collect(),
                        created_at: std::time::Instant::now(),
                    }).await;

                    // One button per result, truncated to 52 chars
                    let buttons: Vec<Vec<InlineKeyboardButton>> = items.iter()
                        .enumerate()
                        .map(|(i, (_, title))| {
                            let label: String = if title.chars().count() > 52 {
                                format!("{}…", title.chars().take(51).collect::<String>())
                            } else {
                                title.clone()
                            };
                            vec![InlineKeyboardButton::callback(label, encode_search_callback(&key, i))]
                        })
                        .collect();

                    let from_cache = response.data.get("from_cache")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let cache_note = if from_cache { " · cached" } else { "" };
                    let text = format!("Search: \"{}\"{}  —  tap to download:", query, cache_note);

                    bot.edit_message_text(msg.chat.id, searching_msg.id, text)
                        .reply_markup(InlineKeyboardMarkup::new(buttons))
                        .await?;
                }
            }
        }
        Err(e) => {
            error!("Search IPC failed: {}", e);
            bot.edit_message_text(msg.chat.id, searching_msg.id, format!(
                "Search error: {}", e
            )).await?;
        }
    }

    Ok(())
}

/// /status - Show active task status
async fn cmd_status(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let stats = state.task_queue.stats().await;
    let user_tasks = state.task_queue.get_user_tasks(msg.chat.id.0).await;

    let mut text = format!(
        "Queue Status:\n\
         Running: {}/{}\n\
         Queued: {}\n\
         Completed: {}\n\
         Failed: {}\n",
        stats.running, stats.max_concurrent,
        stats.queued, stats.completed, stats.failed,
    );

    if !user_tasks.is_empty() {
        text.push_str("\nYour tasks:\n");
        for task in user_tasks.iter().take(10) {
            let bar = progress_bar(task.progress);
            text.push_str(&format!(
                "  {} {:?} {} {}%\n",
                &task.task_id[..8], task.status, bar, task.progress
            ));
        }
    } else {
        text.push_str("\nNo active tasks.");
    }

    bot.send_message(msg.chat.id, text).await?;
    Ok(())
}

/// /cancel <task_id> - Cancel a running task
async fn cmd_cancel(
    bot: Bot,
    msg: Message,
    task_id_prefix: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let prefix = task_id_prefix.trim().to_string();
    if prefix.is_empty() {
        bot.send_message(msg.chat.id, "❌ *Cancel Download*\n\nUsage: `/cancel <task-id>`\n\nGet task IDs using `/status`")
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    // Find matching task
    let user_tasks = state.task_queue.get_user_tasks(msg.chat.id.0).await;
    let matching = user_tasks.iter().find(|t| t.task_id.starts_with(&prefix));

    match matching {
        Some(task) => {
            let full_id = task.task_id.clone();
            state.task_queue.cancel(&full_id).await;
            state.dispatcher.remove_pending(&full_id).await;
            bot.send_message(msg.chat.id, format!(
                "Cancelled task [{}]", &full_id[..8]
            )).await?;
        }
        None => {
            bot.send_message(msg.chat.id, format!(
                "No task found matching \"{}\".\nUse /status to see task IDs.", prefix
            )).await?;
        }
    }

    Ok(())
}

/// /history - Show download history
async fn cmd_history(bot: Bot, msg: Message) -> ResponseResult<()> {
    bot.send_message(msg.chat.id, "Download history coming soon.\nUse /status to see active tasks.").await?;
    Ok(())
}

/// /ping - Health check
async fn cmd_ping(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let task_id = Uuid::new_v4().to_string();
    let request = health_check_request(&task_id);

    match state.dispatcher.send_and_wait(&request, 10).await {
        Ok(response) => {
            let version = response.data.get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let handlers = response.data.get("handlers")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let stats = state.task_queue.stats().await;
            bot.send_message(msg.chat.id, format!(
                "✅ *System Status*\n\n\
                 🤖 Worker: `{}`\n\
                 ⚙️ Handlers: `{}`\n\
                 ⏳ Queue: `{}/{}` running\n\n✓ All systems operational",
                version, handlers, stats.running, stats.max_concurrent
            ))
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }
        Err(e) => {
            bot.send_message(msg.chat.id, format!("🔴 *Worker Offline*\n\nError: {}", e))
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }
    }

    Ok(())
}

/// /upcook <content> - Update cookies.txt (admin only)
async fn cmd_upcook(
    bot: Bot,
    msg: Message,
    content: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    // Admin-only check
    let is_admin = state.admin_chat_id
        .map(|id| id == msg.chat.id.0)
        .unwrap_or(false);

    if !is_admin {
        bot.send_message(msg.chat.id, "🔒 Admin Command\n\nThis command is restricted to administrators only.")
            .await?;
        return Ok(());
    }

    let content = content.trim().to_string();

    // Strip surrounding brackets: /upcook [content] → content
    let content = if content.starts_with('[') && content.ends_with(']') {
        content[1..content.len()-1].trim().to_string()
    } else {
        content
    };

    if content.is_empty() {
        bot.send_message(msg.chat.id,
            "Usage: /upcook [cookie content]\n\n\
             Paste the Netscape cookie file content inside brackets."
        ).await?;
        return Ok(());
    }

    let cookie_path = std::env::var("YOUTUBE_COOKIE_FILE")
        .unwrap_or_else(|_| "./cookies.txt".to_string());

    // Resolve relative to WORKER_DIR
    let worker_dir = std::env::var("WORKER_DIR").unwrap_or_else(|_| ".".to_string());
    let full_path = if std::path::Path::new(&cookie_path).is_relative() {
        std::path::PathBuf::from(&worker_dir).join(&cookie_path)
    } else {
        std::path::PathBuf::from(&cookie_path)
    };

    match std::fs::write(&full_path, &content) {
        Ok(_) => {
            let size = content.len();
            let lines = content.lines().count();
            info!("Cookies updated by admin: {} ({} bytes, {} lines)", full_path.display(), size, lines);
            bot.send_message(msg.chat.id, format!(
                "Cookies updated!\nFile: {}\nSize: {} bytes ({} lines)",
                full_path.display(), size, lines
            )).await?;
        }
        Err(e) => {
            error!("Failed to write cookies: {}", e);
            bot.send_message(msg.chat.id, format!("Failed to write cookies: {}", e)).await?;
        }
    }

    Ok(())
}

/// Show playlist confirmation dialog — prompts user for playlist vs single video.
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
        format!("{}\u{2026}", &url[..59])
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
        "Playlist detected!\n{}\n\nDownload the full playlist or just this video?",
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

/// Show playlist confirmation dialog — prompts user for playlist vs single video.
/// Handle plain messages (auto-detect links).
pub async fn handle_message(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    if let Some(text) = msg.text() {
        // Track user in DB (captures username from Telegram)
        if let Some(pool) = &state.db_pool {
            let username = msg.from()
                .and_then(|u| u.username.as_deref());
            let _ = hermes_shared::db::upsert_user(pool, msg.chat.id.0, username).await;
        }

        let links = link_detector::detect_links(text);
        if !links.is_empty() {
            let first = &links[0];
            if first.is_telegram() {
                // Telegram links: forward all detected links
                info!("Auto-detected {} Telegram link(s)", links.len());
                cmd_telegram_forward(bot, msg, links, state).await?;
            } else if first.is_supported() {
                info!("Auto-detected link: {:?}", first);
                if first.is_playlist() {
                    cmd_playlist_confirm(bot, msg, first.url().to_string(), state).await?;
                } else {
                    cmd_download(bot, msg, first.url().to_string(), state).await?;
                }
            } else {
                info!("Unsupported link detected: {}", first.url());
                bot.send_message(msg.chat.id,
                    "This link type is not supported yet.\n\
                     Currently YouTube and Telegram links are supported.\n\
                     Other link support coming soon!"
                ).await?;
            }
        }
    }
    Ok(())
}

/// Generate a simple text progress bar.
fn progress_bar(percent: u8) -> String {
    let filled = (percent as usize) / 5; // 20 chars total
    let empty = 20_usize.saturating_sub(filled);
    format!("[{}{}]", "=".repeat(filled), " ".repeat(empty))
}
