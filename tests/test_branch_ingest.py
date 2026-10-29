"""Branch-lineage ingestion (plan §5 P5a.4, DR-41).

Two lineages diverging from one base: each reads its own content at its
own commit times, the trunk never sees the branch's, `verify_history
--branch` reports per-lineage coverage, and the shared-content case
stores one blob row. Tombstones, fork discipline, the merge trap
(D-225), and the trunk-branch refusal are pinned here; the axis tests
live in `test_commit_stamps.py`, the op shape in
`test_ingest_history.py`.
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
    _core = None  # type: ignore[assignment]
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


def _digest(content: str) -> str:
    import hashlib
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


def _commit(repo, message, date, files, delete=()):
    for name, content in files.items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline="\n")
    for name in delete:
        (repo / name).unlink()
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env=_stamp_env(date))


def _sha(repo, rev):
    out = subprocess.run(["git", "rev-parse", rev], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


def _forked_repo(path):
    """main c1..c4, feature @c2 (f1: modify+new+delete, f2: re-add+shared),
    feature2 @c3 (g1: new file), m1 merging feature2 into main."""
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    _commit(path, "c2", "2026-01-02T10:00:00+00:00",
            {"a.py": "x = 2\n", "b.py": "y = 1\n"})
    _commit(path, "c3", "2026-01-03T10:00:00+00:00", {"a.py": "x = 3\n"})
    # Resolve the fork commits by message-independent ancestry (HEAD~
    # arithmetic shifts as the fixture grows).
    c1 = _sha(path, "main~2")
    c2 = _sha(path, "main~1")
    _git(path, "checkout", "-qb", "feature", c2)
    _commit(path, "f1", "2026-01-04T10:00:00+00:00",
            {"a.py": "x = 2\nfeature = 1\n",
             "c.py": "def g():\n    return 1\n"},
            delete=("b.py",))
    _commit(path, "f2", "2026-01-05T10:00:00+00:00",
            {"b.py": "y = 2\n", "d.py": "x = 3\n"})
    _git(path, "checkout", "-q", "main")
    _commit(path, "c4", "2026-01-06T10:00:00+00:00", {"a.py": "x = 4\n"})
    c3 = _sha(path, "main~1")
    _git(path, "checkout", "-qb", "feature2", c3)
    _commit(path, "g1", "2026-01-07T10:00:00+00:00", {"e.py": "e = 1\n"})
    _git(path, "checkout", "-q", "main")
    return path, {"c1": c1, "c2": c2, "c3": c3,
                  "c4": _sha(path, "main"),
                  "f1": _sha(path, "feature~1"), "f2": _sha(path, "feature")}


def _store_path(repo):
    return repo / ".coderadar" / "store" / "coderadar.db"


def _load(repo):
    from coderadar import coldstart
    coldstart.build_graph(str(repo))
    from coderadar import CodeGraph
    return CodeGraph(str(_store_path(repo)))


def _branch_gens():
    from coderadar._core import recorded_generations
    return recorded_generations(None, None, "feature")


def _refs(ids, generation, branch):
    from coderadar._core import source_refs_at
    return source_refs_at(ids, generation, branch)["refs"]


def _ingest_trunk_to_c4(repo, shas):
    report = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
    assert report["lineage"] == "main"
    assert report["head"] == shas["c4"]
    return report


def _ingest_feature(repo, shas):
    report = ops.ingest_history(str(repo), branch="feature",
                                store_path=str(_store_path(repo)))
    assert report["lineage"] == "feature"
    assert report["fork_point"] == shas["c2"]
    assert report["head"] == shas["f2"]
    return report


class TestBranchReadsOwnContent:
    def test_branch_and_trunk_diverge(self, tmp_path):
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        report = _ingest_feature(repo, shas)
        assert report["commits_ingested"] == 2
        assert report["files_failed"] == []
        graph = _load(repo)

        branch_gens = _branch_gens()
        assert len(branch_gens) >= 2
        branch_refs = _refs(["a.py::module", "b.py::module", "c.py::module"],
                            branch_gens[-1], "feature")
        assert branch_refs["a.py::module"]["digest"] == _digest("x = 2\nfeature = 1\n")
        assert branch_refs["b.py::module"]["digest"] == _digest("y = 2\n")
        # The branch-born file's entities live on the branch lineage.
        from coderadar._core import all_concept_ids
        assert "c.py::module" in all_concept_ids("feature")
        assert "c.py::g" in all_concept_ids("feature")

        trunk_gens = graph.verify_history(all_generations=True)["generations"]
        trunk_tip = trunk_gens[-1]["timestamp"]
        trunk_refs = _refs(["a.py::module", "b.py::module"], trunk_tip, "main")
        assert trunk_refs["a.py::module"]["digest"] == _digest("x = 4\n")
        assert trunk_refs["b.py::module"]["digest"] == _digest("y = 1\n")
        assert "c.py::module" not in all_concept_ids("main")
        assert "c.py::g" not in all_concept_ids("main")

    def test_tombstone_hides_the_branch_deleted_file(self, tmp_path):
        # f1 deletes b.py; f2 re-adds it. Mid-branch b.py is absent on the
        # branch and present on the trunk — the tombstone, not fallback.
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        ops.ingest_history(str(repo), branch="feature",
                           store_path=str(_store_path(repo)))
        graph = _load(repo)

        from coderadar._core import recorded_generations
        branch_gens = recorded_generations(None, None, "feature")
        # f1's generations precede f2's: b.py absent early, present late.
        early = [g for g in branch_gens]
        first_refs = _refs(["b.py::module"], early[0], "feature")
        assert first_refs.get("b.py::module") is None
        last_refs = _refs(["b.py::module"], early[-1], "feature")
        assert last_refs["b.py::module"]["digest"] == _digest("y = 2\n")

        trunk_tip = graph.verify_history(all_generations=True)["generations"][-1]["timestamp"]
        trunk_refs = _refs(["b.py::module"], trunk_tip, "main")
        assert trunk_refs["b.py::module"]["digest"] == _digest("y = 1\n")

    def test_verify_branch_reports_per_lineage_coverage(self, tmp_path):
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        _ingest_feature(repo, shas)
        graph = _load(repo)

        verdict = graph.verify_history(all_generations=True, branch="feature")
        assert verdict["byte_coverage_pct"] == 100.0, verdict["totals"]
        assert verdict["totals"].get("no_digest", 0) == 0

    def test_shared_content_stores_one_blob_row(self, tmp_path):
        # f2's d.py repeats trunk a.py@c3's bytes exactly: content-addressed
        # sharing across lineages is one row, not two.
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        _ingest_feature(repo, shas)
        digest = _digest("x = 3\n")
        with sqlite3.connect(str(_store_path(repo))) as conn:
            (rows,) = conn.execute(
                "SELECT COUNT(*) FROM blobs WHERE sha256 = ?", (digest,)).fetchone()
        assert rows == 1


class TestForkDiscipline:
    def test_dry_run_forks_nothing(self, tmp_path):
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        store = str(_store_path(repo))
        report = ops.ingest_history(str(repo), branch="feature",
                                    dry_run=True, store_path=store)
        assert report["dry_run"] is True
        assert report["fork_point"] == shas["c2"]
        assert report["commits_ingested"] == 0
        names = [b["name"] for b in ops.branches(graph=_load(repo))]
        assert names == ["main"], names
        # The real run still works afterwards: the dry run recorded nothing.
        real = ops.ingest_history(str(repo), branch="feature", store_path=store)
        assert real["commits_ingested"] == 2

    def test_resume_and_idempotent_noop(self, tmp_path):
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        first = _ingest_feature(repo, shas)
        second = ops.ingest_history(str(repo), branch="feature",
                                    store_path=str(_store_path(repo)))
        assert second["commits_ingested"] == 0
        assert second["head"] == first["head"] == shas["f2"]
        assert second["from"] == shas["f2"]

    def test_trunk_walk_of_a_branch_is_refused(self, tmp_path):
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        # An explicit trunk lineage with a non-main git branch is a
        # category error: branch history is not trunk history (a bare
        # branch="feature" without a lineage routes onto its own lineage).
        with pytest.raises(Exception, match="trunk"):
            ops.ingest_history(str(repo), branch="feature", lineage="main",
                               store_path=str(_store_path(repo)))

    def test_fork_mismatch_refuses(self, tmp_path):
        # Rewinding the branch below the recorded fork moves history out
        # from under the lineage: refusing beats writing two histories.
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        _ingest_feature(repo, shas)
        _git(repo, "update-ref", "refs/heads/feature", shas["c1"])
        with pytest.raises(Exception, match="fork-mismatch"):
            ops.ingest_history(str(repo), branch="feature",
                               store_path=str(_store_path(repo)))

    def test_manually_forked_lineage_is_refused(self, tmp_path):
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        _load(repo)
        _core.fork_branch("rogue", "main")
        _git(repo, "branch", "rogue", shas["c2"])
        with pytest.raises(Exception, match="lineage-exists"):
            ops.ingest_history(str(repo), branch="rogue", lineage="rogue",
                               store_path=str(_store_path(repo)))

    def test_cli_ingests_a_branch_lineage(self, tmp_path):
        from click.testing import CliRunner
        from coderadar.cli import main as cli_main
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        store = str(_store_path(repo))
        previous = os.getcwd()
        # chdir into tmp first: bare _enter_project would otherwise walk up
        # from the pytest cwd, find this repository's own marker, and
        # activate its config globally for every test after this one.
        os.chdir(tmp_path)
        try:
            result = CliRunner().invoke(cli_main, [
                "ingest-history", "--repo", str(repo), "--store", store,
                "--branch", "feature",
            ])
        finally:
            os.chdir(previous)
        assert result.exit_code == 0, result.output
        assert "(lineage feature)" in result.output
        assert "ingested=2" in result.output
        assert shas["c2"][:12] in result.output


class TestMergeTrap:
    def test_merge_with_branch_born_file_skips_concepts(self, tmp_path):
        # feature2 born at c3 adds e.py; main advances to c4; m1 merges.
        # The trunk can never mint e.py's branch-born id (D-225) — bytes
        # and spine land, entities stay on the branch, and the skip is
        # auditable instead of aborting the merge ingest.
        repo, shas = _forked_repo(tmp_path / "repo")
        _ingest_trunk_to_c4(repo, shas)
        ops.ingest_history(str(repo), branch="feature2",
                           store_path=str(_store_path(repo)))
        _git(repo, "merge", "-q", "--no-ff", "-m", "m1", "feature2")
        shas["m1"] = _sha(repo, "main")
        report = ops.ingest_history(str(repo), store_path=str(_store_path(repo)))
        assert report["commits_ingested"] == 1
        assert report["head"] == shas["m1"]
        # e.py's bytes and spine land; its entities stay on feature2.
        failed = {f["path"]: f["reason"] for f in report["files_failed"]}
        assert failed.get("e.py", "").startswith("cross-lineage-id:feature2"), failed
        from coderadar._core import all_concept_ids
        graph = _load(repo)
        assert "e.py::module" not in all_concept_ids("main")
        assert "e.py::module" in all_concept_ids("feature2")
        graph = _load(repo)
        chain = graph.history_of("e.py")
        assert chain["count"] == 1
        assert chain["versions"][0]["digest"] == _digest("e = 1\n")
        # And the trunk merge itself is honestly covered.
        verdict = graph.verify_history(all_generations=True)
        assert verdict["byte_coverage_pct"] == 100.0, verdict["totals"]
