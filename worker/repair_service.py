"""
Symlink Repair and Maintenance Service
Background service for detecting and fixing broken symlinks and corrupted files
"""

import asyncio
import os
import json
import logging
from pathlib import Path
from typing import Optional, Tuple
from datetime import datetime


logger = logging.getLogger(__name__)


class SymlinkRepairService:
    """
    Background service to maintain symlink health and detect file corruption.

    Features:
    - Hourly scan for broken symlinks
    - Automatic repair of fixable symlinks
    - Removal of permanently broken symlinks
    - Corruption detection via file size comparison
    - Database cleanup
    """

    def __init__(self, storage_root: str, database, interval_seconds: int = 3600):
        """
        Initialize repair service.

        Args:
            storage_root: Root directory for downloads
            database: Database instance for tracking
            interval_seconds: Scan interval (default: 3600s = 1 hour)
        """
        self.storage_root = storage_root
        self.downloads_dir = Path(storage_root)
        self.pool_dir = self.downloads_dir / ".storage" / "tracks"
        self.database = database
        self.interval = interval_seconds
        self.running = False
        logger.info(f"SymlinkRepairService initialized (interval: {interval_seconds}s)")

    async def start(self) -> None:
        """Start background repair service."""
        self.running = True
        logger.info("SymlinkRepairService started")

        while self.running:
            try:
                await self.scan_and_repair()
                await self.detect_corruption()
            except Exception as e:
                logger.error(f"Error in symlink repair cycle: {e}")

            await asyncio.sleep(self.interval)

    async def stop(self) -> None:
        """Stop background service."""
        self.running = False
        logger.info("SymlinkRepairService stopped")

    async def scan_and_repair(self) -> None:
        """
        Scan all user directories for symlinks.
        Detect broken ones and repair or remove.
        """
        logger.info(f"Starting symlink scan in {self.downloads_dir}")

        broken_count = 0
        repaired_count = 0
        healthy_count = 0

        try:
            for root, dirs, files in os.walk(self.downloads_dir):
                # Skip .storage directory (central pool)
                if '.storage' in root:
                    continue

                for filename in files:
                    file_path = os.path.join(root, filename)

                    try:
                        if os.path.islink(file_path):
                            if os.path.exists(file_path):
                                # Symlink is healthy
                                healthy_count += 1
                                logger.debug(f"Healthy symlink: {file_path}")
                            else:
                                # Symlink is broken - try to repair or remove
                                if await self._repair_broken_symlink(file_path):
                                    repaired_count += 1
                                    logger.info(f"Repaired symlink: {file_path}")
                                else:
                                    broken_count += 1
                                    logger.warning(f"Removed broken symlink: {file_path}")
                    except Exception as e:
                        logger.error(f"Error checking symlink {file_path}: {e}")

        except Exception as e:
            logger.error(f"Error during symlink scan: {e}")

        if broken_count + repaired_count + healthy_count > 0:
            logger.info(
                f"Symlink scan complete: {healthy_count} healthy, "
                f"{repaired_count} repaired, {broken_count} removed"
            )

    async def _repair_broken_symlink(self, symlink_path: str) -> bool:
        """
        Attempt to repair a broken symlink.

        If physical file still exists in pool, recreate the symlink.
        Otherwise, remove the symlink and database entry.

        Args:
            symlink_path: Path to broken symlink

        Returns:
            True if repaired, False if removed permanently
        """
        try:
            # Get target of symlink
            if os.name == 'nt':  # Windows
                import ctypes
                try:
                    target = os.readlink(symlink_path)
                except (OSError, AttributeError):
                    logger.warning(f"Cannot read symlink target on Windows: {symlink_path}")
                    # Remove if we can't read it
                    try:
                        os.remove(symlink_path)
                    except:
                        pass
                    return False
            else:
                target = os.readlink(symlink_path)

            # Try to find the target file
            symlink_dir = os.path.dirname(symlink_path)
            full_target = os.path.normpath(os.path.join(symlink_dir, target))

            if os.path.exists(full_target):
                # Target exists, but symlink still broken? Recreate it
                try:
                    os.remove(symlink_path)
                    if os.name == 'nt':  # Windows
                        target = target.replace('/', '\\')
                    os.symlink(target, symlink_path)
                    logger.info(f"Successfully repaired symlink: {symlink_path}")
                    return True
                except Exception as e:
                    logger.error(f"Failed to recreate symlink: {e}")
                    return False

            else:
                # Target doesn't exist - check database for file location
                try:
                    result = await self.database.fetch_one(
                        'SELECT file_hash_sha1 FROM user_symlinks WHERE symlink_path = ?',
                        (symlink_path,)
                    )

                    if result:
                        file_hash = result.get('file_hash_sha1')
                        pool_result = await self.database.fetch_one(
                            'SELECT physical_path FROM file_storage WHERE file_hash_sha1 = ?',
                            (file_hash,)
                        )

                        if pool_result:
                            physical_path = pool_result.get('physical_path')

                            if os.path.exists(physical_path):
                                # Physical file exists, recreate symlink
                                try:
                                    os.remove(symlink_path)
                                    new_rel_path = os.path.relpath(
                                        physical_path,
                                        os.path.dirname(symlink_path)
                                    )
                                    if os.name == 'nt':  # Windows
                                        new_rel_path = new_rel_path.replace('/', '\\')
                                    os.symlink(new_rel_path, symlink_path)
                                    logger.info(f"Repaired symlink from database: {symlink_path}")
                                    return True
                                except Exception as e:
                                    logger.error(f"Failed to recreate symlink from DB: {e}")
                                    return False
                except Exception as e:
                    logger.error(f"Error querying database for symlink: {e}")

                # Can't repair - remove symlink and database entry
                try:
                    await self.database.execute(
                        'DELETE FROM user_symlinks WHERE symlink_path = ?',
                        (symlink_path,)
                    )
                    await self.database.connection.commit()
                    os.remove(symlink_path)
                    logger.info(f"Removed broken symlink (no recovery possible): {symlink_path}")
                    return False
                except Exception as e:
                    logger.error(f"Error removing broken symlink: {e}")
                    return False

        except Exception as e:
            logger.error(f"Unexpected error in _repair_broken_symlink: {e}")
            return False

    async def detect_corruption(self) -> None:
        """
        Check file integrity by comparing disk size with metadata size.
        If corrupted, flag for re-download and log warning.
        """
        logger.debug("Starting corruption detection scan")

        corruption_found = 0

        try:
            if not self.pool_dir.exists():
                return

            for hash_dir in self.pool_dir.iterdir():
                if not hash_dir.is_dir():
                    continue

                # Find the original_file (extension varies)
                original_files = list(hash_dir.glob("original_file.*"))

                if not original_files:
                    continue

                file_path = original_files[0]
                metadata_file = hash_dir / "metadata.json"

                if not metadata_file.exists():
                    logger.debug(f"No metadata for {hash_dir.name}")
                    continue

                try:
                    metadata = json.loads(metadata_file.read_text())
                    expected_size = metadata.get('size', 0)
                    actual_size = os.path.getsize(str(file_path))

                    if actual_size != expected_size:
                        corruption_found += 1
                        logger.warning(
                            f"Corruption detected: {file_path} "
                            f"(expected {expected_size} bytes, got {actual_size} bytes)"
                        )

                        # Update last check time in database
                        try:
                            file_hash = hash_dir.name
                            await self.database.execute(
                                '''UPDATE file_metadata
                                   SET corruption_checks = corruption_checks + 1,
                                       last_checked_at = ?
                                   WHERE file_hash_sha1 = ?''',
                                (datetime.now().isoformat(), file_hash)
                            )
                            await self.database.connection.commit()
                        except Exception as e:
                            logger.error(f"Failed to update corruption detection: {e}")

                except json.JSONDecodeError as e:
                    logger.error(f"Invalid metadata JSON in {metadata_file}: {e}")
                except Exception as e:
                    logger.error(f"Error checking corruption for {file_path}: {e}")

        except Exception as e:
            logger.error(f"Error during corruption detection: {e}")

        if corruption_found > 0:
            logger.warning(f"Corruption detection complete: {corruption_found} corrupted files found")

    async def cleanup_orphaned_entries(self) -> None:
        """
        Clean database entries for files/symlinks that no longer exist on disk.
        """
        logger.debug("Starting orphaned entry cleanup")

        try:
            # Find symlinks in database that don't exist on disk
            symlinks = await self.database.fetch_all(
                'SELECT id, symlink_path FROM user_symlinks'
            )

            removed = 0
            for symlink_entry in symlinks:
                symlink_path = symlink_entry.get('symlink_path')

                if not os.path.exists(symlink_path) and not os.path.islink(symlink_path):
                    # Entry exists in DB but symlink doesn't exist on disk
                    try:
                        await self.database.execute(
                            'DELETE FROM user_symlinks WHERE id = ?',
                            (symlink_entry.get('id'),)
                        )
                        await self.database.connection.commit()
                        removed += 1
                        logger.debug(f"Removed orphaned DB entry: {symlink_path}")
                    except Exception as e:
                        logger.error(f"Failed to remove orphaned entry: {e}")

            if removed > 0:
                logger.info(f"Orphaned entry cleanup: removed {removed} entries")

        except Exception as e:
            logger.error(f"Error during orphaned entry cleanup: {e}")
