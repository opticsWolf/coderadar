"""v0.8 P2-4: incremental cold start (guide: docs/v0.8-p2-agent-ux-guide.md).

A warm project already has a Macrame ledger; `coldstart.build_graph` loads
it (milliseconds) and updates only the files that changed, instead of
re-parsing everything. The fallbacks — no store, unloadable store — keep
the full analyze.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest

try:
    import coderadar
    from coderadar import coldstart
    from coderadar._core import analyze as _analyze, graph_stats, search_entities
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


@pytest.fixture
def repo(tmp_path):
    """A project that has been indexed once: marker + store + two files."""
    root = tmp_path / "repo"
    (root / "pkg").mkdir(parents=True)
    (root / "pkg" / "alpha.py").write_text(
        "def alpha_fn():\n"
        "    \"\"\"STORE_ONLY_TOKEN lives in the ledger copy.\"\"\"\n"
        "    return 1\n", encoding="utf-8")
    (root / "pkg" / "beta.py").write_text(
        "def beta_fn():\n    return 1\n", encoding="utf-8")
    (root / ".coderadar" / "store").mkdir(parents=True)
    previous = Path(os.getcwd())
    os.chdir(root)
    try:
        _analyze(".", create_store=True)
        yield root
    finally:
        os.chdir(previous)


def _touch_newer(path: Path, plus_s: float = 10.0) -> None:
    t = time.time() + plus_s
    os.utime(path, (t, t))


def test_incremental_updates_only_the_stale_file(repo, monkeypatch):
    beta = repo / "pkg" / "beta.py"
    beta.write_text("def beta_fn_v2():\n    return 99\n", encoding="utf-8")
    _touch_newer(beta)

    analyze_calls: list = []
    update_calls: list = []

    def spy_analyze(*a, **k):
        analyze_calls.append((a, k))

    orig_update = coderadar.CodeGraph.update_file

    def spy_update(self, file_path, content=None, force=False):
        update_calls.append(file_path)
        return orig_update(self, file_path, content, force)

    monkeypatch.setattr(coderadar, "analyze", spy_analyze)
    monkeypatch.setattr(coderadar.CodeGraph, "update_file", spy_update)

    coldstart.build_graph(".", create_store=False)

    # The load path, not the analyze path — and exactly the stale file.
    assert analyze_calls == [], analyze_calls
    assert update_calls == ["pkg/beta.py"], update_calls

    # The aged file is current: new symbol in, old symbol out.
    names = {h.get("name") for h in search_entities("beta", 50, "function")}
    assert "beta_fn_v2" in names
    assert "beta_fn" not in names

    # The untouched file came from the store copy: the entity is back with
    # its docstring (captured by P2-1's backfill and carried in the ledger).
    hit = next(h for h in search_entities("alpha_fn", 50, "function")
               if h.get("name") == "alpha_fn")
    assert "STORE_ONLY_TOKEN" in (hit.get("docstring") or "")

    # The ledger revision is live on the loaded graph (load is the only
    # path that stamps LEDGER_REVISION — analyze does not, yet).
    assert graph_stats().get("revision") is not None


def test_fresh_store_skips_the_update_scan(repo):
    update_calls: list = []
    orig_update = coderadar.CodeGraph.update_file

    def spy_update(self, file_path, content=None, force=False):
        update_calls.append(file_path)
        return orig_update(self, file_path, content, force)

    monkeypatch_update = pytest.MonkeyPatch()
    monkeypatch_update.setattr(coderadar.CodeGraph, "update_file", spy_update)
    try:
        coldstart.build_graph(".", create_store=False)
    finally:
        monkeypatch_update.undo()

    assert update_calls == []


def test_no_store_falls_back_to_analyze(tmp_path):
    root = tmp_path / "bare"
    root.mkdir()
    (root / "solo.py").write_text(
        "def solo_fn():\n    return 1\n", encoding="utf-8")

    coldstart.build_graph(root, create_store=False)

    names = {h.get("name") for h in search_entities("solo_fn", 50, "function")}
    assert "solo_fn" in names


def test_unloadable_store_falls_back_to_analyze(repo):
    db = coldstart.store_db_path(repo)
    assert db is not None
    db.write_bytes(b"not a macrame store")

    coldstart.build_graph(repo, create_store=False)

    names = {h.get("name") for h in search_entities("beta_fn", 50, "function")}
    assert "beta_fn" in names


def test_stale_source_files_tracks_the_store(repo):
    db = coldstart.store_db_path(repo)
    assert db is not None

    assert coldstart.store_is_fresh(repo, db)
    assert coldstart.stale_source_files(repo, db) == []

    alpha = repo / "pkg" / "alpha.py"
    _touch_newer(alpha)
    assert not coldstart.store_is_fresh(repo, db)
    assert coldstart.stale_source_files(repo, db) == [alpha]


def test_background_index_uses_the_incremental_build(repo):
    # End to end: the MCP startup path on a warm repo goes through
    # coldstart.build_graph (load + update), not a bare analyze.
    from coderadar.mcp import startup

    index = startup.BackgroundIndex(".")
    outcome = index.wait(timeout=60)
    assert outcome.ready, outcome.error
    stats = graph_stats()
    assert stats.get("functions", 0) >= 2
