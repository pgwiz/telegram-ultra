/// API route handlers for Hermes Dashboard.
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::io::ReaderStream;
use tracing::{info, warn, error};

use hermes_shared::db;

use crate::auth;
use crate::AppState;

// ====== REQUEST / RESPONSE TYPES ======

#[derive(Deserialize)]
pub struct RequestOtpBody {
    pub chat_id: i64,
}

#[derive(Deserialize)]
pub struct VerifyOtpBody {
    pub chat_id: i64,
    pub otp: String,
}

#[derive(Serialize)]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Serialize)]
pub struct BotInfoResponse {
    pub username: String,
    pub first_name: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub expires_in: i64,
    pub chat_id: i64,
}

#[derive(Deserialize)]
pub struct TasksQuery {
    pub status: Option<String>,
}

#[derive(Deserialize)]
pub struct DownloadBody {
    pub url: String,
    #[serde(default = "default_download_type")]
    pub download_type: String,
}

fn default_download_type() -> String {
    "audio".to_string()
}

#[derive(Deserialize)]
pub struct BatchDownloadBody {
    pub urls: Vec<String>,
    #[serde(default = "default_download_type")]
    pub download_type: String,
}

#[derive(Deserialize)]
pub struct UpdateTaskBody {
    pub url: Option<String>,
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    /// Comma-separated service names: hermes-bot,hermes-api,hermes-ui
    pub service: Option<String>,
    /// Number of lines (default 200, max 1000)
    pub lines: Option<u32>,
    /// Time filter: "1h", "6h", "24h", "7d"
    pub since: Option<String>,
    /// Minimum log level: "error", "warning", "info", "debug"
    pub level: Option<String>,
}

// ====== AUTH ROUTES ======

/// POST /api/auth/request-otp
pub async fn request_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RequestOtpBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<MessageResponse>)> {
    let chat_id = body.chat_id;

    // Ensure user exists in DB (sessions have FK to users)
    let _ = db::upsert_user(&state.pool, chat_id, None).await;

    // Rate limit: max 3 OTP requests per hour
    let recent = db::count_recent_otp_requests(&state.pool, chat_id, 3600)
        .await
        .unwrap_or(0);

    if recent >= 3 {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(MessageResponse {
                message: "Too many OTP requests. Try again later.".to_string(),
            }),
        ));
    }

    // Generate OTP
    let otp = auth::generate_otp();

    // Store in DB
    if let Err(e) = db::create_otp_session(&state.pool, chat_id, &otp).await {
        warn!("Failed to create OTP session: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MessageResponse {
                message: "Failed to create OTP session".to_string(),
            }),
        ));
    }

    // Send via Telegram
    if let Err(e) = auth::send_telegram_otp(&state.bot_token, chat_id, &otp).await {
        warn!("Failed to send OTP: {}", e);
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(MessageResponse {
                message: format!("Failed to send OTP via Telegram: {}", e),
            }),
        ));
    }

    info!("OTP requested for chat_id {}", chat_id);
    Ok(Json(MessageResponse {
        message: "OTP sent to your Telegram. Check your messages.".to_string(),
    }))
}

/// POST /api/auth/verify-otp
pub async fn verify_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyOtpBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<MessageResponse>)> {
    let chat_id = body.chat_id;
    let otp = body.otp.trim().to_string();

    if otp.len() != 6 || otp.parse::<u32>().is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: "Invalid OTP format. Must be 6 digits.".to_string(),
            }),
        ));
    }

    // Verify OTP
    let valid = db::verify_otp_session(&state.pool, chat_id, &otp)
        .await
        .unwrap_or(false);

    if !valid {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(MessageResponse {
                message: "Invalid or expired OTP code.".to_string(),
            }),
        ));
    }

    // Ensure user exists
    let _ = db::upsert_user(&state.pool, chat_id, None).await;

    // Create JWT
    let token = auth::create_jwt(chat_id, &state.jwt_secret, state.session_ttl).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MessageResponse {
                message: format!("Failed to create session: {}", e),
            }),
        )
    })?;

    // Store session in DB
    if let Err(e) = db::create_jwt_session(&state.pool, chat_id, &token, state.session_ttl).await {
        warn!("Failed to store JWT session: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MessageResponse {
                message: "Failed to create session".to_string(),
            }),
        ));
    }

    info!("User {} authenticated via OTP", chat_id);

    // Set cookie header
    let cookie = format!(
        "hermes_token={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        token, state.session_ttl
    );

    let mut headers = HeaderMap::new();
    headers.insert("Set-Cookie", cookie.parse().unwrap());

    Ok((
        headers,
        Json(AuthResponse {
            token,
            expires_in: state.session_ttl,
            chat_id,
        }),
    ))
}

/// DELETE /api/auth/logout
pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user = auth::authenticate(&headers, &state).await;

    if let Ok(u) = &user {
        let _ = db::delete_session(&state.pool, &u.token).await;
        info!("User {} logged out", u.chat_id);
    }

    // Clear cookie
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        "Set-Cookie",
        "hermes_token=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"
            .parse()
            .unwrap(),
    );

    (
        resp_headers,
        Json(MessageResponse {
            message: "Logged out".to_string(),
        }),
    )
}

/// GET /api/bot-info - Public endpoint returning bot username and display name
pub async fn bot_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<BotInfoResponse>, (StatusCode, Json<MessageResponse>)> {
    let url = format!("https://api.telegram.org/bot{}/getMe", state.bot_token);

    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(MessageResponse { message: format!("Telegram API unreachable: {}", e) }),
    ))?;

    let json: serde_json::Value = resp.json().await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(MessageResponse { message: e.to_string() }),
    ))?;

    let result = &json["result"];
    Ok(Json(BotInfoResponse {
        username:   result["username"].as_str().unwrap_or("").to_string(),
        first_name: result["first_name"].as_str().unwrap_or("Hermes Bot").to_string(),
    }))
}

/// GET /api/auth/allow-status — public, returns whether an OTP-free login window is active
pub async fn allow_status(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    match hermes_shared::db::get_allow_window_remaining(&state.pool).await {
        Ok(Some(secs)) => Json(serde_json::json!({ "active": true, "remaining_secs": secs })),
        _ => Json(serde_json::json!({ "active": false, "remaining_secs": 0 })),
    }
}

#[derive(Deserialize)]
pub struct QuickLoginBody {
    pub chat_id: i64,
}

/// POST /api/auth/quick-login — OTP-free login during an active allow window
pub async fn quick_login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<QuickLoginBody>,
) -> Result<(StatusCode, Json<AuthResponse>), (StatusCode, Json<MessageResponse>)> {
    let remaining = hermes_shared::db::get_allow_window_remaining(&state.pool)
        .await
        .unwrap_or(None);

    if remaining.is_none() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(MessageResponse { message: "No active login window.".to_string() }),
        ));
    }

    let chat_id = body.chat_id;
    let _ = hermes_shared::db::upsert_user(&state.pool, chat_id, None).await;

    let token = auth::create_jwt(chat_id, &state.jwt_secret, state.session_ttl)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(MessageResponse { message: e })))?;

    hermes_shared::db::create_jwt_session(&state.pool, chat_id, &token, state.session_ttl)
        .await
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MessageResponse { message: e.to_string() }),
        ))?;

    Ok((
        StatusCode::OK,
        Json(AuthResponse { token, expires_in: state.session_ttl, chat_id }),
    ))
}

#[derive(Deserialize)]
pub struct TokenLoginBody {
    pub token: String,
}

/// POST /api/auth/token-login — Login via a bypass token (from /allow botp)
pub async fn token_login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TokenLoginBody>,
) -> Result<(StatusCode, Json<AuthResponse>), (StatusCode, Json<MessageResponse>)> {
    let bypass_token = body.token.trim();
    if bypass_token.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MessageResponse { message: "Token is required".to_string() }),
        ));
    }

    let chat_id = match hermes_shared::db::validate_bypass_token(&state.pool, bypass_token).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(MessageResponse { message: "Invalid or expired token".to_string() }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(MessageResponse { message: format!("Token validation error: {}", e) }),
            ));
        }
    };

    // Ensure user exists
    let _ = hermes_shared::db::upsert_user(&state.pool, chat_id, None).await;

    let jwt = auth::create_jwt(chat_id, &state.jwt_secret, state.session_ttl)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(MessageResponse { message: e })))?;

    hermes_shared::db::create_jwt_session(&state.pool, chat_id, &jwt, state.session_ttl)
        .await
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MessageResponse { message: e.to_string() }),
        ))?;

    info!("Token login successful for chat_id={}", chat_id);

    Ok((
        StatusCode::OK,
        Json(AuthResponse { token: jwt, expires_in: state.session_ttl, chat_id }),
    ))
}

// ====== DOWNLOAD ROUTE ======

/// POST /api/download - Queue a download from the web dashboard
pub async fn submit_download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DownloadBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    let url = body.url.trim().to_string();
    if url.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "URL is required" })),
        ));
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let task_type = "youtube_dl";
    let label = Some(body.download_type.as_str());

    match db::create_web_task(&state.pool, &task_id, user.chat_id, &url, task_type, label).await {
        Ok(_) => {
            info!("Web download queued: task={} chat_id={} url={}", task_id, user.chat_id, url);
            Ok((
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "task_id": task_id,
                    "message": "Download queued",
                    "status": "web_queued"
                })),
            ))
        }
        Err(e) => {
            warn!("Failed to create web task: {}", e);
            Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to queue: {}", e) })),
            ))
        }
    }
}

/// POST /api/download/batch - Queue multiple downloads at once
pub async fn batch_download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<BatchDownloadBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    let urls: Vec<String> = body.urls.iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();

    if urls.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No valid URLs provided" })),
        ));
    }

    if urls.len() > 20 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Maximum 20 URLs per batch" })),
        ));
    }

    let task_type = "youtube_dl";
    let label = Some(body.download_type.as_str());
    let mut created = Vec::new();
    let mut errors = Vec::new();

    for url in &urls {
        let task_id = uuid::Uuid::new_v4().to_string();
        match db::create_web_task(&state.pool, &task_id, user.chat_id, url, task_type, label).await {
            Ok(_) => {
                info!("Batch download queued: task={} url={}", task_id, url);
                created.push(serde_json::json!({ "task_id": task_id, "url": url }));
            }
            Err(e) => {
                warn!("Batch task failed: url={} error={}", url, e);
                errors.push(serde_json::json!({ "url": url, "error": format!("{}", e) }));
            }
        }
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "created": created.len(),
            "failed": errors.len(),
            "tasks": created,
            "errors": errors,
        })),
    ))
}

// ====== TASK ROUTES ======

/// GET /api/tasks
pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TasksQuery>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    match db::get_user_tasks_by_status(&state.pool, user.chat_id, query.status.as_deref()).await {
        Ok(tasks) => Ok((StatusCode::OK, Json(serde_json::json!({ "tasks": tasks })))),
        Err(e) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to fetch tasks: {}", e) })),
        )),
    }
}

/// GET /api/tasks/:id
pub async fn get_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    match db::get_task_by_id(&state.pool, &task_id).await {
        Ok(Some(task)) => {
            if task.chat_id != user.chat_id {
                return Ok((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "error": "Access denied" })),
                ));
            }
            Ok((StatusCode::OK, Json(serde_json::json!({ "task": task }))))
        }
        Ok(None) => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Task not found" })),
        )),
        Err(e) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{}", e) })),
        )),
    }
}

/// DELETE /api/tasks/:id
pub async fn cancel_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    // Verify ownership
    match db::get_task_by_id(&state.pool, &task_id).await {
        Ok(Some(task)) => {
            if task.chat_id != user.chat_id {
                return Ok((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "error": "Access denied" })),
                ));
            }
        }
        Ok(None) => {
            return Ok((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Task not found" })),
            ));
        }
        Err(e) => {
            return Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("{}", e) })),
            ));
        }
    }

    match db::cancel_task(&state.pool, &task_id).await {
        Ok(true) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "message": "Task cancelled" })),
        )),
        Ok(false) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "Task cannot be cancelled (already finished)" })),
        )),
        Err(e) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{}", e) })),
        )),
    }
}

/// POST /api/tasks/:id/retry - Re-queue a failed/cancelled task
pub async fn retry_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    // Verify ownership
    match db::get_task_by_id(&state.pool, &task_id).await {
        Ok(Some(task)) => {
            if task.chat_id != user.chat_id {
                return Ok((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "Access denied" }))));
            }
        }
        Ok(None) => return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Task not found" })))),
        Err(e) => return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{}", e) })))),
    }

    match db::retry_task(&state.pool, &task_id).await {
        Ok(true) => {
            info!("Task {} retried by user {}", task_id, user.chat_id);
            Ok((StatusCode::OK, Json(serde_json::json!({ "message": "Task re-queued" }))))
        }
        Ok(false) => Ok((StatusCode::CONFLICT, Json(serde_json::json!({ "error": "Task cannot be retried (must be cancelled, error, or done)" })))),
        Err(e) => Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{}", e) })))),
    }
}

/// PUT /api/tasks/:id - Update a queued task's URL or label
pub async fn update_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Json(body): Json<UpdateTaskBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    // Verify ownership
    match db::get_task_by_id(&state.pool, &task_id).await {
        Ok(Some(task)) => {
            if task.chat_id != user.chat_id {
                return Ok((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "Access denied" }))));
            }
        }
        Ok(None) => return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Task not found" })))),
        Err(e) => return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{}", e) })))),
    }

    match db::update_task(&state.pool, &task_id, body.url.as_deref(), body.label.as_deref()).await {
        Ok(true) => Ok((StatusCode::OK, Json(serde_json::json!({ "message": "Task updated" })))),
        Ok(false) => Ok((StatusCode::CONFLICT, Json(serde_json::json!({ "error": "Task cannot be edited (must be queued)" })))),
        Err(e) => Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{}", e) })))),
    }
}

// ====== FILES ROUTES ======

/// GET /api/files
pub async fn list_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    match db::get_user_completed_files(&state.pool, user.chat_id).await {
        Ok(files) => Ok((StatusCode::OK, Json(serde_json::json!({ "files": files })))),
        Err(e) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{}", e) })),
        )),
    }
}

/// GET /api/files/:id/download - Serve a completed download file
pub async fn download_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    let task = db::get_task_by_id(&state.pool, &task_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(auth::ErrorBody { error: format!("{}", e) })))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(auth::ErrorBody { error: "Task not found".into() })))?;

    if task.chat_id != user.chat_id {
        return Err((StatusCode::FORBIDDEN, Json(auth::ErrorBody { error: "Access denied".into() })));
    }

    let file_path = task.file_path
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(auth::ErrorBody { error: "No file for this task".into() })))?;

    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err((StatusCode::NOT_FOUND, Json(auth::ErrorBody { error: "File not found on disk".into() })));
    }

    let filename = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");

    let file = tokio::fs::File::open(&file_path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(auth::ErrorBody { error: format!("Cannot open file: {}", e) })))?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let content_type = if filename.ends_with(".mp4") || filename.ends_with(".mkv") || filename.ends_with(".webm") {
        "video/mp4"
    } else if filename.ends_with(".mp3") {
        "audio/mpeg"
    } else if filename.ends_with(".m4a") || filename.ends_with(".aac") {
        "audio/mp4"
    } else if filename.ends_with(".opus") || filename.ends_with(".ogg") {
        "audio/ogg"
    } else if filename.ends_with(".flac") {
        "audio/flac"
    } else if filename.ends_with(".wav") {
        "audio/wav"
    } else {
        "application/octet-stream"
    };

    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', "_"));

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    ))
}

/// GET /api/dl/:task_id - Public (no auth) file download via temporary token.
///
/// The token is the task_id itself; a short-lived entry is created in the
/// sessions table by the bot when a file is too large to send via Telegram.
pub async fn public_download_file(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    // Validate token
    let _chat_id = hermes_shared::db::validate_file_download_token(&state.pool, &task_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;  // 404 = expired or never created

    let task = db::get_task_by_id(&state.pool, &task_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let file_path = task.file_path.ok_or(StatusCode::NOT_FOUND)?;

    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    let filename = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");

    let file = tokio::fs::File::open(&file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let content_type = if filename.ends_with(".mp4") || filename.ends_with(".mkv") || filename.ends_with(".webm") {
        "video/mp4"
    } else if filename.ends_with(".mp3") {
        "audio/mpeg"
    } else if filename.ends_with(".m4a") || filename.ends_with(".aac") {
        "audio/mp4"
    } else {
        "application/octet-stream"
    };

    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', "_"));

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    ))
}

/// DELETE /api/files/:id - Delete a completed download file from disk and DB
pub async fn delete_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    let task = match db::get_task_by_id(&state.pool, &task_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Task not found" })))),
        Err(e) => return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{}", e) })))),
    };

    if task.chat_id != user.chat_id {
        return Ok((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "Access denied" }))));
    }

    // Delete file from disk
    if let Some(ref file_path) = task.file_path {
        let path = std::path::Path::new(file_path);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(path) {
                warn!("Failed to delete file {}: {}", file_path, e);
            }
        }
        // Also try to clean up the empty task directory
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent); // only succeeds if empty
        }
    }

    // Delete task from DB
    match db::delete_task(&state.pool, &task_id).await {
        Ok(_) => {
            info!("File deleted: task={} by user={}", task_id, user.chat_id);
            Ok((StatusCode::OK, Json(serde_json::json!({ "message": "File deleted" }))))
        }
        Err(e) => Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{}", e) })))),
    }
}

/// DELETE /api/files/history - Clear all completed download history and files
pub async fn clear_history(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    match db::clear_user_history(&state.pool, user.chat_id).await {
        Ok(file_paths) => {
            let mut deleted_files = 0;
            for path_opt in &file_paths {
                if let Some(file_path) = path_opt {
                    let path = std::path::Path::new(file_path);
                    if path.exists() {
                        if std::fs::remove_file(path).is_ok() {
                            deleted_files += 1;
                        }
                        // Try to clean up empty parent dir
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::remove_dir(parent);
                        }
                    }
                }
            }
            info!("History cleared: user={}, records={}, files_deleted={}", user.chat_id, file_paths.len(), deleted_files);
            Ok((StatusCode::OK, Json(serde_json::json!({
                "message": format!("Cleared {} records, deleted {} files", file_paths.len(), deleted_files)
            }))))
        }
        Err(e) => Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{}", e) })))),
    }
}

// ====== ADMIN ROUTES ======

/// GET /api/admin/stats
pub async fn admin_stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let _admin = auth::authenticate_admin(&headers, &state).await?;

    match db::get_system_stats(&state.pool).await {
        Ok(stats) => Ok((StatusCode::OK, Json(serde_json::json!({ "stats": stats })))),
        Err(e) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{}", e) })),
        )),
    }
}

/// GET /api/admin/users
pub async fn admin_users(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let _admin = auth::authenticate_admin(&headers, &state).await?;

    match db::get_all_users(&state.pool).await {
        Ok(users) => Ok((StatusCode::OK, Json(serde_json::json!({ "users": users })))),
        Err(e) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{}", e) })),
        )),
    }
}

/// GET /api/admin/logs - Fetch recent system logs from journald
pub async fn admin_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let _admin = auth::authenticate_admin(&headers, &state).await?;

    // Validate and parse service names (whitelist only known services)
    let allowed_services = ["hermes-bot", "hermes-api", "hermes-ui"];
    let services: Vec<&str> = match &query.service {
        Some(s) => s.split(',')
            .map(|v| v.trim())
            .filter(|v| allowed_services.contains(v))
            .collect(),
        None => allowed_services.to_vec(),
    };

    if services.is_empty() {
        return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "No valid service names. Use: hermes-bot, hermes-api, hermes-ui"
        }))));
    }

    // Clamp lines
    let lines = query.lines.unwrap_or(200).min(1000);

    // Validate since parameter
    let since = match query.since.as_deref() {
        Some("1h") => Some("1 hour ago"),
        Some("6h") => Some("6 hours ago"),
        Some("24h") => Some("24 hours ago"),
        Some("7d") => Some("7 days ago"),
        Some(_) => {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "Invalid 'since' value. Use: 1h, 6h, 24h, 7d"
            }))));
        }
        None => None,
    };

    // Validate level filter (applied post-fetch since journald --priority
    // doesn't work for tracing-based logs — all entries share the same
    // systemd priority regardless of tracing level).
    let level_filter: Option<&str> = match query.level.as_deref() {
        Some("error") | Some("err") => Some("error"),
        Some("warning") | Some("warn") => Some("warn"),
        Some("info") => Some("info"),
        Some("debug") => Some("debug"),
        Some(_) => {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "Invalid 'level' value. Use: error, warning, info, debug"
            }))));
        }
        None => None,
    };

    // When filtering by level, fetch extra lines since we filter after parsing.
    let fetch_lines = if level_filter.is_some() { lines * 5 } else { lines };

    // Build journalctl command
    let mut cmd = tokio::process::Command::new("journalctl");

    // Add unit filters
    for svc in &services {
        cmd.arg("-u").arg(svc);
    }

    cmd.arg("--no-pager");
    cmd.arg("--output=json");
    cmd.arg(format!("--lines={}", fetch_lines));
    cmd.arg("--reverse"); // newest first

    if let Some(s) = since {
        cmd.arg(format!("--since={}", s));
    }

    // Execute
    match cmd.output().await {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!("journalctl failed: {}", stderr);
                return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                    "error": format!("journalctl failed: {}", stderr.chars().take(200).collect::<String>())
                }))));
            }

            let stdout = String::from_utf8_lossy(&output.stdout);

            // Parse each JSON line from journalctl --output=json
            let mut logs: Vec<serde_json::Value> = Vec::new();
            for line in stdout.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(entry) => {
                        // Extract relevant fields
                        let timestamp = entry.get("__REALTIME_TIMESTAMP")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|us| {
                                // Convert microseconds to ISO 8601
                                let secs = (us / 1_000_000) as i64;
                                let nanos = ((us % 1_000_000) * 1000) as u32;
                                chrono::DateTime::from_timestamp(secs, nanos)
                                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default();

                        let service = entry.get("SYSLOG_IDENTIFIER")
                            .or_else(|| entry.get("_SYSTEMD_UNIT"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        // journalctl --output=json returns MESSAGE as a string normally,
                        // but encodes it as a u8 byte array when the message contains
                        // non-ASCII characters (emoji, escape codes, etc).
                        let message = {
                            let msg_val = entry.get("MESSAGE");
                            if let Some(s) = msg_val.and_then(|v| v.as_str()) {
                                s.to_string()
                            } else if let Some(arr) = msg_val.and_then(|v| v.as_array()) {
                                let bytes: Vec<u8> = arr.iter()
                                    .filter_map(|b| b.as_u64().map(|n| n as u8))
                                    .collect();
                                String::from_utf8_lossy(&bytes).into_owned()
                            } else {
                                String::new()
                            }
                        };

                        // Parse log level from message text.
                        // Rust tracing:   "2026-02-23T11:59:08Z  INFO module: msg"
                        // Python logging: "2026-02-23 11:59:08,123 - name - INFO - msg"
                        // Journald priority is the same for all entries from one process,
                        // so we extract level from message content instead.
                        let level = if message.contains("  ERROR ") || message.starts_with("ERROR ")
                            || message.contains(" - ERROR - ")
                        {
                            "error"
                        } else if message.contains("  WARN ") || message.starts_with("WARN ")
                            || message.contains(" - WARNING - ")
                        {
                            "warn"
                        } else if message.contains("  DEBUG ") || message.starts_with("DEBUG ")
                            || message.contains(" - DEBUG - ")
                        {
                            "debug"
                        } else if message.contains("  TRACE ") || message.starts_with("TRACE ") {
                            "debug"
                        } else if message.contains("  INFO ") || message.starts_with("INFO ")
                            || message.contains(" - INFO - ")
                        {
                            "info"
                        } else {
                            // Fall back to journald priority
                            let priority_num = entry.get("PRIORITY")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse::<u8>().ok())
                                .unwrap_or(6);
                            match priority_num {
                                0..=3 => "error",
                                4 => "warn",
                                5..=6 => "info",
                                _ => "debug",
                            }
                        };

                        // Post-fetch level filter: skip entries below requested level
                        let dominated = match level_filter {
                            Some("error") => level != "error",
                            Some("warn") => level == "info" || level == "debug",
                            Some("info") => level == "debug",
                            _ => false,
                        };
                        if dominated {
                            continue;
                        }

                        logs.push(serde_json::json!({
                            "timestamp": timestamp,
                            "service": service,
                            "level": level,
                            "message": message,
                        }));

                        // Stop once we have enough entries
                        if logs.len() >= lines as usize {
                            break;
                        }
                    }
                    Err(_) => {
                        // Skip malformed lines
                    }
                }
            }

            Ok((StatusCode::OK, Json(serde_json::json!({
                "logs": logs,
                "count": logs.len(),
                "services": services,
            }))))
        }
        Err(e) => {
            error!("Failed to execute journalctl: {}", e);
            Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": format!("Cannot read logs: {}", e)
            }))))
        }
    }
}

// ====== ADMIN SETTINGS ======

/// Default settings with descriptions.
fn default_settings() -> serde_json::Value {
    serde_json::json!({
        "max_concurrent_tasks": { "value": "3", "type": "number", "min": 1, "max": 10, "description": "Maximum simultaneous downloads" },
        "queue_mode": { "value": "parallel", "type": "select", "options": ["parallel", "sequential"], "description": "Download queue mode" },
        "rate_limit.search": { "value": "60", "type": "number", "min": 1, "max": 1000, "description": "Search requests per hour per user" },
        "rate_limit.download": { "value": "20", "type": "number", "min": 1, "max": 500, "description": "Downloads per hour per user" },
        "rate_limit.playlist": { "value": "10", "type": "number", "min": 1, "max": 100, "description": "Playlist downloads per hour per user" },
    })
}

/// GET /api/admin/settings
pub async fn admin_get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let _admin = auth::authenticate_admin(&headers, &state).await?;

    let defaults = default_settings();
    let defaults_obj = defaults.as_object().unwrap();

    // Build response with DB overrides merged into defaults
    let mut settings = serde_json::Map::new();
    match db::get_all_config(&state.pool).await {
        Ok(pairs) => {
            let db_map: std::collections::HashMap<String, String> =
                pairs.into_iter().collect();

            for (key, meta) in defaults_obj {
                let mut entry = meta.clone();
                if let Some(db_val) = db_map.get(key) {
                    entry["value"] = serde_json::Value::String(db_val.clone());
                }
                settings.insert(key.clone(), entry);
            }
        }
        Err(e) => {
            // Return defaults on DB error
            tracing::warn!("Failed to read config from DB: {}", e);
            for (key, meta) in defaults_obj {
                settings.insert(key.clone(), meta.clone());
            }
        }
    }

    Ok((StatusCode::OK, Json(serde_json::json!({ "settings": settings }))))
}

/// PUT /api/admin/settings - Update settings
pub async fn admin_update_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let _admin = auth::authenticate_admin(&headers, &state).await?;

    let defaults = default_settings();
    let defaults_obj = defaults.as_object().unwrap();
    let updates = match body.get("settings").and_then(|v| v.as_object()) {
        Some(obj) => obj,
        None => {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "Missing 'settings' object in request body"
            }))));
        }
    };

    let mut saved = 0u32;
    for (key, value) in updates {
        // Only allow known keys
        let meta = match defaults_obj.get(key) {
            Some(m) => m,
            None => continue,
        };

        let val_str = match value.as_str() {
            Some(s) => s.to_string(),
            None => value.to_string().trim_matches('"').to_string(),
        };

        // Validate based on type
        let setting_type = meta.get("type").and_then(|v| v.as_str()).unwrap_or("text");
        match setting_type {
            "number" => {
                let num: i64 = match val_str.parse() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let min = meta.get("min").and_then(|v| v.as_i64()).unwrap_or(0);
                let max = meta.get("max").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
                let clamped = num.clamp(min, max);
                if let Err(e) = db::set_config(&state.pool, key, &clamped.to_string()).await {
                    tracing::warn!("Failed to set config {}: {}", key, e);
                    continue;
                }
            }
            "select" => {
                let options = meta.get("options")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                if !options.contains(&val_str.as_str()) {
                    continue;
                }
                if let Err(e) = db::set_config(&state.pool, key, &val_str).await {
                    tracing::warn!("Failed to set config {}: {}", key, e);
                    continue;
                }
            }
            _ => {
                if let Err(e) = db::set_config(&state.pool, key, &val_str).await {
                    tracing::warn!("Failed to set config {}: {}", key, e);
                    continue;
                }
            }
        }
        saved += 1;
    }

    Ok((StatusCode::OK, Json(serde_json::json!({
        "message": format!("Saved {} setting(s). Queue/concurrency changes take effect on bot restart.", saved),
        "saved": saved,
    }))))
}

// ====== USER PREFERENCES ======

/// GET /api/user/preferences
pub async fn get_user_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    let prefs = db::get_user_preferences(&state.pool, user.chat_id).await;
    Ok((StatusCode::OK, Json(serde_json::json!({
        "preferences": prefs
    }))))
}

/// PUT /api/user/preferences
pub async fn update_user_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<auth::ErrorBody>)> {
    let user = auth::authenticate(&headers, &state).await?;

    let obj = match body.get("preferences").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "Missing 'preferences' object in request body"
            }))));
        }
    };

    // Start with current preferences as base
    let mut prefs = db::get_user_preferences(&state.pool, user.chat_id).await;

    // Validate and apply each field
    if let Some(v) = obj.get("audio_format").and_then(|v| v.as_str()) {
        if ["mp3", "m4a", "opus", "flac"].contains(&v) {
            prefs.audio_format = v.to_string();
        } else {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "audio_format must be one of: mp3, m4a, opus, flac"
            }))));
        }
    }

    if let Some(v) = obj.get("audio_quality").and_then(|v| v.as_str()) {
        if ["0", "128", "192", "256", "320"].contains(&v) {
            prefs.audio_quality = v.to_string();
        } else {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "audio_quality must be one of: 0, 128, 192, 256, 320"
            }))));
        }
    }

    if let Some(v) = obj.get("default_mode").and_then(|v| v.as_str()) {
        if ["audio", "video"].contains(&v) {
            prefs.default_mode = v.to_string();
        } else {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "default_mode must be one of: audio, video"
            }))));
        }
    }

    if let Some(v) = obj.get("dedup_enabled") {
        if let Some(b) = v.as_bool() {
            prefs.dedup_enabled = b;
        } else {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "dedup_enabled must be a boolean"
            }))));
        }
    }

    if let Some(v) = obj.get("video_quality").and_then(|v| v.as_str()) {
        if ["best", "1080", "720", "480"].contains(&v) {
            prefs.video_quality = v.to_string();
        } else {
            return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "video_quality must be one of: best, 1080, 720, 480"
            }))));
        }
    }

    match db::update_user_preferences(&state.pool, user.chat_id, &prefs).await {
        Ok(_) => Ok((StatusCode::OK, Json(serde_json::json!({
            "message": "Preferences saved",
            "preferences": prefs,
        })))),
        Err(e) => Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
            "error": format!("Failed to save preferences: {}", e)
        })))),
    }
}
