"""
IPC Protocol Handler for Hermes Media Worker
Handles JSON communication via stdin/stdout with Rust bot
"""

import json
import sys
import logging
from typing import Dict, Callable, Optional, Any
from dataclasses import asdict


# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s',
    stream=sys.stderr  # Log to stderr so stdout stays clean for IPC
)
logger = logging.getLogger(__name__)


class IPCHandler:
    """
    Handles IPC communication between Rust bot and Python worker.

    Communication is line-delimited JSON:
    - Rust sends requests via stdin
    - Python sends responses via stdout
    """

    def __init__(self):
        self.handlers: Dict[str, Callable] = {}
        self.request_count = 0
        self.response_count = 0

    def register(self, action: str, handler: Callable) -> None:
        """
        Register handler for IPC action.

        Args:
            action: Action name (e.g., 'youtube_dl', 'search')
            handler: Async function to handle action
        """
        self.handlers[action] = handler
        logger.info(f"Registered handler for action: {action}")

    def send_response(self, task_id: str, event: str, data: Optional[Dict[str, Any]] = None) -> None:
        """
        Send response message to Rust bot via stdout.

        Args:
            task_id: Task ID from request
            event: Event type (e.g., 'progress', 'done', 'error')
            data: Event data dictionary
        """
        response = {
            'task_id': task_id,
            'event': event,
            'data': data or {}
        }

        try:
            json_str = json.dumps(response)
            print(json_str, flush=True)
            self.response_count += 1

            if self.response_count % 10 == 0:
                logger.debug(f"Sent {self.response_count} responses total")

        except Exception as e:
            logger.error(f"Failed to serialize response: {e}", exc_info=True)
            self.send_error(task_id, f"Serialization error: {e}")

    def send_error(self, task_id: str, message: str, error_code: Optional[str] = None) -> None:
        """
        Send error message to Rust bot.

        Args:
            task_id: Task ID
            message: Human-readable error message
            error_code: Optional error code
        """
        data = {
            'message': message,
            'error_code': error_code or 'UNKNOWN_ERROR'
        }
        self.send_response(task_id, 'error', data)
        logger.warning(f"Error response sent for task {task_id}: {message}")

    def send_progress(self, task_id: str, percent: int, speed: Optional[str] = None,
                      eta_seconds: Optional[int] = None, status: Optional[str] = None) -> None:
        """
        Send progress update to Rust bot.

        Args:
            task_id: Task ID
            percent: Progress percentage (0-100)
            speed: Download speed string (e.g., "1.2MB/s")
            eta_seconds: Estimated time remaining in seconds
            status: Status string (e.g., "downloading", "converting")
        """
        data = {
            'percent': min(100, max(0, percent)),
            'speed': speed or '',
            'eta': eta_seconds or 0,
            'status': status or 'processing'
        }
        self.send_response(task_id, 'progress', data)

    async def process_request(self, request: Dict[str, Any]) -> None:
        """
        Process single IPC request.

        Args:
            request: Parsed JSON request from stdin
        """
        try:
            task_id = request.get('task_id', 'unknown')
            action = request.get('action')

            if not action:
                self.send_error(task_id, "Missing 'action' field in request")
                return

            if action not in self.handlers:
                self.send_error(task_id, f"Unknown action: {action}")
                logger.warning(f"Unknown action requested: {action}")
                return

            # Call registered handler
            handler = self.handlers[action]
            logger.info(f"Processing action '{action}' for task {task_id}")

            # Handler is responsible for sending responses
            await handler(self, task_id, request)

        except Exception as e:
            task_id = request.get('task_id', 'unknown')
            self.send_error(task_id, f"Handler error: {str(e)}")
            logger.error(f"Exception in process_request: {e}", exc_info=True)

    async def run(self) -> None:
        """
        Main event loop - read JSON from stdin, dispatch to handlers.

        This runs indefinitely, reading one JSON object per line.
        """
        logger.info("🚀 Hermes Media Worker started")
        logger.info(f"Registered handlers: {list(self.handlers.keys())}")

        try:
            for line in sys.stdin:
                line = line.strip()

                # Skip empty lines
                if not line:
                    continue

                self.request_count += 1

                try:
                    request = json.loads(line)
                    logger.debug(f"Received request {self.request_count}: {request.get('action')}")

                    # Process request (handler will send responses)
                    await self.process_request(request)

                except json.JSONDecodeError as e:
                    logger.error(f"IPC JSON decode error: {e} for line: {line[:100]}")
                    self.send_error('unknown', f"Invalid JSON: {e}")

        except KeyboardInterrupt:
            logger.info("Worker interrupted by keyboard")
        except EOFError:
            logger.info("End of stdin reached, shutting down")
        except Exception as e:
            logger.critical(f"Fatal error in main loop: {e}", exc_info=True)

        logger.info(f"📊 Worker shutdown. Processed {self.request_count} requests, sent {self.response_count} responses")

    def validate_request(self, request: Dict[str, Any], required_fields: list) -> bool:
        """
        Validate request has required fields.

        Args:
            request: Request dictionary
            required_fields: List of required field names

        Returns:
            True if all fields present
        """
        for field in required_fields:
            if field not in request or request[field] is None:
                return False
        return True


# Global IPC handler instance
ipc_handler = IPCHandler()


def create_ipc_handler() -> IPCHandler:
    """Factory function to create IPC handler."""
    return ipc_handler


def send_response(task_id: str, event: str, data: Optional[Dict[str, Any]] = None) -> None:
    """Convenience function to send response."""
    ipc_handler.send_response(task_id, event, data)


def send_error(task_id: str, message: str, error_code: Optional[str] = None) -> None:
    """Convenience function to send error."""
    ipc_handler.send_error(task_id, message, error_code)


def send_progress(task_id: str, percent: int, speed: Optional[str] = None,
                  eta_seconds: Optional[int] = None, status: Optional[str] = None) -> None:
    """Convenience function to send progress."""
    ipc_handler.send_progress(task_id, percent, speed, eta_seconds, status)
