"""P5b.7 — rename links (`docs/temporal-p5b-and-beyond.md` §6 P5b.7, A-11).

A rename git detects is a *link row*, never a re-key: the old ids, their
history and their annotations stay where they are, and reads follow the
link forward. These tests pin the write side (the report, the similarity)
and every read the plan names: annotation and comment listings,
`history_of`, and id lookups.
"""

from __future__ import annotations

import json
import os
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover - CI without the extension
    _core = None  # type: ignore[assignment]
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

BODY = "def foo():\n    return 1\n"


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
        target.write_text(content, encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env=_stamp_env(date))


def _store_path(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


def _ingest(repo):
    return ops.ingest_history(str(repo), store_path=str(_store_path(repo)))


def _load(repo):
    from coderadar import CodeGraph, coldstart
    coldstart.build_graph(str(repo))
    return CodeGraph(str(_store_path(repo)))


def _renamed_repo(path):
    """main: c1 old.py; c2 renames it to new.py byte for byte."""
    repo = _init_repo(path)
    _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"old.py": BODY})
    _ingest(repo)
    _git(repo, "mv", "old.py", "new.py")
    _git(repo, "commit", "-qm", "rename",
         env=_stamp_env("2026-01-02T10:00:00+00:00"))
    return repo


class TestTheWriteSide:
    def test_the_report_names_the_rename_and_its_similarity(self, tmp_path):
        repo = _renamed_repo(tmp_path / "repo")
        report = _ingest(repo)
        assert report["renames"] == [
            {"old": "old.py", "new": "new.py", "similarity": 100}
        ], report["renames"]
        # The module link plus every entity whose name survived: `foo`.
        assert report["renames_recorded"] >= 2, report

    def test_a_content_changing_rename_records_no_invented_score(self, tmp_path):
        # Q-16 wants the score on the row so a wrong pair is visible. git2
        # exposes no score, so the honest answer for a pair that needed
        # content changes is `null`, never a measured-looking number.
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"old.py": BODY})
        _ingest(repo)
        _git(repo, "mv", "old.py", "new.py")
        body = BODY + "\ndef bar():\n    return 2\n"
        (repo / "new.py").write_text(body, encoding="utf-8", newline="\n")
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", "rename and grow",
             env=_stamp_env("2026-01-02T10:00:00+00:00"))
        report = _ingest(repo)
        assert report["renames"] == [
            {"old": "old.py", "new": "new.py", "similarity": None}
        ], report["renames"]

    def test_the_chain_is_walkable_from_either_end(self, tmp_path):
        repo = _renamed_repo(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        forward = json.loads(_core.rename_chain("old.py::foo"))
        assert forward["id"] == "new.py::foo", forward
        assert forward["hops"] == [{
            "from": "old.py::foo",
            "to": "new.py::foo",
            "commit": _head(repo),
            "similarity": 100,
        }], forward
        # An id that was never renamed is itself, with no hops.
        plain = json.loads(_core.rename_chain("new.py::foo"))
        assert plain == {"id": "new.py::foo", "hops": []}, plain


def _head(repo):
    out = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


class TestTheReadsFollowIt:
    def test_the_annotation_is_listed_under_both_names(self, tmp_path):
        # F6's test is the shortest form of this (tests/test_p5b_truth.py);
        # this one pins the marker and the both-directions answer.
        repo = _renamed_repo(tmp_path / "repo")
        _load(repo)
        ops.annotate("old.py::foo", "- check the return value")
        _ingest(repo)
        _load(repo)
        ops.annotate("new.py::foo", "- and a second note after the rename")

        after = ops.annotations_on("new.py::foo")
        assert after["count"] == 2, after
        marked = [v for v in after["versions"] if "via_rename" in v]
        assert len(marked) == 1, after
        assert marked[0]["via_rename"]["from"] == "old.py::foo"
        assert marked[0]["via_rename"]["to"] == "new.py::foo"
        assert marked[0]["via_rename"]["similarity"] == 100

        before = ops.annotations_on("old.py::foo")
        assert before["count"] == 2, before
        # Asking about the old name mirrors it: the row minted after the
        # rename is the one reached through the link.
        reached = [v for v in before["versions"] if "via_rename" in v]
        assert len(reached) == 1, before
        assert reached[0]["id"].startswith("annotation:new.py::foo:"), before

    def test_a_comment_follows_the_same_chain(self, tmp_path):
        repo = _renamed_repo(tmp_path / "repo")
        _load(repo)
        ops.comment("old.py::foo", "the return value matters here")
        _ingest(repo)
        _load(repo)
        listed = ops.comments_on("new.py::foo")
        assert listed["count"] == 1, listed
        assert listed["versions"][0]["via_rename"]["from"] == "old.py::foo"

    def test_history_continues_across_two_renames(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        first = "def foo():\n    return 1\n"
        second = "def foo():\n    return 2\n"
        third = "def foo():\n    return 3\n"
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"old.py": first})
        _ingest(repo)
        _git(repo, "mv", "old.py", "mid.py")
        (repo / "mid.py").write_text(second, encoding="utf-8", newline="\n")
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", "rename 1",
             env=_stamp_env("2026-01-02T10:00:00+00:00"))
        _ingest(repo)
        _git(repo, "mv", "mid.py", "new.py")
        (repo / "new.py").write_text(third, encoding="utf-8", newline="\n")
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", "rename 2",
             env=_stamp_env("2026-01-03T10:00:00+00:00"))
        _ingest(repo)
        _load(repo)

        chain = ops.history_of("new.py")
        assert chain["count"] == 3, chain
        # Newest first, and the digests are the three bodies in order — the
        # chain crossed two renames without losing a version.
        import hashlib
        digests = [v["digest"] for v in chain["versions"]]
        assert digests == [
            hashlib.sha256(third.encode()).hexdigest(),
            hashlib.sha256(second.encode()).hexdigest(),
            hashlib.sha256(first.encode()).hexdigest(),
        ], digests
        # The versions reached through a link say which file they are and
        # which hop led there; the newest file's own version does not.
        assert "via_rename" not in chain["versions"][0]
        assert chain["versions"][1]["file"] == "mid.py"
        assert chain["versions"][1]["via_rename"]["from"] == "mid.py::module"
        assert chain["versions"][2]["file"] == "old.py"
        assert chain["versions"][2]["via_rename"]["from"] == "old.py::module"

    def test_find_entity_resolves_a_renamed_id(self, tmp_path):
        repo = _renamed_repo(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        found = ops.find_entity("old.py::foo")
        assert found is not None, "the old id must resolve forward"
        assert found["id"] == "new.py::foo", found
        assert found["via_rename"]["from"] == "old.py::foo"
        # A direct hit stays unmarked, so a pre-phase response is unchanged.
        direct = ops.find_entity("new.py::foo")
        assert direct is not None and "via_rename" not in direct, direct
