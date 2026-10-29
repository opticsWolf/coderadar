"""Commit-driven history ingest, op level (plan §5 P5a.3, DR-41).

Done clause: one op takes a repository and a lineage and writes history —
per-commit blob puts, one stamped flush per commit, scoped edges, a
tree-scoped sweep, and the `kv` cursor advance — with the report shape as
the contract and typed refusals for everything out of order.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
import time

import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


def _git(repo, *args, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    subprocess.run(["git", *args], cwd=str(repo), env=merged,
                   check=True, capture_output=True)


def _commit_env(author_date, committer_date):
    return {
        "GIT_AUTHOR_NAME": "T Author",
        "GIT_AUTHOR_EMAIL": "author@t.t",
        "GIT_COMMITTER_NAME": "T Committer",
        "GIT_COMMITTER_EMAIL": "committer@t.t",
        "GIT_AUTHOR_DATE": author_date,
        "GIT_COMMITTER_DATE": committer_date,
    }


def _init_repo(path):
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    return path


def _commit(repo, message, author_date, committer_date,
            files=None, delete=()):
    for name, content in (files or {}).items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline="\n")
    for name in delete:
        _git(repo, "rm", "-q", name)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message,
         env=_commit_env(author_date, committer_date))
    out = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


def _three_commit_repo(root):
    repo = _init_repo(root / "repo")
    c1 = _commit(repo, "one", "2026-01-01T10:00:00+00:00", "2026-01-01T10:00:00+00:00",
                 files={"a.py": "x = 1\n"})
    c2 = _commit(repo, "two", "2026-01-02T10:00:00+00:00", "2026-01-02T10:00:00+00:00",
                 files={"a.py": "x = 1\ny = 2\n", "b.py": "def f():\n    return 1\n"})
    c3 = _commit(repo, "three", "2026-01-03T10:00:00+00:00", "2026-01-03T10:00:00+00:00",
                 files={"a.py": "x = 3\n"}, delete=["b.py"])
    return repo, (c1, c2, c3)


def _store_path(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


def _load(repo):
    from coderadar import CodeGraph, coldstart
    coldstart.build_graph(str(repo))
    return CodeGraph(str(_store_path(repo)))


def _generations():
    from coderadar._core import recorded_generations
    return recorded_generations(None, None, "main")


def _digest(content: str) -> str:
    return hashlib.sha256(content.encode()).hexdigest()


REQUIRED_KEYS = {
    "branch", "lineage", "from", "head", "dry_run",
    "commits_ingested", "commits_skipped", "files_parsed", "files_failed",
    "files_excluded", "blobs_stored", "blobs_deduped", "stamps_nudged",
    "ghost_commits", "elapsed_secs",
}


class TestReportShape:
    def test_full_ingest_reports_the_contract(self, tmp_path):
        repo, (_, _, c3) = _three_commit_repo(tmp_path)
        report = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))

        assert REQUIRED_KEYS <= set(report), REQUIRED_KEYS - set(report)
        assert report["branch"] == "main"
        assert report["lineage"] == "main"
        assert report["from"] is None
        assert report["head"] == c3
        assert report["dry_run"] is False
        assert report["commits_ingested"] == 3
        assert report["commits_skipped"] == []
        # c1: a.py; c2: a.py + b.py; c3: a.py (b.py deleted, not parsed).
        assert report["files_parsed"] == 4
        assert report["files_failed"] == []
        assert report["files_excluded"] == 0
        assert report["blobs_stored"] == 4
        assert report["blobs_deduped"] == 0
        assert report["stamps_nudged"] == 0
        assert report["ghost_commits"] == 0
        assert report["elapsed_secs"] >= 0.0

    def test_final_state_matches_the_tip(self, tmp_path):
        repo, (_, _, _) = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        graph = _load(repo)

        gens = _generations()
        assert len(gens) >= 3, "one generation per commit at minimum"
        final = graph.as_of(gens[-1])
        a = final.find("a.py::module")
        assert a is not None
        from coderadar._core import source_refs_at
        refs = source_refs_at(["a.py::module", "b.py::module"], gens[-1], "main")["refs"]
        assert refs["a.py::module"]["digest"] == _digest("x = 3\n")
        # b.py was deleted in c3: the retire cascade closed it.
        assert refs.get("b.py::module") is None
        assert ops.blob_get(refs["a.py::module"]["digest"]) == b"x = 3\n"

    def test_verify_history_reports_full_coverage(self, tmp_path):
        repo, _ = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        graph = _load(repo)

        verdict = graph.verify_history(all_generations=True)
        assert verdict["byte_coverage_pct"] == 100.0, verdict["totals"]
        assert verdict["totals"].get("no_digest", 0) == 0

    def test_history_chain_follows_commit_order(self, tmp_path):
        repo, _ = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        graph = _load(repo)

        chain = graph.history_of("a.py")["versions"]
        assert [v["digest"] for v in chain] == [
            _digest("x = 3\n"), _digest("x = 1\ny = 2\n"), _digest("x = 1\n")]
        assert [v["valid_from"] for v in chain] == [
            "2026-01-03T10:00:00.000000Z",
            "2026-01-02T10:00:00.000000Z",
            "2026-01-01T10:00:00.000000Z"]


class TestCommitConcepts:
    def test_each_commit_mints_a_metadata_concept(self, tmp_path):
        repo, (c1, c2, c3) = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))

        con = sqlite3.connect(str(_store_path(repo)))
        try:
            rows = con.execute(
                "SELECT id, content, extra FROM concepts WHERE id LIKE 'commit:%'"
            ).fetchall()
        finally:
            con.close()
        assert {row[0] for row in rows} == {f"commit:{c}" for c in (c1, c2, c3)}
        for _, content, extra in rows:
            assert json.loads(content)["kind"] == "commit"
            meta = json.loads(extra)["coderadar"]
            assert meta["author_name"] == "T Author"
            assert meta["committer_email"] == "committer@t.t"
            assert meta["stamp_nudged"] is False

    def test_commit_concepts_stay_out_of_entity_listings(self, tmp_path):
        repo, _ = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        _load(repo)

        from coderadar._core import (
            all_concept_ids,
            module_concept_ids,
            visible_all_concept_ids,
            visible_module_concept_ids,
        )
        for listing in (all_concept_ids("main"),
                        module_concept_ids("main"),
                        visible_all_concept_ids("main"),
                        visible_module_concept_ids("main")):
            assert not [i for i in listing if i.startswith("commit:")], listing


class TestResumption:
    def test_interrupted_ingest_resumes_from_the_cursor(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        c1 = _commit(repo, "one", "2026-01-01T10:00:00+00:00", "2026-01-01T10:00:00+00:00",
                     files={"a.py": "x = 1\n"})
        first = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        assert first["head"] == c1
        time.sleep(0.05)

        c2 = _commit(repo, "two", "2026-01-02T10:00:00+00:00", "2026-01-02T10:00:00+00:00",
                     files={"a.py": "x = 2\n"})
        second = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        assert second["commits_ingested"] == 1
        assert second["head"] == c2
        assert second["from"] == c1

        _load(repo)
        gens = _generations()
        refs = _core.source_refs_at(["a.py::module"], gens[-1], "main")["refs"]
        assert refs["a.py::module"]["digest"] == _digest("x = 2\n")

    def test_a_second_run_is_an_idempotent_noop(self, tmp_path):
        repo, (_, _, c3) = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        _load(repo)
        before = _generations()

        again = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        assert again["commits_ingested"] == 0
        assert again["commits_skipped"] == []
        assert again["head"] == c3
        assert _generations() == before

    def test_empty_window_at_the_tip_writes_nothing(self, tmp_path):
        repo, (_, _, c3) = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))

        at_tip = ops.ingest_history(
            str(repo), from_sha=c3, store_path=str(_store_path(repo)))
        assert at_tip["commits_ingested"] == 0
        assert at_tip["head"] == c3


class TestRefusals:
    def test_out_of_order_from_is_refused(self, tmp_path):
        repo, (c1, _, _c3) = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        _load(repo)
        before_gens = _generations()
        before_chain = _core.source_refs_at(
            ["a.py::module"], before_gens[-1], "main")

        with pytest.raises(ops.InvalidRequest, match="out-of-order"):
            ops.ingest_history(str(repo), from_sha=c1,
                               store_path=str(_store_path(repo)))
        # Nothing moved: the refusal wrote nothing.
        assert _generations() == before_gens
        assert _core.source_refs_at(
            ["a.py::module"], before_gens[-1], "main") == before_chain

    def test_legacy_index_now_store_is_refused(self, tmp_path, monkeypatch):
        import coderadar
        project = tmp_path / "legacy"
        project.mkdir()
        (project / "m.py").write_text("x = 1\n", encoding="utf-8")
        monkeypatch.chdir(project)
        coderadar.analyze(".", create_store=True)
        legacy_store = project / ".coderadar" / "store" / "coderadar.db"
        assert legacy_store.exists()

        repo, _ = _three_commit_repo(tmp_path / "other")
        with pytest.raises(ops.InvalidRequest, match="index-now"):
            ops.ingest_history(str(repo), store_path=str(legacy_store))

    def test_unknown_branch_is_refused(self, tmp_path):
        repo, _ = _three_commit_repo(tmp_path)
        with pytest.raises(ops.InvalidRequest, match="no-such-branch"):
            ops.ingest_history(str(repo), branch="no-such-branch",
                               store_path=str(_store_path(repo)))

    def test_unknown_from_is_refused(self, tmp_path):
        repo, _ = _three_commit_repo(tmp_path)
        with pytest.raises(ops.InvalidRequest, match="0000000000"):
            ops.ingest_history(str(repo), from_sha="0" * 40,
                               store_path=str(_store_path(repo)))

    def test_missing_store_path_is_refused(self, tmp_path):
        repo, _ = _three_commit_repo(tmp_path)
        with pytest.raises(ops.InvalidRequest, match="store_path"):
            ops.ingest_history(str(repo))

    def test_commit_time_store_refuses_index_now_writes(self, tmp_path):
        repo, _ = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        from coderadar import coldstart
        coldstart.build_graph(str(repo))
        with pytest.raises(Exception, match="commit-time"):
            _core.update_file("a.py", "x = 9\n", False)


class TestCli:
    def test_ingest_history_cli_reports_counts(self, tmp_path):
        import os

        from click.testing import CliRunner
        from coderadar.cli import main
        repo, _ = _three_commit_repo(tmp_path)
        previous = os.getcwd()
        # chdir into tmp first: bare _enter_project would otherwise walk up
        # from the pytest cwd, find this repository's own marker, and
        # activate its config globally for every test after this one.
        os.chdir(tmp_path)
        try:
            result = CliRunner().invoke(main, [
                "ingest-history", "--repo", str(repo),
                "--store", str(_store_path(repo)),
            ])
        finally:
            os.chdir(previous)
        assert result.exit_code == 0, result.output
        assert "ingested=3" in result.output


class TestDryRun:
    def test_dry_run_plans_without_writing(self, tmp_path):
        repo, (c1, c2, c3) = _three_commit_repo(tmp_path)
        store = _store_path(repo)
        report = ops.ingest_history(str(repo), dry_run=True, store_path=str(store))

        assert report["dry_run"] is True
        assert report["commits_ingested"] == 0
        assert [s["reason"] for s in report["commits_skipped"]] == ["dry-run"] * 3
        assert [s["sha"] for s in report["commits_skipped"]] == [c1, c2, c3]
        assert report["head"] is None
        assert not store.exists(), "a dry run must not create the store"

    def test_dry_run_on_an_ingested_store_still_checks(self, tmp_path):
        repo, (c1, _, _) = _three_commit_repo(tmp_path)
        ops.ingest_history(str(repo), store_path=str(_store_path(repo)))

        with pytest.raises(ops.InvalidRequest, match="out-of-order"):
            ops.ingest_history(str(repo), from_sha=c1, dry_run=True,
                               store_path=str(_store_path(repo)))
