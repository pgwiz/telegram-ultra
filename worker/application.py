"""
Hermes Media Worker - Main Orchestrator
Entry point for Python worker subprocess
Handles media intelligence operations via IPC protocol
"""

import asyncio
import os
import sys
import logging
from worker.config import config
from worker.ipc import ipc_handler
from worker.cookies import cookie_manager

# Import handlers
from worker.youtube_dl import handle_youtube_download
from worker.youtube_search import handle_youtube_search, handle_get_video_info, handle_get_formats
from worker.playlist_dl import handle_playlist_download
from worker.playlist_utils import get_playlist_preview

# Import database and cache
from worker.database import get_database, close_database
from worker.cache import CacheManager


# Setup logging to stderr
logging.basicConfig(
    level=getattr(logging, config.LOG_LEVEL.upper(), logging.INFO),
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s',
    stream=sys.stderr
)
logger = logging.getLogger(__name__)


def setup_handlers():
    """Register all IPC action handlers."""
    # YouTube operations
    ipc_handler.register('youtube_dl', handle_youtube_download)
    ipc_handler.register('youtube_search', handle_youtube_search)
    ipc_handler.register('get_video_info', handle_get_video_info)
    ipc_handler.register('get_formats', handle_get_formats)
    ipc_handler.register('playlist', handle_playlist_download)

    # Playlist preview (list first N tracks without downloading)
    async def playlist_preview(ipc, task_id, request):
        """Preview first N tracks of a playlist."""
        url = request.get('url')
        preview_count = request.get('params', {}).get('preview_count', 5)
        result = await get_playlist_preview(url, preview_count)
        if result:
            ipc.send_response(task_id, 'done', result)
        else:
            ipc.send_error(task_id, "Failed to fetch playlist preview")

    ipc_handler.register('playlist_preview', playlist_preview)

    # Admin handlers
    async def cache_cleanup(ipc, task_id, request):
        """Cleanup expired cache entries."""
        await CacheManager.cleanup()
        ipc.send_response(task_id, 'cache_cleanup_done', {})

    async def cache_stats(ipc, task_id, request):
        """Get cache statistics."""
        stats = await CacheManager.get_stats()
        ipc.send_response(task_id, 'cache_stats', stats)

    ipc_handler.register('cache_cleanup', cache_cleanup)
    ipc_handler.register('cache_stats', cache_stats)

    # Health check
    async def health_check(ipc, task_id, request):
        """Simple health check handler."""
        ipc.send_response(task_id, 'health_ok', {
            'worker': 'Hermes Media Worker',
            'version': '1.0.0-phase-c',
            'config': config.to_dict(),
            'handlers': ['youtube_dl', 'youtube_search', 'get_video_info', 'get_formats', 'playlist', 'playlist_preview', 'cache_cleanup', 'cache_stats', 'health_check']
        })

    ipc_handler.register('health_check', health_check)
    logger.info("✅ All handlers registered (Phase C with caching)")

    # MTProto upload (only when MPROTO=true)
    if os.getenv("MPROTO", "false").lower() == "true":
        from worker.mtproto_upload import handle_mtproto_upload
        ipc_handler.register('mtproto_upload', handle_mtproto_upload)
        logger.info("✅ MTProto upload handler registered")


def log_startup():
    """Log startup information."""
    logger.info("=" * 60)
    logger.info("🚀 HERMES MEDIA WORKER - Starting")
    logger.info("=" * 60)
    logger.info(f"Configuration:")
    logger.info(f"  - Download dir: {config.DOWNLOAD_DIR}")
    logger.info(f"  - Temp dir: {config.TEMP_DIR}")
    logger.info(f"  - Cookie file: {config.COOKIES_FILE}")
    logger.info(f"  - Max retries: {config.MAX_RETRIES}")
    logger.info(f"  - Log level: {config.LOG_LEVEL}")
    logger.info(f"  - Search cache enabled: {config.ENABLE_SEARCH_CACHE}")
    logger.info("=" * 60)


async def main():
    """Main entry point."""
    try:
        log_startup()

        # Initialize database
        logger.info("💾 Initializing database...")
        db = await get_database()
        logger.info("✅ Database initialized and migrations completed")

        # Get cache stats
        from worker.cache import CacheManager
        cache_stats = await CacheManager.get_stats()
        logger.info(f"📊 Cache initialized: {cache_stats}")

        # Verify cookies on startup
        cookie_manager.verify_on_startup()

        # Connect MTProto client if enabled
        if os.getenv("MPROTO", "false").lower() == "true":
            from worker.mtproto_client import mtproto
            try:
                await mtproto.connect()
                logger.info("✅ MTProto client connected")
            except Exception as e:
                logger.error(f"⚠️ MTProto connect failed — large file uploads will fall back: {e}")

        # Setup handlers
        setup_handlers()

        # Start symlink repair service (maintenance only — does NOT delete pool files)
        from worker.repair_service import SymlinkRepairService
        repair_svc = SymlinkRepairService(config.DOWNLOAD_DIR, db, interval_seconds=3600)
        asyncio.create_task(repair_svc.start())
        logger.info("🔗 Symlink repair service started (hourly scan)")

        # Start IPC event loop
        logger.info("📡 Starting IPC listener (reading from stdin)")
        await ipc_handler.run()

    except KeyboardInterrupt:
        logger.info("Interrupted by user")
        sys.exit(0)
    except Exception as e:
        logger.critical(f"Fatal error: {e}", exc_info=True)
        sys.exit(1)
    finally:
        # Cleanup MTProto
        if os.getenv("MPROTO", "false").lower() == "true":
            try:
                from worker.mtproto_client import mtproto
                await mtproto.disconnect()
            except Exception:
                pass
        # Cleanup database
        await close_database()


if __name__ == '__main__':
    # Run main event loop
    asyncio.run(main())
