"""P5d slice B, first half: an observation names its lineage.

`docs/temporal-p5b-and-beyond.md` §3.1.3 / A-8 / D-4 a1. The law this file
pins: a bare observation write lands where the *tree* is — on the branch a
commit-time store recorded as checked out — because a branch's edit is that
lineage's belief, not the trunk's. A branch-owned file updates its own mint
row (W1); an inherited file writes the branch's overlay version over it
(W2), and the trunk's row is untouched either way. An index-now store keeps
one lineage by construction and ignores the record.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess

import pytest
from coderadar import load, ops

try:
    from coderadar import _core
except ImportError:  # pragma: no cover - CI without the extension
    _core = None  # type: ignore[assignment]

pytestmark = pytest.mark.skipif(
    _core is None, reason="Rust _core extension not built")

# Windows CRLF hazard: build source text from chr(10), never a literal
# escape in this file, so the builder cannot silently write backslash-n.
NL = chr(10)
BODY = f"def greet(name):{NL}    return 'hello ' + name{NL}"
BODY2 = f"def greet(name):{NL}    return 'hi ' + name{NL}"
BRANCH_BODY = f"def greet(name):{NL}    return 'branch ' + name{NL}"
FEAT = f"def g():{NL}    return 1{NL}"
FEAT2 = f"def g():{NL}    return 2{NL}"


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


def _init_repo(path):
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    return path


def _commit(repo, message, date, files=None):
    for name, content in (files or {}).items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline=NL)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env=_stamp_env(date))


def _store_path(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


def _ingest(repo, branch="main"):
    return ops.ingest_history(str(repo), branch=branch,
                              store_path=str(_store_path(repo)))


def _open(repo):
    return load(str(_store_path(repo)), str(repo))


def _rows(repo, sql, args=()):
    with sqlite3.connect(str(_store_path(repo))) as conn:
        return conn.execute(sql, args).fetchall()


def _kv(repo, key):
    rows = _rows(repo, "SELECT value FROM kv_store WHERE key = ?", (key,))
    return rows[0][0] if rows else None


def _diverged_repo(path):
    """main: c1 app.py, c2 edits it. feature: f1 adds feat.py on top of c1."""
    repo = _init_repo(path)
    _commit(repo, "c1", "2026-01-01T10:00:00+00:00",
            {"app.py": BODY, ".gitignore": ".coderadar/" + NL})
    _ingest(repo)
    _git(repo, "checkout", "-qb", "feature")
    _commit(repo, "f1", "2026-01-02T10:00:00+00:00", {"feat.py": FEAT})
    _ingest(repo, "feature")
    _git(repo, "checkout", "-q", "main")
    _commit(repo, "c2", "2026-01-03T10:00:00+00:00", {"app.py": BODY2})
    _ingest(repo)
    return repo


class TestTheObservationNamesItsLineage:
    def test_a_branch_owned_file_updates_on_the_branch(self, tmp_path):
        """The edit is the branch's belief: its own mint row moves, once."""
        repo = _diverged_repo(tmp_path / "repo")
        _core.record_checkout(str(_store_path(repo)), "feature")
        os.chdir(repo)
        graph = _open(repo)

        before = _rows(repo, "SELECT content FROM concepts"
                             " WHERE id = 'feat.py::g'")[0][0]
        report = graph.update_file("feat.py", FEAT2, source="watcher")

        assert report.lineage == "feature", report.lineage
        assert report.source == "watcher"
        rows = _rows(repo, "SELECT branch_id, content FROM concepts"
                           " WHERE id = 'feat.py::g'")
        assert len(rows) == 1, rows
        branch, content = rows[0]
        assert branch == "feature"
        assert content != before, "the branch's own belief moved"
        # One row per id: the W1 update replaced the mint's content, it did
        # not write a second row.
        assert _rows(repo, "SELECT COUNT(*) FROM concepts"
                          " WHERE id = 'feat.py::g'")[0][0] == 1
        extra = json.loads(_rows(repo, "SELECT extra FROM concepts"
                                       " WHERE id = 'feat.py::module'")[0][0])
        assert extra["coderadar"]["source"] == "watcher"
        assert extra["coderadar"]["source_blob"] == hashlib.sha256(
            FEAT2.encode()).hexdigest()

    def test_an_inherited_file_gets_the_branchs_overlay_row(self, tmp_path):
        """The trunk's row is the trunk's: the branch's edit is a version."""
        repo = _diverged_repo(tmp_path / "repo")
        _core.record_checkout(str(_store_path(repo)), "feature")
        os.chdir(repo)
        graph = _open(repo)

        trunk_before = _rows(repo, "SELECT content, extra FROM concepts"
                                   " WHERE id = 'app.py::module'")[0]
        report = graph.update_file("app.py", BRANCH_BODY, source="watcher")

        assert report.lineage == "feature", report.lineage
        versions = _rows(
            repo,
            "SELECT branch_id FROM concept_versions WHERE id = 'app.py::module'",
        )
        assert versions and all(b == "feature" for (b,) in versions), versions
        # The trunk still believes c2's content — untouched by the branch.
        trunk_after = _rows(repo, "SELECT content, extra FROM concepts"
                                  " WHERE id = 'app.py::module'")[0]
        assert trunk_after == trunk_before
        # And the overlay row is the branch's, carrying the observed bytes.
        (overlay_extra,) = _rows(
            repo,
            "SELECT extra FROM concept_versions WHERE id = 'app.py::module'",
        )[0]
        tag = json.loads(overlay_extra)["coderadar"]
        assert tag["source"] == "watcher"
        assert tag["source_blob"] == hashlib.sha256(
            BRANCH_BODY.encode()).hexdigest()

    def test_the_observation_mark_is_per_lineage(self, tmp_path):
        """A branch edit seeds the branch's nudge chain, not the trunk's."""
        repo = _diverged_repo(tmp_path / "repo")
        _core.record_checkout(str(_store_path(repo)), "feature")
        os.chdir(repo)
        graph = _open(repo)
        assert _kv(repo, "coderadar.observed.feature") is None

        graph.update_file("feat.py", FEAT2, source="watcher")

        assert _kv(repo, "coderadar.observed.feature"), "the branch's mark"
        assert _kv(repo, "coderadar.observed.main") is None, (
            "an edit on the branch is not a trunk observation")

        _core.record_checkout(str(_store_path(repo)), "main")
        graph.update_file("app.py", BODY2, source="watcher")
        assert _kv(repo, "coderadar.observed.main")

    def test_a_deletion_on_the_branch_retires_only_the_branch(self, tmp_path):
        repo = _diverged_repo(tmp_path / "repo")
        _core.record_checkout(str(_store_path(repo)), "feature")
        os.chdir(repo)
        graph = _open(repo)

        graph.remove_file("feat.py")

        rows = _rows(repo, "SELECT branch_id, retired FROM concepts"
                           " WHERE id = 'feat.py::g'")
        assert rows == [("feature", 1)], rows
        # The trunk's own rows are not the branch's to retire.
        live = _rows(repo, "SELECT retired FROM concepts"
                           " WHERE id = 'app.py::module'")
        assert live == [(0,)], live

    def test_analyze_refuses_a_branch_checkout(self, tmp_path):
        """A whole-tree reindex on a branch would assert branch content on
        the trunk; until the checkout sweep exists, the act refuses."""
        repo = _diverged_repo(tmp_path / "repo")
        _core.record_checkout(str(_store_path(repo)), "feature")
        os.chdir(repo)
        from coderadar import analyze

        with pytest.raises(ValueError, match="checked out on"):
            analyze(str(repo))

    def test_an_index_now_store_keeps_one_lineage(self, tmp_path):
        """No commit-time shape, no checkout: the write stays on the trunk."""
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00",
                {"app.py": BODY, ".gitignore": ".coderadar/" + NL})
        os.chdir(repo)
        from coderadar import analyze
        graph = analyze(str(repo), create_store=True)
        graph.record_checkout("feature")

        report = graph.update_file("app.py", BODY2, source="watcher")

        assert report.lineage == "main", report.lineage
        rows = _rows(repo, "SELECT id FROM concept_versions"
                           " WHERE branch_id = 'feature'")
        assert rows == [], rows
