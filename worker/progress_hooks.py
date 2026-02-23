"""
Progress parsing for yt-dlp output
Extracts real-time progress, speed, and ETA from yt-dlp stderr
"""

import re
import logging
from typing import Optional, Dict, Any
from dataclasses import dataclass


logger = logging.getLogger(__name__)


@dataclass
class DownloadProgress:
    """Structured download progress."""
    percent: int = 0
    speed: str = "0 B/s"
    eta_seconds: int = 0
    downloaded_bytes: int = 0
    total_bytes: int = 0
    status: str = "pending"  # downloading|converting|done

    def to_dict(self) -> dict:
        """Convert to dictionary for IPC."""
        return {
            'percent': self.percent,
            'speed': self.speed,
            'eta': self.eta_seconds,
            'downloaded': self.downloaded_bytes,
            'total': self.total_bytes,
            'status': self.status,
        }


class ProgressParser:
    """Parse yt-dlp progress output."""

    # Regex patterns for yt-dlp output lines
    PATTERN_PROGRESS = re.compile(
        r'\[download\]\s+(?P<percent>\d+\.\d+)%.*?'
        r'at\s+(?P<speed>\S+)\s+'
        r'ETA\s+(?P<eta>\S+)'
    )

    PATTERN_DOWNLOADING = re.compile(
        r'\[download\]\s+(?P<percent>\d+\.\d+)%'
    )

    PATTERN_CONVERTING = re.compile(
        r'\[ExtractAudio\].*Converting.*'
    )

    PATTERN_DESTINATION = re.compile(
        r'\[(?:ExtractAudio|download|Merger)\]\s+Destination:\s+(?P<path>.+)$'
    )

    PATTERN_ALREADY_EXISTS = re.compile(
        r'.*already exists.*'
    )

    PATTERN_ALREADY_DOWNLOADED = re.compile(
        r'\[download\]\s+(?P<path>.+?)\s+has already been downloaded'
    )

    @staticmethod
    def parse_line(line: str, current_progress: Optional[DownloadProgress] = None) -> Optional[Dict[str, Any]]:
        """
        Parse single yt-dlp output line.

        Args:
            line: Output line from yt-dlp stderr
            current_progress: Previous progress state

        Returns:
            If line contains progress info: dict with 'progress' key
            If line contains completion info: dict with 'done' or 'status' key
            Otherwise: None
        """
        if not line:
            return None

        progress = current_progress or DownloadProgress()

        # Check for progress line with full info
        match = ProgressParser.PATTERN_PROGRESS.search(line)
        if match:
            progress.percent = int(float(match.group('percent')))
            progress.speed = match.group('speed')
            progress.eta_seconds = ProgressParser.parse_eta(match.group('eta'))
            progress.status = 'downloading'
            return {'progress': progress.to_dict()}

        # Check for simple progress percentage
        match = ProgressParser.PATTERN_DOWNLOADING.search(line)
        if match:
            progress.percent = int(float(match.group('percent')))
            progress.status = 'downloading'
            return {'progress': progress.to_dict()}

        # Check for audio conversion
        if ProgressParser.PATTERN_CONVERTING.search(line):
            progress.status = 'converting'
            progress.percent = min(95, progress.percent + 2)  # Bump up toward 95%
            return {'progress': progress.to_dict()}

        # Check for destination file
        match = ProgressParser.PATTERN_DESTINATION.search(line)
        if match:
            return {'destination': match.group('path')}

        # Check for "has already been downloaded" (captures file path)
        match = ProgressParser.PATTERN_ALREADY_DOWNLOADED.search(line)
        if match:
            return {'destination': match.group('path'), 'done': True}

        # Check for file already exists
        if ProgressParser.PATTERN_ALREADY_EXISTS.search(line):
            return {'already_exists': True}

        # Check for completion indicators
        if '[youtube]' in line and 'Downloading' not in line and line.strip():
            # Could indicate video info fetched or processing
            return None

        # Check for error indicators
        if 'ERROR' in line.upper() or 'error' in line:
            return {'error': line.strip()}

        # Check for completion
        if '[download]' in line and '100%' in line:
            progress.percent = 100
            progress.status = 'done'
            return {'progress': progress.to_dict(), 'done': True}

        return None

    @staticmethod
    def parse_eta(eta_str: str) -> int:
        """
        Parse ETA string to seconds.

        Args:
            eta_str: ETA string like "2:30", "1:45:30", "Unknown"

        Returns:
            Estimated seconds, or 0 if unparseable
        """
        if not eta_str or eta_str.lower() == 'unknown':
            return 0

        try:
            parts = eta_str.split(':')

            if len(parts) == 2:
                minutes, seconds = int(parts[0]), int(parts[1])
                return minutes * 60 + seconds

            elif len(parts) == 3:
                hours, minutes, seconds = int(parts[0]), int(parts[1]), int(parts[2])
                return hours * 3600 + minutes * 60 + seconds

        except (ValueError, IndexError):
            pass

        return 0

    @staticmethod
    def parse_size(size_str: str) -> int:
        """
        Parse file size string to bytes.

        Args:
            size_str: Size string like "1.2MB", "500KB", "1GB"

        Returns:
            Size in bytes
        """
        if not size_str:
            return 0

        multipliers = {
            'B': 1,
            'KB': 1024,
            'MB': 1024 ** 2,
            'GB': 1024 ** 3,
            'TB': 1024 ** 4,
        }

        for unit, mult in multipliers.items():
            if unit in size_str.upper():
                try:
                    number = float(size_str.replace(unit, '').strip())
                    return int(number * mult)
                except ValueError:
                    return 0

        return 0


class StreamProgressCollector:
    """Collect progress from yt-dlp process stream."""

    def __init__(self):
        self.current_progress = DownloadProgress()
        self.last_percent = 0
        self.updates_since_last_emit = 0
        self.throttle_threshold = 2  # Emit every 2 updates (prevents spam)

    def process_line(self, line: str) -> Optional[Dict[str, Any]]:
        """
        Process output line and return appropriate event if needed.

        Args:
            line: Line from yt-dlp output

        Returns:
            Event dict if significant change, None otherwise
        """
        result = ProgressParser.parse_line(line, self.current_progress)

        if not result:
            return None

        # Update current progress
        if 'progress' in result:
            self.current_progress = DownloadProgress(**(result['progress']))
            self.updates_since_last_emit += 1

            # Throttle: only emit if significant change or threshold reached
            percent_change = abs(self.current_progress.percent - self.last_percent)
            if percent_change >= 5 or self.updates_since_last_emit >= self.throttle_threshold:
                self.last_percent = self.current_progress.percent
                self.updates_since_last_emit = 0
                return result

        if 'destination' in result or 'done' in result or 'error' in result:
            # Always emit these
            return result

        return None

    def get_current_progress(self) -> DownloadProgress:
        """Get current progress state."""
        return self.current_progress

    def reset(self):
        """Reset for next download."""
        self.current_progress = DownloadProgress()
        self.last_percent = 0
        self.updates_since_last_emit = 0
