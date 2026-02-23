/// Hermes Download Bot - Main Entry Point
///
/// Telegram bot built with teloxide that orchestrates a Python media worker
/// via IPC for downloading YouTube audio and playlists.
mod commands;
mod callback_state;
mod link_detector;
mod workers;

use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::CallbackQuery;
use tracing::{info, error, warn};

use hermes_shared::task_queue::TaskQueue;
use workers::python_dispatcher::PythonDispatcher;
use callback_state::CallbackStateStore;
use commands::{AppState, Command};

#[tokio::main]
async fn main() {
    // Load .env file
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("hermes_bot=info".parse().unwrap())
                .add_directive("hermes_shared=info".parse().unwrap()),
        )
        .init();

    info!("=== Hermes Download Bot Starting ===");

    // Read configuration from environment
    let bot_token = std::env::var("TELOXIDE_TOKEN")
        .expect("TELOXIDE_TOKEN must be set");
    let download_dir = std::env::var("DOWNLOAD_DIR")
        .unwrap_or_else(|_| "./downloads".to_string());
    let worker_dir = std::env::var("WORKER_DIR")
        .unwrap_or_else(|_| ".".to_string());
    let python_bin = std::env::var("PYTHON_BIN").ok();
    let max_concurrent: usize = std::env::var("MAX_CONCURRENT_TASKS")
        .unwrap_or_else(|_| "3".to_string())
        .parse()
        .unwrap_or(3);

    // Ensure download directory exists
    std::fs::create_dir_all(&download_dir).expect("Failed to create download directory");

    // Initialize Python worker dispatcher
    let dispatcher = PythonDispatcher::new(
        std::path::PathBuf::from(&worker_dir),
        python_bin,
    );

    // Start the Python worker
    if let Err(e) = dispatcher.start().await {
        error!("Failed to start Python worker: {}", e);
        std::process::exit(1);
    }

    info!("Python worker started successfully");

    // Connect to shared database (for web queue polling)
    let database_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "./hermes.db".to_string());
    let database_path = std::path::Path::new(&database_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&database_path));
    // Strip Windows UNC prefix (\\?\) which breaks SQLite URL parsing
    let db_path_str = database_path.display().to_string();
    let db_path_str = db_path_str.strip_prefix(r"\\?\").unwrap_or(&db_path_str).to_string();
    let database_url = format!("sqlite://{}?mode=rwc", db_path_str);
    info!("Database path: {}", db_path_str);
    let db_pool = match hermes_shared::db::create_pool(&database_url).await {
        Ok(pool) => {
            if let Err(e) = hermes_shared::db::run_migrations(&pool).await {
                error!("DB migration error: {}", e);
            }
            info!("Connected to database for web queue polling");
            Some(pool)
        }
        Err(e) => {
            error!("Failed to connect to database (web downloads disabled): {}", e);
            None
        }
    };

    // Initialize task queue
    let task_queue = TaskQueue::new(max_concurrent);

    // Initialize callback state store
    let callback_store = CallbackStateStore::new();

    // Create shared application state
    let state = Arc::new(AppState {
        dispatcher,
        task_queue,
        download_dir: download_dir.clone(),
        callback_store: callback_store.clone(),
        db_pool: db_pool.clone(),
    });

    // Build and start the Telegram bot
    let bot = Bot::new(bot_token);

    // Explicitly delete any existing webhook before polling
    // (prevents 409 Conflict if a webhook was previously set)
    match bot.delete_webhook().send().await {
        Ok(_) => info!("Webhook cleared (ready for polling)"),
        Err(e) => warn!("Failed to delete webhook: {} (continuing anyway)", e),
    }

    // Sync commands with Telegram (enables autocomplete menu)
    use teloxide::utils::command::BotCommands;
    match bot.set_my_commands(Command::bot_commands()).await {
        Ok(_) => info!("Bot commands synced with Telegram"),
        Err(e) => error!("Failed to sync bot commands: {}", e),
    }

    // Notify admin that bot is online
    let admin_chat_id = std::env::var("ADMIN_CHAT_ID").ok()
        .and_then(|s| s.parse::<i64>().ok());
    if let Some(admin_id) = admin_chat_id {
        let db_status = if db_pool.is_some() { "connected" } else { "offline" };
        let msg = format!(
            "Hermes Bot online\nWorker: ready\nDB: {}\nQueue: {}/{} slots",
            db_status, 0, max_concurrent
        );
        match bot.send_message(ChatId(admin_id), msg).await {
            Ok(_) => info!("Admin startup notification sent"),
            Err(e) => warn!("Failed to send admin notification: {}", e),
        }
    }

    info!("Bot initialized, starting dispatcher...");

    // Set up command handler, message handler, and callback query handler
    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint({
                    let state = state.clone();
                    move |bot: Bot, msg: Message, cmd: Command| {
                        let state = state.clone();
                        async move { commands::handle_command(bot, msg, cmd, state).await }
                    }
                }),
        )
        .branch(
            Update::filter_message()
                .endpoint({
                    let state = state.clone();
                    move |bot: Bot, msg: Message| {
                        let state = state.clone();
                        async move { commands::handle_message(bot, msg, state).await }
                    }
                }),
        )
        .branch(
            Update::filter_callback_query()
                .endpoint({
                    let state = state.clone();
                    move |bot: Bot, q: CallbackQuery| {
                        let state = state.clone();
                        async move { commands::handle_callback_query(bot, q, state).await }
                    }
                }),
        );

    // Spawn background cleanup task for expired callback states
    let cleanup_store = callback_store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            cleanup_store.cleanup_expired(300).await; // 5 min TTL
        }
    });

    // Spawn web download queue poller
    if let Some(pool) = db_pool {
        let web_state = state.clone();
        let web_bot = bot.clone();
        tokio::spawn(async move {
            use hermes_shared::ipc_protocol::download_request;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                match hermes_shared::db::claim_web_queued_tasks(&pool).await {
                    Ok(tasks) if !tasks.is_empty() => {
                        for task in tasks {
                            let chat_id = ChatId(task.chat_id);
                            let task_id = task.id.clone();
                            let short_id = task_id.chars().take(8).collect::<String>();
                            let url = task.url.clone();
                            let label = task.label.clone().unwrap_or_else(|| "audio".to_string());
                            let is_video = label == "video";
                            let mode = if is_video {
                                crate::callback_state::DownloadMode::Video
                            } else {
                                crate::callback_state::DownloadMode::Audio
                            };

                            info!("Processing web-queued task {} for chat {}", short_id, task.chat_id);

                            // Notify user
                            let notify_result = web_bot.send_message(
                                chat_id,
                                format!("Web download started [{}]\n{}", short_id, url),
                            ).await;

                            let status_msg_id = match notify_result {
                                Ok(msg) => msg.id,
                                Err(e) => {
                                    error!("Failed to notify user about web task: {}", e);
                                    continue;
                                }
                            };

                            // Build output dir and IPC request
                            let out_dir = commands::task_output_dir(
                                &web_state.download_dir, task.chat_id, &task_id,
                            );
                            let request = download_request(
                                &task_id, &url, !is_video, &out_dir,
                            );

                            // Enqueue in task queue
                            web_state.task_queue.enqueue(&task_id, task.chat_id, "youtube_dl").await;

                            // Execute download in background
                            let bot_clone = web_bot.clone();
                            let state_clone = web_state.clone();
                            tokio::spawn(async move {
                                let _ = commands::execute_download_and_send(
                                    &bot_clone, chat_id, status_msg_id,
                                    &short_id, &label, &task_id,
                                    &request, mode, &state_clone,
                                ).await;
                            });
                        }
                    }
                    Ok(_) => {} // No tasks
                    Err(e) => {
                        tracing::warn!("Web queue poll error: {}", e);
                    }
                }
            }
        });
        info!("Web download queue poller started");
    }

    // Run the bot
    Dispatcher::builder(bot, handler)
        .default_handler(|upd| async move {
            warn!("Unhandled update: {:?}", upd.kind);
        })
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    // Cleanup on shutdown
    info!("Bot shutting down...");
    if let Err(e) = state.dispatcher.stop().await {
        error!("Error stopping worker: {}", e);
    }
    info!("Hermes Download Bot stopped.");
}
