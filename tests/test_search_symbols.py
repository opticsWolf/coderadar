"""FTS5 keyword search E2E (plan §1.10, DR-34).

`ops.search_symbols` over a real analyzed+stored project: ranking pins,
order-determinism (same query twice, byte-identical), the `[]`/error
edges, hostile-input safety, and the 1s latency gate (DR-33 lesson: catch
pathological, never flake — FTS answers in ms at this scale).
"""

from __future__ import annotations

import json
import time

import coderadar
import pytest
from coderadar import ops

try:
    from coderadar._core import search_symbols as _search_symbols  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

WATCH = '''\
def start_watcher(path):
    """Debounce the file watcher before restarting the index."""
    return path
'''

RENAME = '''\
class MutationEngine:
    """Plans source mutations."""

    def plan_rename(self, old, new):
        """Rename a function across the project."""
        return (old, new)
'''

SVC = '''\
def authenticate(token):
    """Check the service token."""
    return token == "x"
'''


@pytest.fixture
def project(tmp_path, monkeypatch):
    root = tmp_path / "fts"
    root.mkdir()
    (root / ".coderadar").mkdir()
    (root / "watch.py").write_text(WATCH, encoding="utf-8")
    (root / "mutate.py").write_text(RENAME, encoding="utf-8")
    (root / "svc.py").write_text(SVC, encoding="utf-8")
    monkeypatch.chdir(root)
    coderadar.analyze(".", create_store=True)
    return root


@pytest.fixture
def storeless_project(tmp_path, monkeypatch):
    root = tmp_path / "plain"
    root.mkdir()
    (root / ".coderadar").mkdir()
    (root / "svc.py").write_text(SVC, encoding="utf-8")
    monkeypatch.chdir(root)
    coderadar.analyze(".", create_store=False)
    return root


class TestRanking:
    def test_watcher_debounce_top1(self, project):
        hits = ops.search_symbols("watcher debounce")
        assert hits, "expected the watcher to match"
        assert hits[0]["id"] == "watch.py::start_watcher"

    def test_rename_function_top1(self, project):
        # "function" is inside every v2 concept JSON (IDF self-penalty);
        # the entity also mentioning rename must still rank first.
        hits = ops.search_symbols("rename function")
        assert hits, "expected the rename planner to match"
        assert hits[0]["id"] == "mutate.py::MutationEngine.plan_rename"

    def test_ranks_ascend_best_first(self, project):
        hits = ops.search_symbols("watcher", top_k=50)
        ranks = [h["rank"] for h in hits]
        assert ranks == sorted(ranks)
        assert all(isinstance(r, float) for r in ranks)

    def test_result_hydrates_via_node(self, project):
        top = ops.search_symbols("service token")[0]
        node = ops.node(top["id"])
        assert node["id"] == "svc.py::authenticate"

    def test_retired_concept_vanishes(self, project):
        assert ops.search_symbols("watcher debounce")[0]["id"] == "watch.py::start_watcher"
        (project / "watch.py").unlink()
        coderadar.CodeGraph().update_file("watch.py")
        assert all(h["id"] != "watch.py::start_watcher"
                   for h in ops.search_symbols("watcher debounce", top_k=50))


class TestDeterminismAndLatency:
    def test_same_query_twice_byte_identical(self, project):
        first = json.dumps(ops.search_symbols("rename function"), sort_keys=True)
        second = json.dumps(ops.search_symbols("rename function"), sort_keys=True)
        assert first == second

    def test_latency_gate(self, project):
        started = time.perf_counter()
        ops.search_symbols("watcher debounce restart index token", top_k=50)
        elapsed = time.perf_counter() - started
        assert elapsed < 1.0, f"keyword search took {elapsed:.2f}s (gate 1s)"


class TestEdges:
    def test_empty_query_is_empty(self, project):
        assert ops.search_symbols("") == []
        assert ops.search_symbols("   ") == []

    def test_no_match_is_empty(self, project):
        assert ops.search_symbols("xylophone zebra quux") == []

    def test_hostile_input_is_safe(self, project):
        # FTS operators / unbalanced quotes degrade, never error. (Safety
        # means "answers, never raises" — single-letter tokens may still
        # match, so only the impossible conjunctions pin empty.)
        assert ops.search_symbols("cats not dogs") == []
        assert ops.search_symbols('say "hi') == []
        assert isinstance(ops.search_symbols("a*b (c)"), list)

    def test_raw_passthrough(self, project):
        # Escaped by default: space is AND, so no one doc has both terms.
        assert ops.search_symbols("watcher svc") == []
        # Raw: the MATCH expression applies.
        raw = ops.search_symbols("watcher OR svc", raw=True)
        ids = [h["id"] for h in raw]
        assert "watch.py::start_watcher" in ids
        assert "svc.py::authenticate" in ids

    def test_storeless_is_invalid_request(self, storeless_project):
        with pytest.raises(ops.InvalidRequest, match="stored graph"):
            ops.search_symbols("token")

    def test_no_graph_is_no_index(self, tmp_path):
        # GLOBAL_GRAPH is process-global, so the unloaded state needs a
        # fresh interpreter (same fresh-process pattern as determinism).
        import subprocess
        import sys

        probe = (
            "import os; os.chdir({work!r}); "
            "from coderadar import ops; "
            "ops.search_symbols('token')"
        )
        proc = subprocess.run(
            [sys.executable, "-c", probe.format(work=str(tmp_path))],
            capture_output=True, text=True, cwd=str(tmp_path),
            check=False,  # failure is the assertion (nonzero + NoIndex)
        )
        assert proc.returncode != 0
        assert "NoIndex" in proc.stderr
