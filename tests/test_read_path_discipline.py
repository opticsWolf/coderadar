"""Read-path discipline E2E (§3.2): no unasked full analyzes.

The cheap path (load stale store + changed-files-only) is the default;
a full walk happens only for `reindex(full=True)` (or a config change,
DR-31). An unchanged-tree reindex must retire nothing and put zero new
blob bytes — the release-gate clause, recorded here.
"""

from __future__ import annotations

import os

import coderadar
import pytest
from coderadar import _core, ops

try:
    from coderadar import _core as _c  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


@pytest.fixture()
def project(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_bytes(b"def alpha():\n    return 1\n")
    (tmp_path / "b.py").write_bytes(b"def beta():\n    return 2\n")
    coderadar.analyze(".", create_store=True)
    return tmp_path


def _marks():
    s = _core.graph_stats()
    return s.get("revision"), s.get("indexed_at")


class TestUnchangedTree:
    def test_reindex_is_zero(self, project):
        before = _marks()
        out = ops.reindex()
        after = _marks()
        # No ledger writes (no retirements, no edge churn) and no commit:
        # revision stable, and indexed_at never advances — the load
        # restores the persisted commit mark, which predates the live one
        # by milliseconds (no commit happened on the cheap path).
        assert after[0] == before[0]
        assert after[1] <= before[1]
        # Zero new blob bytes: the load resets cumulative stats and the
        # cheap path puts nothing (a full analyze would refill stored > 0).
        assert out["blobs"]["stored"] == 0
        assert out["stats"]["functions"] == 2

    def test_full_reindex_rewalks(self, project):
        before = _marks()
        out = ops.reindex(full=True)
        after = _marks()
        # A full walk recommits (indexed_at advances) and re-puts every
        # file's bytes — the opposite of the cheap path, on demand only.
        assert after[1] >= before[1]
        assert out["blobs"]["stored"] == 2
        assert out["stats"]["functions"] == 2


class TestChangedFilesOnly:
    def test_changed_file_updates_alone(self, project):
        (project / "a.py").write_bytes(b"def alpha():\n    return 10\n")
        # Force past the store_is_fresh grace deterministically.
        db = project / ".coderadar" / "store" / "coderadar.db"
        old = db.stat().st_mtime
        os.utime(project / "a.py", (old + 10, old + 10))
        out = ops.reindex()
        assert out["blobs"]["stored"] == 1
        assert _core.lookup_entity("a.py::alpha")["signature"] == "def alpha()"
        assert _core.lookup_entity("b.py::beta") is not None
        # ... and the tree is fresh again afterwards (reset the artificial
        # future mtime first — real clocks never go backwards like the
        # fixture above did).
        os.utime(project / "a.py")
        assert ops.status()["store_fresh"] is True


class TestStalenessBanner:
    def test_status_reports_fresh_then_stale(self, project):
        assert ops.status()["store_fresh"] is True
        db = project / ".coderadar" / "store" / "coderadar.db"
        old = db.stat().st_mtime
        os.utime(project / "b.py", (old + 10, old + 10))
        assert ops.status()["store_fresh"] is False
