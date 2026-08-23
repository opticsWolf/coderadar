"""Ask the client where the project is, once, on the first tool call.

The ladder in `roots.py` has three rungs, and only two of them can be climbed
at startup. `roots/list` is a server-to-client request, and MCP gives the
server no `initialize` hook to send one from — worse, awaiting a
server-to-client request during `initialize` deadlocks, because the
dispatcher does not read further inbound messages until the handshake
returns.

So the top rung is climbed lazily: the first inbound tool call carries a
session, the session can ask, and by then the handshake is long finished.
This runs exactly once per connection, and only when startup's answer was a
guess — an unconfirmed root, or an index that came back empty. A confirmed
marker is better evidence than a client's declared workspace and is left
alone.
"""

from __future__ import annotations

import os
import threading


from typing import Any, Awaitable, Callable, Optional

import structlog

from .roots import (
    ResolvedRoot,
    adopt_project_root,
    client_roots,
    describe,
    resolve_from_client_roots,
)
from .startup import BackgroundIndex

log = structlog.get_logger(__name__)


class LazyRootRetry:
    """One connection's worth of "did we land in the right project?"."""

    def __init__(
        self,
        resolved: ResolvedRoot,
        index: BackgroundIndex,
        path_flag: Optional[str] = None,
        index_is_empty: Optional[Callable[[], bool]] = None,
    ):
        self.resolved = resolved
        self._index = index
        self._path_flag = path_flag
        self._index_is_empty = index_is_empty or _index_is_empty
        self._lock = threading.Lock()
        self._attempted = False

    # ── decision ─────────────────────────────────────────────────────────

    def should_ask(self) -> bool:
        """Is there anything the client could tell us that we do not know?

        Two cases, and only two. An unconfirmed root means nothing on disk
        agreed with our guess. An empty index means we walked a real
        directory and found no code in it, which usually means it was the
        wrong directory. A confirmed root that produced entities is not
        improved by asking.
        """
        with self._lock:
            if self._attempted:
                return False
        if not self.resolved.confirmed:
            return True
        return self._index_is_empty()

    # ── action ───────────────────────────────────────────────────────────

    async def attempt(self, session: Any) -> bool:
        """Ask, re-resolve, and move if the answer is better. Returns whether
        anything changed."""
        with self._lock:
            if self._attempted:
                return False
            self._attempted = True

        uris = await client_roots(session)
        if not uris:
            return False

        candidate = resolve_from_client_roots(uris)
        if candidate is None or candidate.path == self.resolved.path:
            return False
        # Only trade up. A client root with no marker in it is not better
        # evidence than the marker we already found.
        if self.resolved.confirmed and not candidate.confirmed:
            return False

        log.info(
            "mcp.root.retargeted",
            was=str(self.resolved.path),
            now=str(candidate.path),
            source=candidate.source,
        )
        self.resolved = candidate
        adopt_project_root(candidate)
        # '.' again, deliberately: entity ids carry the prefix analyze walked,
        # and `_reindex` re-walks '.'. See roots.adopt_project_root.
        self._index.restart(".")
        return True

    def describe(self) -> str:
        return describe(self.resolved)


def _index_is_empty() -> bool:
    try:
        from coderadar._core import graph_stats
        return graph_stats().get("modules", 0) == 0
    except (ImportError, RuntimeError):
        return True


# ── module-level handle ───────────────────────────────────────────────────

_RETRY: Optional[LazyRootRetry] = None
_RETRY_LOCK = threading.Lock()


def configure(retry: Optional[LazyRootRetry]) -> None:
    global _RETRY
    with _RETRY_LOCK:
        _RETRY = retry


def current() -> Optional[LazyRootRetry]:
    with _RETRY_LOCK:
        return _RETRY


def make_middleware() -> Callable[..., Awaitable[Any]]:
    """Server middleware that runs the retry before the first tool call.

    Middleware is the one place in this SDK that sees every inbound request
    *and* holds the session, which is what `roots/list` needs. Anything it
    raises would fail the request it wraps, so it swallows its own failures:
    a client that will not answer a roots request is a reason to keep serving
    the root we have.
    """

    async def middleware(ctx: Any, call_next: Callable[[Any], Awaitable[Any]]) -> Any:
        if getattr(ctx, "method", None) == "tools/call":
            retry = current()
            if retry is not None and retry.should_ask():
                try:
                    await retry.attempt(ctx.session)
                except Exception:  # noqa: BLE001
                    log.warning("mcp.root.retry_failed", exc_info=True)
        return await call_next(ctx)

    return middleware
