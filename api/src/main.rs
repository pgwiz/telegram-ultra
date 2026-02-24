/// Hermes API Server
///
/// REST API for the Hermes Download Nexus web dashboard.
/// Provides OTP authentication, task management, and admin endpoints.
mod auth;
mod routes;

use axum::routing::{delete, get, post, put};
use axum::Router;
use sqlx::sqlite::SqlitePool;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

/// Shared application state for all API handlers.
pub struct AppState {
    pub pool: SqlitePool,
    pub bot_token: String,
    pub jwt_secret: String,
    pub admin_chat_id: i64,
    pub session_ttl: i64,
    pub download_dir: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env
    dotenvy::dotenv().ok();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hermes_api=info,tower_http=info".into()),
        )
        .init();

    // Config
    let database_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "./hermes.db".to_string());
    let database_path = std::path::Path::new(&database_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&database_path));
    // Strip Windows UNC prefix (\\?\) which breaks SQLite URL parsing
    let db_path_str = database_path.display().to_string();
    let db_path_str = db_path_str.strip_prefix(r"\\?\").unwrap_or(&db_path_str).to_string();
    info!("Database path: {}", db_path_str);
    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .or_else(|_| std::env::var("TELOXIDE_TOKEN"))
        .expect("TELEGRAM_BOT_TOKEN or TELOXIDE_TOKEN must be set");
    let jwt_secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    let admin_chat_id: i64 = std::env::var("ADMIN_CHAT_ID")
        .expect("ADMIN_CHAT_ID must be set")
        .parse()
        .expect("ADMIN_CHAT_ID must be a number");
    let session_ttl: i64 = std::env::var("SESSION_TTL_SECS")
        .unwrap_or_else(|_| "600".to_string())
        .parse()
        .unwrap_or(600);
    let api_host = std::env::var("API_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let api_port: u16 = std::env::var("API_PORT")
        .unwrap_or_else(|_| "8081".to_string())
        .parse()
        .unwrap_or(8081);
    let cleanup_interval: u64 = std::env::var("SESSION_CLEANUP_INTERVAL")
        .unwrap_or_else(|_| "300".to_string())
        .parse()
        .unwrap_or(300);
    let download_dir = std::env::var("DOWNLOAD_DIR")
        .unwrap_or_else(|_| "./downloads".to_string());

    // Database
    let database_url = format!("sqlite://{}?mode=rwc", db_path_str);
    let pool = hermes_shared::db::create_pool(&database_url).await?;
    hermes_shared::db::run_migrations(&pool).await?;

    // App state
    let state = Arc::new(AppState {
        pool: pool.clone(),
        bot_token,
        jwt_secret,
        admin_chat_id,
        session_ttl,
        download_dir,
    });

    // Background session cleanup
    let cleanup_pool = pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(cleanup_interval));
        loop {
            interval.tick().await;
            match hermes_shared::db::cleanup_expired_sessions(&cleanup_pool).await {
                Ok(n) if n > 0 => info!("Cleaned up {} expired sessions", n),
                Err(e) => tracing::warn!("Session cleanup error: {}", e),
                _ => {}
            }
        }
    });

    // CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Router
    let app = Router::new()
        // Auth routes (no auth required)
        .route("/api/auth/request-otp", post(routes::request_otp))
        .route("/api/auth/verify-otp", post(routes::verify_otp))
        .route("/api/auth/allow-status", get(routes::allow_status))
        .route("/api/auth/quick-login", post(routes::quick_login))
        .route("/api/bot-info", get(routes::bot_info))
        // Auth-protected routes
        .route("/api/auth/logout", delete(routes::logout))
        .route("/api/download", post(routes::submit_download))
        .route("/api/download/batch", post(routes::batch_download))
        .route("/api/tasks", get(routes::list_tasks))
        .route("/api/tasks/:id", get(routes::get_task))
        .route("/api/tasks/:id", delete(routes::cancel_task))
        .route("/api/tasks/:id", put(routes::update_task))
        .route("/api/tasks/:id/retry", post(routes::retry_task))
        .route("/api/files", get(routes::list_files))
        .route("/api/files/history", delete(routes::clear_history))
        .route("/api/files/:id/download", get(routes::download_file))
        .route("/api/files/:id", delete(routes::delete_file))
        // Admin routes
        .route("/api/admin/stats", get(routes::admin_stats))
        .route("/api/admin/users", get(routes::admin_users))
        .route("/api/admin/logs", get(routes::admin_logs))
        .layer(cors)
        .layer(axum::Extension(state.clone()))
        .with_state(state);

    // Bind
    let addr = format!("{}:{}", api_host, api_port);
    info!("Hermes API listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
