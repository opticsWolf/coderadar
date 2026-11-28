"""Ask the client where the project is, once, on the first tool call.

The three rungs and the trade-up rule live in Rust (`mcp.roots` and the
native `lazy_should_ask` / `lazy_trade_up`). This module keeps what cannot
cross the FFI: the per-connection "already attempted" flag, the async
`roots/list` call on the session, and the server middleware shape.
"""

from __future__ import annotations

import json
import threading
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from .. import (
    lazy_should_ask as _should_ask,
    lazy_trade_up as _trade_up,
    stats as _stats,
)
from ..markers import find_marker
from .roots import (
    ResolvedRoot,
    adopt_project_root,
    client_roots,
    describe,
)
from .startup import BackgroundIndex


def _index_is_empty() -> bool:
    try:
        return _stats().get("modules", 0) == 0
    except (ImportError, RuntimeError):
        return True


def _as_json(resolved: ResolvedRoot) -> str:
    return json.dumps(
        {
            "path": str(resolved.path),
            "source": resolved.source,
            "marker": str(resolved.marker) if resolved.marker is not None else None,
        }
    )


def _from_json(report: dict | None) -> ResolvedRoot | None:
    if report is None:
        return None
    path = Path(report["path"])
    return ResolvedRoot(
        path=path,
        source=report["source"],
        marker=find_marker(path) if report.get("confirmed") else None,
    )


class LazyRootRetry:
    """One connection's worth of "did we land in the right project?"."""

    def __init__(
        self,
        resolved: ResolvedRoot,
        index: BackgroundIndex,
        path_flag: str | None = None,
        index_is_empty: Callable[[], bool] | None = None,
    ):
        self.resolved = resolved
        self._index = index
        self._path_flag = path_flag
        self._index_is_empty = index_is_empty or _index_is_empty
        self._lock = threading.Lock()
        self._attempted = False

    def should_ask(self) -> bool:
        """Is there anything the client could tell us that we do not know?"""
        with self._lock:
            attempted = self._attempted
        return _should_ask(attempted, self.resolved.confirmed, self._index_is_empty())

    def mark_user_chosen(self, resolved: ResolvedRoot) -> None:
        """Record an explicit ``coderadar_set_project`` choice."""
        with self._lock:
            self._attempted = True
            self.resolved = resolved

    async def attempt(self, session: Any) -> bool:
        """Ask, re-resolve, and move if the answer is better."""
        with self._lock:
            if self._attempted:
                return False
            self._attempted = True

        uris = await client_roots(session)
        if not uris:
            return False

        candidate = _from_json(_trade_up(_as_json(self.resolved), uris))
        if candidate is None:
            return False

        self.resolved = candidate
        adopt_project_root(candidate)
        self._index.restart(".")
        return True

    def describe(self) -> str:
        return describe(self.resolved)


_RETRY: LazyRootRetry | None = None
_RETRY_LOCK = threading.Lock()


def configure(retry: LazyRootRetry | None) -> None:
    global _RETRY
    with _RETRY_LOCK:
        _RETRY = retry


def current() -> LazyRootRetry | None:
    with _RETRY_LOCK:
        return _RETRY


def make_middleware() -> Callable[..., Awaitable[Any]]:
    """Server middleware that runs the retry before the first tool call."""

    async def middleware(ctx: Any, call_next: Callable[[Any], Awaitable[Any]]) -> Any:
        if getattr(ctx, "method", None) == "tools/call":
            retry = current()
            if retry is not None and retry.should_ask():
                try:
                    await retry.attempt(ctx.session)
                except Exception:  # noqa: BLE001 - keep serving the root we have
                    pass
        return await call_next(ctx)

    return middleware


__all__ = [
    "LazyRootRetry",
    "configure",
    "current",
    "make_middleware",
]
