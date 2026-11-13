"""Commit-message bodies (P6c slice, DR-45).

New ingests carry `body` in `commit_concept`; the backfill heals
subject-only rows from `git log` on each row's own mint lineage,
idempotently. The heal path itself is pinned in Rust
(`backfill_tests`); here: bodies land at ingest, the backfill is a
no-op on fresh rows, refusals refuse, and the CLI works.
"""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover - gate for CI without the extension
    _core = None  # type: ignore[assignment]
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


def _git(repo, *args, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    subprocess.run(["git", *args], cwd=str(repo), env=merged,
                   check=True, capture_output=True)


def _stamp_env(date):
    return {
        "GIT_AUTHOR_NAME": "T Author",
        "GIT_AUTHOR_EMAIL": "author@t.t",
        "GIT_COMMITTER_NAME": "T Committer",
        "GIT_COMMITTER_EMAIL": "committer@t.t",
        "GIT_AUTHOR_DATE": date,
        "GIT_COMMITTER_DATE": date,
    }


def _commit(repo, message, date, files):
    for name, content in files.items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env=_stamp_env(date))


def _repo(path):
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "subject only", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    _commit(path, "with body\n\nfirst body line\nsecond body line",
            "2026-01-02T10:00:00+00:00", {"a.py": "x = 2\n"})
    return path


def _content(store, commit_id):
    with sqlite3.connect(str(store)) as conn:
        (raw,) = conn.execute(
            "SELECT content FROM concepts WHERE id = ?", (commit_id,)).fetchone()
    return json.loads(raw)


class TestIngestStoresBodies:
    def test_subject_and_body_land_at_ingest(self, tmp_path):
        repo = _repo(tmp_path / "repo")
        store = tmp_path / "store.db"
        ops.ingest_history(str(repo), store_path=str(store))
        out = subprocess.run(
            ["git", "log", "--format=%H", "--reverse"],
            cwd=str(repo), check=True, capture_output=True, text=True,
        )
        shas = out.stdout.split()
        assert len(shas) == 2
        first = _content(store, f"commit:{shas[0]}")
        assert first["subject"] == "subject only"
        assert first.get("body", "") == ""
        second = _content(store, f"commit:{shas[1]}")
        assert second["subject"] == "with body"
        assert second["body"] == "first body line\nsecond body line"


class TestBackfill:
    def test_fresh_rows_heal_zero_with_auditable_skip(self, tmp_path):
        repo = _repo(tmp_path / "repo")
        store = tmp_path / "store.db"
        ops.ingest_history(str(repo), store_path=str(store))
        report = ops.backfill_commit_messages(str(repo), str(store))
        assert report["store"] == str(store)
        assert report["repo"] == str(repo)
        assert report["commits_healed"] == 0
        # The subject-only commit still qualifies for selection (empty
        # body) and skips honestly: git has no body for it either.
        reasons = [s["reason"] for s in report["commits_skipped"]]
        assert reasons == ["no-body"], report
        assert isinstance(report["elapsed_secs"], float)

    def test_second_run_is_stable(self, tmp_path):
        repo = _repo(tmp_path / "repo")
        store = tmp_path / "store.db"
        ops.ingest_history(str(repo), store_path=str(store))
        first = ops.backfill_commit_messages(str(repo), str(store))
        second = ops.backfill_commit_messages(str(repo), str(store))
        # elapsed_secs is timing, not state: everything else is identical.
        for report in (first, second):
            assert isinstance(report.pop("elapsed_secs"), float)
        assert first == second

    def test_bad_repo_refuses(self, tmp_path):
        store = tmp_path / "store.db"
        store.touch()
        with pytest.raises(Exception, match="[Rr]epo"):
            ops.backfill_commit_messages(str(tmp_path / "nope"), str(store))

    def test_index_now_store_refuses(self, tmp_path):
        from coderadar import coldstart
        repo = _repo(tmp_path / "repo")
        coldstart.build_graph(str(repo), create_store=True)
        old = repo / ".coderadar" / "store" / "coderadar.db"
        with pytest.raises(Exception, match="[Ii]ndex-now|commit-time"):
            ops.backfill_commit_messages(str(repo), str(old))
        # body) and skips honestly: git has no body for it either.