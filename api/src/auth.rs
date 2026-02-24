/// OTP generation, JWT management, and auth helpers.
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::Json;
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::AppState;

/// JWT Claims payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// Subject: chat_id as string
    pub sub: String,
    /// Expiration (Unix timestamp)
    pub exp: usize,
    /// Issued at (Unix timestamp)
    pub iat: usize,
}

/// Authenticated user info.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub chat_id: i64,
    pub token: String,
}

/// Error response body.
#[derive(Serialize)]
pub struct ErrorBody {
    pub error: String,
}

/// Generate a random 6-digit OTP code.
pub fn generate_otp() -> String {
    let mut rng = rand::thread_rng();
    let code: u32 = rng.gen_range(100_000..999_999);
    code.to_string()
}

/// Send an OTP code to a Telegram user via Bot API.
pub async fn send_telegram_otp(
    bot_token: &str,
    chat_id: i64,
    otp: &str,
) -> Result<(), String> {
    let url = format!(
        "https://api.telegram.org/bot{}/sendMessage",
        bot_token
    );

    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": format!(
            "Your Hermes Dashboard OTP code:\n\n<code>{}</code>\n\nThis code expires in 5 minutes.\nDo not share this code with anyone.",
            otp
        ),
        "parse_mode": "HTML"
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to send Telegram message: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!("Telegram API error {}: {}", status, text);
        return Err(format!("Telegram API error: {}", status));
    }

    info!("OTP sent to chat_id {}", chat_id);
    Ok(())
}

/// Create a JWT token for a chat_id.
pub fn create_jwt(chat_id: i64, secret: &str, ttl_secs: i64) -> Result<String, String> {
    let now = Utc::now();
    let exp = now + Duration::seconds(ttl_secs);

    let claims = Claims {
        sub: chat_id.to_string(),
        exp: exp.timestamp() as usize,
        iat: now.timestamp() as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| format!("JWT encode error: {}", e))
}

/// Validate a JWT token and return the claims.
pub fn validate_jwt(token: &str, secret: &str) -> Result<Claims, String> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|e| format!("JWT validation error: {}", e))?;

    Ok(token_data.claims)
}

/// Extract JWT token from request headers (Authorization header or cookie).
fn extract_token(headers: &HeaderMap) -> Option<String> {
    // Try Authorization: Bearer <token>
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return Some(token.to_string());
        }
    }

    // Fallback: try cookie "hermes_token"
    if let Some(cookies) = headers.get("cookie").and_then(|v| v.to_str().ok()) {
        for cookie in cookies.split(';').map(|c| c.trim()) {
            if let Some(token) = cookie.strip_prefix("hermes_token=") {
                return Some(token.to_string());
            }
        }
    }

    None
}

/// Authenticate user from request headers. Returns AuthUser or error response.
pub async fn authenticate(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<AuthUser, (StatusCode, Json<ErrorBody>)> {
    let token = extract_token(headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "No authentication token provided".to_string(),
            }),
        )
    })?;

    // Validate JWT
    let claims = validate_jwt(&token, &state.jwt_secret).map_err(|e| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody { error: e }),
        )
    })?;

    let chat_id: i64 = claims.sub.parse().map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "Invalid token subject".to_string(),
            }),
        )
    })?;

    // Validate against DB session
    let valid = hermes_shared::db::validate_session(&state.pool, &token)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody {
                    error: "Session validation failed".to_string(),
                }),
            )
        })?;

    if valid.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "Session expired or invalid".to_string(),
            }),
        ));
    }

    Ok(AuthUser {
        chat_id,
        token,
    })
}

/// Authenticate admin user. Returns chat_id or error response.
pub async fn authenticate_admin(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<AuthUser, (StatusCode, Json<ErrorBody>)> {
    let user = authenticate(headers, state).await?;

    if user.chat_id != state.admin_chat_id {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorBody {
                error: "Admin access required".to_string(),
            }),
        ));
    }

    Ok(user)
}
