# Hermes Download Nexus — Architecture Guide

## System Overview

Hermes is a self-hosted Telegram media downloader with a web dashboard. It consists of four cooperating processes:

```
┌─────────────────────────────────────────────────────┐
│  Telegram Users                                     │
│  (messages, inline keyboards)                       │
└────────────────────┬────────────────────────────────┘
                     │ Bot API (teloxide)
┌────────────────────▼────────────────────────────────┐
│  BOT  (Rust · teloxide · tokio)         :8080        │
│  • Parses messages, detects links                   │
│  • Multi-step confirmation dialogs                  │
│  • Sends/receives files via Telegram                │
│  • Spawns Python worker as child process            │
└────────────────────┬────────────────────────────────┘
                     │ stdin/stdout JSON-lines (IPC)
┌────────────────────▼────────────────────────────────┐
│  WORKER  (Python · yt-dlp · asyncio)                │
│  • Handles youtube_dl, playlist, search, etc.       │
│  • Downloads media, archives playlists to ZIP       │
│  • Reports progress events back to bot              │
└────────────────────┬────────────────────────────────┘
                     │ SQLite (WAL mode, shared DB)
┌────────────────────▼────────────────────────────────┐
│  SHARED DB  (hermes.db · SQLite)                    │
│  • tasks, sessions, users, search_cache tables      │
└────────────────────┬────────────────────────────────┘
                     │ REST + static files
┌────────────────────▼────────────────────────────────┐
│  API  (Rust · axum · sqlx)              :8081        │
│  • OTP auth → JWT → session in DB                   │
│  • Task CRUD, file downloads, admin stats           │
└────────────────────┬────────────────────────────────┘
                     │ HTTP proxy (/api → :8081)
┌────────────────────▼────────────────────────────────┐
│  UI  (Node.js · Express · vanilla JS)   :3000        │
│  • Static SPA, proxies /api calls                   │
│  • Login page → Dashboard (tasks, files, admin)     │
└─────────────────────────────────────────────────────┘
```

## Tech Stack

| Component | Language | Key Libraries |
|-----------|----------|---------------|
| Bot       | Rust     | teloxide 0.13, tokio, serde_json, regex, uuid, sqlx |
| Worker    | Python 3 | yt-dlp, asyncio, zipfile, sqlite3 |
| API       | Rust     | axum, sqlx (SQLite), jsonwebtoken, reqwest, tower-http |
| UI        | Node.js  | express, http-proxy-middleware, vanilla JS |
| Shared    | Rust     | sqlx, serde, thiserror (db, IPC types, task queue) |

## Workspace Structure

```
telegram-ultra/
├── bot/                    # Telegram bot (Rust binary)
│   └── src/
│       ├── main.rs         # Startup, AppState construction, handler dispatch
│       ├── commands.rs     # All command and callback handlers
│       ├── callback_state.rs  # In-memory state stores (callbacks, search, playlist)
│       ├── link_detector.rs   # URL regex detection (YouTube, Telegram, generic)
│       └── workers/
│           └── python_dispatcher.rs  # Child process manager + IPC channel routing
│
├── worker/                 # Python download worker
│   ├── application.py      # IPC handler registration, startup
│   ├── ipc.py              # Stdin/stdout JSON-lines loop
│   ├── youtube_dl.py       # Single video download via yt-dlp
│   ├── playlist_dl.py      # Playlist download + ZIP archiving
│   ├── youtube_search.py   # YouTube search via yt-dlp
│   ├── cache.py            # Search result caching (SQLite)
│   ├── database.py         # Task status persistence helpers
│   ├── config.py           # WorkerConfig dataclass (env vars)
│   ├── progress_hooks.py   # yt-dlp progress hook → IPC progress events
│   └── utils.py            # Sanitize filenames, format sizes, etc.
│
├── api/                    # REST API server (Rust binary)
│   └── src/
│       ├── main.rs         # Axum app, routes, CORS, background cleanup
│       ├── routes.rs       # All route handlers (20 endpoints)
│       └── auth.rs         # OTP generation, JWT encode/validate, session auth
│
├── shared/                 # Shared Rust library (hermes_shared crate)
│   └── src/
│       ├── lib.rs          # Re-exports
│       ├── db.rs           # SQLite pool, migrations, all DB CRUD
│       ├── ipc_protocol.rs # IPCRequest/IPCResponse types + builder helpers
│       ├── task_queue.rs   # TaskQueue (semaphore-based concurrency control)
│       └── errors.rs       # HermesError, IpcError
│
├── ui/                     # Web dashboard (Node.js)
│   ├── server.js           # Express server, /api proxy to :8081
│   └── public/
│       ├── login.html      # Login/OTP entry page
│       ├── index.html      # Main dashboard SPA
│       └── assets/
│           ├── app.js      # Dashboard JS (task list, file browser, admin)
│           └── style.css   # Styles
│
└── migrations/             # SQLite migration SQL files
```

## Key Data Flows

### 1. Single YouTube Video Download
```
User → /download URL
  → Bot: detect_first_link() → YoutubeVideo
  → Bot: cmd_download() → download_request() → IPCRequest
  → Bot: task_queue.enqueue() + db.create_task()
  → PythonDispatcher.send() → worker stdin
  → Worker: handle_youtube_dl() → yt-dlp subprocess
  → Worker: progress events → stdout
  → Bot: IPCResponse(progress) → edit Telegram message
  → Worker: IPCResponse(done, files=[...])
  → Bot: send files via Telegram, update DB
```

### 2. Playlist Download (with confirmation)
```
User → plain message with playlist URL
  → Bot: detect_links() → YoutubePlaylist
  → Bot: cmd_playlist_confirm() → inline keyboard sent
  → User clicks "Download Playlist" → pc:KEY:p callback
  → Bot: show limit buttons (10/25/50/All)
  → User clicks "25 tracks" → pl:KEY:25 callback
  → Bot: show format buttons (Audio MP3 / Video MP4)
  → User clicks "Audio (MP3)" → pf:KEY:a callback
  → Bot: playlist_request_opts() → IPCRequest(playlist, max_items=25)
  → Worker: playlist_dl.py → download 25 tracks → ZIP
  → Bot: send ZIP to Telegram
```

### 3. Web Dashboard Login
```
User → opens dashboard
  → clicks "Request OTP"
  → POST /api/auth/request-otp (chat_id)
  → API generates 6-digit OTP → stores in DB (5 min TTL)
  → API calls Telegram Bot API sendMessage → OTP delivered to user's DM
  → User enters OTP → POST /api/auth/verify-otp
  → API validates OTP → creates JWT + DB session
  → JWT stored as hermes_token cookie
  → Dashboard loads, all /api calls use cookie auth
```

## Concurrency Model

- **Bot**: Fully async Tokio. Each download runs in a `tokio::spawn` task.
- **Task Queue**: `TaskQueue` (shared crate) uses `tokio::sync::Semaphore` to cap concurrent downloads (default: 3 per `MAX_CONCURRENT_TASKS` env var).
- **Worker**: Single Python process, handles one IPC request at a time (sequential stdin loop). Multiple simultaneous downloads from bot hit the semaphore queue.
- **API**: Axum is fully async; independent of bot/worker.

## Shared Database

All components share `hermes.db` (SQLite in WAL mode):

| Table | Writer | Readers |
|-------|--------|---------|
| `tasks` | Bot (create), Worker (update status) | API (list/get), UI |
| `sessions` | API | API |
| `users` | API | API |
| `search_cache` | Worker | Worker |

## Environment Variables

Key variables used across all components:

```env
# Bot + Worker
TELOXIDE_TOKEN=<bot_token>
DOWNLOAD_DIR=./downloads
DATABASE_PATH=./hermes.db
WORKER_DIR=./                 # where worker/ package lives
PYTHON_BIN=python3
MAX_CONCURRENT_TASKS=3

# API
TELEGRAM_BOT_TOKEN=<same_token>
JWT_SECRET=<random_secret>
ADMIN_CHAT_ID=<your_telegram_chat_id>
API_HOST=127.0.0.1
API_PORT=8081
SESSION_TTL_SECS=600

# UI
NODE_UI_PORT=3000

# Worker-specific
YOUTUBE_COOKIE_FILE=./cookies.txt
BEST_AUDIO_LIMIT_MB=15
YT_TIMEOUT=300
ARCHIVE_MAX_SIZE_MB=100
ENABLE_SEARCH_CACHE=true
CACHE_EXPIRY_HOURS=24
```
