"""P5b.6 slice B (red): branch generations (D-5).

A branch whose cursor left its history — a rebase, a force-push, a
rewritten commit — does not corrupt anything and does not refuse: it gets
a *new generation*. The name resolves to the newest generation, the old
one's rows and shas stay readable, and a rewritten trunk refuses with
`TrunkRewritten` because no generation can repair history the trunk no
longer shares.

Fixture: main c1..c3, feature f1 off c2. Then either a rebase onto main's
tip (new merge base) or a rewrite that keeps the merge base.
"""
import hashlib
import os
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:
    _CORE = False

pytestmark = pytest.mark.skipif(
    not _CORE, reason="Rust _core extension not built")


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
    out = subprocess.run(["git", "rev-parse", rev], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


def _repo(path):
    """main c1..c3; feature f1 off c2 (own commit)."""
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    _commit(path, "c2", "2026-01-02T10:00:00+00:00",
            {"a.py": "x = 2\n", "b.py": "y = 1\n"})
    c2 = _sha(path, "main")
    # c3 changes b.py, never a.py: a rebase of the branch (which edits
    # a.py) is conflict-free, and the new base is visible in b.py.
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


def _kv(store, key):
    import sqlite3
    con = sqlite3.connect(str(store))
    try:
        row = con.execute("SELECT value FROM kv_store WHERE key = ?",
                          (key,)).fetchone()
    finally:
        con.close()
    return row[0] if row else None


def _refs(ids, ts, branch, **kw):
    from coderadar import _core as _C
    return _C.source_refs_at(ids, ts, branch, **kw)


def test_a_rebase_onto_the_trunk_tip_starts_a_new_generation(tmp_path):
    repo, shas = _repo(tmp_path / "repo")
    store = _store(tmp_path)
    _migrate(repo, store)
    # Rebase feature onto main's tip: new base, new commit, same content.
    _git(repo, "checkout", "-q", "feature")
    _git(repo, "rebase", "-q", "main", env=_stamp_env("2026-01-06T10:00:00+00:00"))
    rebased = _sha(repo, "feature")
    _git(repo, "checkout", "-q", "main")
    assert rebased != shas["f1"]

    report = ops.ingest_history(str(repo), branch="feature", store_path=str(store))
    assert report["generation"] == 2, report
    assert report["lineage"] == "feature#2", report
    assert report["superseded"] == "feature", report
    assert _kv(store, "coderadar.ingest.generation.feature") == "2"
    assert _kv(store, "coderadar.branch.lineage.feature") == "feature#2"

    # The name resolves to the newest generation, at its own commit.
    out = _refs(["a.py::module"],
                "2026-01-07T12:00:00.000000Z", "feature")
    assert out["sha"] == rebased, out
    assert out["lineage"] == "feature#2", out
    # The generation forked at main's tip, not at the old base. (b.py is
    # trunk-owned, so its read leaks the trunk's present until P5c — that
    # is D-6's parent_drift, not this phase's subject.)
    assert report["fork_point"] == shas["c3"], report
    assert out["refs"]["a.py::module"]["digest"] == _digest("x = 2\nfeature = 1\n")


def test_the_old_generation_stays_readable_by_sha(tmp_path):
    repo, shas = _repo(tmp_path / "repo")
    store = _store(tmp_path)
    migrated = _migrate(repo, store)
    assert migrated["branch_reports"]["feature"]["fork_point"] == shas["c2"]
    _git(repo, "checkout", "-q", "feature")
    _git(repo, "rebase", "-q", "main", env=_stamp_env("2026-01-06T10:00:00+00:00"))
    _git(repo, "checkout", "-q", "main")
    ops.ingest_history(str(repo), branch="feature", store_path=str(store))

    out = _refs(["a.py::module"], "", "feature", at=shas["f1"])
    assert out["sha"] == shas["f1"], out
    assert out["lineage"] == "feature", out
    assert out["refs"]["a.py::module"]["digest"] == _digest("x = 2\nfeature = 1\n")


def test_a_rewrite_that_keeps_the_base_still_starts_a_generation(tmp_path):
    # Same merge base (c2), different commit: the cursor left its history
    # without the fork moving. NotAncestor, not ForkMismatch.
    repo, shas = _repo(tmp_path / "repo")
    store = _store(tmp_path)
    _migrate(repo, store)
    _git(repo, "checkout", "-q", "feature")
    _git(repo, "reset", "-q", "--hard", shas["c2"])
    _commit(repo, "f2", "2026-01-06T10:00:00+00:00",
            {"a.py": "x = 2\nfeature = 2\n"})
    rewritten = _sha(repo, "feature")
    _git(repo, "checkout", "-q", "main")
    report = ops.ingest_history(str(repo), branch="feature", store_path=str(store))
    assert report["generation"] == 2, report
    out = _refs(["a.py::module"], "2026-01-07T12:00:00.000000Z", "feature")
    assert out["sha"] == rewritten, out
    assert out["refs"]["a.py::module"]["digest"] == _digest("x = 2\nfeature = 2\n")


def test_a_rewritten_trunk_refuses(tmp_path):
    repo, _shas = _repo(tmp_path / "repo")
    store = _store(tmp_path)
    _migrate(repo, store)
    # Rewrite main's tip: no generation can repair it (D-5).
    _git(repo, "reset", "-q", "--hard", "main~1")
    _commit(repo, "c3'", "2026-01-07T10:00:00+00:00", {"a.py": "x = 9\n"})
    with pytest.raises(Exception, match="TrunkRewritten"):
        ops.ingest_history(str(repo), store_path=str(store))
    assert _kv(store, "coderadar.ingest.generation.main") is None
