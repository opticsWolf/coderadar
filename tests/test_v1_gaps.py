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
