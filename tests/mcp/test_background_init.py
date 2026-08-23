"""The handshake must not wait for the index.

`mcp serve` used to index the whole project before constructing the server,
so `initialize` went unanswered for seconds on a small project and minutes on
a large one — which clients read as a hung server. Indexing moved to a
background thread, and every tool handler passes through `ensure_ready()`.
"""

from __future__ import annotations

import threading
import time

import pytest

from coderadar.mcp.startup import (
    BackgroundIndex,
    IndexStatus,
    configure,
    current,
    ensure_ready,
    progress_message,
)


@pytest.fixture(autouse=True)
def _no_leftover_handle():
    configure(None)
    yield
    configure(None)


class TestBackgroundIndex:
    def test_start_returns_before_the_index_finishes(self):
        release = threading.Event()
        index = BackgroundIndex(analyze=lambda root: release.wait(5))

        began = time.perf_counter()
        index.start()
        handed_back = time.perf_counter() - began

        assert handed_back < 0.5, "start() blocked on the index"
        assert index.status is IndexStatus.INDEXING
        release.set()
        assert index.wait(timeout=5).ready

    def test_start_is_idempotent(self):
        runs = []
        index = BackgroundIndex(analyze=lambda root: runs.append(root))
        for _ in range(5):
            index.start()
        index.wait(timeout=5)
        assert runs == ["."], "the index ran more than once"

    def test_concurrent_starts_still_index_once(self):
        runs = []
        gate = threading.Barrier(4)

        def slow(root):
            time.sleep(0.05)
            runs.append(root)

        index = BackgroundIndex(analyze=slow)

        def racer():
            gate.wait(timeout=5)
            index.start()

        threads = [threading.Thread(target=racer) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=5)

        index.wait(timeout=5)
        assert len(runs) == 1

    def test_a_failing_index_is_reported_not_swallowed(self):
        def boom(root):
            raise ValueError("no such directory")

        index = BackgroundIndex(analyze=boom)
        outcome = index.wait(timeout=5)

        assert outcome.status is IndexStatus.FAILED
        assert not outcome.ready
        assert "no such directory" in (outcome.error or "")
        assert "failed" in progress_message(outcome)

    def test_waiting_past_the_budget_reports_progress_not_a_lie(self):
        release = threading.Event()
        index = BackgroundIndex(analyze=lambda root: release.wait(5))
        try:
            outcome = index.wait(timeout=0.1)
            assert outcome.status is IndexStatus.INDEXING
            assert not outcome.ready
            assert "still being indexed" in progress_message(outcome)
        finally:
            release.set()

    def test_elapsed_stops_moving_once_the_index_is_done(self):
        index = BackgroundIndex(analyze=lambda root: None)
        index.wait(timeout=5)
        first = index.elapsed
        time.sleep(0.05)
        assert index.elapsed == first


class TestEnsureReady:
    def test_no_handle_means_get_out_of_the_way(self):
        # A directly constructed server has no background index; the tools'
        # own `requires_index` guard still catches an absent graph.
        assert ensure_ready().ready
        assert current() is None

    def test_a_handler_arriving_early_waits_for_the_index(self):
        release = threading.Event()
        index = BackgroundIndex(analyze=lambda root: release.wait(5))
        configure(index)
        index.start()

        threading.Timer(0.1, release.set).start()
        assert ensure_ready(timeout=5).ready

    def test_ensure_ready_starts_the_index_if_nothing_else_did(self):
        runs = []
        configure(BackgroundIndex(analyze=lambda root: runs.append(root)))
        assert ensure_ready(timeout=5).ready
        assert runs == ["."]


class TestTheGuardConsultsIt:
    def test_a_tool_called_mid_index_reports_progress(self, monkeypatch):
        from coderadar.mcp import server as server_mod
        from coderadar.mcp import startup

        # Shorten the handler's patience rather than the index; the point is
        # what a handler says when its budget runs out, not how long it is.
        monkeypatch.setattr(startup, "DEFAULT_WAIT_SECONDS", 0.1)

        release = threading.Event()
        configure(BackgroundIndex(analyze=lambda root: release.wait(5)))

        @server_mod.requires_index
        def tool() -> str:
            return "answered from the graph"

        try:
            answer = tool()
        finally:
            release.set()

        assert "still being indexed" in answer
        assert "answered from the graph" not in answer

    def test_a_tool_called_after_a_failed_index_says_so(self, monkeypatch):
        from coderadar.mcp import server as server_mod

        def boom(root):
            raise RuntimeError("index blew up")

        configure(BackgroundIndex(analyze=boom))

        @server_mod.requires_index
        def tool() -> str:
            return "answered from the graph"

        answer = tool()
        assert "index blew up" in answer
        assert "answered from the graph" not in answer
