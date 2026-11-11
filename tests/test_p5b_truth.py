"""P5b.0 truth tests (`docs/temporal-p5b-and-beyond.md` §1, §6 P5b.0).

One test per finding, each stating what a git-filled store must answer and
marked `xfail(strict=True)` with the phase that flips it. They are the
programme's acceptance gates: when a phase lands, its test XPASSes, strict
mode fails the run, and the marker comes off in the same change. Each
reason names what the store does today, checked by running the file with
``--runxfail``.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
from collections import defaultdict

import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover - gate for CI without the extension
    _core = None  # type: ignore[assignment]
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

# The open end, either spelling: the engine's OPEN_SENTINEL on 0.22-pin
# stores, the pre-0.22 spelling on older ones (storage.rs TS_OPEN /
# TS_OPEN_LEGACY; is_open_end reads both). Fixed 2026-11-11 in P5b.1's
# proof: the lone legacy spelling matched nothing on a 0.22-pin store,
# so the test passed vacuously — a forged doubled-open read clean.
TS_OPEN = "9999-12-31T23:59:59.999999Z"
TS_OPEN_LEGACY = "9999-12-31T00:00:00.000000Z"
TS_OPEN_ENDS = (TS_OPEN, TS_OPEN_LEGACY)


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


def _latest_generation(branch="main"):
    from coderadar._core import recorded_generations
    return recorded_generations(None, None, branch)[-1]


def _refs(ids, ts, branch="main"):
    return _core.source_refs_at(ids, ts, branch)["refs"]


def _three_commits(root):
    """main: c1 a.py; c2 a.py changed + b.py; c3 a.py changed."""
    repo = _init_repo(root / "repo")
    _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
    _commit(repo, "c2", "2026-01-02T10:00:00+00:00",
            {"a.py": "x = 2\n", "b.py": "def f():\n    return 1\n"})
    _commit(repo, "c3", "2026-01-03T10:00:00+00:00", {"a.py": "x = 3\n"})
    return repo


class TestF1CommitTimesAreQueryable:
    @pytest.mark.xfail(strict=True, reason=(
        "F1: reads fold on recorded_at, and every row of a git-filled store "
        "was recorded on the ingest day, so a read at a commit's own date "
        "folds an empty prefix. Flips in P5b.3a (horizon + commit index)."))
    def test_a_read_between_two_commits_returns_the_earlier_one(self, tmp_path):
        repo = _three_commits(tmp_path)
        _ingest(repo)
        _load(repo)
        refs = _refs(["a.py::module", "b.py::module"],
                     "2026-01-02T12:00:00.000000Z")
        assert refs.get("a.py::module", {}).get("digest") == _digest("x = 2\n")
        assert "b.py::module" in refs


class TestF2OneOpenIntervalPerConcept:
    # Flipped in P5b.1/M6-g (macrame-db 0.22.0, D-312): the one-write
    # supersession closes the previous interval, so the doubled-open
    # shape is impossible.
    def test_no_concept_holds_two_open_intervals(self, tmp_path):
        repo = _three_commits(tmp_path)
        _ingest(repo)
        con = sqlite3.connect(str(_store_path(repo)))
        try:
            rows = con.execute(
                "SELECT entity_id, payload FROM transaction_log "
                "WHERE table_name = 'concepts' ORDER BY seq_id").fetchall()
        finally:
            con.close()
        # Current belief per (concept, valid start): the latest row written
        # for that start (Macrame D-291's definition).
        belief = {}
        for entity_id, payload in rows:
            row = json.loads(payload)
            belief[(entity_id, row["valid_from"])] = row["valid_to"]
        open_starts = defaultdict(int)
        for (entity_id, _start), valid_to in belief.items():
            if valid_to in TS_OPEN_ENDS:
                open_starts[entity_id] += 1
        doubled = {i: n for i, n in open_starts.items() if n > 1}
        assert doubled == {}, doubled


class TestF3BranchSeesItsMergeBaseOnly:
    @pytest.mark.xfail(strict=True, reason=(
        "F3: Macrame cuts a fork at the recorded instant fork() runs, after "
        "the whole trunk was ingested, so the branch inherits trunk content "
        "from after its merge-base. Flips in P5c (U-3)."))
    def test_branch_does_not_see_trunk_commits_after_its_fork(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
        _commit(repo, "c2", "2026-01-02T10:00:00+00:00",
                {"a.py": "x = 2\n", "b.py": "y = 1\n"})
        _git(repo, "checkout", "-qb", "feature")
        _commit(repo, "f1", "2026-01-03T10:00:00+00:00", {"b.py": "y = 2\n"})
        _git(repo, "checkout", "-q", "main")
        _commit(repo, "c3", "2026-01-04T10:00:00+00:00",
                {"a.py": "x = 3\n", "d.py": "z = 1\n"})
        _ingest(repo)
        _ingest(repo, "feature")
        _load(repo)
        refs = _refs(["a.py::module", "b.py::module", "d.py::module"],
                     _latest_generation("feature"), "feature")
        assert refs["b.py::module"]["digest"] == _digest("y = 2\n")
        assert "d.py::module" not in refs, "d.py was added on main after the fork"
        assert refs["a.py::module"]["digest"] == _digest("x = 2\n"), \
            "a.py changed on main after the fork"


class TestF4MergeBringsBranchFiles:
    # Flipped in M6-g (macrame-db 0.22.0, D-315): the merge's branch-born
    # ids are adoptions - the trunk mints its OWN first versions, and the
    # entities are the trunk's.
    def test_trunk_holds_the_merged_files_entities(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"a.py": "x = 1\n"})
        _git(repo, "checkout", "-qb", "feature")
        _commit(repo, "f1", "2026-01-02T10:00:00+00:00",
                {"e.py": "def g():\n    return 1\n"})
        _git(repo, "checkout", "-q", "main")
        _commit(repo, "c2", "2026-01-03T10:00:00+00:00", {"a.py": "x = 2\n"})
        _ingest(repo)
        _ingest(repo, "feature")
        _git(repo, "merge", "-q", "--no-ff", "-m", "m1", "feature",
             env=_stamp_env("2026-01-04T10:00:00+00:00"))
        report = _ingest(repo)
        _load(repo)
        from coderadar._core import all_concept_ids
        trunk = set(all_concept_ids("main"))
        assert {"e.py::module", "e.py::g"} <= trunk, report["files_failed"]


class TestF5RecordedEdits:
    @pytest.mark.xfail(strict=True, reason=(
        "F5: a git-filled store refuses update_file (INV-12), so a successful "
        "mutation ends ApplyIncomplete and the edit is invisible to a no-at "
        "read. Flips in P5d (observation writes)."))
    def test_a_mutation_on_a_git_filled_store_is_recorded(self, tmp_path, monkeypatch):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00",
                {"app.py": 'def greet(name):\n    return "hello " + name\n'})
        _ingest(repo)
        monkeypatch.chdir(repo)
        graph = _load(repo)
        plan = graph.plan_replace_body("app.py::greet", 'return "hi"', dry_run=True)
        result = graph.apply(plan)
        assert result.status == "Applied", (result.status, result.sync)
        on_disk = (repo / "app.py").read_text(encoding="utf-8")
        refs = _refs(["app.py::module"], _latest_generation())
        assert refs["app.py::module"]["digest"] == _digest(on_disk)


class TestF6RenameKeepsAnnotations:
    @pytest.mark.xfail(strict=True, reason=(
        "F6: ids are built from the path and a rename ingests as delete + add, "
        "so an annotation on the old id does not reach the renamed entity. "
        "Flips in P5b.7 (rename links)."))
    def test_an_annotation_follows_its_entity_across_a_rename(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        body = "def foo():\n    return 1\n"
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"old.py": body})
        _ingest(repo)
        _load(repo)
        ops.annotate("old.py::foo", "- check the return value")
        _git(repo, "mv", "old.py", "new.py")
        _git(repo, "commit", "-qm", "rename",
             env=_stamp_env("2026-01-02T10:00:00+00:00"))
        _ingest(repo)
        _load(repo)
        listed = ops.annotations_on("new.py::foo")
        assert listed["count"] == 1, listed
