"""
One-time setup script — authenticate the Telethon MTProto session.

Run once on the server:
    cd /opt/hermes && source .venv/bin/activate
    python3 worker/setup_session.py

After this the session file persists and the worker connects automatically.
"""
import os
import sys

try:
    from dotenv import load_dotenv
    from telethon.sync import TelegramClient
except ImportError:
    print("Missing dependencies. Install with:")
    print("  pip install telethon tgcrypto python-dotenv")
    sys.exit(1)

load_dotenv()

API_ID       = os.getenv("TELEGRAM_API_ID")
API_HASH     = os.getenv("TELEGRAM_API_HASH")
PHONE        = os.getenv("TELEGRAM_PHONE")
SESSION_PATH = os.getenv("MTPROTO_SESSION_PATH", "./hermes_session")

if not all([API_ID, API_HASH, PHONE]):
    print("ERROR: Missing env vars — set TELEGRAM_API_ID, TELEGRAM_API_HASH, TELEGRAM_PHONE")
    sys.exit(1)

print(f"Authenticating MTProto session for {PHONE}")
print(f"Session will be saved to: {SESSION_PATH}.session")

client = TelegramClient(SESSION_PATH, int(API_ID), API_HASH)
client.start(phone=PHONE)

me = client.get_me()
print(f"\n✅ Authenticated as: {me.first_name} (@{me.username})")
print(f"   User ID: {me.id}")
print(f"   Session: {SESSION_PATH}.session")

# Verify storage channel
channel_id = os.getenv("STORAGE_CHANNEL_ID")
if channel_id:
    try:
        entity = client.get_entity(int(channel_id))
        print(f"\n✅ Storage channel accessible: {entity.title}")
    except Exception as e:
        print(f"\nWARN: Could not verify storage channel ({channel_id}): {e}")
        print("Make sure STORAGE_CHANNEL_ID is set correctly and your account is in the channel.")

client.disconnect()
print("\nDone. You can now start the Hermes bot with MPROTO=true.")
