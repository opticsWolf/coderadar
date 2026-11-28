"""Answer the handshake first, index second.

The index itself is a Rust handle (`core_indexer::mcp_runtime::BackgroundIndex`
behind the native `startup_*` functions): generation tracking, the shared build
lock, status/elapsed and the bounded, heart-beating wait are all engine-side.
This module is the Python-shaped view of that handle plus the two env knobs.

A custom `analyze` callable is still accepted — it is what the reference tests
stub the build with — and it runs *through* the same Rust worker: the native
closure calls back into Python for the build.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass, field
from enum import Enum

from .. import (
    startup_configure as _configure,
    startup_elapsed as _elapsed,
    startup_progress_message as _progress,
    startup_restart as _restart,
    startup_root as _root,
    startup_start as _start,
    startup_status as _status,
    startup_wait as _wait,
)


def _env_seconds(name: str, default: float, minimum: float = 1.0) -> float:
    """Read a seconds-valued env knob defensively."""
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        value = float(raw)
    except ValueError:
        return default
    return max(value, minimum)


#: How long a tool handler waits for a running index before answering with
#: progress instead.
DEFAULT_WAIT_SECONDS = _env_seconds("CODERADAR_INDEX_WAIT", 25)

#: Seconds between "still indexing" heartbeats on stderr while a handler waits.
HEARTBEAT_SECONDS = _env_seconds("CODERADAR_INDEX_HEARTBEAT", 5)


class IndexStatus(str, Enum):
    NOT_STARTED = "not_started"
    INDEXING = "indexing"
    READY = "ready"
    FAILED = "failed"


@dataclass
class IndexOutcome:
    """What `ensure_ready` found, and what the handler should do about it."""

    status: IndexStatus
    elapsed: float = 0.0
    error: str | None = None
    stats: dict = field(default_factory=dict)

    @property
    def ready(self) -> bool:
        return self.status is IndexStatus.READY


def _outcome(report: dict) -> IndexOutcome:
    return IndexOutcome(
        status=IndexStatus(report.get("status", "not_started")),
        elapsed=float(report.get("elapsed") or 0.0),
        error=report.get("error"),
        stats=dict(report.get("stats") or {}),
    )


class BackgroundIndex:
    """One project's index, built once, off the request path."""

    def __init__(self, root: str = ".", analyze=None):
        self._root = str(root)
        self._analyze = analyze
        self._started = False

    def _install(self) -> None:
        _configure(self._root, self._analyze)

    def start(self) -> None:
        """Begin indexing if it is not already begun."""
        if self._started:
            return
        self._install()
        _start()
        self._started = True

    def restart(self, root: str | None = None) -> None:
        """Index again, optionally somewhere else."""
        if root is not None:
            self._root = str(root)
        self._started = False
        self._install()
        _restart(self._root)
        self._started = True

    @property
    def root(self) -> str:
        return self._root if not self._started else _root()

    @property
    def status(self) -> IndexStatus:
        if not self._started:
            return IndexStatus.NOT_STARTED
        return IndexStatus(_status().get("status", "not_started"))

    @property
    def elapsed(self) -> float:
        return _elapsed() if self._started else 0.0

    def wait(self, timeout: float | None = None) -> IndexOutcome:
        """Start if needed, wait up to `timeout`, and report where we got to."""
        self.start()
        return _outcome(_wait(timeout))


# ── module-level handle ───────────────────────────────────────────────────

_INDEX: BackgroundIndex | None = None


def configure(index: BackgroundIndex | None) -> None:
    """Install the handle every handler will consult. None clears it."""
    global _INDEX
    _INDEX = index


def current() -> BackgroundIndex | None:
    return _INDEX


def ensure_ready(timeout: float | None = None) -> IndexOutcome:
    """Called by every tool handler before it touches the graph."""
    index = current()
    if index is None:
        return IndexOutcome(status=IndexStatus.READY)
    return index.wait(timeout)


def progress_message(outcome: IndexOutcome) -> str:
    """What to tell the agent when the index is not ready yet."""
    report = {
        "status": outcome.status.value,
        "elapsed": outcome.elapsed,
        "error": outcome.error,
    }
    return _progress(json.dumps(report))


__all__ = [
    "DEFAULT_WAIT_SECONDS",
    "HEARTBEAT_SECONDS",
    "BackgroundIndex",
    "IndexOutcome",
    "IndexStatus",
    "configure",
    "current",
    "ensure_ready",
    "progress_message",
]
