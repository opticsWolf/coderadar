"""P9 \u2014 the temporal read surface (docs/temporal-p5b-and-beyond.md \u00a7P9).

The op landed first; this is the surface that exposes it. `coderadar_as_of`
carries the axis flag (`valid`), the address (`at`), the lineage (`branch`)
and the historically strict arm (`strict`), and `coderadar_changes_between`
carries the axis flag. The tools are thin, so the tests pin the two things
a caller can observe: the schema advertises the arguments, and the answer
text names the axis it came from.
"""
from __future__ import annotations

import os
import subprocess

import pytest
from coderadar.mcp import server as server_mod

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover - CI without the extension
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

ID = "a.py::foo"


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


def _repo(path):
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "c1", "2026-01-01T10:00:00+00:00",
            {"a.py": "def foo():\n    return 1\n"})
    _commit(path, "c2", "2026-01-01T11:00:00+00:00",
            {"a.py": "def foo():\n    return 2\n"})
    return path


def _store(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


def _loaded(repo):
    """Ingest, then attach the store so the tools' `graph=None` path reads it."""
    from coderadar import load, ops
    ops.ingest_history(str(repo), store_path=str(_store(repo)))
    load(str(_store(repo)), str(repo))
    return repo


def _params(tool_name: str) -> dict:
    tools = server_mod.create_server(None)._tool_manager.list_tools()
    tool = next(t for t in tools if t.name == tool_name)
    return (tool.parameters or {}).get("properties", {})


class TestTheSchemaAdvertisesTheSurface:
    def test_as_of_takes_the_axis_the_address_and_the_lineage(self):
        props = _params("coderadar_as_of")
        for name in ("at", "branch", "valid", "strict"):
            assert name in props, (name, sorted(props))

    def test_changes_between_takes_the_axis(self):
        assert "valid" in _params("coderadar_changes_between")


class TestTheAnswersNameTheirAxis:
    def test_a_commit_address_reads_the_valid_axis(self, tmp_path):
        repo = _loaded(_repo(tmp_path / "repo"))
        sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(repo),
                             check=True, capture_output=True,
                             text=True).stdout.strip()
        text = server_mod._as_of(None, "", "", [ID], at=sha, valid=True)
        assert "valid axis" in text, text
        assert "foo" in text, text

    def test_the_same_address_reads_the_recorded_axis_by_default(self, tmp_path):
        repo = _loaded(_repo(tmp_path / "repo"))
        sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(repo),
                             check=True, capture_output=True,
                             text=True).stdout.strip()
        text = server_mod._as_of(None, "", "", [ID], at=sha)
        assert "git history" in text, text

    def test_an_unknown_address_is_a_reason_not_a_blank(self, tmp_path):
        _loaded(_repo(tmp_path / "repo"))
        text = server_mod._as_of(None, "", "", [ID], at="0" * 40, valid=True)
        assert "unknown commit" in text, text

    def test_changes_between_can_diff_the_valid_axis(self, tmp_path):
        _loaded(_repo(tmp_path / "repo"))
        text = server_mod._changes_between(
            None, "2026-01-01T10:30:00.000000Z",
            "2026-01-01T11:30:00.000000Z", None, "main", valid=True)
        assert "History diff failed" not in text, text
        assert "foo" in text, text
