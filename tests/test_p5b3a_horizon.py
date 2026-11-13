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


# --- Regressions found while landing the arm (P5b.3a slice B) -------------
#
# Both were red against the first cut of this slice and are the reason the
# backfill learned to fill (not overwrite) and to read `concept_cuts`:
# a forward ingest knows the real completion (after the sweep, before the
# next commit's first write); a derived one is only the newest instant the
# commit's own rows landed, and the branch sweep's cut is one of them.

def _cut_stamps(store_path):
    """stamp -> recorded_at list over `concept_cuts` (the branch sweep)."""
    con = sqlite3.connect(str(store_path))
    try:
        rows = con.execute(
            "SELECT cut_at, recorded_at FROM concept_cuts").fetchall()
    finally:
        con.close()
    by_stamp = {}
    for cut_at, recorded_at in rows:
        by_stamp.setdefault(cut_at, []).append(recorded_at)
    return by_stamp


def test_forward_ingest_keeps_the_completion_it_recorded(tmp_path):
    # The first import's tail backfill runs after the loop wrote every
    # completion; it must not replace them with its derived (earlier)
    # instant, or a fold at the "completion" misses the sweep.
    repo = _three_commits(tmp_path)
    _ingest(repo)
    stamps = _commit_stamps(_store_path(repo))
    concepts = _concepts_by_start(_store_path(repo))
    for sha, stamp in stamps.items():
        done = _kv(_store_path(repo), f"coderadar.commit_done.main.{sha}")
        assert done is not None, sha
        assert done >= max(concepts[stamp]), (sha, done, concepts[stamp])
    horizon = _kv(_store_path(repo), "coderadar.horizon.main")
    assert horizon is not None
    for sha in stamps:
        assert _kv(_store_path(repo),
                   f"coderadar.commit_done.main.{sha}") < horizon


def test_branch_sweep_cut_bounds_the_completion(tmp_path):
    # A branch commit that drops an inherited function writes its cut in the
    # sweep, AFTER the concept flush. The completion must cover it: a fold
    # at the completion sees the departed entity gone (that is what the
    # git-history arm folds for a pre-horizon read).
    repo = tmp_path / "repo"
    repo.mkdir(parents=True, exist_ok=True)
    _git(repo, "init", "-q", "-b", "main", ".")
    _git(repo, "config", "user.name", "T")
    _git(repo, "config", "user.email", "t@t.t")
    _git(repo, "config", "core.autocrlf", "false")
    (repo / "a.py").write_text("def f():\n    return 1\ndef h():\n    return 2\n",
                               encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "c1", env=_stamp_env("2026-01-01T10:00:00+00:00"))
    _git(repo, "checkout", "-qb", "feature")
    ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
    (repo / "a.py").write_text("def h():\n    return 2\n",
                               encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "e1", env=_stamp_env("2026-01-05T10:00:00+00:00"))
    sha = subprocess.run(["git", "rev-parse", "feature"], cwd=str(repo),
                         check=True, capture_output=True, text=True).stdout.strip()
    report = ops.ingest_history(str(repo), branch="feature",
                                store_path=str(_store_path(repo)))
    assert report["head"] == sha
    done = _kv(_store_path(repo), f"coderadar.commit_done.feature.{sha}")
    assert done is not None
    cuts = _cut_stamps(_store_path(repo))
    commit_stamp = _commit_stamps(_store_path(repo))[sha]
    assert cuts.get(commit_stamp), "the sweep wrote a cut at the commit's stamp"
    assert done >= max(cuts[commit_stamp]), (done, cuts[commit_stamp])
    horizon = _kv(_store_path(repo), "coderadar.horizon.feature")
    assert done < horizon


def test_derived_completion_covers_the_cut_on_a_rebuilt_index(tmp_path):
    # A pre-phase store has no completion keys: the migration derives them
    # from the ledger, and the derivation must read `concept_cuts` too —
    # otherwise the derived instant precedes the sweep and the arm folds a
    # departed entity back to life.
    repo = tmp_path / "repo"
    repo.mkdir(parents=True, exist_ok=True)
    _git(repo, "init", "-q", "-b", "main", ".")
    _git(repo, "config", "user.name", "T")
    _git(repo, "config", "user.email", "t@t.t")
    _git(repo, "config", "core.autocrlf", "false")
    (repo / "a.py").write_text("def f():\n    return 1\ndef h():\n    return 2\n",
                               encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "c1", env=_stamp_env("2026-01-01T10:00:00+00:00"))
    _git(repo, "checkout", "-qb", "feature")
    ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
    (repo / "a.py").write_text("def h():\n    return 2\n",
                               encoding="utf-8", newline="\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "e1", env=_stamp_env("2026-01-05T10:00:00+00:00"))
    sha = subprocess.run(["git", "rev-parse", "feature"], cwd=str(repo),
                         check=True, capture_output=True, text=True).stdout.strip()
    ops.ingest_history(str(repo), branch="feature",
                       store_path=str(_store_path(repo)))

    # Simulate the pre-phase store: completions absent.
    con = sqlite3.connect(str(_store_path(repo)))
    try:
        con.execute("DELETE FROM kv_store WHERE key LIKE 'coderadar.commit_done.%'")
        con.commit()
    finally:
        con.close()
    report = ops.backfill_commit_index(
        "feature", store_path=str(_store_path(repo)))
    assert report["commits_indexed"] == 1
    done = _kv(_store_path(repo), f"coderadar.commit_done.feature.{sha}")
    assert done is not None
    cuts = _cut_stamps(_store_path(repo))
    commit_stamp = _commit_stamps(_store_path(repo))[sha]
    assert done >= max(cuts[commit_stamp]), (done, cuts[commit_stamp])

    # And the arm, folding at that derived instant, sees the cut.
    from coderadar import CodeGraph, coldstart
    coldstart.build_graph(str(repo))
    CodeGraph(str(_store_path(repo)))
    from coderadar import _core as _C
    out = _C.source_refs_at(["a.py::f"], done, "feature")
    assert out["refs"].get("a.py::f") is None, out["refs"]
