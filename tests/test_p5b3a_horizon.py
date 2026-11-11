"""P5b.3a slice A (horizon + commit index in kv).

The per-lineage horizon (``coderadar.horizon.{lineage}``) and the per-commit
completion instants (``coderadar.commit_done.{lineage}.{sha}``) are the two
kv writes the git-history arm stands on. A completion instant must bound its
commit: at or after every row the commit wrote (flush AND sweep) and before
the next commit's first row, so a recorded fold at the instant sees exactly
the commit's state. The backfill rebuilds both from the ledger for stores
ingested before this phase.
"""
import json
import sqlite3
import subprocess

import pytest

from coderadar import ops

_OPEN_SENTINEL = "9999-12-31T23:59:59.999999Z"
_OPEN_LEGACY = "9999-12-31T00:00:00.000000Z"

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:
    _CORE = False

pytestmark = pytest.mark.skipif(
    not _CORE, reason="Rust _core extension not built")


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


def _ingest(repo):
    return ops.ingest_history(str(repo), branch="main",
                              store_path=str(_store_path(repo)))


def _kv(store_path, key):
    con = sqlite3.connect(str(store_path))
    try:
        row = con.execute("SELECT value FROM kv_store WHERE key = ?",
                          (key,)).fetchone()
    finally:
        con.close()
    return row[0] if row else None


def _shas_in_order(repo):
    out = subprocess.run(["git", "log", "--format=%H", "--reverse", "main"],
                         cwd=str(repo), check=True, capture_output=True,
                         text=True)
    return [line for line in out.stdout.splitlines() if line]


def _concepts_by_start(store_path):
    """stamp -> list of recorded_at, attributed to the writing commit.

    Mirrors the backfill's write-time snapshot rule: an open row joins its
    valid_from's group, a close row its valid_to's — a close's valid_from
    is an older commit's stamp and must never join its group.
    """
    con = sqlite3.connect(str(store_path))
    try:
        rows = con.execute(
            "SELECT payload, recorded_at FROM transaction_log "
            "WHERE table_name = 'concepts'").fetchall()
    finally:
        con.close()
    by_start = {}
    for payload, recorded_at in rows:
        row = json.loads(payload)
        valid_to = row.get("valid_to")
        is_open = valid_to in (None, _OPEN_SENTINEL, _OPEN_LEGACY)
        stamp = row["valid_from"] if is_open else valid_to
        by_start.setdefault(stamp, []).append(recorded_at)
    return by_start


def _commit_stamps(store_path):
    """sha -> earliest valid_from over its commit-concept versions."""
    con = sqlite3.connect(str(store_path))
    try:
        rows = con.execute(
            "SELECT entity_id, payload FROM transaction_log "
            "WHERE table_name = 'concepts' AND entity_id LIKE 'commit:%'"
        ).fetchall()
    finally:
        con.close()
    stamps = {}
    for entity_id, payload in rows:
        sha = entity_id.split(":", 1)[1]
        stamp = json.loads(payload)["valid_from"]
        if sha not in stamps or stamp < stamps[sha]:
            stamps[sha] = stamp
    return stamps


def _assert_index_valid(store_path, shas):
    by_start = _concepts_by_start(store_path)
    stamps = _commit_stamps(store_path)
    for i, sha in enumerate(shas):
        stamp = stamps[sha]
        assert stamp in by_start, (sha[:8], sorted(by_start))
        done = _kv(store_path, f"coderadar.commit_done.main.{sha}")
        assert done, f"no commit_done for {sha[:8]}"
        assert max(by_start[stamp]) <= done, (sha[:8], max(by_start[stamp]), done)
        if i + 1 < len(shas):
            nxt = stamps[shas[i + 1]]
            assert nxt in by_start, (shas[i + 1][:8], sorted(by_start))
            assert done < min(by_start[nxt]), (sha[:8], done, min(by_start[nxt]))


def test_horizon_written_once_and_never_moved(tmp_path):
    repo = _three_commits(tmp_path)
    _ingest(repo)
    store = _store_path(repo)
    first = _kv(store, "coderadar.horizon.main")
    assert first, "no horizon after first ingest"
    _ingest(repo)
    assert _kv(store, "coderadar.horizon.main") == first


def test_commit_done_instants_bound_each_commit(tmp_path):
    repo = _three_commits(tmp_path)
    _ingest(repo)
    _assert_index_valid(_store_path(repo), _shas_in_order(repo))


def test_backfill_rebuilds_index_from_ledger(tmp_path):
    repo = _three_commits(tmp_path)
    _ingest(repo)
    store = _store_path(repo)
    con = sqlite3.connect(str(store))
    con.execute("DELETE FROM kv_store WHERE key LIKE 'coderadar.horizon.%'"
                " OR key LIKE 'coderadar.commit_done.%'")
    con.commit()
    con.close()
    assert _kv(store, "coderadar.horizon.main") is None
    report = ops.backfill_commit_index(branch="main",
                                       store_path=str(store))
    assert report["commits_indexed"] == 3, report
    assert report["collisions"] == [], report
    assert _kv(store, "coderadar.horizon.main"), "no horizon after backfill"
    _assert_index_valid(store, _shas_in_order(repo))


def test_commit_time_format_flag(tmp_path):
    repo = _three_commits(tmp_path)
    _ingest(repo)
    assert _kv(_store_path(repo), "coderadar.commit_time.format") == "1"
