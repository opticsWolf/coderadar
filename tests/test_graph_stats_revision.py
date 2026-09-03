"""v0.8 P2: `analyze` now stamps the ledger revision.

Before this change `LEDGER_REVISION` (which `graph_stats()["revision"]`
returns) was written only by the load path, so a freshly-`analyze`d graph
reported a stale or absent revision. This pins the fix.

The check runs in a child process: `LEDGER_REVISION` is a process-global
static that starts as ``None``, so a fresh process is the only way to be
sure the revision we see came from *this* `analyze` call and not from a
sibling test that happened to load a store first.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

try:
    from coderadar._core import analyze as _analyze
    from coderadar._core import graph_stats
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


_CHILD = textwrap.dedent('''
    import os
    from pathlib import Path
    from coderadar._core import analyze
    from coderadar._core import graph_stats

    root = Path(%r)
    (root / "mod.py").write_text("def alpha():\\n    return 1\\n", encoding="utf-8")
    prev = os.getcwd()
    os.chdir(root)
    try:
        # create_store=True plants .coderadar/ so the store is attached and
        # the seq_anchor is real; create_store=False on a second pass proves
        # the same store still reports the revision after a re-analyze.
        analyze(".", create_store=True)
        analyze(".", create_store=False)
        print(graph_stats().get("revision"))
    finally:
        os.chdir(prev)
''')


def test_analyze_reports_a_live_revision(tmp_path, monkeypatch):
    script = _CHILD % str(tmp_path)
    # pytest's pythonpath config does not reach a child process; give it the
    # same source root the parent imports coderadar from.
    src = Path(__file__).resolve().parents[1] / "py_agent" / "src"
    env = os.environ.copy()
    env["PYTHONPATH"] = str(src) + (os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else "")
    monkeypatch.setenv("PYTHONPATH", env["PYTHONPATH"])
    proc = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True, text=True, timeout=120,
    )
    assert proc.returncode == 0, proc.stderr
    revision = proc.stdout.strip()
    assert revision and revision != "None", (
        f"analyze did not stamp the ledger revision: {proc.stdout!r}"
    )
