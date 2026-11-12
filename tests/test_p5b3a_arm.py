"""P5b.3a slice B (red): the git-history arm of the default read.

Reads at a time before the horizon resolve through the commit index to
the last commit at or before the read instant and fold at its completion
instant, labelling the answer ``git-history`` with the sha. After the
horizon (or with no horizon) the recorded fold answers, labelled
``recorded``. Before the first commit the read refuses (BeforeHistory);
``strict=True`` refuses on the git-history arm too (BeforeHorizon);
``at=<sha>`` addresses a commit directly and reports ``dirty_at_commit``.
"""
import hashlib
import subprocess

import pytest

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:
    _CORE = False

pytestmark = pytest.mark.skipif(
    not _CORE, reason="Rust _core extension not built")

from coderadar import _core as _C  # noqa: E402
from coderadar import ops  # noqa: E402


def _digest(content: str) -> str:
    return hashlib.sha256(content.encode()).hexdigest()


def _git(repo, *args, env=None):
    import os
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


def _three_commits(root):
    repo = root / "repo"
    repo.mkdir(parents=True, exist_ok=True)
    _git(repo, "init", "-q", "-b", "main", ".")
    _git(repo, "config", "user.name", "T")
    _git(repo, "config", "user.email", "t@t.t")
    _git(repo, "config", "core.autocrlf", "false")

    def commit(msg, date, files):
        for name, content in files.items():
            (repo / name).write_text(content, encoding="utf-8", newline="\n")
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", msg, env=_stamp_env(date))

    commit("c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    commit("c2", "2026-01-02T10:00:00+00:00",
           {"a.py": "x = 2\n", "b.py": "def f():\n    return 1\n"})
    commit("c3", "2026-01-03T10:00:00+00:00", {"a.py": "x = 3\n"})
    return repo


def _store_path(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


def _load(repo):
    from coderadar import CodeGraph, coldstart
    coldstart.build_graph(str(repo))
    return CodeGraph(str(_store_path(repo)))


def _shas_in_order(repo):
    out = subprocess.run(["git", "log", "--format=%H", "--reverse", "main"],
                         cwd=str(repo), check=True, capture_output=True,
                         text=True)
    return [line for line in out.stdout.splitlines() if line]


def _fixture(tmp_path):
    repo = _three_commits(tmp_path)
    ops.ingest_history(str(repo), branch="main",
                       store_path=str(_store_path(repo)))
    _load(repo)
    return repo


def test_git_history_arm_returns_the_earlier_commit(tmp_path):
    repo = _fixture(tmp_path)
    shas = _shas_in_order(repo)
    out = _C.source_refs_at(["a.py::module", "b.py::module"],
                            "2026-01-02T12:00:00.000000Z", "main")
    assert out["basis"] == "git-history", out
    assert out["sha"] == shas[1], out
    assert out["horizon"], out
    refs = out["refs"]
    assert refs["a.py::module"]["digest"] == _digest("x = 2\n")
    assert "b.py::module" in refs


def test_recorded_arm_after_the_horizon(tmp_path):
    repo = _fixture(tmp_path)
    out = _C.source_refs_at(["a.py::module"],
                            "2999-01-01T00:00:00.000000Z", "main")
    assert out["basis"] == "recorded", out
    assert out["sha"] is None, out
    assert out["refs"]["a.py::module"]["digest"] == _digest("x = 3\n")


def test_before_history_refuses_with_a_reason(tmp_path):
    _fixture(tmp_path)
    with pytest.raises(ValueError, match="BeforeHistory"):
        _C.source_refs_at(["a.py::module"],
                          "2025-01-01T00:00:00.000000Z", "main")


def test_strict_refuses_the_git_history_arm(tmp_path):
    _fixture(tmp_path)
    with pytest.raises(ValueError, match="BeforeHorizon"):
        _C.source_refs_at(["a.py::module"],
                          "2026-01-02T12:00:00.000000Z", "main",
                          strict=True)


def test_at_sha_addresses_the_commit(tmp_path):
    repo = _fixture(tmp_path)
    shas = _shas_in_order(repo)
    out = _C.source_refs_at(["a.py::module"],
                            "2026-01-03T12:00:00.000000Z", "main",
                            at=shas[0])
    assert out["basis"] == "git-history", out
    assert out["sha"] == shas[0], out
    assert out["refs"]["a.py::module"]["digest"] == _digest("x = 1\n")
    assert out["dirty_at_commit"] == [], out


def test_unknown_sha_refuses(tmp_path):
    _fixture(tmp_path)
    with pytest.raises(ValueError, match="[Uu]nknown commit"):
        _C.source_refs_at(["a.py::module"],
                          "2026-01-03T12:00:00.000000Z", "main",
                          at="0" * 40)


def test_lookup_entities_at_carries_the_basis(tmp_path):
    _fixture(tmp_path)
    out = _C.lookup_entities_at(["a.py::module"],
                                "2026-01-02T12:00:00.000000Z", "main")
    assert out["basis"] == "git-history", out
    assert out["entities"]["a.py::module"] is not None


# --- Slice B, Python surface: every read names its basis (P5b.3a, §3.1.1) ---

def _snapshot(repo, ts, **kw):
    from coderadar import CodeGraph
    return CodeGraph(str(_store_path(repo))).as_of(ts, **kw)


def test_snapshot_names_the_git_history_arm(tmp_path):
    repo = _fixture(tmp_path)
    shas = _shas_in_order(repo)
    snap = _snapshot(repo, "2026-01-02T12:00:00.000000Z")
    assert snap.basis == "git-history"
    assert snap.commit == shas[1]
    assert snap.horizon, "a git-filled store has a horizon"
    assert snap.dirty_at_commit == []
    entity = snap.find("a.py::module")
    assert entity is not None
    assert _digest(repo_content_at(repo, shas[1], "a.py")) == _digest("x = 2\n")


def repo_content_at(repo, sha, path):
    out = subprocess.run(["git", "show", f"{sha}:{path}"], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout


def test_snapshot_names_the_recorded_arm(tmp_path):
    repo = _fixture(tmp_path)
    snap = _snapshot(repo, "2999-01-01T00:00:00.000000Z")
    assert snap.basis == "recorded"
    assert snap.commit is None
    assert snap.find("a.py::module") is not None


def test_snapshot_strict_refuses_the_git_history_arm(tmp_path):
    from coderadar import ops
    repo = _fixture(tmp_path)
    # Construction is lazy (no index scan per snapshot); the arm resolves
    # at the first read, and that is where the refusal lands.
    with pytest.raises(ops.InvalidRequest, match="BeforeHorizon"):
        _snapshot(repo, "2026-01-02T12:00:00.000000Z", strict=True).basis


def test_snapshot_before_history_refuses(tmp_path):
    from coderadar import ops
    repo = _fixture(tmp_path)
    with pytest.raises(ops.InvalidRequest, match="BeforeHistory"):
        _snapshot(repo, "2025-01-01T00:00:00.000000Z").find("a.py::module")


def test_snapshot_at_sha_addresses_the_commit(tmp_path):
    repo = _fixture(tmp_path)
    shas = _shas_in_order(repo)
    snap = _snapshot(repo, "", at=shas[0])
    assert snap.basis == "git-history"
    assert snap.commit == shas[0]
    # c1's a.py (x = 1) is what this commit says; c2/c3 changed it later.
    assert snap.find("a.py::module") is not None
    assert _C.source_refs_at(["a.py::module"], "", "main",
                             at=shas[0])["refs"]["a.py::module"]["digest"] \
        == _digest("x = 1\n")


def test_ops_as_of_carries_the_arm(tmp_path):
    repo = _fixture(tmp_path)
    shas = _shas_in_order(repo)
    out = ops.as_of("2026-01-02T12:00:00.000000Z",
                    symbols=["a.py::module"], graph=None)
    assert out["basis"] == "git-history", out
    assert out["commit"] == shas[1]
    assert out["horizon"]
    assert out["entities"]["a.py::module"] is not None
    assert out["fold_ts"] <= out["horizon"]
