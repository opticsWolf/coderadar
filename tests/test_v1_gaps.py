"""Phase 1 tests for the v1-gap-cleanup branch.

- test_real_repo_traversal_latency  (plan 1.4)
- test_concurrent_reads             (plan 1.5)

Both require the Rust _core extension. The real-repo benchmark skips
gracefully when `codegraph-main` is not checked out (path overridable via the
CODEGRAPH_MAIN env var).
"""

import json
import os
import sys
import threading
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

try:
    from coderadar._core import analyze, get_smells, search_entities, traverse, traverse_unresolved
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


_COLD_START_LOAD_SCRIPT = """
import json, sys, time

sys.path.insert(0, sys.argv[1])
_t0 = time.perf_counter()
import coderadar  # noqa: F401  (import time is part of the fresh-process cost)
from coderadar._core import query_graph, search_entities, traverse, graph_stats
import_s = time.perf_counter() - _t0

db_path, root = sys.argv[2], sys.argv[3]

# Fresh process: the process-global graph is empty, so this is the real
# ledger -> in-memory projection path an agent pays on every session.
t_load0 = time.perf_counter()
coderadar.load(db_path, root)
t_loaded = time.perf_counter()

funcs = search_entities("", 5, "function")
t_search = time.perf_counter()
assert funcs, "search after load returned nothing"
# Deterministic, non-trivial anchor for the walk: the most-called function,
# with a name tiebreak. A plain search hit is usually a leaf and would time
# an empty walk; this one has a real blast radius, so resolution and edge
# following are actually exercised.
t_anchor0 = time.perf_counter()
hub_rows = query_graph(
    "functions where caller_count > 0 order by caller_count desc limit 50"
)
hub_rows.sort(key=lambda r: (-int(r.get("caller_count") or 0), r["name"]))
if hub_rows:
    start = hub_rows[0]
else:  # repo with no call edges: fall back to a deterministic name
    pool = sorted(search_entities("", 100000, "function"), key=lambda f: f["name"])
    start = pool[0]
t_anchor = time.perf_counter()

nodes = traverse(start["id"], 3, ["calls"], "both", None)
t_done = time.perf_counter()

# Second pair on the same loaded graph: no store I/O, no first-call
# one-time costs. This is the per-call cost once a session is up.
t_warm0 = time.perf_counter()
search_entities("", 5, "function")
t_warm_search = time.perf_counter()
traverse(start["id"], 3, ["calls"], "both", None)
t_warm_traverse = time.perf_counter()

stats = graph_stats()
print(json.dumps({
    "import_s": import_s,
    "load_s": t_loaded - t_load0,
    "search_s": t_search - t_loaded,
    "anchor_s": t_anchor - t_anchor0,
    "traverse_s": t_done - t_anchor,
    "first_query_s": t_search - t_load0,   # load -> first query result
    "total_s": t_done - t_load0,           # load + first search + hub + traverse
    "warm_search_s": t_warm_search - t_warm0,
    "warm_traverse_s": t_warm_traverse - t_warm_search,
    "file_count": stats.get("file_count"),
    "functions": stats.get("functions"),
    "call_edges": stats.get("call_edges"),
    "revision": stats.get("revision"),
    "traverse_nodes": len(nodes),
    "start_name": start["name"],
    "start_callers": start.get("caller_count"),
}))
"""


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
@pytest.mark.slow
def test_cold_start_load_latency():
    """v0.8 P1 §16 — cold-start load(db) + one search + one depth-3 traverse.

    Two fresh subprocesses, because "cold start" mixes two costs that the
    code controls very differently:

    * run 1 touches the fixture store for the first time on this boot. Its
      load time is dominated by disk/WAL page-cache misses and can swing
      3-4x on identical code.
    * run 2 is a new process (empty global graph, full ledger fold) but
      with the store pages already resident. That is the warm application
      path the gate is about: reconstruct + resolve, not disk speed.

    Run 1 is printed as the cache state encountered; run 2 is gated. Both
    legs still rebuild the graph from the transaction log, so a regression
    in reconstruction or resolution fails run 2. The traverse starts from
    the most-called function (query language, name tiebreak), not a random
    search hit, so the walk has a real blast radius instead of one leaf.

    DR-33 re-baseline (history-aware): load folds the whole transaction log
    (308K rows vs 8K live concepts here and climbing), so cost is O(history)
    on a monotonically growing fixture. A fixed wall gate would be a
    guaranteed future flake. The gate is 1.5s base + 20µs/log-row
    (~7.7s at current sizes, ~1.6x the warm measurement; a 2x code
    regression still fails). Search/traverse get loose 1s sanity gates and
    both runs keep a 60s wall hang guard. Original intent (≪ the 46.2s full
    analyze recorded in docs/v0.8-p1-cold-start-design.md §16) still holds:
    the gate sits ~6x under it at current history sizes.
    """
    import shutil
    import sqlite3
    import subprocess

    if not os.path.isdir(_CODEGRAPH_MAIN):
        pytest.skip(f"codegraph-main not found at {_CODEGRAPH_MAIN}")

    root = Path(_CODEGRAPH_MAIN)
    db = root / ".coderadar" / "store" / "coderadar.db"
    if not db.exists():
        # Build the ledger once; the build itself is not the measured leg.
        analyze(_CODEGRAPH_MAIN, create_store=True)
        assert db.exists(), "analyze(create_store=True) did not create the store"

    def run_fresh():
        t_wall0 = time.perf_counter()
        proc = subprocess.run(
            [sys.executable, "-c", _COLD_START_LOAD_SCRIPT,
             str(Path(__file__).parent.parent / "py_agent" / "src"),
             str(db), str(root)],
            capture_output=True, text=True, timeout=300,
            check=False,  # returncode inspected by the caller below
        )
        wall_s = time.perf_counter() - t_wall0
        return proc, wall_s

    def measure():
        proc, wall_s = run_fresh()
        if proc.returncode != 0 and "meta_version" in (proc.stderr + proc.stdout):
            # A v1 (pre-v2-concept) store is a hard error for load — rebuild
            # it once, then re-measure.
            shutil.rmtree(root / ".coderadar", ignore_errors=True)
            analyze(_CODEGRAPH_MAIN, create_store=True)
            proc, wall_s = run_fresh()
        assert proc.returncode == 0, (
            f"cold-start subprocess failed: {proc.stderr[-2000:]}"
        )
        return json.loads(proc.stdout.strip().splitlines()[-1]), wall_s

    # Run 1: whatever page-cache state the machine is in. Reported, not gated.
    cold, cold_wall = measure()
    # Run 2: fresh process again, same ledger, now resident pages. Gated.
    warm, warm_wall = measure()

    assert warm["revision"] == cold["revision"], (
        f"store changed between the two runs ({cold['revision']} -> "
        f"{warm['revision']}); the fixture is not hermetic"
    )

    _lc = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        log_rows = _lc.execute("SELECT COUNT(*) FROM transaction_log").fetchone()[0]
        live_concepts = _lc.execute(
            "SELECT COUNT(*) FROM concepts WHERE retired = 0"
        ).fetchone()[0]
        live_links = _lc.execute("SELECT COUNT(*) FROM links_current").fetchone()[0]
    finally:
        _lc.close()
    db_bytes = db.stat().st_size

    print(
        f"\n[P1 16] store: {db_bytes / 1e6:.0f} MB, {log_rows} log rows, "
        f"{live_concepts} live concepts, {live_links} current links "
        f"(revision {warm['revision']})"
    )
    for label, rep, wall_s in (("run1 (cold cache)", cold, cold_wall),
                               ("run2 (warm cache)", warm, warm_wall)):
        print(
            f"  {label}: import {rep['import_s'] * 1000:.0f}ms, "
            f"load(db) {rep['load_s'] * 1000:.0f}ms, "
            f"load->first query {rep['first_query_s'] * 1000:.0f}ms, "
            f"wall {wall_s * 1000:.0f}ms"
        )
        print(
            f"    {rep['file_count']} files, {rep['functions']} functions, "
            f"{rep['call_edges']} call edges"
        )
        print(
            f"    first search {rep['search_s'] * 1000:.0f}ms; "
            f"warm search {rep['warm_search_s'] * 1000:.0f}ms; "
            f"hub lookup (query_graph) {rep['anchor_s'] * 1000:.0f}ms"
        )
        print(
            f"    depth-3 traverse from {rep['start_name']!r} "
            f"({rep['start_callers']} callers): {rep['traverse_nodes']} nodes "
            f"in {rep['traverse_s'] * 1000:.0f}ms first / "
            f"{rep['warm_traverse_s'] * 1000:.0f}ms warm"
        )

    load_gate = 1.5 + log_rows * 0.00002
    print(f"  cache effect: run1 load is "
          f"{cold['load_s'] / max(warm['load_s'], 1e-9):.2f}x run2")
    print(f"  gate: load(db) < {load_gate:.1f}s (1.5s + 20us/log row), "
          f"query legs < 1s, wall < 60s")

    assert warm["traverse_nodes"] > 0, "traverse from a loaded graph returned nothing"
    assert warm["load_s"] < load_gate, (
        f"warm load too slow: {warm['load_s']:.2f}s for {log_rows} log rows "
        f"(gate {load_gate:.1f}s = 1.5s + 20us/row)"
    )
    assert warm["warm_search_s"] < 1.0, (
        f"warm search too slow: {warm['warm_search_s']:.2f}s"
    )
    assert warm["anchor_s"] < 1.0, (
        f"hub query_graph too slow: {warm['anchor_s']:.2f}s"
    )
    assert warm["warm_traverse_s"] < 1.0, (
        f"warm traverse too slow: {warm['warm_traverse_s']:.2f}s"
    )
    assert cold_wall < 60, f"cold run hung: {cold_wall:.2f}s wall (hang guard 60s)"
    assert warm_wall < 60, f"warm run hung: {warm_wall:.2f}s wall (hang guard 60s)"


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

    from coderadar import render

    result = SimpleNamespace(
        status="Applied", files_written=["a.py"], syntax_errors=[], backup_path=None,
    )

    # apply path: unverified sites → loud warning
    out = render.mutation_applied(result, unverified_sites=[{"line": 3}, {"line": 7}])
    assert "⚠️ **WARNING: 2 call site(s)" in out, out

    # dry-run path: unverified sites → loud warning
    plan = SimpleNamespace(
        tool="update_signature", id="p1", affected_files=["a.py"],
        diff_preview="", unverified_sites=[{"line": 5}], warnings=[],
    )
    out2 = render.mutation_plan(plan)
    assert "⚠️ **WARNING: 1 call site(s)" in out2, out2

    # no sites → no warning
    out3 = render.mutation_applied(result, unverified_sites=[])
    assert "WARNING" not in out3, out3


def test_as_of_temporal_traversal():
    """Plan 2.5 — traverse(as_of=ts) reads the Macrame ledger temporally."""
    import os
    import tempfile
    import time
    from datetime import datetime, timezone

    def now_ts():
        # Real microsecond precision (%f), not a hardcoded .000000: the
        # ledger stores edge valid_from at microsecond resolution, and a
        # second-truncated ts can land BEFORE the edge's valid_from when
        # both fall in the same second, silently hiding the edge.
        return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")

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
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")

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
                except Exception as e:  # noqa: BLE001 - pragma: no cover - failure path
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
