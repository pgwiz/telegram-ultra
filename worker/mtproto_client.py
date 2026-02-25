"""
MTProto client singleton for Hermes.
Manages a persistent Telethon TelegramClient used for large file uploads.
"""
import os
import asyncio
import logging
from typing import Optional

from dotenv import load_dotenv
from telethon import TelegramClient

load_dotenv()
logger = logging.getLogger(__name__)

# ── Config (read once at import time) ─────────────────────────────────────────
API_ID       = int(os.getenv("TELEGRAM_API_ID", "0"))
API_HASH     = os.getenv("TELEGRAM_API_HASH", "")
SESSION_PATH = os.getenv("MTPROTO_SESSION_PATH", "./hermes_session")
CHANNEL_ID   = int(os.getenv("STORAGE_CHANNEL_ID", "0"))


class HermesMTProto:
    """
    Singleton MTProto client.
    Call connect() once at worker startup; reuse .client everywhere.
    """

    _instance: Optional["HermesMTProto"] = None
    _client:   Optional[TelegramClient]  = None

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    async def connect(self) -> None:
        """Open the Telethon client and verify the session is authorised."""
        if self._client and self._client.is_connected():
            return

        self._client = TelegramClient(
            SESSION_PATH,
            API_ID,
            API_HASH,
            connection_retries=5,
            retry_delay=1,
        )
        await self._client.connect()

        if not await self._client.is_user_authorized():
            raise RuntimeError(
                "MTProto session not authenticated. "
                "Run: python3 worker/setup_session.py"
            )

        me = await self._client.get_me()
        logger.info(f"MTProto connected as @{me.username} (ID: {me.id})")

    async def disconnect(self) -> None:
        """Gracefully close the connection."""
        if self._client:
            await self._client.disconnect()
            logger.info("MTProto disconnected")

    @property
    def client(self) -> TelegramClient:
        if not self._client or not self._client.is_connected():
            raise RuntimeError(
                "MTProto client not connected. connect() must be called first."
            )
        return self._client


# Module-level singleton
mtproto = HermesMTProto()
