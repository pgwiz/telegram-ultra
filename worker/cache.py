"""
Caching layer for Hermes Media Worker
Reduces API calls by caching search results and metadata
"""

import hashlib
import json
import logging
from typing import Optional, List, Dict, Any
from datetime import datetime, timedelta
from worker.database import get_database
from worker.config import config


logger = logging.getLogger(__name__)


class MetadataCache:
    """Cache for YouTube video metadata."""

    CACHE_EXPIRY_HOURS = config.CACHE_EXPIRY_HOURS

    @staticmethod
    async def get(video_id: str) -> Optional[Dict[str, Any]]:
        """
        Get cached video metadata.

        Args:
            video_id: YouTube video ID

        Returns:
            Cached metadata dict, or None if expired/not found
        """
        try:
            db = await get_database()

            result = await db.fetch_one(
                """
                SELECT * FROM youtube_metadata_cache
                WHERE video_id = ? AND (expires_at IS NULL OR expires_at > ?)
                """,
                (video_id, datetime.now())
            )

            if result:
                # Update access stats
                await db.update(
                    """
                    UPDATE youtube_metadata_cache
                    SET access_count = access_count + 1, last_accessed = ?
                    WHERE video_id = ?
                    """,
                    (datetime.now(), video_id)
                )

                logger.debug(f"Cache hit for video {video_id[:8]}...")
                return result

            logger.debug(f"Cache miss for video {video_id[:8]}...")
            return None

        except Exception as e:
            logger.warning(f"Cache get failed for {video_id}: {e}")
            return None

    @staticmethod
    async def set(video_id: str, metadata: Dict[str, Any], ttl_hours: int = None) -> bool:
        """
        Cache video metadata.

        Args:
            video_id: YouTube video ID
            metadata: Metadata dictionary
            ttl_hours: Time to live in hours (default from config)

        Returns:
            True if cached successfully
        """
        try:
            ttl_hours = ttl_hours or MetadataCache.CACHE_EXPIRY_HOURS
            expires_at = datetime.now() + timedelta(hours=ttl_hours)

            db = await get_database()

            await db.insert(
                """
                INSERT OR REPLACE INTO youtube_metadata_cache
                (video_id, title, uploader, duration_seconds, thumbnail_url,
                 is_age_restricted, is_playlist, is_private, expires_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    video_id,
                    metadata.get('title', ''),
                    metadata.get('uploader', ''),
                    metadata.get('duration_seconds', 0),
                    metadata.get('thumbnail_url', ''),
                    metadata.get('is_age_restricted', False),
                    metadata.get('is_playlist', False),
                    metadata.get('is_private', False),
                    expires_at
                )
            )

            logger.debug(f"Cached video {video_id[:8]}... (expires in {ttl_hours}h)")
            return True

        except Exception as e:
            logger.warning(f"Cache set failed for {video_id}: {e}")
            return False

    @staticmethod
    async def clear_expired() -> int:
        """
        Clear expired cache entries.

        Returns:
            Number of rows deleted
        """
        try:
            db = await get_database()

            deleted = await db.delete(
                """
                DELETE FROM youtube_metadata_cache
                WHERE expires_at IS NOT NULL AND expires_at < ?
                """,
                (datetime.now(),)
            )

            if deleted > 0:
                logger.info(f"Cleared {deleted} expired cache entries")

            return deleted

        except Exception as e:
            logger.warning(f"Cache clear failed: {e}")
            return 0


class SearchCache:
    """Cache for YouTube search results."""

    CACHE_EXPIRY_HOURS = config.CACHE_EXPIRY_HOURS

    @staticmethod
    def _hash_query(query: str) -> str:
        """Generate cache key from query."""
        return hashlib.md5(query.lower().encode()).hexdigest()

    @staticmethod
    async def get(query: str) -> Optional[List[Dict[str, Any]]]:
        """
        Get cached search results.

        Args:
            query: Search query string

        Returns:
            Cached results list, or None if expired/not found
        """
        try:
            query_hash = SearchCache._hash_query(query)
            db = await get_database()

            result = await db.fetch_one(
                """
                SELECT results_json FROM search_cache
                WHERE query_hash = ? AND (expires_at IS NULL OR expires_at > ?)
                """,
                (query_hash, datetime.now())
            )

            if result:
                # Update access stats
                await db.update(
                    """
                    UPDATE search_cache
                    SET access_count = access_count + 1, last_accessed = ?
                    WHERE query_hash = ?
                    """,
                    (datetime.now(), query_hash)
                )

                try:
                    results = json.loads(result['results_json'])
                    logger.debug(f"Cache hit for search '{query[:30]}...'")
                    return results
                except (json.JSONDecodeError, TypeError):
                    logger.warning(f"Failed to decode cached results for '{query}'")
                    return None

            logger.debug(f"Cache miss for search '{query[:30]}...'")
            return None

        except Exception as e:
            logger.warning(f"Search cache get failed: {e}")
            return None

    @staticmethod
    async def set(query: str, results: List[Dict[str, Any]], ttl_hours: int = None) -> bool:
        """
        Cache search results.

        Args:
            query: Search query string
            results: Results list
            ttl_hours: Time to live in hours (default from config)

        Returns:
            True if cached successfully
        """
        if not config.ENABLE_SEARCH_CACHE:
            return False

        try:
            ttl_hours = ttl_hours or SearchCache.CACHE_EXPIRY_HOURS
            query_hash = SearchCache._hash_query(query)
            expires_at = datetime.now() + timedelta(hours=ttl_hours)

            try:
                results_json = json.dumps(results)
            except (TypeError, ValueError) as e:
                logger.warning(f"Failed to serialize results for '{query}': {e}")
                return False

            db = await get_database()

            await db.insert(
                """
                INSERT OR REPLACE INTO search_cache
                (query, query_hash, results_json, expires_at)
                VALUES (?, ?, ?, ?)
                """,
                (query, query_hash, results_json, expires_at)
            )

            logger.debug(f"Cached search '{query[:30]}...' ({len(results)} results, expires in {ttl_hours}h)")
            return True

        except Exception as e:
            logger.warning(f"Search cache set failed: {e}")
            return False

    @staticmethod
    async def clear_expired() -> int:
        """
        Clear expired search cache entries.

        Returns:
            Number of rows deleted
        """
        try:
            db = await get_database()

            deleted = await db.delete(
                """
                DELETE FROM search_cache
                WHERE expires_at IS NOT NULL AND expires_at < ?
                """,
                (datetime.now(),)
            )

            if deleted > 0:
                logger.info(f"Cleared {deleted} expired search cache entries")

            return deleted

        except Exception as e:
            logger.warning(f"Search cache clear failed: {e}")
            return 0


class CacheManager:
    """Overall cache management."""

    @staticmethod
    async def cleanup() -> None:
        """Clean up all expired cache entries."""
        try:
            logger.info("Running cache cleanup...")

            metadata_deleted = await MetadataCache.clear_expired()
            search_deleted = await SearchCache.clear_expired()

            total = metadata_deleted + search_deleted
            logger.info(f"Cache cleanup complete: {total} entries deleted")

        except Exception as e:
            logger.error(f"Cache cleanup failed: {e}")

    @staticmethod
    async def clear_all() -> None:
        """Clear all cache (for testing or admin purposes)."""
        try:
            db = await get_database()

            await db.delete("DELETE FROM youtube_metadata_cache")
            await db.delete("DELETE FROM search_cache")

            logger.info("All cache entries cleared")

        except Exception as e:
            logger.error(f"Clear all cache failed: {e}")

    @staticmethod
    async def get_stats() -> Dict[str, Any]:
        """Get cache statistics."""
        try:
            db = await get_database()

            metadata_count = await db.fetch_one(
                "SELECT COUNT(*) as count FROM youtube_metadata_cache"
            )
            search_count = await db.fetch_one(
                "SELECT COUNT(*) as count FROM search_cache"
            )

            return {
                'metadata_entries': metadata_count['count'] if metadata_count else 0,
                'search_entries': search_count['count'] if search_count else 0,
                'cache_enabled': config.ENABLE_SEARCH_CACHE,
                'ttl_hours': config.CACHE_EXPIRY_HOURS,
            }

        except Exception as e:
            logger.error(f"Get cache stats failed: {e}")
            return {}
