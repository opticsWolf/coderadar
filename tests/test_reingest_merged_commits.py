"""The merge-commit heal (Amendment 2 section 10, item 4, K8).

A pre-0.22 trunk walk met a merge whose files the merging lineage had
not ingested yet: spine and bytes landed, the old shape refused its own
minting, and that refusal was not yet the D-315 adoption. The heal
re-walks those commits at their own stamps and gives the foreign-minted
ids their trunk-side first version, touching nothing already claimed by
the trunk and never moving a cursor.

The fixture ingests the merging lineage BEFORE the merge lands, which
is the operator order the heal serves: the fork point is computed
against main's tip, and once a merge makes main reach the branch tip
the branch ingest walks nothing at all.
"""

from __future__ import annotations

import os
import sqlite3
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover - gate for CI without the extension
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

NL = chr(10)


def _nl(*lines):
    return NL.join(lines) + NL


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


def _commit(repo, message, date, files=None, delete=()):
    for name, content in (files or {}).items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline=NL)
    for name in delete:
        (repo / name).unlink()
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env=_stamp_env(date))


def _sha(repo, rev):
    out = subprocess.run(["git", "rev-parse", rev], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


def _committer_date(repo, rev):
    out = subprocess.run(["git", "show", "-s", "--format=%cI", rev],
                         cwd=str(repo), check=True, capture_output=True,
                         text=True)
    return out.stdout.strip()


def zulu(iso):
    # Compares second marks across the layouts the heal meets:
    # '2026-02-01T10:00:00.000000Z' (the ledger) and the git ISO the
    # commit carries ('2026-02-01T10:00:00+00:00' or '...Z').
    cut = iso.find(".")
    if cut == -1:
        cut = iso.find("+")
    if cut == -1:
        cut = iso.rfind("Z")
    return iso if cut == -1 else iso[:cut]


def _store_path(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


LIB_X = _nl("def x_fn():", "    return 1")
GONE = _nl("def gone_fn():", "    return 2")
FRESH = _nl("def fresh_fn():", "    return 3")

ONE = "x = 1" + NL


def _build_merge_side(path):
    # A on main; lib_x.py + gone.py on feat; main back at A. The merge
    # and its tail build later (_apply_merge), AFTER the branch ingest --
    # the fixture's operator half.
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "one", "2026-01-01T10:00:00+00:00", {"a.py": ONE})
    a = _sha(path, "main")
    _git(path, "checkout", "-qb", "feat", a)
    _commit(path, "feature", "2026-01-04T10:00:00+00:00",
            {"lib_x.py": LIB_X, "gone.py": GONE})
    _git(path, "checkout", "-q", "main")
    return a


def _apply_merge(path):
    # main receives feat: the merge commit carries a file of its own
    # (fresh.py, a staged add inside the merge state -- its tree change
    # belongs to the merge and is minted nowhere until the heal runs),
    # then a tail commit deletes gone.py.
    _git(path, "merge", "--no-commit", "--no-ff", "feat")
    _commit(path, "merge", "2026-02-01T10:00:00+00:00", {"fresh.py": FRESH})
    m = _sha(path, "main")
    _commit(path, "tail", "2026-02-02T10:00:00+00:00", delete=["gone.py"])
    return m


class TestHeal:
    def test_the_heal_adopts_the_vintage_skip_at_the_merge_s_stamp(
            self, tmp_path):
        repo = tmp_path / "repo"
        _build_merge_side(repo)
        store = _store_path(repo)
        feat = ops.ingest_history(str(repo), branch="feat",
                                  store_path=str(store))
        assert feat["commits_ingested"] == 1
        m = _apply_merge(repo)

        report = ops.reingest_merged_commits(str(repo), store_path=str(store))
        assert report["branch"] == "main"
        assert report["commits_ingested"] == 1
        adoptions = [f for f in report["adoptions"]
                     if f["path"] == "lib_x.py"]
        assert len(adoptions) == 1
        assert adoptions[0]["reason"] == "adopted-first-version"
        assert adoptions[0]["sha"] == m
        named = {(f["path"], f["reason"]) for f in report["files_failed"]}
        assert ("gone.py", "gone-by-head") in named
        assert ("fresh.py", "no-mint") in named

        # The healed shape: a trunk-side version stamped at the merge
        # commit while the provenance row keeps the mint lineage (D-315
        # by construction -- the id's concepts row is never retouched).
        mint_sql = ("SELECT branch_id FROM concepts"
                    " WHERE id = 'lib_x.py::module'")
        ver_sql = ("SELECT valid_from, recorded_at FROM concept_versions"
                   " WHERE id = 'lib_x.py::module'"
                   " AND branch_id IN ('', 'main')")
        with sqlite3.connect(str(store)) as conn:
            (mint_branch,) = conn.execute(mint_sql).fetchone()
            (stamp, _t,) = conn.execute(ver_sql).fetchone()
        assert mint_branch == "feat"
        # The bitemporal ingest rule: valid_from is the commit's own
        # stamp (P5b's commit index), recorded_at is the writer's now --
        # the heal is no exception, so the version asserts the MERGE's
        # instant, not the heal's day.
        assert zulu(stamp) == zulu(_committer_date(repo, m))

    def test_the_heal_is_idempotent_a_second_run_adopts_nothing(
            self, tmp_path):
        repo = tmp_path / "repo"
        _build_merge_side(repo)
        store = _store_path(repo)
        ops.ingest_history(str(repo), branch="feat", store_path=str(store))
        _apply_merge(repo)
        first = ops.reingest_merged_commits(str(repo), store_path=str(store))
        assert [f for f in first["adoptions"] if f["path"] == "lib_x.py"]

        second = ops.reingest_merged_commits(str(repo), store_path=str(store))
        assert second["commits_ingested"] == 1
        assert [f for f in second["adoptions"]
                if f["path"] == "lib_x.py"] == []
        named = {(f["path"], f["reason"]) for f in second["files_failed"]}
        assert ("lib_x.py", "already-adopted") in named
        assert ("gone.py", "gone-by-head") in named

    def test_the_dry_run_names_the_set_and_writes_nothing(self, tmp_path):
        repo = tmp_path / "repo"
        _build_merge_side(repo)
        store = _store_path(repo)
        ops.ingest_history(str(repo), branch="feat", store_path=str(store))
        _apply_merge(repo)
        dry = ops.reingest_merged_commits(str(repo), store_path=str(store),
                                          dry_run=True)
        named = {(f["path"], f["reason"]) for f in dry["adoptions"]}
        assert ("lib_x.py", "dry-run") in named
        assert dry["commits_ingested"] == 0
        ver_count_sql = ("SELECT COUNT(*) FROM concept_versions"
                         " WHERE id = 'lib_x.py::module'")
        claim_sql = ("SELECT COUNT(*) FROM concepts"
                     " WHERE id = 'lib_x.py::module'"
                     " AND branch_id IN ('', 'main')")
        with sqlite3.connect(str(store)) as conn:
            (versions,) = conn.execute(ver_count_sql).fetchone()
            (claimed,) = conn.execute(claim_sql).fetchone()
        assert (versions, claimed) == (0, 0), "the dry run wrote nothing"

        real = ops.reingest_merged_commits(str(repo), store_path=str(store))
        assert [f for f in real["adoptions"] if f["path"] == "lib_x.py"] != []

    def test_the_heal_refuses_a_branch_and_a_missing_store(self, tmp_path):
        repo = tmp_path / "repo"
        _build_merge_side(repo)
        store = _store_path(repo)
        ops.ingest_history(str(repo), branch="feat", store_path=str(store))
        _apply_merge(repo)
        with pytest.raises(ops.InvalidRequest, match="trunk"):
            ops.reingest_merged_commits(str(repo), branch="feat",
                                        store_path=str(store))
        with pytest.raises(ops.EngineError):
            ops.reingest_merged_commits(str(repo),
                                        store_path=str(store) + ".missing")
