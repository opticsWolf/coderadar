"""P5d slice B, second half: git drives the sync (A-8 / D-4 a1).

`ops.sync_git` is the op the ref watcher calls. This file pins its four acts
and the two sweeps the plan specifies:

* a new commit is ingested by the trunk (the ref-changed trigger);
* a startup sync records edits made while CodeRadar was off, and a file that
  was reverted while it was off is returned to its committed content;
* a checkout returns the left lineage's dirty files to *its* head, and the
  carried edit is recorded on the new lineage — the working tree reads as the
  new lineage's, the old one reads as its committed head.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
import time

import pytest

from coderadar import GitWatcher, load, ops

try:
    from coderadar import _core
except ImportError:  # pragma: no cover - CI without the extension
    _core = None  # type: ignore[assignment]

pytestmark = pytest.mark.skipif(
    _core is None, reason="Rust _core extension not built")

NL = chr(10)
A_BODY = f"def a():{NL}    return 1{NL}"
B_BODY = f"def b():{NL}    return 2{NL}"
A_EDIT = f"def a():{NL}    return 111{NL}"
TS_OPEN = "9999-12-31T23:59:59.999999Z"
TS_OPEN_LEGACY = "9999-12-31T00:00:00.000000Z"


def _git(repo, *args, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    subprocess.run(["git", *args], cwd=str(repo), env=merged,
                   check=True, capture_output=True)


def _stamp_env(committer_date):
    return {
        "GIT_AUTHOR_NAME": "T", "GIT_AUTHOR_EMAIL": "t@t.t",
        "GIT_COMMITTER_NAME": "T", "GIT_COMMITTER_EMAIL": "t@t.t",
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


def _ingest_all(repo):
    ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
    _git(repo, "checkout", "-qb", "feature")
    _commit(repo, "f1", "2026-01-02T10:00:00+00:00", {"b.py": B_BODY})
    ops.ingest_history(str(repo), "feature", store_path=str(_store_path(repo)))
    _git(repo, "checkout", "-q", "main")
    # main's second commit touches a *different* file, so a.py stays
    # identical at both heads and a dirty a.py can be carried across the
    # checkout (the D-4 a1 shape).
    _commit(repo, "c2", "2026-01-03T10:00:00+00:00", {"c.py": "x = 1" + NL})
    ops.ingest_history(str(repo), store_path=str(_store_path(repo)))


def _diverged(path):
    repo = _init_repo(path)
    _commit(repo, "c1", "2026-01-01T10:00:00+00:00",
            {"a.py": A_BODY, ".gitignore": ".coderadar/" + NL})
    _ingest_all(repo)
    return repo


def _rows(repo, sql, args=()):
    with sqlite3.connect(str(_store_path(repo))) as conn:
        return conn.execute(sql, args).fetchall()


def _belief(repo, entity_id, lineage):
    """The lineage's open belief for an id: its digest, source tag."""
    if lineage == "main":
        rows = _rows(repo, "SELECT extra, retired FROM concepts"
                           " WHERE id = ? AND branch_id = 'main'", (entity_id,))
        extra, retired = rows[-1]
    else:
        rows = _rows(repo, "SELECT extra, valid_to FROM concept_versions"
                           " WHERE id = ? AND branch_id = ?", (entity_id, lineage))
        extra, valid_to = rows[-1]
        retired = 0 if valid_to in (TS_OPEN, TS_OPEN_LEGACY) else 1
    assert rows, (entity_id, lineage, rows)
    tag = json.loads(extra)["coderadar"]
    return tag, retired


def _sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class TestCommitSync:
    def test_a_new_commit_is_ingested_by_the_trunk(self, tmp_path):
        repo = _diverged(tmp_path / "repo")
        os.chdir(repo)
        ops.sync_git(str(repo), store_path=str(_store_path(repo)))
        _commit(repo, "c3", "2026-01-04T10:00:00+00:00", {"d.py": "d = 4" + NL})

        report = ops.sync_git(str(repo), store_path=str(_store_path(repo)))

        assert report["head_moved"] is True
        assert report["checkout_changed"] is False
        trunk = [i for i in report["ingested"] if i["branch"] == "main"]
        assert trunk and trunk[-1]["commits_ingested"] == 1, report["ingested"]
        # The commit's concept is the ingest's own record that it ran.
        rows = _rows(repo, "SELECT id FROM concepts WHERE id LIKE 'commit:%'")
        assert rows, "the commit concept was written"

    def test_a_merged_branch_name_is_recorded_as_an_address(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00",
                {"a.py": A_BODY, ".gitignore": ".coderadar/" + NL})
        _ingest_all(repo)
        _git(repo, "branch", "merged")
        report = ops.sync_git(str(repo), store_path=str(_store_path(repo)))
        assert report["addresses_recorded"] >= 1, report
        rows = _rows(repo, "SELECT value FROM kv_store"
                           " WHERE key = 'coderadar.branch.tip.merged'")
        assert rows and len(rows[0][0]) == 40, rows


class TestTheStartupSweep:
    def test_an_edit_made_while_off_is_recorded_and_a_revert_returns_it(self, tmp_path):
        repo = _diverged(tmp_path / "repo")
        os.chdir(repo)
        graph = load(str(_store_path(repo)), str(repo))
        ops.sync_git(str(repo), graph=graph)  # clean baseline
        (repo / "a.py").write_text(A_EDIT, encoding="utf-8", newline=NL)

        report = ops.sync_git(str(repo), graph=graph)
        observed = [o for o in report["observations"] if o["file"] == "a.py"]
        assert observed and observed[0]["source"] == "watch", report
        tag, _ = _belief(repo, "a.py::module", "main")
        assert tag["source"] == "watch"
        assert tag["source_blob"] == _sha(repo / "a.py")

        # Nothing changed since: the sweep must not re-assert the same bytes.
        again = ops.sync_git(str(repo), graph=graph)
        assert [o for o in again["observations"] if o["file"] == "a.py"] == []

        # Reverted while CodeRadar was off: the belief returns to HEAD.
        _git(repo, "checkout", "--", "a.py")
        reverted = ops.sync_git(str(repo), graph=graph)
        assert any(o["file"] == "a.py" for o in reverted["observations"]), reverted
        tag, retired = _belief(repo, "a.py::module", "main")
        assert tag["source"] == "watch"
        assert retired == 0
        assert tag["source_blob"] == _sha(repo / "a.py")
        head_text = _core.git_blob_text(str(repo), _head_sha(repo), "a.py")
        assert (repo / "a.py").read_text(encoding="utf-8") == head_text


def _head_sha(repo):
    return _core.git_head(str(repo))["sha"]


class TestTheCheckoutSweep:
    def test_the_left_lineage_returns_to_its_head_and_the_edit_carries(self, tmp_path):
        repo = _diverged(tmp_path / "repo")
        os.chdir(repo)
        _git(repo, "checkout", "-q", "feature")
        graph = load(str(_store_path(repo)), str(repo))
        ops.sync_git(str(repo), graph=graph)
        (repo / "a.py").write_text(A_EDIT, encoding="utf-8", newline=NL)
        ops.sync_git(str(repo), graph=graph)  # a.py is feature's observation now

        _git(repo, "checkout", "-q", "main")
        report = ops.sync_git(str(repo), graph=graph)

        assert report["checkout_changed"] is True, report
        by_lineage = {(o["file"], o["lineage"], o["source"])
                      for o in report["observations"]}
        assert ("a.py", "feature", "checkout") in by_lineage, by_lineage
        assert ("a.py", "main", "checkout") in by_lineage, by_lineage

        # The left lineage reads as its committed head...
        feature_tag, feature_retired = _belief(repo, "a.py::module", "feature")
        feature_head = _core.git_blob_text(str(repo), _feature_sha(repo), "a.py")
        assert feature_tag["source_blob"] == hashlib.sha256(
            feature_head.encode()).hexdigest()
        assert feature_retired == 0
        # ... the new lineage as the working tree, which still carries the edit.
        main_tag, _ = _belief(repo, "a.py::module", "main")
        assert main_tag["source"] == "checkout"
        assert main_tag["source_blob"] == _sha(repo / "a.py")
        assert (repo / "a.py").read_text(encoding="utf-8") == A_EDIT


def _feature_sha(repo):
    return _git_out(repo, "rev-parse", "feature")


def _git_out(repo, *args):
    return subprocess.run(["git", *args], cwd=str(repo), check=True,
                          capture_output=True, text=True).stdout.strip()


class TestTheRefWatcher:
    def test_a_commit_triggers_a_sync(self, tmp_path):
        repo = _diverged(tmp_path / "repo")
        os.chdir(repo)
        watcher = GitWatcher(str(repo), store_path=str(_store_path(repo)),
                             poll_ms=50)
        try:
            watcher.start()
            _commit(repo, "c3", "2026-01-05T10:00:00+00:00", {"e.py": "e = 5" + NL})
            deadline = time.time() + 20.0
            while time.time() < deadline:
                if any(i["commits_ingested"] for r in watcher.reports
                       for i in r["ingested"]):
                    break
                time.sleep(0.05)
            assert watcher.last_error is None, watcher.last_error
            assert any(i["branch"] == "main" and i["commits_ingested"]
                       for r in watcher.reports for i in r["ingested"]), \
                [r["ingested"] for r in watcher.reports]
        finally:
            watcher.stop()
