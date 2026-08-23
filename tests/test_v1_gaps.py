"""Phase 1 tests for the v1-gap-cleanup branch.

- test_real_repo_traversal_latency  (plan 1.4)
- test_concurrent_reads             (plan 1.5)

Both require the Rust _core extension. The real-repo benchmark skips
gracefully when `codegraph-main` is not checked out (path overridable via the
CODEGRAPH_MAIN env var).
"""

import os
import sys
import threading
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

try:
    from coderadar._core import analyze, search_entities, traverse, get_smells, traverse_unresolved
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

E2E_DIR = Path(__file__).parent / "fixtures" / "python" / "e2e_project"
_CODEGRAPH_MAIN = os.environ.get(
    "CODEGRAPH_MAIN", "D:/User/Documents/Python/codegraph-main"
)


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
@pytest.mark.slow
def test_real_repo_traversal_latency():
    """Plan 1.4 — measure cold-start analyze() + depth-3 traverse latency.

    Generous sanity bounds only (this is a benchmark, not a perf gate);
    the real numbers are printed for recording in traverse-smell-status.md §7.
    """
    if not os.path.isdir(_CODEGRAPH_MAIN):
        pytest.skip(f"codegraph-main not found at {_CODEGRAPH_MAIN}")

    t0 = time.perf_counter()
    result = analyze(_CODEGRAPH_MAIN)
    analyze_ms = (time.perf_counter() - t0) * 1000
    assert result["files_indexed"] > 0

    funcs = search_entities("", 1, "function")
    assert funcs, "expected at least one function in codegraph-main"
    start_id = funcs[0]["id"]
    start_name = funcs[0]["name"]

    t0 = time.perf_counter()
    nodes = traverse(start_id, 3, ["calls"], "both", None)
    traverse_ms = (time.perf_counter() - t0) * 1000

    print(
        f"\n[1.4] codegraph-main analyze: {analyze_ms:.0f}ms "
        f"({result['files_indexed']} files, {result['entities_extracted']} entities)"
    )
    print(
        f"[1.4] depth-3 traverse from {start_name!r}: "
        f"{len(nodes)} nodes in {traverse_ms:.1f}ms"
    )

    assert analyze_ms < 300_000, f"analyze too slow: {analyze_ms:.0f}ms"
    assert traverse_ms < 60_000, f"traverse too slow: {traverse_ms:.0f}ms"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
def test_traverse_unresolved_counts():
    """Plan 2.3 — traverse_unresolved reports targets the walk can't follow."""
    import os
    import tempfile

    d = tempfile.mkdtemp()
    with open(os.path.join(d, "mod.py"), "w") as f:
        f.write("def foo():\n    undefined_func()\n")
    analyze(d)

    funcs = search_entities("foo", 5, "function")
    assert funcs
    foo_id = funcs[0]["id"]

    n = traverse_unresolved(foo_id, 2, ["calls"], "both")
    assert n == 1, f"expected 1 unresolved call, got {n}"

    n_up = traverse_unresolved(foo_id, 2, ["calls"], "in")
    assert n_up == 0, f"expected 0 upstream unresolved, got {n_up}"


def test_unverified_sites_warning():
    """Plan 2.4 — mutation renderers surface unverified_sites loudly."""
    from types import SimpleNamespace
    from coderadar.mcp.server import _format_mutation_applied, _format_mutation_plan

    result = SimpleNamespace(
        status="Applied", files_written=["a.py"], syntax_errors=[], backup_path=None,
    )

    # apply path: unverified sites → loud warning
    out = _format_mutation_applied(result, unverified_sites=[{"line": 3}, {"line": 7}])
    assert "⚠️ **WARNING: 2 call site(s)" in out, out

    # dry-run path: unverified sites → loud warning
    plan = SimpleNamespace(
        tool="update_signature", id="p1", affected_files=["a.py"],
        diff_preview="", unverified_sites=[{"line": 5}], warnings=[],
    )
    out2 = _format_mutation_plan(plan)
    assert "⚠️ **WARNING: 1 call site(s)" in out2, out2

    # no sites → no warning
    out3 = _format_mutation_applied(result, unverified_sites=[])
    assert "WARNING" not in out3, out3


def test_as_of_temporal_traversal():
    """Plan 2.5 — traverse(as_of=ts) reads the Macrame ledger temporally."""
    import os
    import tempfile
    import time
    from datetime import datetime, timezone

    def now_ts():
        return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000000Z")

    d = tempfile.mkdtemp()
    with open(os.path.join(d, "a.py"), "w") as f:
        f.write("def a():\n    return b()\n\ndef b(): return 1\n")
    analyze(d, create_store=True)
    ts1 = now_ts()
    a_id = next(f["id"] for f in search_entities("a", 5, "function") if f["name"] == "a")

    res = traverse(a_id, 2, ["calls"], "out", ts1)
    names = [r.get("name") for r in res]
    assert "b" in names, f"as_of(ts1) should include b, got {names}"

    # Mutate: a now calls c instead of b; ensure a new second passes.
    time.sleep(1.2)
    with open(os.path.join(d, "a.py"), "w") as f:
        f.write("def a():\n    return c()\n\ndef b(): return 1\n\ndef c(): return 2\n")
    analyze(d)

    res1 = traverse(a_id, 2, ["calls"], "out", ts1)
    names1 = [r.get("name") for r in res1]
    assert "b" in names1, f"as_of(ts1) should still include b, got {names1}"
    assert "c" not in names1, f"as_of(ts1) should not include c (added later), got {names1}"


def test_as_of_upstream_and_both_rejected():
    """Plan 2.5 — as_of traversal is downstream-only; upstream/both must raise."""
    import os
    import tempfile
    from datetime import datetime, timezone

    d = tempfile.mkdtemp()
    with open(os.path.join(d, "a.py"), "w") as f:
        f.write("def a():\n    return b()\n\ndef b(): return 1\n")
    analyze(d)
    a_id = next(f["id"] for f in search_entities("a", 5, "function") if f["name"] == "a")
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000000Z")

    for direction in ("in", "upstream", "both"):
        with pytest.raises(NotImplementedError):
            traverse(a_id, 2, ["calls"], direction, ts)


def test_concurrent_reads():
    """Plan 1.5 — N concurrent get_smells/traverse reads must not deadlock.

    Pins the parking_lot::RwLock N-reader property: many threads reading
    through the FFI (with py.allow_threads releasing the GIL) all return
    correct results and none deadlock.
    """
    analyze(str(E2E_DIR))

    funcs = search_entities("", 100, "function")
    assert funcs, "expected functions in e2e_project fixture"
    ids = [f["id"] for f in funcs]

    N_THREADS = 8
    ITERS = 3
    results = []
    errors = []

    def worker():
        for _ in range(ITERS):
            for fid in ids:
                try:
                    nodes = traverse(fid, 2, ["calls"], "both", None)
                    results.append(len(nodes))
                    get_smells(None, None)  # engine run under the read lock
                except Exception as e:  # pragma: no cover - failure path
                    errors.append(e)

    threads = [threading.Thread(target=worker) for _ in range(N_THREADS)]
    t0 = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=60)
    elapsed = time.perf_counter() - t0

    alive = [t for t in threads if t.is_alive()]
    assert not alive, f"deadlock: {len(alive)} threads still alive after 60s"
    assert not errors, f"errors during concurrent reads: {errors[:3]}"
    assert len(results) == N_THREADS * ITERS * len(ids), (
        f"expected {N_THREADS * ITERS * len(ids)} traverse results, "
        f"got {len(results)}"
    )

    print(
        f"\n[1.5] {N_THREADS} threads x {ITERS} iters x {len(ids)} ids: "
        f"{len(results)} traverse results in {elapsed:.2f}s, 0 errors"
    )


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
@pytest.mark.slow
def test_get_smells_releases_read_lock_for_writer():
    """Plan 2.6 — a writer must not be starved by an in-flight smell run.

    Builds a synthetic graph large enough that the smell engine runs for a
    measurable time, then races `analyze` (writer) against `get_smells`.
    Asserts the writer completes while the engine is STILL running — i.e. the
    `GLOBAL_GRAPH` read lock was released before the engine loop (2.6), not
    held throughout (which would block the writer, or deadlock, pre-2.6).
    """
    import os
    import tempfile

    def _gen(n):
        lines = []
        for i in range(n):
            lines.append(f"class C{i}:")
            lines.append(f"    f0 = {i}")
            lines.append(f"    f1 = {i + 1}")
            for j in range(3):
                lines.append(f"    def m{j}(self, x):")
                lines.append("        return x + 1")
        return "\n".join(lines) + "\n"

    big = tempfile.mkdtemp()
    with open(os.path.join(big, "big.py"), "w") as f:
        f.write(_gen(4000))
    analyze(big)

    # Baseline: how long does the engine run take on this graph?
    t0 = time.perf_counter()
    get_smells(None, None)
    smell_ms = (time.perf_counter() - t0) * 1000
    if smell_ms < 300:
        pytest.skip(f"graph too small to measure lock release ({smell_ms:.0f}ms)")

    small = tempfile.mkdtemp()
    with open(os.path.join(small, "s.py"), "w") as f:
        f.write("def helper():\n    return 1\n")

    state = {"smell_done": False, "writer_done": False, "writer_ms": None}

    def run_smells():
        try:
            get_smells(None, None)
        finally:
            state["smell_done"] = True

    def run_writer():
        t0 = time.perf_counter()
        try:
            analyze(small)
        finally:
            state["writer_ms"] = (time.perf_counter() - t0) * 1000
            state["writer_done"] = True

    smell_t = threading.Thread(target=run_smells, daemon=True)
    writer_t = threading.Thread(target=run_writer, daemon=True)
    smell_t.start()
    time.sleep(0.2)  # let the engine enter its run loop (snapshot is instant)
    writer_t.start()

    writer_t.join(timeout=30)
    assert state["writer_done"], (
        "writer was starved/deadlocked — read lock held during the engine run"
    )

    engine_still_running = not state["smell_done"]
    smell_t.join(timeout=30)
    assert not smell_t.is_alive(), "smell thread did not finish"

    print(
        f"\n[2.6] smell run {smell_ms:.0f}ms baseline; writer finished in "
        f"{state['writer_ms']:.0f}ms while engine "
        f"{'still running' if engine_still_running else 'already done'}"
    )

    assert engine_still_running, (
        f"writer waited for the smell run to finish ({state['writer_ms']:.0f}ms "
        f"vs {smell_ms:.0f}ms baseline) — read lock was held during the engine loop"
    )
