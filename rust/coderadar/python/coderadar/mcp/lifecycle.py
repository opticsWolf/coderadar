"""Do not outlive the client.

Both guards are host-process concerns, so the threads/timers that watch live
here — but the liveness probe itself is the engine's
(`core_indexer::mcp_runtime::process_alive`): on Windows it asks the process
handle instead of `os.kill(pid, 0)`, which would terminate the target.
"""

from __future__ import annotations

import os
import sys
import threading
from collections.abc import Awaitable, Callable
from typing import Any

from .. import lifecycle_parent_alive as _parent_alive

#: How long to wait for `initialize` before concluding nobody is coming.
HANDSHAKE_TIMEOUT_SECONDS = float(os.environ.get("CODERADAR_HANDSHAKE_TIMEOUT", "60"))

#: How often to check that the process that launched us is still there.
PARENT_POLL_SECONDS = float(os.environ.get("CODERADAR_PARENT_POLL", "5"))


def _leave(reason: str, code: int = 0) -> None:
    """Log why, then go — without unwinding through the transport."""
    try:
        print(f"[coderadar] mcp.lifecycle.exit reason={reason}", file=sys.stderr, flush=True)
    except Exception:  # noqa: BLE001 - exiting anyway
        pass
    os._exit(code)


def _parent_is_alive(original_ppid: int) -> bool:
    """Is the process that launched us still running?"""
    if os.name != "nt" and os.getppid() != original_ppid:
        return False
    return _parent_alive(original_ppid)


class ParentWatchdog:
    """Exit when the client process goes away."""

    def __init__(
        self,
        interval: float = PARENT_POLL_SECONDS,
        on_orphan: Callable[[str], None] = _leave,
        alive: Callable[[int], bool] | None = None,
    ):
        self._interval = interval
        self._on_orphan = on_orphan
        self._alive = alive or _parent_is_alive
        self._ppid = os.getppid()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread is not None:
            return
        if self._ppid <= 1:
            return
        self._thread = threading.Thread(
            target=self._run, name="coderadar-parent-watchdog", daemon=True
        )
        self._thread.start()

    def _run(self) -> None:
        while not self._stop.wait(self._interval):
            if self._stop.is_set():
                return
            if not self._alive(self._ppid):
                self._on_orphan(f"parent process {self._ppid} exited")
                return

    def stop(self) -> None:
        """Stop polling. Returns once no further poll can happen."""
        self._stop.set()
        thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=max(1.0, self._interval * 10))

    def check_once(self) -> bool:
        """Run one poll's worth of logic. Returns whether the parent is alive."""
        return self._alive(self._ppid)


class HandshakeTimeout:
    """Exit if `initialize` never arrives."""

    def __init__(
        self,
        seconds: float = HANDSHAKE_TIMEOUT_SECONDS,
        on_timeout: Callable[[str], None] = _leave,
    ):
        self._seconds = seconds
        self._on_timeout = on_timeout
        self._timer: threading.Timer | None = None
        self._lock = threading.Lock()
        self.disarmed = False

    def arm(self) -> None:
        if self._seconds <= 0:
            return
        with self._lock:
            if self._timer is not None or self.disarmed:
                return
            self._timer = threading.Timer(self._seconds, self._fire)
            self._timer.daemon = True
            self._timer.start()

    def _fire(self) -> None:
        with self._lock:
            if self.disarmed:
                return
        self._on_timeout(f"no MCP handshake within {self._seconds:.0f}s")

    def disarm(self) -> None:
        """Idempotent — every inbound message calls this."""
        with self._lock:
            if self.disarmed:
                return
            self.disarmed = True
            timer, self._timer = self._timer, None
        if timer is not None:
            timer.cancel()


_HANDSHAKE: HandshakeTimeout | None = None


def configure(handshake: HandshakeTimeout | None) -> None:
    global _HANDSHAKE
    _HANDSHAKE = handshake


def current() -> HandshakeTimeout | None:
    return _HANDSHAKE


def make_middleware() -> Callable[..., Awaitable[Any]]:
    """Disarm the handshake timeout as soon as the client says anything."""

    async def middleware(ctx: Any, call_next: Callable[[Any], Awaitable[Any]]) -> Any:
        handshake = current()
        if handshake is not None and not handshake.disarmed:
            handshake.disarm()
        return await call_next(ctx)

    return middleware


def install(seconds: float | None = None) -> tuple[HandshakeTimeout, ParentWatchdog]:
    """Arm both guards. Call from `serve` before the transport starts."""
    handshake = HandshakeTimeout(HANDSHAKE_TIMEOUT_SECONDS if seconds is None else seconds)
    configure(handshake)
    handshake.arm()
    watchdog = ParentWatchdog()
    watchdog.start()
    return handshake, watchdog


__all__ = [
    "HANDSHAKE_TIMEOUT_SECONDS",
    "PARENT_POLL_SECONDS",
    "HandshakeTimeout",
    "ParentWatchdog",
    "configure",
    "current",
    "install",
    "make_middleware",
]
