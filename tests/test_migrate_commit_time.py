"""Commit-time migration (plan §5 P5a.5, DR-41).

A new store built from git history: the trunk first, then each branch
onto its own lineage, one embeddings pass over the final state, and a
per-branch coverage sweep in the report. The old store is never
touched; re-running no-ops per branch.
"""

from __future__ import annotations

import hashlib
import os
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


def _small_repo(path):
    """main c1..c3, feature @c2 with one divergent commit."""
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    _commit(path, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    _commit(path, "c2", "2026-01-02T10:00:00+00:00",
            {"a.py": "x = 2\n", "b.py": "y = 1\n"})
    _commit(path, "c3", "2026-01-03T10:00:00+00:00", {"a.py": "x = 3\n"})
    c2 = _sha(path, "main~1")
    _git(path, "checkout", "-qb", "feature", c2)
    _commit(path, "f1", "2026-01-04T10:00:00+00:00",
            {"a.py": "x = 2\nfeature = 1\n",
             "c.py": "def g():\n    return 1\n"})
    _git(path, "checkout", "-q", "main")
    return path, {"c3": _sha(path, "main"), "f1": _sha(path, "feature")}


def _dest(tmp_path):
    return tmp_path / "migrated" / "coderadar.db"


def _gens(branch):
    from coderadar._core import recorded_generations
    return recorded_generations(None, None, branch)


def _refs(ids, generation, branch):
    from coderadar._core import source_refs_at
    return source_refs_at(ids, generation, branch)["refs"]


class TestMigrate:
    def test_migrates_trunk_and_branch(self, tmp_path):
        repo, _shas = _small_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        report = ops.migrate_to_commit_time(str(repo), str(dest))
        assert report["branches"] == ["main", "feature"]
        assert report["commits_ingested"] == 4
        assert report["commits_skipped"] == 0
        assert report["files_failed"] == 0
        assert report["repo"] == str(repo)
        assert report["store"] == str(dest)

        trunk_refs = _refs(["a.py::module", "b.py::module"],
                           _gens("main")[-1], "main")
        assert trunk_refs["a.py::module"]["digest"] == _digest("x = 3\n")
        assert trunk_refs["b.py::module"]["digest"] == _digest("y = 1\n")
        branch_refs = _refs(["a.py::module", "c.py::module", "b.py::module"],
                            _gens("feature")[-1], "feature")
        assert branch_refs["a.py::module"]["digest"] == _digest("x = 2\nfeature = 1\n")
        assert "c.py::g" in _refs(["c.py::g"], _gens("feature")[-1], "feature")
        # The trunk never sees the branch-born file.
        assert "c.py::module" not in trunk_refs

        assert report["verify"]["main"]["byte_coverage_pct"] == 100.0
        assert report["verify"]["feature"]["byte_coverage_pct"] == 100.0

    def test_history_chain_is_lineage_blind(self, tmp_path):
        # Branch content appears in the file's digest chain (P5a.4): the
        # chain is per-file, not per-lineage.
        from coderadar import CodeGraph
        repo, _ = _small_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        ops.migrate_to_commit_time(str(repo), str(dest))
        chain = CodeGraph(str(dest)).history_of("a.py")
        assert chain["count"] == 4
        assert [v["digest"] for v in chain["versions"]] == [
            _digest("x = 2\nfeature = 1\n"),
            _digest("x = 3\n"),
            _digest("x = 2\n"),
            _digest("x = 1\n"),
        ]

    def test_explicit_branches_prepend_the_trunk(self, tmp_path):
        repo, _ = _small_repo(tmp_path / "repo")
        report = ops.migrate_to_commit_time(str(repo), str(_dest(tmp_path)),
                                            branches=["feature"], verify=False,
                                            embed=False)
        assert report["branches"] == ["main", "feature"]
        assert report["commits_ingested"] == 4
        assert report["verify"] is None
        assert report["embeddings"] == {"embedded": False}

    def test_no_trunk_refuses(self, tmp_path):
        repo = tmp_path / "solo"
        repo.mkdir(parents=True, exist_ok=True)
        _git(repo, "init", "-q", "-b", "solo", ".")
        _git(repo, "config", "user.name", "T")
        _git(repo, "config", "user.email", "t@t.t")
        _commit(repo, "s1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
        with pytest.raises(Exception, match="no 'main'"):
            ops.migrate_to_commit_time(str(repo), str(_dest(tmp_path)))

    def test_list_branches_names_local_branches(self, tmp_path):
        repo, _ = _small_repo(tmp_path / "repo")
        assert list(_core.list_branches(str(repo))) == ["feature", "main"]


class TestEmbeddingsOnce:
    def test_one_pass_over_final_state(self, tmp_path):
        # The vector pass covers the final-state projection exactly once:
        # total equals the live entity count, not generations × entities,
        # and the index answers after a fresh load (durable, not memory).
        from coderadar._core import graph_stats
        repo, _ = _small_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        report = ops.migrate_to_commit_time(str(repo), str(dest))
        embeddings = report["embeddings"]
        assert embeddings["embedded"] is True
        assert embeddings["errors"] == 0
        stats = graph_stats()
        live = sum(stats.get(k, 0) for k in (
            "functions", "classes", "modules",
            "imports", "constants", "type_aliases"))
        assert embeddings["total"] == live == embeddings["generated"]
        assert embeddings["cached"] == 0
        assert embeddings["persisted"] == live

        hits = ops.search_similar("feature flag module", top_k=5)
        assert hits, "the migrated vector index must answer"
        assert any(h["id"].startswith("a.py") for h in hits)

        from coderadar import load as _cold_load
        _cold_load(str(dest), root=str(repo))
        assert ops.search_similar("feature flag module", top_k=5), \
            "vectors must survive a fresh load"

    def test_rerun_noops_completed_branches(self, tmp_path):
        repo, _ = _small_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        first = ops.migrate_to_commit_time(str(repo), str(dest))
        second = ops.migrate_to_commit_time(str(repo), str(dest))
        assert second["branches"] == first["branches"]
        assert second["commits_ingested"] == 0
        assert second["verify"]["main"]["byte_coverage_pct"] == 100.0


class TestOldStoreUntouched:
    def test_migration_never_writes_the_old_store(self, tmp_path):
        # Byte-hashing the file would catch WAL checkpoints, not writes
        # (the store is WAL-mode: closing a handle checkpoints and moves
        # bytes). The semantic guarantee is logical: the same generations,
        # the same concepts — and the old store still opens and answers.
        from coderadar import coldstart
        from coderadar._core import all_concept_ids, recorded_generations
        repo, _shas = _small_repo(tmp_path / "repo")
        coldstart.build_graph(str(repo), create_store=True)
        old_db = repo / ".coderadar" / "store" / "coderadar.db"
        assert old_db.exists()
        before_gens = recorded_generations(None, None, "main")
        before_ids = sorted(all_concept_ids("main"))
        assert before_ids, "the old store must describe something"

        ops.migrate_to_commit_time(str(repo), str(_dest(tmp_path)),
                                   embed=False, verify=False)

        from coderadar import load as _cold_load
        graph = _cold_load(str(old_db), root=str(repo))
        assert recorded_generations(None, None, "main") == before_gens
        assert sorted(all_concept_ids("main")) == before_ids
        assert next(iter(graph.query("modules")))["id"].endswith("::module")


class TestCli:
    def test_cli_migrates(self, tmp_path):
        from click.testing import CliRunner
        from coderadar.cli import main as cli_main
        repo, _ = _small_repo(tmp_path / "repo")
        dest = _dest(tmp_path)
        previous = os.getcwd()
        # chdir into tmp first: bare _enter_project would otherwise walk up
        # from the pytest cwd, find this repository's own marker, and
        # activate its config globally for every test after this one.
        os.chdir(tmp_path)
        try:
            result = CliRunner().invoke(cli_main, [
                "migrate-to-commit-time", "--repo", str(repo),
                "--store", str(dest), "--no-embed", "--no-verify",
            ])
        finally:
            os.chdir(previous)
        assert result.exit_code == 0, result.output
        assert "migrated" in result.output
        assert "feature" in result.output
