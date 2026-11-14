"""P5b.4 \u2014 addresses (`docs/temporal-p5b-and-beyond.md` \u00a7P5b.4).

`at=` accepts a sha or a ref (A-3). A name resolves to the tip the store
records for it \u2014 the name's own, its lineage's, or (for a generation,
which has no tip key) the lineage's horizon \u2014 and the answer is folded at
that commit. Nothing rounds to a neighbour: an unknown sha or name refuses
by name.

The valid-axis half of `at=` lives with the axis (`tests/test_p5b3_valid.py`):
an address resolves to the instant to fold there, and an address into a
branch refuses with `ValidBranchNeedsU2`.
"""
from __future__ import annotations

import hashlib
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
    return subprocess.run(["git", "rev-parse", rev], cwd=str(repo),
                          check=True, capture_output=True, text=True).stdout.strip()


def _repo(path):
    """main c1..c3; feature f1 off c2 (a.py edited on the branch)."""
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
    return path, {"c2": c2, "c3": _sha(path, "main"), "f1": f1}


def _store(tmp_path):
    return tmp_path / "store" / "coderadar.db"


def _migrate(repo, store):
    return ops.migrate_to_commit_time(str(repo), str(store), embed=False,
                                      verify=False)


def _refs(ids, ts, branch, **kw):
    from coderadar import _core as _C
    return _C.source_refs_at(ids, ts, branch, **kw)


class TestANameAddressesItsTip:
    def test_a_branch_name_reads_the_commit_it_records(self, tmp_path):
        repo, shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        _migrate(repo, store)
        by_name = ops.as_of("", branch="feature", at="feature",
                            symbols=["a.py::module"])
        by_sha = ops.as_of("", branch="feature", at=shas["f1"],
                           symbols=["a.py::module"])
        assert by_name["basis"] == "git-history", by_name
        assert by_name["commit"] == shas["f1"], by_name
        assert by_name["address"] is True, by_name
        assert by_name["entities"]["a.py::module"] == \
            by_sha["entities"]["a.py::module"]
        # The bytes are f1's, not the branch's present: the address named a
        # commit, and a trunk file read at it stays the trunk's.
        out = _refs(["a.py::module"], "", "feature", at="feature")
        assert out["refs"]["a.py::module"]["digest"] == \
            _digest("x = 2\nfeature = 1\n")

    def test_the_trunk_name_reads_the_head(self, tmp_path):
        repo, shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        _migrate(repo, store)
        by_name = ops.as_of("", branch="main", at="main",
                            symbols=["a.py::module"])
        assert by_name["commit"] == shas["c3"], by_name
        assert by_name["address"] is True, by_name


class TestAGenerationStaysAddressable:
    def test_a_rewritten_name_and_its_new_generation_both_resolve(self, tmp_path):
        repo, shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        _migrate(repo, store)
        # A rebase onto main's tip is the rewrite P5b.6 (D-5) answers with a
        # new generation: `feature` moves to feature#2, feature#1 freezes.
        _git(repo, "checkout", "-q", "feature")
        _git(repo, "rebase", "-q", "main",
             env=_stamp_env("2026-01-06T10:00:00+00:00"))
        rebased = _sha(repo, "feature")
        _git(repo, "checkout", "-q", "main")
        assert rebased != shas["f1"]
        report = ops.ingest_history(str(repo), branch="feature",
                                    store_path=str(store))
        assert report["lineage"] == "feature#2", report

        # The name follows its newest generation (D-5)…
        moved = ops.as_of("", branch="feature", at="feature",
                          symbols=["a.py::module"])
        assert moved["commit"] == rebased, moved
        # …and the generation names itself: it has no branch-tip key of its
        # own, so its horizon answers — the newest commit it completed.
        generation = ops.as_of("", branch="feature", at="feature#2",
                               symbols=["a.py::module"])
        assert generation["commit"] == rebased, generation
        assert generation["address"] is True, generation
        # The frozen generation is still readable by the sha that names it
        # (P5b.6: a generation is addressed by its commits).
        out = _refs(["a.py::module"], "", "feature", at=shas["f1"])
        assert out["sha"] == shas["f1"], out
        assert out["refs"]["a.py::module"]["digest"] == \
            _digest("x = 2\nfeature = 1\n")


class TestTheCliCanSayIt:
    def test_an_address_read_needs_no_positional_timestamp(self):
        # `coderadar as-of --at SHA` is the documented spelling, so the
        # parser must accept it: the address *is* the instant. (Found in the
        # 0.16.0 docs audit: QUICKREF taught the spelling the CLI refused.)
        from coderadar.cli import main
        command = main.commands["as-of"]
        ctx = command.make_context("as-of", ["--at", "f" * 40])
        assert ctx.params["at"] == "f" * 40
        assert ctx.params["timestamp"] == ""
        assert ctx.params["valid"] is False


class TestNothingRounds:
    def test_an_unknown_name_refuses_on_both_axes(self, tmp_path):
        repo, _shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        _migrate(repo, store)
        with pytest.raises(ops.InvalidRequest, match="UnknownBranch"):
            ops.as_of("", branch="main", at="no-such-branch",
                      symbols=["a.py::module"])
        with pytest.raises(ops.InvalidRequest, match="UnknownBranch"):
            ops.as_of("", branch="main", at="no-such-branch",
                      symbols=["a.py::module"], valid=True)

    def test_an_unknown_sha_refuses(self, tmp_path):
        repo, _shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        _migrate(repo, store)
        with pytest.raises(ops.InvalidRequest, match="unknown commit"):
            ops.as_of("", branch="main", at="f" * 40,
                      symbols=["a.py::module"])

    def test_a_timestamp_address_restates_the_instant(self, tmp_path):
        repo, _shas = _repo(tmp_path / "repo")
        store = _store(tmp_path)
        _migrate(repo, store)
        ts = "2026-01-02T12:00:00.000000Z"
        positional = ops.as_of(ts, branch="main", symbols=["a.py::module"])
        addressed = ops.as_of("", branch="main", at=ts, symbols=["a.py::module"])
        assert addressed["fold_ts"] == positional["fold_ts"], addressed
        assert addressed["entities"] == positional["entities"], addressed
        assert addressed["address"] is False, addressed
