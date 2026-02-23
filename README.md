# Hermes Download Nexus

Telegram bot + web dashboard for downloading YouTube media. Rust backend, Python worker, Node.js dashboard.

## Quick Install (Ubuntu 22.04+)

```bash
curl -fsSL https://raw.githubusercontent.com/pgwiz/telegram-ultra/main/deploy/hermes-pgwiz | sudo bash -s install
```

This installs everything: Rust, Node.js, Python venv, builds the project, sets up systemd services, and configures the `.env` interactively.

## Manual Install

```bash
git clone https://github.com/pgwiz/telegram-ultra.git /opt/hermes
cd /opt/hermes
sudo bash deploy/hermes-pgwiz install
```

## Management

After install, the `hermes-pgwiz` command is available system-wide:

```bash
hermes-pgwiz              # Interactive menu
hermes-pgwiz start        # Start all services
hermes-pgwiz stop         # Stop all services
hermes-pgwiz restart      # Restart all services
hermes-pgwiz status       # Show service status
hermes-pgwiz logs         # Tail all logs
hermes-pgwiz logs bot     # Tail bot logs only
hermes-pgwiz update       # Git pull + rebuild + restart
hermes-pgwiz build        # Rebuild Rust crates only
hermes-pgwiz setup-env    # Reconfigure .env
```

## Architecture

```
telegram-ultra/
  bot/          Rust - Telegram bot (teloxide), spawns Python worker
  api/          Rust - REST API server (axum), OTP auth, task management
  shared/       Rust - Shared library (DB, models, IPC protocol)
  worker/       Python - yt-dlp download worker, IPC via stdin/stdout
  ui/           Node.js - Express dashboard, proxies to API
  deploy/       systemd services + hermes-pgwiz management script
  migrations/   SQLite migrations
```

**Stack:** Rust (teloxide 0.12, axum 0.7, sqlx 0.7) | Python (aiosqlite, yt-dlp) | Node.js (Express)

## Services

| Service | Default Port | Description |
|---------|-------------|-------------|
| hermes-bot | - | Telegram bot, polls for web-queued tasks |
| hermes-api | 8081 | REST API with JWT auth |
| hermes-ui | 3000 | Web dashboard (proxies /api to API) |

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `TELEGRAM_BOT_TOKEN` | Yes | - | Bot token from @BotFather |
| `ADMIN_CHAT_ID` | Yes | - | Your Telegram user ID |
| `JWT_SECRET` | Yes | - | Secret for signing JWT tokens |
| `DATABASE_PATH` | No | `./hermes.db` | SQLite database path |
| `DOWNLOAD_DIR` | No | `./downloads` | Where files are saved |
| `API_PORT` | No | `8081` | API server port |
| `NODE_UI_PORT` | No | `3000` | Dashboard port |
| `SESSION_TTL_SECS` | No | `600` | Session lifetime in seconds |
| `PYTHON_BIN` | No | `python` | Path to Python binary (set to venv) |

## Bot Commands

| Command | Description |
|---------|-------------|
| `/start` | Welcome message |
| `/download <url>` | Download YouTube video/audio |
| `/dv <url>` | Download with video quality selection |
| `/da <url>` | Download with audio quality selection |
| `/search <query>` | Search YouTube |
| `/help` | Show help |

Paste a YouTube link directly (no command needed) and the bot auto-detects it.

## Web Dashboard

Access at `http://your-server:3000` after starting services. Login flow:

1. Enter your Telegram chat ID
2. Receive OTP code via the bot
3. Enter the code to get a session

Features: task monitoring, file downloads, delete files, clear history, admin panel.

## License

MIT
