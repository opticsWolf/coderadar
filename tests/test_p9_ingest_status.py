"""P9 \u2014 the read-only ingest report (docs/temporal-p5b-and-beyond.md \u00a7P9).

Q-10's answer: expose the ingest *report*, never the write. This is the
report as state, not as a run \u2014 per name and per lineage the cursor, the
stamp, the horizon, the tip, the generation, and how many commits and
completion marks the store holds. Ghost commits are a fact about one run
and stay out; coverage stays `verify_history`'s question.
"""
from __future__ import annotations

import os
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover - CI without the extension
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


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
    return subprocess.run(["git", "rev-parse", rev], cwd=str(repo),
                          check=True, capture_output=True,
                          text=True).stdout.strip()


def _repo(path):
    """main c1..c3; feature f1 off c2; `old` never advanced past c2."""
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    _commit(path, "c2", "2026-01-02T10:00:00+00:00",
            {"a.py": "x = 2\n", "b.py": "y = 1\n"})
    c2 = _sha(path, "main")
    _commit(path, "c3", "2026-01-03T10:00:00+00:00", {"b.py": "y = 2\n"})
    _git(path, "checkout", "-qb", "feature", c2)
    _commit(path, "f1", "2026-01-04T10:00:00+00:00",
            {"a.py": "x = 2\nfeature = 1\n"})
    f1 = _sha(path, "feature")
    _git(path, "checkout", "-q", "main")
    _git(path, "branch", "old", c2)
    return path, {"c2": c2, "c3": _sha(path, "main"), "f1": f1}


def _store(tmp_path):
    return tmp_path / "store" / "coderadar.db"


def _rows(report):
    return {row["name"]: row for row in report["lineages"]}


class TestTheReportIsStateNotARun:
    def test_a_built_store_reports_its_lineages_and_addresses(self, tmp_path):
        repo, shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(store), embed=False,
                                   verify=False)
        report = ops.ingest_status(store_path=str(store))
        assert report["format"] == "1", report
        rows = _rows(report)
        assert set(rows) == {"main", "feature", "old"}, sorted(rows)
        main = rows["main"]
        assert main["lineage"] == "main", main
        assert main["cursor"] == shas["c3"], main
        assert main["tip"] == shas["c3"], main
        assert main["commits"] == 3, main
        assert main["completions"] == 3, main
        assert main["address_only"] is False, main
        assert main["stamp"] and main["horizon"], main
        # `old` never advanced: its tip is c2, which main already holds, so
        # it is an address (D-6 c) and not a lineage of its own.
        old = rows["old"]
        assert old["address_only"] is True, old
        assert old["tip"] == shas["c2"], old
        assert old["lineage"] == "main", old
        assert old["commits"] == 3, old

    def test_a_new_generation_appears_under_its_name(self, tmp_path):
        repo, _shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(store), embed=False,
                                   verify=False)
        _git(repo, "checkout", "-q", "feature")
        _git(repo, "rebase", "-q", "main",
             env=_stamp_env("2026-01-06T10:00:00+00:00"))
        rebased = _sha(repo, "feature")
        _git(repo, "checkout", "-q", "main")
        report = ops.ingest_history(str(repo), branch="feature",
                                    store_path=str(store))
        assert report["lineage"] == "feature#2", report
        rows = _rows(ops.ingest_status(store_path=str(store)))
        feature = rows["feature"]
        assert feature["generation"] == 2, feature
        assert feature["lineage"] == "feature#2", feature
        assert feature["tip"] == rebased, feature
        # The generation answers as itself — a lineage, not a name.
        assert rows["feature#2"]["lineage"] == "feature#2", rows["feature#2"]


class TestTheReportNamesTheRepair:
    def test_a_missing_completion_is_reported_as_incomplete(self, tmp_path):
        repo, _shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        ops.ingest_history(str(repo), store_path=str(store))
        # A pre-horizon store: the commit rows are there, the marks are not.
        ops.ingest_status(store_path=str(store))
        import sqlite3
        rows = sqlite3.connect(str(store)).execute(
            "SELECT key FROM kv_store WHERE key LIKE 'coderadar.commit_done.main.%'"
        ).fetchall()
        assert rows, "the ingest must have written completion marks"
        con = sqlite3.connect(str(store))
        try:
            con.execute("DELETE FROM kv_store WHERE key = ?", (rows[0][0],))
            con.commit()
        finally:
            con.close()
        report = ops.ingest_status(store_path=str(store))
        from coderadar import render
        text = render.ingest_status(report)
        assert "incomplete" in text, text
        assert "backfill_commit_index" in text, text

    def test_a_store_that_does_not_exist_refuses(self, tmp_path):
        with pytest.raises(ops.InvalidRequest):
            ops.ingest_status(store_path=str(tmp_path / "nope.db"))
