"""P5b.6 slice A (red): branch selection and commit addresses.

D-6 c: a branch whose tip is already on an ingested lineage becomes no
lineage — its name is an address, resolved to that tip commit and read
through the commit index. A branch with commits of its own stays a
lineage. Every branch read reports `parent_drift`: the parent commits
after its fork point that it can still see (0 on the trunk's tip).

Fixtures are deliberately small: `old` sits on main's chain (merged),
`feature` carries its own commit, and `origin/feature` names the same tip
as `feature` from the remote side.
"""
import hashlib
import os
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:
    _CORE = False

pytestmark = pytest.mark.skipif(
    not _CORE, reason="Rust _core extension not built")


def _digest(content: str) -> str:
    return hashlib.sha256(content.encode()).hexdigest()


def _git(repo, *args, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    subprocess.run(["git", *args], cwd=str(repo), env=merged,
                   check=True, capture_output=True)


def _stamp_env(committer_date):
    return {
        "GIT_AUTHOR_NAME": "T Author",
        "GIT_AUTHOR_EMAIL": "author@t.t",
        "GIT_COMMITTER_NAME": "T Committer",
        "GIT_COMMITTER_EMAIL": "committer@t.t",
        "GIT_AUTHOR_DATE": committer_date,
        "GIT_COMMITTER_DATE": committer_date,
    }


def _commit(repo, message, date, files):
    for name, content in files.items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env=_stamp_env(date))


def _sha(repo, rev):
    out = subprocess.run(["git", "rev-parse", rev], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


def _branchy_repo(path):
    """main c1..c3; `old` = c2 (merged, no commits of its own);
    `feature` = c2 + f1 (its own commit); `origin/feature` = f1."""
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    _commit(path, "c2", "2026-01-02T10:00:00+00:00",
            {"a.py": "x = 2\n", "b.py": "y = 1\n"})
    c2 = _sha(path, "main")
    _git(path, "branch", "old", c2)
    _commit(path, "c3", "2026-01-03T10:00:00+00:00", {"a.py": "x = 3\n"})
    _git(path, "checkout", "-qb", "feature", c2)
    _commit(path, "f1", "2026-01-04T10:00:00+00:00",
            {"a.py": "x = 2\nfeature = 1\n"})
    f1 = _sha(path, "feature")
    _git(path, "update-ref", "refs/remotes/origin/feature", f1)
    _git(path, "checkout", "-q", "main")
    return path, {"c2": c2, "c3": _sha(path, "main"), "f1": f1}


def _dest(tmp_path):
    return tmp_path / "migrated" / "coderadar.db"


def _refs(ids, ts, branch, **kw):
    from coderadar import _core as _C
    return _C.source_refs_at(ids, ts, branch, **kw)


class TestSelection:
    def test_a_merged_branch_is_an_address_not_a_lineage(self, tmp_path):
        repo, shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        report = ops.migrate_to_commit_time(str(repo), str(dest),
                                            embed=False, verify=False)
        # `old` never advanced: its tip is c2, on main's first-parent chain.
        assert report["branches"] == ["main", "feature"], report["branches"]
        assert "old" not in report["branch_reports"]
        assert report["addresses"]["old"] == shas["c2"], report.get("addresses")

    def test_an_address_name_reads_its_tip_commit(self, tmp_path):
        repo, shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                                   verify=False)
        out = _refs(["a.py::module"], "2026-01-02T12:00:00.000000Z", "old")
        assert out["basis"] == "git-history", out
        assert out["sha"] == shas["c2"], out
        assert out["address"] is True, out
        assert out["refs"]["a.py::module"]["digest"] == _digest("x = 2\n")

    def test_a_branch_with_its_own_commits_stays_a_lineage(self, tmp_path):
        repo, _shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        report = ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                                            verify=False)
        assert "feature" in report["branches"]
        assert report["branch_reports"]["feature"]["lineage"] == "feature"
        assert "feature" not in report["addresses"]

    def test_a_remote_name_sharing_a_lineage_tip_is_an_address(self, tmp_path):
        # origin/feature names f1, which the feature lineage already holds:
        # a second lineage would trip cross-lineage-id, so it is an address.
        repo, shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        report = ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                                            verify=False)
        assert report["addresses"]["origin/feature"] == shas["f1"], \
            report.get("addresses")
        assert "origin/feature" not in report["branches"]

    def test_an_unknown_name_refuses(self, tmp_path):
        repo, _shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                                   verify=False)
        with pytest.raises(ValueError, match="[Uu]nknown"):
            _refs(["a.py::module"], "2026-01-02T12:00:00.000000Z", "nosuch")


class TestParentDrift:
    def test_the_trunk_reports_zero_drift(self, tmp_path):
        repo, _shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                                   verify=False)
        out = _refs(["a.py::module"], "2026-01-05T00:00:00.000000Z", "main")
        assert out["parent_drift"] == 0, out

    def test_a_branch_reports_the_parent_commits_after_its_fork(self, tmp_path):
        # feature forked at c2; main has c3 after it. Until U-3 the branch
        # can still see it, so the read says so.
        repo, _shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                                   verify=False)
        out = _refs(["a.py::module"], "2026-01-05T00:00:00.000000Z", "feature")
        assert out["parent_drift"] == 1, out

    def test_an_address_reports_drift_zero(self, tmp_path):
        repo, _shas = _branchy_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                                   verify=False)
        out = _refs(["a.py::module"], "2026-01-02T12:00:00.000000Z", "old")
        assert out["parent_drift"] == 0, out


# --- The Python surface carries the same two facts ------------------------

def test_snapshot_marks_an_address_name(tmp_path):
    repo, shas = _branchy_repo(tmp_path / "repo")
    dest = _dest(tmp_path)
    ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                               verify=False)
    from coderadar import CodeGraph
    snap = CodeGraph(str(dest)).as_of("2026-01-02T12:00:00.000000Z",
                                      branch="old")
    assert snap.address is True
    assert snap.commit == shas["c2"]
    assert snap.basis == "git-history"
    assert snap.parent_drift == 0


def test_ops_as_of_reports_parent_drift(tmp_path):
    repo, _shas = _branchy_repo(tmp_path / "repo")
    dest = _dest(tmp_path)
    ops.migrate_to_commit_time(str(repo), str(dest), embed=False,
                               verify=False)
    out = ops.as_of("2026-01-05T00:00:00.000000Z",
                    symbols=["a.py::module"], branch="feature")
    assert out["parent_drift"] == 1, out
    assert out["address"] is False, out
