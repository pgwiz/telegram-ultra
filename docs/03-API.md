# Hermes API — Developer Guide

## Overview

The REST API is an Axum server (`api/`) running on port **8081**.
It provides task management, authentication, file serving, and admin features for the web dashboard.
It shares the same SQLite database (`hermes.db`) as the bot.

---

## Authentication System

### Flow

```
1. User opens dashboard  →  enters their Telegram chat_id
2. POST /api/auth/request-otp  →  6-digit OTP generated
3. API calls Telegram Bot API to DM the OTP to the user
4. User receives OTP in Telegram  →  enters it in dashboard
5. POST /api/auth/verify-otp  →  API validates OTP, issues JWT
6. JWT stored as hermes_token cookie  →  all subsequent requests use it
7. API validates cookie on every auth-protected route:
   - JWT signature + expiry check
   - DB session lookup (token must exist in sessions table)
```

### Alternate Flow: `/allow N` (Quick Login)

Admin can run `/allow 5` in Telegram → bot opens a 5-minute window.
During that window, POST `/api/auth/quick-login` returns a JWT without OTP.
The window is stored as an `allow_window` session token in the DB.

### JWT Claims

```json
{ "sub": "123456789", "exp": 1234567890, "iat": 1234567000 }
```

`sub` is `chat_id` as string. Token validated against `jwt_secret` (HS256).
Sessions table enforces server-side revocation (logout deletes the row).

### Token Extraction

The `authenticate()` function checks in order:
1. `Authorization: Bearer <token>` header
2. `hermes_token` cookie

### Admin Auth

`authenticate_admin()` calls `authenticate()` first, then checks `chat_id == admin_chat_id`.
Admin-only routes return `403 Forbidden` for non-admin users.

---

## Endpoints

### Auth (no authentication required)

#### `POST /api/auth/request-otp`
Request an OTP to be sent to a Telegram chat.

**Request:**
```json
{ "chat_id": 123456789 }
```
**Response:** `200 { "message": "OTP sent" }` or `429 Too Many Requests` (3/hour limit)

---

#### `POST /api/auth/verify-otp`
Submit the OTP received in Telegram.

**Request:**
```json
{ "chat_id": 123456789, "otp": "482910" }
```
**Response:**
```json
{ "token": "<jwt>", "expires_in": 600, "chat_id": 123456789 }
```
Sets `Set-Cookie: hermes_token=<jwt>; HttpOnly`.

---

#### `POST /api/auth/quick-login`
Login without OTP (requires active `/allow` window from admin).

**Request:** `{}` (no body needed — checks allow_window session)
**Response:** Same JWT response as verify-otp.

---

#### `GET /api/auth/allow-status`
Check if a quick-login window is currently open.

**Response:** `{ "allowed": true }` or `{ "allowed": false }`

---

#### `GET /api/bot-info`
Returns basic bot info (no auth needed — used on login page).

**Response:** `{ "username": "MyBot", "first_name": "Hermes" }`

---

### Auth-Protected Endpoints

All routes below require a valid `hermes_token` cookie or `Authorization: Bearer` header.

---

#### `DELETE /api/auth/logout`
Invalidates the current session (deletes session row from DB).

**Response:** `{ "message": "Logged out" }`

---

#### `POST /api/download`
Submit a single URL for download.

**Request:**
```json
{ "url": "https://youtu.be/...", "download_type": "audio" }
```
`download_type`: `"audio"` (default) or `"video"`

**Response:**
```json
{ "task_id": "abc123...", "message": "Download queued" }
```

---

#### `POST /api/download/batch`
Submit multiple URLs at once.

**Request:**
```json
{ "urls": ["https://youtu.be/...", "..."], "download_type": "video" }
```
**Response:**
```json
{ "tasks": [{ "task_id": "...", "url": "...", "status": "queued" }, ...] }
```

---

#### `GET /api/tasks`
List all tasks for the authenticated user.

**Query params:** `?status=queued|running|completed|failed` (optional filter)

**Response:**
```json
[{
  "task_id": "abc123",
  "chat_id": 123456789,
  "kind": "youtube_dl",
  "url": "https://youtu.be/...",
  "status": "completed",
  "label": "audio",
  "created_at": "2025-01-01T12:00:00Z",
  "completed_at": "2025-01-01T12:01:30Z"
}]
```

---

#### `GET /api/tasks/:id`
Get a single task by ID.

**Response:** Single task object (same schema as above).

---

#### `DELETE /api/tasks/:id`
Cancel/delete a task.

**Response:** `{ "message": "Task cancelled" }`

---

#### `PUT /api/tasks/:id`
Update task metadata (label or URL).

**Request:** `{ "label": "My Playlist", "url": "..." }` (both optional)

**Response:** Updated task object.

---

#### `POST /api/tasks/:id/retry`
Retry a failed task.

**Response:** `{ "task_id": "...", "message": "Task re-queued" }`

---

#### `GET /api/files`
List downloaded files for the authenticated user.

**Response:**
```json
[{
  "id": "abc123",
  "filename": "song.mp3",
  "size": 4194304,
  "created_at": "2025-01-01T12:00:00Z",
  "download_url": "/api/files/abc123/download"
}]
```

---

#### `GET /api/files/:id/download`
Stream a file to the browser.

Sets `Content-Disposition: attachment; filename="..."` for auto-download.
Uses `tokio_util::io::ReaderStream` for zero-copy async streaming.

---

#### `DELETE /api/files/:id`
Delete a downloaded file from disk.

**Response:** `{ "message": "File deleted" }`

---

#### `DELETE /api/files/history`
Clear download history (removes DB records; optionally removes files).

---

### Admin Endpoints

Require `chat_id == ADMIN_CHAT_ID`.

#### `GET /api/admin/stats`
System stats snapshot.

**Response:**
```json
{
  "total_tasks": 150,
  "completed": 142,
  "failed": 8,
  "active_sessions": 2,
  "total_users": 5
}
```

---

#### `GET /api/admin/users`
List all users who have logged into the dashboard.

**Response:** Array of `{ chat_id, created_at, last_seen }`.

---

#### `GET /api/admin/logs`
Fetch recent system logs from journald or log files.

**Query params:**
- `service`: comma-separated — `hermes-bot,hermes-api,hermes-ui`
- `lines`: number of lines (default 200, max 1000)
- `since`: `"1h"`, `"6h"`, `"24h"`, `"7d"`
- `level`: `"error"`, `"warning"`, `"info"`, `"debug"`

---

## Error Responses

All errors return JSON with an `error` field:

```json
{ "error": "Session expired or invalid" }
```

| Status | Meaning |
|--------|---------|
| 400 | Bad request / missing field |
| 401 | No token, invalid JWT, or session expired |
| 403 | Authenticated but not admin |
| 404 | Task/file not found |
| 429 | Rate limited (OTP requests) |
| 500 | Internal server error |

---

## Database

The API uses `hermes_shared::db` functions. All SQL is in `shared/src/db.rs`.

| Function | Description |
|----------|-------------|
| `create_pool(url)` | Opens SQLite connection pool (WAL mode, 5 connections) |
| `run_migrations(pool)` | Applies SQL migration files from `migrations/` |
| `upsert_user(pool, chat_id, username)` | Insert or update user record |
| `create_otp_session(pool, chat_id, otp)` | Store OTP with 5-min TTL |
| `verify_otp_session(pool, chat_id, otp)` | Validate OTP and consume it |
| `create_session(pool, chat_id, token, ttl)` | Create JWT session record |
| `validate_session(pool, token)` | Check session is still valid |
| `cleanup_expired_sessions(pool)` | Remove expired sessions (called every `SESSION_CLEANUP_INTERVAL` secs) |
| `create_task(pool, task_id, chat_id, kind, url, label)` | Insert task record |
| `update_task_status(pool, task_id, status)` | Update task status |
| `list_tasks(pool, chat_id, status_filter)` | List tasks for user |

---

## CORS

Wide-open CORS for development:
```rust
CorsLayer::new()
    .allow_origin(Any)
    .allow_methods(Any)
    .allow_headers(Any)
```

For production, restrict `allow_origin` to your dashboard domain.

---

## Configuration

See `api/src/main.rs` — all config is read from env vars:

| Var | Default | Description |
|-----|---------|-------------|
| `DATABASE_PATH` | `./hermes.db` | Path to SQLite database |
| `TELEGRAM_BOT_TOKEN` | required | Bot token for OTP delivery |
| `JWT_SECRET` | required | HMAC-SHA256 signing key |
| `ADMIN_CHAT_ID` | required | Admin's Telegram chat ID |
| `API_HOST` | `0.0.0.0` | Bind host |
| `API_PORT` | `8081` | Bind port |
| `SESSION_TTL_SECS` | `600` | JWT/session lifetime |
| `SESSION_CLEANUP_INTERVAL` | `300` | Cleanup task interval (secs) |
| `DOWNLOAD_DIR` | `./downloads` | Where downloaded files live |
