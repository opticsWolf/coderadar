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
    from coderadar import _core  # noqa: F401 - the import is the availability probe
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
LIB_X_EDIT = _nl("def x_fn():", "    return 5")
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
    # then a tail commit EDITS lib_x.py and deletes gone.py -- the
    # post-merge trunk commit that edits a merged file is the shape the
    # trunk-edit findings need, the E commit.
    _git(path, "merge", "--no-commit", "--no-ff", "feat")
    _commit(path, "merge", "2026-02-01T10:00:00+00:00", {"fresh.py": FRESH})
    m = _sha(path, "main")
    _commit(path, "tail", "2026-02-02T10:00:00+00:00",
            {"lib_x.py": LIB_X_EDIT}, delete=["gone.py"])
    e = _sha(path, "main")
    return m, e


class TestHeal:
    def test_the_heal_adopts_the_vintage_skip_at_the_merge_s_stamp(
            self, tmp_path):
        repo = tmp_path / "repo"
        _build_merge_side(repo)
        store = _store_path(repo)
        feat = ops.ingest_history(str(repo), branch="feat",
                                  store_path=str(store))
        assert feat["commits_ingested"] == 1
        m, e = _apply_merge(repo)

        report = ops.reingest_merged_commits(str(repo), store_path=str(store))
        assert report["branch"] == "main"
        # Every chain commit walks and settles its flush: the trunk
        # commit, the merge, the edit.
        assert report["commits_ingested"] == 3
        assert report["head"] == e
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
                   " AND branch_id IN ('', 'main')"
                   " ORDER BY valid_from")
        with sqlite3.connect(str(store)) as conn:
            (mint_branch,) = conn.execute(mint_sql).fetchone()
            rows = conn.execute(ver_sql).fetchall()
        assert mint_branch == "feat"
        assert len(rows) == 2, "the merge's version and the edit's re-assert"
        # The bitemporal ingest rule: valid_from is the commit's own
        # stamp (P5b's commit index), recorded_at is the writer's now --
        # the heal is no exception, so the versions assert the MERGE's
        # and the edit's instants, not the heal's day.
        stamps = [zulu(v) for (v, _t) in rows]
        assert stamps[0] == zulu(_committer_date(repo, m))
        assert stamps[1] == zulu(_committer_date(repo, e))

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
        assert second["commits_ingested"] == 3
        assert [f for f in second["adoptions"]
                if f["path"] == "lib_x.py"] == []
        named = {(f["path"], f["reason"]) for f in second["files_failed"]}
        # A re-read's re-asserts land byte-identically (the write's own
        # unchanged filter drops them): no lib_x entry in either list,
        # which is where the op's idempotence lives.
        assert ("lib_x.py", "already-adopted") not in named
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
        # Finding 3: the dry run agrees with the real run about a file
        # the trunk deleted afterwards -- the same verdict, prefixed.
        failed = {(f["path"], f["reason"]) for f in dry["files_failed"]}
        assert ("gone.py", "dry-run:gone-by-head") in failed
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

    def test_a_function_dropped_after_the_merge_retires_on_main_only(
            self, tmp_path):
        # The tail removes y_fn from the merged file. Both the heal and a
        # full trunk walk retire it on main at the tail's stamp; the
        # branch that minted it keeps believing it (post-merge review).
        two = _nl("def x_fn():", "    return 1", "",
                  "def y_fn():", "    return 2")
        open_end = "9999-12-31T23:59:59.999999Z"
        open_sql = ("SELECT COUNT(*) FROM concept_versions"
                    " WHERE id = 'lib_x.py::y_fn' AND branch_id = 'main'"
                    " AND valid_to = ?")
        mint_sql = ("SELECT branch_id, retired, valid_to FROM concepts"
                    " WHERE id = 'lib_x.py::y_fn'")
        for walk_first in (False, True):
            repo = tmp_path / f"repo{int(walk_first)}"
            repo.mkdir()
            _git(repo, "init", "-q", "-b", "main", ".")
            _git(repo, "config", "user.name", "T")
            _git(repo, "config", "user.email", "t@t.t")
            _git(repo, "config", "core.autocrlf", "false")
            _commit(repo, "one", "2026-01-01T10:00:00+00:00", {"a.py": ONE})
            _git(repo, "checkout", "-qb", "feat")
            _commit(repo, "feature", "2026-01-04T10:00:00+00:00",
                    {"lib_x.py": two})
            _git(repo, "checkout", "-q", "main")
            store = _store_path(repo)
            ops.ingest_history(str(repo), branch="feat",
                               store_path=str(store))
            _git(repo, "merge", "-q", "--no-ff", "-m", "merge", "feat",
                 env=_stamp_env("2026-02-01T10:00:00+00:00"))
            _commit(repo, "tail", "2026-02-02T10:00:00+00:00",
                    {"lib_x.py": LIB_X_EDIT})
            if walk_first:
                walk = ops.ingest_history(str(repo), branch="main",
                                          store_path=str(store))
                assert walk["files_failed"] == []
            heal = ops.reingest_merged_commits(str(repo),
                                               store_path=str(store))
            assert [f for f in heal["files_failed"]
                    if f["reason"].startswith("retire-failed")] == []
            with sqlite3.connect(str(store)) as conn:
                (still_open,) = conn.execute(open_sql, (open_end,)).fetchone()
                mint = conn.execute(mint_sql).fetchone()
            assert still_open == 0, f"main still believes y_fn ({walk_first})"
            assert mint == ("feat", 0, open_end), \
                f"the branch's mint was touched ({walk_first}): {mint}"

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
