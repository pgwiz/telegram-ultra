"""
Storage Manager for Smart Track Deduplication System
Handles file hashing, symlink creation, and central pool management
"""

import asyncio
import hashlib
import os
import json
import shutil
import logging
from pathlib import Path
from typing import Optional, Tuple, Any
from datetime import datetime


logger = logging.getLogger(__name__)


class StorageManager:
    """
    Manages intelligent file storage with symlink-based deduplication.

    Architecture:
    - Central pool: .storage/tracks/<sha1_hash>/original_file.<ext>
    - User symlinks: <chat_id>/<task_id>/file.mp3 -> ../../.storage/tracks/<hash>/original_file.mp3
    """

    def __init__(self, storage_root: str):
        """
        Initialize StorageManager.

        Args:
            storage_root: Root directory for downloads (e.g. /downloads)
        """
        self.storage_root = storage_root
        self.pool_dir = Path(storage_root) / ".storage" / "tracks"
        self.pool_dir.mkdir(parents=True, exist_ok=True)
        logger.info(f"StorageManager initialized with pool: {self.pool_dir}")

    async def get_file_hash(self, file_path: str) -> str:
        """
        Calculate SHA-1 hash of file for content-based identification.
        Runs in a thread pool to avoid blocking the async event loop.

        Args:
            file_path: Path to file to hash

        Returns:
            SHA-1 hexdigest
        """
        loop = asyncio.get_event_loop()
        return await loop.run_in_executor(None, self._hash_file_sync, file_path)

    @staticmethod
    def _hash_file_sync(file_path: str) -> str:
        """Synchronous SHA-1 hash computation (called via executor)."""
        sha1 = hashlib.sha1()
        try:
            with open(file_path, 'rb') as f:
                for chunk in iter(lambda: f.read(65536), b''):
                    sha1.update(chunk)
            return sha1.hexdigest()
        except Exception as e:
            logger.error(f"Failed to hash file {file_path}: {e}")
            raise

    async def store_or_link(
        self,
        source_file: str,
        target_path: str,
        database: Any,
        user_chat_id: int,
        youtube_url: str = None,
        title: str = None,
        use_symlink: bool = True
    ) -> Tuple[bool, str]:
        """
        Store file in central pool or create symlink to existing copy.

        If file is new (not in pool):
            1. Calculate SHA-1 hash
            2. Move to .storage/tracks/<hash>/original_file.<ext>
            3. Store metadata in .storage/tracks/<hash>/metadata.json
            4. Create symlink in user directory
            5. Track in database

        If file already exists (same content):
            1. Calculate SHA-1 hash
            2. Find existing file in pool
            3. Create symlink to existing file
            4. Track in database

        Args:
            source_file: Path to downloaded file
            target_path: Where user expects file (e.g. /downloads/<chat_id>/<task_id>/song.mp3)
            database: Database instance for tracking
            user_chat_id: Chat ID of user
            youtube_url: YouTube URL of source (optional)
            title: Track title (optional)
            use_symlink: Whether to create symlink or copy (default: True)

        Returns:
            (success: bool, final_path: str)
        """
        if not os.path.exists(source_file):
            logger.error(f"Source file does not exist: {source_file}")
            return False, target_path

        try:
            # Calculate file hash
            file_hash = await self.get_file_hash(source_file)
            file_size = os.path.getsize(source_file)
            file_ext = Path(source_file).suffix[1:] if Path(source_file).suffix else 'mp3'

            logger.debug(f"File hash: {file_hash} (size: {file_size} bytes)")

            # Determine pool path
            pool_hash_dir = self.pool_dir / file_hash
            pool_file = pool_hash_dir / f"original_file.{file_ext}"

            # Check if file already exists in pool
            if pool_file.exists():
                logger.info(f"File already in pool: {file_hash}")

                # Update youtube_url if caller provides a specific video URL
                # (fixes old entries that stored a playlist URL instead of individual video URL)
                if youtube_url and 'watch?v=' in youtube_url and 'list=' not in youtube_url:
                    try:
                        await database.execute(
                            'UPDATE file_storage SET youtube_url = ? WHERE file_hash_sha1 = ? AND youtube_url != ?',
                            [youtube_url, file_hash, youtube_url]
                        )
                        await database.connection.commit()
                    except Exception as e:
                        logger.debug(f"Failed to update youtube_url for {file_hash}: {e}")

                if use_symlink:
                    # Create symlink to existing file
                    await self._create_symlink(
                        pool_file, target_path, file_hash, database, user_chat_id
                    )
                    # Clean up source file
                    try:
                        os.remove(source_file)
                    except Exception as e:
                        logger.warning(f"Failed to remove temp source file: {e}")
                    return True, target_path
                else:
                    # Copy instead of symlink (user opted out of dedup)
                    os.makedirs(os.path.dirname(target_path), exist_ok=True)
                    shutil.copy2(source_file, target_path)
                    try:
                        os.remove(source_file)
                    except:
                        pass
                    return True, target_path

            else:
                # New file - move to pool and create symlink
                logger.info(f"Storing new file in pool: {file_hash}")

                # Ensure pool directory exists
                pool_hash_dir.mkdir(parents=True, exist_ok=True)

                # Move file to pool
                shutil.move(source_file, str(pool_file))
                logger.debug(f"Moved file to pool: {pool_file}")

                # Store metadata
                metadata = {
                    "size": file_size,
                    "hash": file_hash,
                    "extension": file_ext,
                    "youtube_url": youtube_url or "unknown",
                    "title": title or "unknown",
                    "downloaded_at": datetime.now().isoformat(),
                    "access_count": 1,
                    "last_accessed_at": datetime.now().isoformat()
                }
                metadata_file = pool_hash_dir / "metadata.json"
                metadata_file.write_text(json.dumps(metadata, indent=2))
                logger.debug(f"Stored metadata: {metadata_file}")

                # Track in database
                try:
                    await database.execute('''
                        INSERT OR IGNORE INTO file_storage
                        (file_hash_sha1, physical_path, file_size_bytes, file_extension,
                         youtube_url, title, is_protected)
                        VALUES (?, ?, ?, ?, ?, ?, ?)
                    ''', [
                        file_hash,
                        str(pool_file),
                        file_size,
                        file_ext,
                        youtube_url or "unknown",
                        title or "unknown",
                        True  # Protect physical pool file
                    ])
                    await database.connection.commit()
                    logger.debug(f"Tracked in database: {file_hash}")
                except Exception as e:
                    logger.error(f"Failed to track file in database: {e}")

                # Create symlink in user directory
                if use_symlink:
                    await self._create_symlink(
                        pool_file, target_path, file_hash, database, user_chat_id
                    )
                    return True, target_path
                else:
                    # Copy from pool to user directory
                    os.makedirs(os.path.dirname(target_path), exist_ok=True)
                    shutil.copy2(str(pool_file), target_path)
                    return True, target_path

        except Exception as e:
            logger.error(f"Error in store_or_link: {e}")
            return False, target_path

    async def _create_symlink(
        self,
        pool_file: Path,
        target_path: str,
        file_hash: str,
        database: Any,
        user_chat_id: int
    ) -> None:
        """
        Create symlink from user directory to pool file.

        Args:
            pool_file: Path to file in pool
            target_path: Where to create symlink
            file_hash: SHA-1 hash of file
            database: Database instance
            user_chat_id: Chat ID of user
        """
        try:
            # Ensure target directory exists
            os.makedirs(os.path.dirname(target_path), exist_ok=True)

            # Calculate relative path from target to pool file
            rel_path = os.path.relpath(str(pool_file), os.path.dirname(target_path))

            # Remove target if it exists
            if os.path.exists(target_path) or os.path.islink(target_path):
                try:
                    os.remove(target_path)
                except Exception as e:
                    logger.warning(f"Failed to remove existing target: {e}")

            # Create symlink
            if os.name == 'nt':  # Windows
                # On Windows, use relative path with backslash conversion
                rel_path_windows = rel_path.replace('/', '\\')
                os.symlink(rel_path_windows, target_path, target_is_directory=False)
            else:  # Unix-like
                os.symlink(rel_path, target_path)

            logger.info(f"Created symlink: {target_path} -> {rel_path}")

            # Track in database
            try:
                await database.execute('''
                    INSERT OR REPLACE INTO user_symlinks
                    (user_chat_id, file_hash_sha1, symlink_path, is_protected, created_at)
                    VALUES (?, ?, ?, ?, ?)
                ''', [
                    user_chat_id,
                    file_hash,
                    target_path,
                    False,  # Not protected by default (can be marked protected later)
                    datetime.now().isoformat()
                ])
                await database.connection.commit()
                logger.debug(f"Tracked symlink in database: {target_path}")
            except Exception as e:
                logger.error(f"Failed to track symlink in database: {e}")

        except Exception as e:
            logger.error(f"Failed to create symlink: {e}")
            raise

    async def get_pool_file_info(self, file_hash: str) -> Optional[dict]:
        """
        Get information about a file in the pool.

        Args:
            file_hash: SHA-1 hash of file

        Returns:
            File info dict or None if not found
        """
        try:
            hash_dir = self.pool_dir / file_hash
            metadata_file = hash_dir / "metadata.json"

            if metadata_file.exists():
                metadata = json.loads(metadata_file.read_text())
                return metadata
            return None
        except Exception as e:
            logger.error(f"Failed to get pool file info: {e}")
            return None

    async def cleanup_broken_symlinks(self, directory: str) -> Tuple[int, int]:
        """
        Clean up broken symlinks in a directory.

        Args:
            directory: Directory to scan

        Returns:
            (repaired_count, removed_count)
        """
        repaired = 0
        removed = 0

        try:
            for root, dirs, files in os.walk(directory):
                # Skip .storage directory
                if '.storage' in root:
                    continue

                for filename in files:
                    file_path = os.path.join(root, filename)

                    if os.path.islink(file_path):
                        if not os.path.exists(file_path):
                            # Symlink is broken
                            logger.warning(f"Broken symlink detected: {file_path}")
                            try:
                                os.remove(file_path)
                                removed += 1
                                logger.debug(f"Removed broken symlink: {file_path}")
                            except Exception as e:
                                logger.error(f"Failed to remove broken symlink: {e}")
        except Exception as e:
            logger.error(f"Error during symlink cleanup: {e}")

        return repaired, removed
