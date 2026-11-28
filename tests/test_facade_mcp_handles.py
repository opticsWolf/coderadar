"""S7 handles: background index, lifecycle guards and the lazy root retry."""

from __future__ import annotations

import asyncio
import importlib
import time
from pathlib import Path
from types import SimpleNamespace

from tests._rust_bridge import rust_bridge


def module(name: str):
    rust_bridge()
    return importlib.import_module(f"coderadar_rs.{name}")


def wait_until(predicate, timeout: float = 10.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.01)
    return predicate()


def test_background_index_runs_a_custom_build_and_reports_ready() -> None:
    startup = module("mcp.startup")
    runs: list[str] = []
    index = startup.BackgroundIndex(analyze=lambda root: runs.append(root))
    assert index.status is startup.IndexStatus.NOT_STARTED

    outcome = index.wait(10)
    assert outcome.ready
    assert outcome.status is startup.IndexStatus.READY
    assert runs == ["."]
    assert index.root == "."


def test_background_index_reports_failure_and_the_message_names_the_fix() -> None:
    startup = module("mcp.startup")

    def boom(root):
        raise RuntimeError("no such tree")

    index = startup.BackgroundIndex(analyze=boom)
    outcome = index.wait(10)
    assert outcome.status is startup.IndexStatus.FAILED
    assert outcome.error and "no such tree" in outcome.error
    message = startup.progress_message(outcome)
    assert "failed to build" in message
    assert "coderadar_reindex" in message


def test_progress_message_for_a_running_index_says_so() -> None:
    startup = module("mcp.startup")
    outcome = startup.IndexOutcome(status=startup.IndexStatus.INDEXING, elapsed=12.0)
    message = startup.progress_message(outcome)
    assert "still being indexed" in message
    assert "12s" in message


def test_ensure_ready_without_a_handle_is_ready() -> None:
    startup = module("mcp.startup")
    startup.configure(None)
    assert startup.ensure_ready().ready


def test_restart_rebuilds_the_same_handle() -> None:
    startup = module("mcp.startup")
    runs: list[str] = []
    index = startup.BackgroundIndex(analyze=lambda root: runs.append(root))
    index.start()
    assert wait_until(lambda: index.status is startup.IndexStatus.READY)
    index.restart("elsewhere")
    assert wait_until(lambda: index.status is startup.IndexStatus.READY)
    assert runs == [".", "elsewhere"]
    startup.configure(None)


def test_parent_watchdog_uses_the_injected_probe_and_can_stop() -> None:
    lifecycle = module("mcp.lifecycle")
    seen: list[str] = []
    watchdog = lifecycle.ParentWatchdog(
        interval=0.01, on_orphan=seen.append, alive=lambda pid: False
    )
    assert watchdog.check_once() is False
    watchdog.start()
    assert wait_until(lambda: bool(seen))
    assert "parent process" in seen[0]
    watchdog.stop()


def test_handshake_timeout_disarms_without_firing() -> None:
    lifecycle = module("mcp.lifecycle")
    fired: list[str] = []
    handshake = lifecycle.HandshakeTimeout(seconds=30, on_timeout=fired.append)
    handshake.arm()
    handshake.disarm()
    handshake.disarm()  # idempotent
    assert handshake.disarmed
    assert fired == []
    zero = lifecycle.HandshakeTimeout(seconds=0, on_timeout=fired.append)
    zero.arm()
    assert not zero.disarmed


class _RetryIndex:
    def __init__(self) -> None:
        self.restarts: list[str] = []

    def restart(self, root: str | None = None) -> None:
        self.restarts.append(root or ".")


class _Session:
    def __init__(self, uri: str) -> None:
        self._uri = uri

    async def list_roots(self):
        return SimpleNamespace(roots=[SimpleNamespace(uri=self._uri)])


def _project(root: Path) -> Path:
    (root / ".coderadar" / "store").mkdir(parents=True)
    nested = root / "src" / "deep"
    nested.mkdir(parents=True)
    return nested


def test_lazy_retry_asks_only_when_the_root_is_unconfirmed() -> None:
    roots = module("mcp.roots")
    lazy = module("mcp.lazy")

    bare = Path.cwd() / "definitely-not-a-project"
    unconfirmed = roots.ResolvedRoot(path=bare, source=roots.CWD, marker=None)
    index = _RetryIndex()
    retry = lazy.LazyRootRetry(unconfirmed, index, index_is_empty=lambda: True)
    assert retry.should_ask() is True
    retry.mark_user_chosen(unconfirmed)
    assert retry.should_ask() is False


def test_lazy_retry_retargets_to_a_confirmed_client_root(tmp_path: Path) -> None:
    roots = module("mcp.roots")
    lazy = module("mcp.lazy")

    nested = _project(tmp_path)
    unconfirmed = roots.ResolvedRoot(path=tmp_path / "elsewhere", source=roots.CWD)
    index = _RetryIndex()
    retry = lazy.LazyRootRetry(unconfirmed, index, index_is_empty=lambda: True)
    uri = "file:///" + str(nested).replace("\\", "/").lstrip("/")

    changed = asyncio.run(retry.attempt(_Session(uri)))
    assert changed is True
    assert retry.resolved.path == tmp_path.resolve()
    assert retry.resolved.confirmed
    assert index.restarts == ["."]
    assert retry.should_ask() is False


def test_lazy_retry_keeps_a_confirmed_root_over_an_unconfirmed_client_one(
    tmp_path: Path,
) -> None:
    roots = module("mcp.roots")
    lazy = module("mcp.lazy")

    nested = _project(tmp_path)
    confirmed = roots.ResolvedRoot(
        path=tmp_path.resolve(), source=roots.CWD, marker=tmp_path / ".coderadar"
    )
    bare = tmp_path / "bare"
    bare.mkdir()
    index = _RetryIndex()
    retry = lazy.LazyRootRetry(confirmed, index, index_is_empty=lambda: True)
    uri = "file:///" + str(bare).replace("\\", "/").lstrip("/")

    assert asyncio.run(retry.attempt(_Session(uri))) is False
    assert retry.resolved.path == tmp_path.resolve()
    assert index.restarts == []
