"""
Hermes Media Worker Package
Multi-language download nexus - Python intelligence layer
"""

__version__ = '1.0.0-alpha'
__author__ = 'Hermes Team'
__description__ = 'Media intelligence worker for Hermes Download Nexus'

from worker.config import config, WorkerConfig
from worker.ipc import ipc_handler, IPCHandler
from worker.error_handlers import get_error, categorize_error, WorkerError

__all__ = [
    'config',
    'WorkerConfig',
    'ipc_handler',
    'IPCHandler',
    'get_error',
    'categorize_error',
    'WorkerError',
]
