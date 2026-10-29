"""Commit-time stamps land, nudge, and order the chain (plan §5 P5a.3, DR-41).

Done clause: `valid_from` is the commit's time (not the author's, not
the ingest's), ties and regressions nudge by the smallest representable
step and say so, and `history_of` over commit-stamped generations
returns commit order. The axis tests; the op-shape tests live in
`test_ingest_history.py`.
"""

from __future__ import annotations

import os
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core  # noqa: F401 - presence gate for the native extension
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


def _git(repo, *args, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    subprocess.run(["git", *args], cwd=str(repo), env=merged,
                   check=True, capture_output=True)


def _commit_env(author_date, committer_date):
    return {
        "GIT_AUTHOR_NAME": "T Author",
        "GIT_AUTHOR_EMAIL": "author@t.t",
        "GIT_COMMITTER_NAME": "T Committer",
        "GIT_COMMITTER_EMAIL": "committer@t.t",
        "GIT_AUTHOR_DATE": author_date,
        "GIT_COMMITTER_DATE": committer_date,
    }


def _init_repo(path):
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    return path


def _commit(repo, message, author_date, committer_date, files):
    for name, content in files.items():
        (repo / name).write_text(content, encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message,
         env=_commit_env(author_date, committer_date))


def _store_path(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


def _history(repo, file="a.py"):
    from coderadar import CodeGraph, coldstart
    coldstart.build_graph(str(repo))
    return CodeGraph(str(_store_path(repo))).history_of(file)["versions"]


class TestCommitterClock:
    def test_valid_from_is_committer_time_not_author_time(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "one",
                "2020-05-05T05:05:05+00:00", "2026-01-01T10:00:00+00:00",
                {"a.py": "x = 1\n"})
        report = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        assert report["stamps_nudged"] == 0

        (version,) = _history(repo)
        assert version["valid_from"] == "2026-01-01T10:00:00.000000Z"

    def test_committer_offset_folds_to_the_same_instant(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "one",
                "2026-01-01T10:00:00+00:00", "2026-01-01T12:00:00+02:00",
                {"a.py": "x = 1\n"})
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))

        (version,) = _history(repo)
        # 12:00 at +02:00 is 10:00Z: the offset folds into the instant at
        # the write boundary (Macrame accepts Zulu only).
        assert version["valid_from"] == "2026-01-01T10:00:00.000000Z"


class TestNudge:
    def test_same_second_tie_advances_one_microsecond(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        stamp = "2026-01-01T10:00:00+00:00"
        _commit(repo, "one", stamp, stamp, {"a.py": "x = 1\n"})
        _commit(repo, "two", stamp, stamp, {"a.py": "x = 2\n"})
        report = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))

        assert report["stamps_nudged"] == 1
        first, second = _history(repo)[::-1]
        assert first["valid_from"] == "2026-01-01T10:00:00.000000Z"
        assert second["valid_from"] == "2026-01-01T10:00:00.000001Z"

    def test_regressing_stamp_is_nudged_never_written_backwards(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "one",
                "2026-06-01T10:00:00+00:00", "2026-06-01T10:00:00+00:00",
                {"a.py": "x = 1\n"})
        _commit(repo, "two",
                "2026-01-01T10:00:00+00:00", "2026-01-01T10:00:00+00:00",
                {"a.py": "x = 2\n"})
        report = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))

        assert report["stamps_nudged"] == 1
        first, second = _history(repo)[::-1]
        assert first["valid_from"] < second["valid_from"]
        assert second["valid_from"] == "2026-06-01T10:00:00.000001Z"
