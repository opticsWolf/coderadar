"""Branch-aware reads (plan §5 P4, DR-40).

Done clause: two lineages with different content for one path each read
their own bytes, off one shared blob table.
"""

from __future__ import annotations

import datetime
import sqlite3
import time

import coderadar
import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

A_V1 = "def f():\n    return 1\n"
A_VB = "def f():\n    return 2\n"
A_V2 = "def f():\n    return 3\n"
B_V1 = "g = 1\n"
B_V2 = "g = 2\n"
FUTURE = "2999-01-01T00:00:00.000000Z"


def _now():
    time.sleep(0.05)
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


@pytest.fixture
def branch_project(tmp_path, monkeypatch):
    root = tmp_path / "branches"
    root.mkdir()
    (root / ".coderadar").mkdir()
    (root / "a.py").write_text(A_V1, encoding="utf-8")
    (root / "b.py").write_text(B_V1, encoding="utf-8")
    monkeypatch.chdir(root)
    coderadar.analyze(".", create_store=True)
    t0 = _now()
    return root, t0


def _store_db():
    from pathlib import Path
    return Path.cwd() / ".coderadar" / "store" / "coderadar.db"


def _blob_rows(sha):
    con = sqlite3.connect(str(_store_db()))
    try:
        (n,) = con.execute(
            "SELECT COUNT(*) FROM blobs WHERE sha256 = ?", (sha,)).fetchone()
        return n
    finally:
        con.close()


def _pointer_row(path, branch):
    pid = f"branch:{branch}:content:{path}"
    con = sqlite3.connect(str(_store_db()))
    try:
        return con.execute(
            "SELECT id, branch_id FROM concepts WHERE id = ? AND retired = 0",
            (pid,),
        ).fetchall()
    finally:
        con.close()


class TestBranchSetup:
    def test_branches_lists_trunk_then_fork(self, branch_project):
        before = ops.branches()
        assert before == [
            {"name": "main", "parent": None, "forked_at": None,
             "created_at": before[0]["created_at"]}]
        assert before[0]["created_at"]
        cut = _core.fork_branch("feat")
        assert cut["name"] == "feat"
        assert cut["parent"] == "main"
        assert cut["forked_at"]
        names = {b["name"]: b for b in ops.branches()}
        assert set(names) == {"main", "feat"}
        assert names["feat"]["parent"] == "main"

    def test_fork_rejects_unknown_parent_and_bad_name(self, branch_project):
        with pytest.raises(ValueError, match="unknown branch"):
            _core.fork_branch("feat", "nope")
        with pytest.raises(ValueError, match="invalid branch"):
            _core.fork_branch("")

    def test_snapshot_defaults_to_main(self, branch_project):
        _, t0 = branch_project
        snap = coderadar.CodeGraph().as_of(t0)
        assert snap.branch == "main"
        assert snap.find("a.py::module")["id"] == "a.py::module"


class TestBranchReads:
    def test_unknown_branch_refuses_at_first_fold(self, branch_project):
        _, t0 = branch_project
        snap = coderadar.CodeGraph().as_of(t0, branch="nope")
        with pytest.raises(ops.InvalidRequest, match="unknown branch"):
            snap.find("a.py::module")
        with pytest.raises(ops.InvalidRequest, match="unknown branch"):
            ops.as_of(t0, symbols=["a.py::module"], branch="nope")

    def test_divergent_bytes_each_lineage_reads_its_own(self, branch_project):
        root, _t0 = branch_project
        disk_v1 = (root / "a.py").read_bytes()
        _core.fork_branch("feat")
        spine_id = _core.write_branch_content("a.py", A_VB.encode(), "feat")
        t1 = _now()
        g = coderadar.CodeGraph()
        branched = g.as_of(t1, branch="feat")
        trunk = g.as_of(t1)
        # The branch reads its own bytes; functions fall back to the full
        # file because spans belong to the trunk's lineage, and slicing one
        # lineage's bytes by another's spans would be a wrong answer.
        assert branched.read_bytes("a.py::module") == A_VB.encode()
        assert branched.read_bytes("a.py::f") == A_VB.encode()
        assert branched.find("a.py::f") is not None
        # The trunk is unaffected: module-concept fallback, original bytes
        # (whole file for the module; node extents exclude the trailing
        # break for the function).
        assert trunk.read_bytes("a.py::module") == disk_v1
        assert trunk.read_bytes("a.py::f") == disk_v1.rstrip(b"\r\n")
        # The spine stays one shared chain across lineages.
        chain = ops.history_of("a.py")
        assert chain["count"] == 2
        assert chain["versions"][0]["digest"] == spine_id.split("@")[1]

    def test_branch_inherits_trunk_at_fork(self, branch_project):
        root, _t0 = branch_project
        disk_v1 = (root / "b.py").read_bytes()
        _core.fork_branch("feat")
        # b.py is never pointed at: the branch must see fork-time bytes
        # while the trunk moves on.
        (root / "b.py").write_text(B_V2, encoding="utf-8")
        coderadar.CodeGraph().update_file("b.py")
        disk_v2 = (root / "b.py").read_bytes()
        t1 = _now()
        g = coderadar.CodeGraph()
        assert g.as_of(t1).read_bytes("b.py::module") == disk_v2
        assert g.as_of(t1, branch="feat").read_bytes("b.py::module") == disk_v1

    def test_shared_bytes_share_one_blob_row(self, branch_project):
        _core.fork_branch("feat")
        _core.fork_branch("other", "main")
        shared = "shared = True\n"
        id_feat = _core.write_branch_content("a.py", shared.encode(), "feat")
        id_other = _core.write_branch_content("a.py", shared.encode(), "other")
        assert id_feat == id_other, "same bytes must mint one shared spine row"
        sha = id_feat.split("@")[1]
        assert _blob_rows(sha) == 1, "one blob row backs both lineages"
        con = sqlite3.connect(str(_store_db()))
        try:
            (n,) = con.execute(
                "SELECT COUNT(*) FROM concepts WHERE id = ?", (id_feat,)).fetchone()
        finally:
            con.close()
        assert n == 1

    def test_repoint_supersedes_and_archive_reclaims(self, branch_project):
        root, _t0 = branch_project
        _core.fork_branch("feat")
        id1 = _core.write_branch_content("a.py", A_VB.encode(), "feat")
        id2 = _core.write_branch_content("a.py", A_V2.encode(), "feat")
        assert id1 != id2
        # Exactly one current pointer (same-id turnover — no writer close
        # step, no terminal rows).
        assert _pointer_row("a.py", "feat") == [
            ("branch:feat:content:a.py", "feat")]
        report = ops.archive(FUTURE)
        assert report["blobs_archived"] >= 1, "superseded branch bytes must go cold"
        t2 = _now()
        g = coderadar.CodeGraph()
        t2 = _now()
        g = coderadar.CodeGraph()
        # Live branch bytes stay readable (hot, pinned by the open row);
        # the trunk never saw either version (disk still holds V1).
        assert g.as_of(t2, branch="feat").read_bytes("a.py::module") == A_V2.encode()
        assert g.as_of(t2).read_bytes("a.py::module") == (root / "a.py").read_bytes()

    def test_trunk_reindex_keeps_branch_pointer(self, branch_project):
        root, _t0 = branch_project
        _core.fork_branch("feat")
        _core.write_branch_content("a.py", A_VB.encode(), "feat")
        # Trunk moves on: file-lifecycle sweeps must not retire a
        # branch's pointer (no file_path, id outside every file range —
        # the sweep cannot see it).
        (root / "a.py").write_text(A_V2, encoding="utf-8")
        coderadar.CodeGraph().update_file("a.py")
        disk_v2 = (root / "a.py").read_bytes()
        t1 = _now()
        g = coderadar.CodeGraph()
        assert g.as_of(t1, branch="feat").read_bytes("a.py::module") == A_VB.encode()
        assert g.as_of(t1).read_bytes("a.py::module") == disk_v2
        assert _pointer_row("a.py", "feat")


class TestBranchOps:
    def test_verify_and_diff_run_on_a_branch(self, branch_project):
        _, t0 = branch_project
        _core.fork_branch("feat")
        _core.write_branch_content("a.py", A_VB.encode(), "feat")
        t1 = _now()
        rep = ops.verify_history(branch="feat")
        assert rep["branch"] == "feat"
        assert rep["totals"]
        diff = ops.changes_between(t0, t1, branch="feat")
        assert set(diff) == set(ops.changes_between(t0, t1).keys())
