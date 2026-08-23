"""Do not outlive the client.

An MCP server over stdio has no lifecycle of its own: it is a child process
whose entire reason to exist is the client on the other end of the pipe. This
one had no hygiene at all — `serve()` was `create_server(graph)` followed by
`server.run(transport="stdio")` — so three ways of being left behind were all
open at once:

- a client that connects and never sends `initialize` leaves the server
  waiting forever;
- a client that is killed rather than closed leaves an orphan holding an
  index of a project nobody is working on;
- an editor restarted a dozen times leaves a dozen of them.

Three small guards, all of which end in the same place: log the reason and
leave. There is nothing to flush — the index is a rebuildable in-memory
projection, and the store is written as it goes.
"""

from __future__ import annotations

import os
import sys
import threading
from typing import Any, Awaitable, Callable, Optional

import structlog

log = structlog.get_logger(__name__)

#: How long to wait for `initialize` before concluding nobody is coming.
HANDSHAKE_TIMEOUT_SECONDS = float(
    os.environ.get("CODERADAR_HANDSHAKE_TIMEOUT", "60"))

#: How often to check that the process that launched us is still there.
PARENT_POLL_SECONDS = float(os.environ.get("CODERADAR_PARENT_POLL", "5"))


def _leave(reason: str, code: int = 0) -> None:
    """Log why, then go — without unwinding through the transport.

    `sys.exit` from a watchdog thread raises `SystemExit` in that thread and
    nowhere else, which is exactly the thread that is not running the server.
    The index is a rebuildable projection and the store is written as it
    goes, so there is nothing that a graceful unwind would save.
    """
    log.info("mcp.lifecycle.exit", reason=reason)
    try:
        sys.stderr.flush()
    except Exception:  # noqa: BLE001
        pass
    os._exit(code)


# ── parent liveness ───────────────────────────────────────────────────────


def _parent_is_alive(original_ppid: int) -> bool:
    """Is the process that launched us still running?

    POSIX and Windows disagree about what a dead parent looks like. On POSIX
    an orphan is reparented, so the ppid simply changes. On Windows the ppid
    is a snapshot that keeps naming a process that no longer exists, so the
    handle has to be asked directly — and `os.kill(pid, 0)` is not the probe
    it is on POSIX: on Windows it terminates the target.
    """
    if os.name != "nt":
        if os.getppid() != original_ppid:
            return False
        try:
            os.kill(original_ppid, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            return True  # alive, just not ours to signal
        return True

    import ctypes
    from ctypes import wintypes

    SYNCHRONIZE = 0x00100000
    WAIT_OBJECT_0 = 0x00000000

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]

    handle = kernel32.OpenProcess(SYNCHRONIZE, False, original_ppid)
    if not handle:
        # No such process — or no rights to it. Treating "cannot tell" as
        # dead would kill a healthy server, so only a missing process counts.
        return ctypes.get_last_error() != 87  # ERROR_INVALID_PARAMETER
    try:
        return kernel32.WaitForSingleObject(handle, 0) != WAIT_OBJECT_0
    finally:
        kernel32.CloseHandle(handle)


class ParentWatchdog:
    """Exit when the client process goes away.

    A client that is killed rather than closed never closes the pipe in a way
    this process notices, and an orphaned server holds an index of a project
    nobody is working on until the machine reboots.
    """

    def __init__(
        self,
        interval: float = PARENT_POLL_SECONDS,
        on_orphan: Callable[[str], None] = _leave,
        alive: Optional[Callable[[int], bool]] = None,
    ):
        self._interval = interval
        self._on_orphan = on_orphan
        self._alive = alive or _parent_is_alive
        self._ppid = os.getppid()
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None

    def start(self) -> None:
        if self._thread is not None:
            return
        # PID 1 (or 0) as a parent means we were started by init or the
        # kernel; there is nothing meaningful to watch.
        if self._ppid <= 1:
            log.debug("mcp.lifecycle.no_parent_to_watch", ppid=self._ppid)
            return
        self._thread = threading.Thread(
            target=self._run, name="coderadar-parent-watchdog", daemon=True)
        self._thread.start()

    def _run(self) -> None:
        while not self._stop.wait(self._interval):
            if not self._alive(self._ppid):
                self._on_orphan(f"parent process {self._ppid} exited")
                return

    def stop(self) -> None:
        self._stop.set()

    def check_once(self) -> bool:
        """Run one poll's worth of logic. Returns whether the parent is alive."""
        return self._alive(self._ppid)


# ── handshake timeout ─────────────────────────────────────────────────────


class HandshakeTimeout:
    """Exit if `initialize` never arrives.

    Armed before the transport starts and disarmed by the first inbound
    message. A connection that never speaks is not a connection worth
    holding an index for.
    """

    def __init__(
        self,
        seconds: float = HANDSHAKE_TIMEOUT_SECONDS,
        on_timeout: Callable[[str], None] = _leave,
    ):
        self._seconds = seconds
        self._on_timeout = on_timeout
        self._timer: Optional[threading.Timer] = None
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
        self._on_timeout(
            f"no MCP handshake within {self._seconds:.0f}s")

    def disarm(self) -> None:
        """Idempotent — every inbound message calls this."""
        with self._lock:
            if self.disarmed:
                return
            self.disarmed = True
            timer, self._timer = self._timer, None
        if timer is not None:
            timer.cancel()


# ── module-level handle and middleware ────────────────────────────────────

_HANDSHAKE: Optional[HandshakeTimeout] = None
_HANDSHAKE_LOCK = threading.Lock()


def configure(handshake: Optional[HandshakeTimeout]) -> None:
    global _HANDSHAKE
    with _HANDSHAKE_LOCK:
        _HANDSHAKE = handshake


def current() -> Optional[HandshakeTimeout]:
    with _HANDSHAKE_LOCK:
        return _HANDSHAKE


def make_middleware() -> Callable[..., Awaitable[Any]]:
    """Disarm the handshake timeout as soon as the client says anything.

    Any inbound message counts, not just `initialize`: a client that is
    talking is a client that is there, and refusing to disarm on a message
    the SDK will reject anyway would kill a live connection.
    """

    async def middleware(ctx: Any, call_next: Callable[[Any], Awaitable[Any]]) -> Any:
        handshake = current()
        if handshake is not None and not handshake.disarmed:
            handshake.disarm()
        return await call_next(ctx)

    return middleware


def install(seconds: Optional[float] = None) -> tuple[HandshakeTimeout, ParentWatchdog]:
    """Arm both guards. Call from `serve` before the transport starts."""
    handshake = HandshakeTimeout(
        HANDSHAKE_TIMEOUT_SECONDS if seconds is None else seconds)
    configure(handshake)
    handshake.arm()

    watchdog = ParentWatchdog()
    watchdog.start()
    return handshake, watchdog
