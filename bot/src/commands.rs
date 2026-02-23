/// Telegram bot command handlers.
///
/// Handles /start, /help, /download, /dv, /da, /search, /status, /cancel, /ping, /upcook.
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, MessageId, Recipient};
use teloxide::utils::command::BotCommands;
use tracing::{info, error, warn};
use uuid::Uuid;
use tokio::time::Instant;

use hermes_shared::ipc_protocol::*;
use hermes_shared::task_queue::TaskQueue;
use sqlx::SqlitePool;

use crate::workers::python_dispatcher::PythonDispatcher;
use crate::callback_state::{
    CallbackStateStore, DownloadMode, FormatOption, PendingSelection,
    decode_callback, encode_callback, encode_cancel, parse_format_options,
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
}

/// Shared application state passed to handlers.
pub struct AppState {
    pub dispatcher: PythonDispatcher,
    pub task_queue: TaskQueue,
    pub download_dir: String,
    pub callback_store: CallbackStateStore,
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
        Command::Search(query) => cmd_search(bot, msg, query, state).await,
        Command::Status => cmd_status(bot, msg, state).await,
        Command::Cancel(task_id) => cmd_cancel(bot, msg, task_id, state).await,
        Command::History => cmd_history(bot, msg).await,
        Command::Ping => cmd_ping(bot, msg, state).await,
        Command::Upcook(content) => cmd_upcook(bot, msg, content, state).await,
    }
}

/// /start - Welcome message
async fn cmd_start(bot: Bot, msg: Message) -> ResponseResult<()> {
    let text = "\
Hermes Download Bot

Send me a YouTube or Telegram link and I'll download it for you.

Commands:
/download <url> - Download audio (default)
/dv <url> - Download video (choose quality)
/da <url> - Download audio (choose quality)
/search <query> - Search YouTube
/status - Check active tasks
/cancel <id> - Cancel a download
/ping - Health check
/help - Show this message

Telegram: Paste t.me links to forward files from channels.
Multiple links = batch forward.

Admin:
/upcook [cookies] - Update cookies.txt

You can also just paste a link directly!";
    bot.send_message(msg.chat.id, text).await?;
    Ok(())
}

/// /help - Show help
async fn cmd_help(bot: Bot, msg: Message) -> ResponseResult<()> {
    cmd_start(bot, msg).await
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
        bot.send_message(msg.chat.id, "Usage: /download <url>").await?;
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
    let kind = if is_playlist { "playlist" } else { "download" };
    let status_msg = bot.send_message(chat_id, format!(
        "Queued {} [{}]\n{}",
        kind, short_id, link.url()
    )).await?;
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
    let kind = kind.to_string();
    tokio::spawn(async move {
        let _ = execute_download_and_send(
            &bot,
            chat_id,
            status_msg_id,
            &short_id,
            &kind,
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
            Ok(_) => {
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

        let mut success = 0usize;
        let mut failed = 0usize;
        let mut last_edit = Instant::now();

        for (i, link) in tg_links.iter().enumerate() {
            match copy_telegram_message(&bot, chat_id, link).await {
                Ok(_) => success += 1,
                Err(e) => {
                    failed += 1;
                    warn!("Telegram forward failed for {}: {}", link.url(), e);
                }
            }

            // Throttle progress edits (every 3 messages or every 3 seconds)
            let done = i + 1;
            if done == total || (done % 3 == 0 && last_edit.elapsed().as_secs() >= 2) {
                let _ = bot.edit_message_text(chat_id, status_id, format!(
                    "Forwarding {}/{}...", done, total
                )).await;
                last_edit = Instant::now();
            }

            // Small delay between copies to avoid rate limiting
            if done < total {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }

        // Final summary
        let summary = if failed == 0 {
            format!("Forwarded {} files", success)
        } else {
            format!("Forwarded {}/{} files ({} failed)", success, total, failed)
        };
        let _ = bot.edit_message_text(chat_id, status_id, summary).await;
    }

    Ok(())
}

/// Copy a single Telegram message from a channel to the user.
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

                // Handle playlist archives
                if let Some(archives) = response.data.get("archives").and_then(|v| v.as_array()) {
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

/// /search <query> - Search YouTube
async fn cmd_search(
    bot: Bot,
    msg: Message,
    query: String,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let query = query.trim().to_string();
    if query.is_empty() {
        bot.send_message(msg.chat.id, "Usage: /search <query>").await?;
        return Ok(());
    }

    let task_id = Uuid::new_v4().to_string();
    let request = search_request(&task_id, &query, 5);

    let searching_msg = bot.send_message(msg.chat.id, format!(
        "Searching: \"{}\"...", query
    )).await?;

    match state.dispatcher.send_and_wait(&request, 30).await {
        Ok(response) => {
            if response.is_error() {
                let err = response.error_message().unwrap_or_else(|| "Search failed".into());
                bot.edit_message_text(msg.chat.id, searching_msg.id, format!(
                    "Search error: {}", err
                )).await?;
            } else {
                let results = response.data.get("results")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                if results.is_empty() {
                    bot.edit_message_text(msg.chat.id, searching_msg.id,
                        format!("No results found for \"{}\".", query)
                    ).await?;
                } else {
                    let mut text = format!("Results for \"{}\":\n\n", query);
                    for (i, result) in results.iter().enumerate() {
                        let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                        let artist = result.get("artist").and_then(|v| v.as_str()).unwrap_or("");
                        let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        if artist.is_empty() {
                            text.push_str(&format!("{}. {}\n{}\n\n", i + 1, title, url));
                        } else {
                            text.push_str(&format!("{}. {} - {}\n{}\n\n", i + 1, artist, title, url));
                        }
                    }

                    let from_cache = response.data.get("from_cache")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if from_cache {
                        text.push_str("(cached)\n");
                    }

                    text.push_str("Send a link to download!");

                    bot.edit_message_text(msg.chat.id, searching_msg.id, text).await?;
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
        bot.send_message(msg.chat.id, "Usage: /cancel <task-id>\nUse /status to see task IDs.").await?;
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
                "Pong!\n\
                 Worker: {}\n\
                 Handlers: {}\n\
                 Queue: {}/{} running",
                version, handlers, stats.running, stats.max_concurrent
            )).await?;
        }
        Err(e) => {
            bot.send_message(msg.chat.id, format!("Worker offline: {}", e)).await?;
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
        bot.send_message(msg.chat.id, "This command is admin-only.").await?;
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
                // YouTube links: download first one
                info!("Auto-detected link: {:?}", first);
                cmd_download(bot, msg, first.url().to_string(), state).await?;
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
