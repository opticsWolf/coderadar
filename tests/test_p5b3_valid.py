"""P5b.3 — the valid axis (trunk) (`docs/temporal-p5b-and-beyond.md` §6 P5b.3).

A read names its axis (INV-16). The recorded default answers "what had
CodeRadar recorded by then", needs P5b.3a's horizon rule, and — before the
horizon — the commit index. `valid=True` answers the other question, "what
was true in the repository at this instant", from the ledger's own
intervals: no horizon, no commit index, `basis == "valid"`.

Trunk reads ship now. Branch reads refuse with `ValidBranchNeedsU2`
(Macrame U-2, 0.23.0) rather than folding the trunk for a branch. `at=`
resolves an address to the pair it denotes (P5b.4): a sha or a name the
store knows, turned into the instant to fold — on this axis the fold then
answers the trunk, and an address into a branch refuses with the same
`ValidBranchNeedsU2` rather than folding the trunk for it.
"""

from __future__ import annotations

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

C1 = "def foo():\n    return 1\n"
C2 = "def foo():\n    return 2\n"
C3 = "def foo():\n    return 3\n"
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


def _ingest(repo, branch="main"):
    return ops.ingest_history(str(repo), branch=branch,
                              store_path=str(_store_path(repo)))


def _load(repo):
    from coderadar import CodeGraph, coldstart
    coldstart.build_graph(str(repo))
    return CodeGraph(str(_store_path(repo)))


def _ref_digest(ts: str, valid: bool = False) -> str | None:
    """The source blob digest at one instant, on either axis."""
    from coderadar._core import entity_source_ref_at
    ref = entity_source_ref_at(ID, ts, "main", valid=valid)
    return (ref or {}).get("digest")


def _three_commits(path):
    """main: c1 (returns 1), c2 (returns 2), c3 (returns 3), an hour apart."""
    repo = _init_repo(path)
    _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"a.py": C1})
    _commit(repo, "c2", "2026-01-01T11:00:00+00:00", {"a.py": C2})
    _commit(repo, "c3", "2026-01-01T12:00:00+00:00", {"a.py": C3})
    return repo


class TestTheValidAxisAnswersByInterval:
    def test_the_two_arms_agree_at_an_instant_between_commits(self, tmp_path):
        # The strongest statement available: at 10:30 (c1 is the last commit
        # at or before it) the valid fold and the git-history arm are two
        # roads to one answer. They share no mechanism — the arm resolves
        # through the commit index, the valid fold through the rows' own
        # intervals.
        repo = _three_commits(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        ts = "2026-01-01T10:30:00.000000Z"
        valid = ops.as_of(ts, symbols=[ID], valid=True)
        assert valid["basis"] == "valid", valid
        assert valid["horizon"] is None, valid
        assert valid["commit"] is None, valid
        assert valid["entities"][ID] is not None, valid
        history = ops.as_of(ts, symbols=[ID])
        assert history["basis"] == "git-history", history
        assert history["entities"][ID] == valid["entities"][ID], (
            "the two arms must agree about what was true at this instant")
        # 11:30 is c2's: the *bytes* differ, on both arms (the entity dict
        # carries spans, which a one-line edit leaves identical).
        later = ops.as_of("2026-01-01T11:30:00.000000Z", symbols=[ID], valid=True)
        assert later["entities"][ID] is not None, later
        early_digest = _ref_digest(ts, valid=True)
        late_digest = _ref_digest("2026-01-01T11:30:00.000000Z", valid=True)
        assert early_digest and late_digest and early_digest != late_digest, (
            "c2 changed the function, so the two instants must differ")
        # ...and the git-history arm names the same bytes at 10:30.
        assert _ref_digest(ts) == early_digest

    def test_a_deleted_entity_is_absent_after_its_deletion(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"a.py": C1})
        _commit(repo, "c2", "2026-01-01T11:00:00+00:00", {"a.py": "x = 1\n"})
        _ingest(repo)
        _load(repo)
        before = ops.as_of("2026-01-01T10:30:00.000000Z", symbols=[ID], valid=True)
        assert before["entities"][ID] is not None, before
        after = ops.as_of("2026-01-01T11:30:00.000000Z", symbols=[ID], valid=True)
        assert after["entities"][ID] is None, after

    def test_changes_between_reads_two_valid_instants(self, tmp_path):
        repo = _three_commits(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        diff = ops.changes_between("2026-01-01T10:30:00.000000Z",
                                   "2026-01-01T12:30:00.000000Z", valid=True)
        # Same id, different content: modified, not added+removed.
        assert diff["counts"]["modified"] >= 1, diff["counts"]
        assert diff["counts"]["added"] == 0, diff["counts"]
        assert diff["counts"]["removed"] == 0, diff["counts"]


class TestTheAxisRefusesWhatItCannotAnswer:
    def test_a_branch_valid_read_names_u2(self, tmp_path):
        repo = _three_commits(tmp_path / "repo")
        _git(repo, "checkout", "-q", "-b", "feature", "HEAD")
        _commit(repo, "f1", "2026-01-01T13:00:00+00:00", {"b.py": "y = 1\n"})
        _ingest(repo)
        _ingest(repo, branch="feature")
        _load(repo)
        with pytest.raises(ops.InvalidRequest, match="ValidBranchNeedsU2"):
            ops.as_of("2026-01-01T13:30:00.000000Z", symbols=[ID],
                      branch="feature", valid=True)
        # The trunk still answers on the same store, so the refusal is the
        # axis and the branch, not the store.
        ok = ops.as_of("2026-01-01T10:30:00.000000Z", symbols=[ID], valid=True)
        assert ok["basis"] == "valid", ok

    def test_an_address_resolves_to_the_instant_it_names(self, tmp_path):
        # P5b.4: the sha of c2 addresses c2's own interval. The proof is
        # that it answers what the *instant* answers — and that it is not
        # the head's answer, which is what rounding would produce.
        repo = _three_commits(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        c2 = subprocess.run(["git", "rev-parse", "HEAD~1"], cwd=str(repo),
                            check=True, capture_output=True, text=True).stdout.strip()
        addressed = ops.as_of("", symbols=[ID], at=c2, valid=True)
        assert addressed["basis"] == "valid", addressed
        assert addressed["commit"] == c2, addressed
        # The address resolved to the commit's own committer time: the
        # fold instant *is* the stamp its rows carry, so nothing rounds.
        assert addressed["fold_ts"] == "2026-01-01T11:00:00.000000Z", addressed
        at_instant = ops.as_of("2026-01-01T11:00:00.000000Z", symbols=[ID],
                               valid=True)
        assert addressed["entities"][ID] == at_instant["entities"][ID]
        assert _ref_digest("2026-01-01T11:00:00.000000Z", valid=True) != \
            _ref_digest("2026-01-01T12:00:00.000000Z", valid=True), (
            "c2 and the head differ in bytes, so addressing the head could not answer c2's interval")

    def test_a_name_addresses_its_tip(self, tmp_path):
        repo = _three_commits(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        by_name = ops.as_of("", symbols=[ID], at="main", valid=True)
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(repo),
                              check=True, capture_output=True, text=True).stdout.strip()
        by_sha = ops.as_of("", symbols=[ID], at=head, valid=True)
        assert by_name["commit"] == head, by_name
        assert by_name["entities"][ID] == by_sha["entities"][ID]

    def test_an_unknown_address_refuses_and_never_rounds(self, tmp_path):
        repo = _three_commits(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        with pytest.raises(ops.InvalidRequest, match="unknown commit"):
            ops.as_of("", symbols=[ID], at="0" * 40, valid=True)
        with pytest.raises(ops.InvalidRequest, match="UnknownBranch"):
            ops.as_of("", symbols=[ID], at="no-such-branch", valid=True)

    def test_an_address_into_a_branch_names_u2(self, tmp_path):
        repo = _three_commits(tmp_path / "repo")
        _git(repo, "checkout", "-q", "-b", "feature", "HEAD")
        _commit(repo, "f1", "2026-01-01T13:00:00+00:00", {"b.py": "y = 1\n"})
        _ingest(repo)
        _ingest(repo, branch="feature")
        _load(repo)
        f1 = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(repo),
                            check=True, capture_output=True, text=True).stdout.strip()
        with pytest.raises(ops.InvalidRequest, match="ValidBranchNeedsU2"):
            ops.as_of("", symbols=[ID], at=f1, valid=True)

    def test_a_snapshot_names_the_valid_axis(self, tmp_path):
        repo = _three_commits(tmp_path / "repo")
        _ingest(repo)
        graph = _load(repo)
        snap = graph.as_of("2026-01-01T10:30:00.000000Z", valid=True)
        found = snap.find(ID)
        assert found is not None, "the valid fold must resolve the entity"
        assert snap.basis == "valid", snap.basis
        assert snap.horizon is None
        assert snap.commit is None
        # The read path beyond `find` (`read_source`) rides the same arm.
        assert snap.read_source(ID) is not None

    def test_the_rendered_read_names_the_valid_axis(self, tmp_path):
        from coderadar import render
        repo = _three_commits(tmp_path / "repo")
        _ingest(repo)
        _load(repo)
        out = ops.as_of("2026-01-01T10:30:00.000000Z", symbols=[ID], valid=True)
        text = render.as_of(out)
        assert "From the valid axis" in text, text
        assert "git history" not in text, text
