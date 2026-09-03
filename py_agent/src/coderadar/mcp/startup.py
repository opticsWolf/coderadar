"""Answer the handshake first, index second.

`mcp serve` used to index the whole project before constructing the server,
so the client's `initialize` sat unanswered for however long the walk took —
seconds on this project, minutes on a large repo. Clients read that as a hung
server rather than a starting one, and some of them give up.

Indexing now runs on a background thread started before the transport does,
and every tool handler calls `ensure_ready()` on its way in. A handler that
arrives while indexing is still running waits for a bounded budget and then
reports progress rather than a wrong answer — "still indexing, 12s elapsed"
is something an agent can act on; an empty result set is not.

The background thread is only correct because `analyze` releases the GIL. It
did not until this release, and starting a thread around a GIL-holding
`analyze` would have frozen the event loop for the whole index — strictly
worse than the honest slow start it replaced.
"""

from __future__ import annotations

import os
import threading
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Optional

#: How long a tool handler waits for a running index before answering with
#: progress instead. Long enough that a small project simply works, short
#: enough that the agent is never left wondering.
DEFAULT_WAIT_SECONDS = float(os.environ.get("CODERADAR_INDEX_WAIT", "25"))


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
    error: Optional[str] = None
    stats: dict = field(default_factory=dict)

    @property
    def ready(self) -> bool:
        return self.status is IndexStatus.READY


class BackgroundIndex:
    """One project's index, built once, off the request path.

    Idempotent by construction: `start()` may be called from anywhere, any
    number of times, and exactly one indexing thread ever runs. That matters
    because both `serve` and the first tool call want to be sure it started,
    and neither should have to know whether the other got there first.
    """

    def __init__(self, root: str = ".", analyze: Optional[Callable[[str], Any]] = None):
        self._root = root
        self._analyze = analyze
        self._lock = threading.Lock()
        self._done = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._status = IndexStatus.NOT_STARTED
        self._error: Optional[str] = None
        self._started_at: float = 0.0
        self._finished_at: float = 0.0
        # Bumped by every restart. A thread from a previous generation is a
        # thread whose answer nobody wants any more, and it must not write
        # its status over the current one.
        self._generation = 0

    # ── lifecycle ────────────────────────────────────────────────────────

    def start(self) -> None:
        """Begin indexing if it is not already begun or finished."""
        with self._lock:
            if self._thread is not None:
                return
            self._status = IndexStatus.INDEXING
            self._started_at = time.monotonic()
            generation, root, done = self._generation, self._root, self._done
            self._thread = threading.Thread(
                target=self._run, args=(generation, root, done),
                name="coderadar-index", daemon=True)
            self._thread.start()

    def _run(self, generation: int, root: str, done: threading.Event) -> None:
        status, error = IndexStatus.READY, None
        try:
            build = self._analyze
            if build is None:
                # v0.8 P2-4: incremental cold start — a warm repo loads its
                # ledger and updates only the files that changed, instead of
                # re-parsing everything. The exception contract is unchanged:
                # whatever build raises is recorded and surfaced by
                # ensure_ready.
                from coderadar import coldstart
                build = coldstart.build_graph
            build(root)
        except BaseException as exc:  # noqa: BLE001 — the thread must not die silently
            status = IndexStatus.FAILED
            error = f"{type(exc).__name__}: {exc}"
        finally:
            with self._lock:
                if generation == self._generation:
                    self._status = status
                    self._error = error
                    self._finished_at = time.monotonic()
            done.set()

    @property
    def root(self) -> str:
        with self._lock:
            return self._root

    def restart(self, root: Optional[str] = None) -> None:
        """Index again, optionally somewhere else.

        The lazy root retry uses this: the first tool call may learn from the
        client that the project is somewhere other than where startup
        guessed, and the index has to follow. A restart while a previous
        index is still running lets that one finish into a graph that this
        one then replaces — the core's `analyze` is a full replacement, not a
        merge, so the last writer wins and the last writer is this one.
        """
        with self._lock:
            if root is not None:
                self._root = root
            self._generation += 1
            self._thread = None
            self._done = threading.Event()
            self._status = IndexStatus.NOT_STARTED
            self._error = None
            self._started_at = 0.0
            self._finished_at = 0.0
        self.start()

    # ── observation ──────────────────────────────────────────────────────

    @property
    def status(self) -> IndexStatus:
        with self._lock:
            return self._status

    @property
    def elapsed(self) -> float:
        if not self._started_at:
            return 0.0
        end = self._finished_at or time.monotonic()
        return end - self._started_at

    def wait(self, timeout: Optional[float] = None) -> IndexOutcome:
        """Start if needed, wait up to `timeout`, and report where we got to."""
        self.start()
        budget = DEFAULT_WAIT_SECONDS if timeout is None else timeout
        with self._lock:
            done = self._done
        done.wait(timeout=budget)
        with self._lock:
            status, error = self._status, self._error
        return IndexOutcome(status=status, elapsed=self.elapsed, error=error)


# ── module-level handle ───────────────────────────────────────────────────
#
# The server is single-project (the core keeps one GLOBAL_GRAPH), so one
# handle is the honest shape. `configure` replaces it; tests do that.

_INDEX: Optional[BackgroundIndex] = None
_INDEX_LOCK = threading.Lock()


def configure(index: Optional[BackgroundIndex]) -> None:
    """Install the handle every handler will consult. None clears it."""
    global _INDEX
    with _INDEX_LOCK:
        _INDEX = index


def current() -> Optional[BackgroundIndex]:
    with _INDEX_LOCK:
        return _INDEX


def ensure_ready(timeout: Optional[float] = None) -> IndexOutcome:
    """Called by every tool handler before it touches the graph.

    With no handle installed — a directly constructed server, or a test —
    this reports READY and gets out of the way: the caller's own
    `requires_index` guard is still there to catch a genuinely absent graph.
    """
    index = current()
    if index is None:
        return IndexOutcome(status=IndexStatus.READY)
    return index.wait(timeout)


def progress_message(outcome: IndexOutcome) -> str:
    """What to tell the agent when the index is not ready yet."""
    if outcome.status is IndexStatus.FAILED:
        return (
            "The project index failed to build, so no code intelligence is "
            f"available: {outcome.error}\n\n"
            "Fix the underlying problem and call codegraph_reindex to retry."
        )
    return (
        f"The project is still being indexed ({outcome.elapsed:.0f}s so far). "
        "Nothing is wrong — the first index walks every source file.\n\n"
        "Retry this call in a few seconds. Answering now would mean answering "
        "from a partial graph, which is worse than waiting."
    )
