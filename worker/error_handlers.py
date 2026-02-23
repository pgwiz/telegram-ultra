"""
Unified error handling for Hermes Media Worker
Categorizes errors into transient vs permanent failures
"""

from enum import Enum
from dataclasses import dataclass
from typing import Optional


class ErrorCategory(Enum):
    """Error categories with retriability."""
    TRANSIENT = "transient"
    AUTH_RELATED = "auth_related"
    PERMANENT = "permanent"


@dataclass
class WorkerError:
    """Structured error representation."""
    code: str
    user_message: str
    technical_message: str
    category: ErrorCategory
    retriable: bool
    exception: Optional[Exception] = None

    def to_dict(self) -> dict:
        """Convert to JSON-serializable dict for IPC."""
        return {
            'code': self.code,
            'user_message': self.user_message,
            'technical_message': self.technical_message,
            'category': self.category.value,
            'retriable': self.retriable,
        }


# Error definitions
ERROR_DEFINITIONS = {
    # Transient errors (safe to retry)
    'NETWORK_TIMEOUT': WorkerError(
        code='NETWORK_TIMEOUT',
        user_message='Network timeout, retrying...',
        technical_message='Connection timeout while downloading',
        category=ErrorCategory.TRANSIENT,
        retriable=True,
    ),
    'SERVICE_UNAVAILABLE': WorkerError(
        code='SERVICE_UNAVAILABLE',
        user_message='YouTube service busy, retrying...',
        technical_message='YouTube service returned 503 or similar',
        category=ErrorCategory.TRANSIENT,
        retriable=True,
    ),
    'RATE_LIMITED': WorkerError(
        code='RATE_LIMITED',
        user_message='Too many requests, waiting before retry...',
        technical_message='HTTP 429 - Rate limited',
        category=ErrorCategory.TRANSIENT,
        retriable=True,
    ),
    'PARTIAL_DOWNLOAD': WorkerError(
        code='PARTIAL_DOWNLOAD',
        user_message='Download interrupted, retrying...',
        technical_message='Download was interrupted before completion',
        category=ErrorCategory.TRANSIENT,
        retriable=True,
    ),

    # Auth-related errors (often fixable by user)
    'REQUIRE_AUTH': WorkerError(
        code='REQUIRE_AUTH',
        user_message='Age-restricted content - need fresh cookies. Use /help for instructions.',
        technical_message='Video requires authentication',
        category=ErrorCategory.AUTH_RELATED,
        retriable=True,
    ),
    'COOKIE_EXPIRED': WorkerError(
        code='COOKIE_EXPIRED',
        user_message='Cookies expired. Export fresh cookies using browser extension.',
        technical_message='Cookie validation failed',
        category=ErrorCategory.AUTH_RELATED,
        retriable=True,
    ),
    'LOGIN_REQUIRED': WorkerError(
        code='LOGIN_REQUIRED',
        user_message='Video requires login. Check /help for cookie setup.',
        technical_message='Authentication required but not available',
        category=ErrorCategory.AUTH_RELATED,
        retriable=True,
    ),

    # Permanent failures (no retry)
    'VIDEO_PRIVATE': WorkerError(
        code='VIDEO_PRIVATE',
        user_message='Video is private or has been deleted.',
        technical_message='Video is private/deleted',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),
    'VIDEO_REMOVED': WorkerError(
        code='VIDEO_REMOVED',
        user_message='Video has been removed.',
        technical_message='Video removed from platform',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),
    'REGION_BLOCKED': WorkerError(
        code='REGION_BLOCKED',
        user_message='Video not available in your region.',
        technical_message='Geographic restriction',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),
    'UNAVAILABLE': WorkerError(
        code='UNAVAILABLE',
        user_message='Video is currently unavailable.',
        technical_message='Video unavailable',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),
    'INVALID_URL': WorkerError(
        code='INVALID_URL',
        user_message='Invalid YouTube URL provided.',
        technical_message='URL format invalid',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),
    'NO_SUITABLE_FORMAT': WorkerError(
        code='NO_SUITABLE_FORMAT',
        user_message='No downloadable format found for this video.',
        technical_message='No compatible audio/video format',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),
    'FILE_SIZE_EXCEEDS_LIMIT': WorkerError(
        code='FILE_SIZE_EXCEEDS_LIMIT',
        user_message=f'File too large (max 15MB for audio). Video exceeds limit.',
        technical_message='File size exceeds BEST_AUDIO_LIMIT_MB',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),

    # System errors
    'UNKNOWN_ERROR': WorkerError(
        code='UNKNOWN_ERROR',
        user_message='Unknown error occurred. Check logs.',
        technical_message='Unclassified error',
        category=ErrorCategory.PERMANENT,
        retriable=False,
    ),
}


def get_error(code: str, override_message: Optional[str] = None) -> WorkerError:
    """
    Get error definition by code.

    Args:
        code: Error code key
        override_message: Optional override for user_message

    Returns:
        WorkerError instance
    """
    error = ERROR_DEFINITIONS.get(code, ERROR_DEFINITIONS['UNKNOWN_ERROR'])

    if override_message:
        error = WorkerError(
            code=error.code,
            user_message=override_message,
            technical_message=error.technical_message,
            category=error.category,
            retriable=error.retriable,
        )

    return error


def categorize_error(exception: Exception) -> WorkerError:
    """
    Categorize an exception into a WorkerError.

    Args:
        exception: Python exception

    Returns:
        WorkerError instance
    """
    error_str = str(exception).lower()

    # Check for patterns in error message
    if 'timeout' in error_str or 'connection' in error_str:
        return get_error('NETWORK_TIMEOUT')

    if '403' in error_str or 'forbidden' in error_str:
        return get_error('REQUIRE_AUTH')

    if '429' in error_str or 'rate' in error_str:
        return get_error('RATE_LIMITED')

    if '503' in error_str or 'unavailable' in error_str:
        return get_error('SERVICE_UNAVAILABLE')

    if 'private' in error_str:
        return get_error('VIDEO_PRIVATE')

    if 'unavailable' in error_str or 'removed' in error_str:
        return get_error('UNAVAILABLE')

    if 'no suitable' in error_str or 'format' in error_str:
        return get_error('NO_SUITABLE_FORMAT')

    # Default to unknown
    error = get_error('UNKNOWN_ERROR')
    error.exception = exception
    return error
