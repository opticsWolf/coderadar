"""P5d's first slice: observation writes on a git-filled store.

`docs/temporal-p5b-and-beyond.md` §3.1.3 (A-7, INV-17, H-26). The law this
file pins: an index-now write on a commit-time store is not a mixture to
refuse — it is an *observation*, stamped at the instant the writer saw the
tree on both axes, tagged with where it came from, and closing the
commit-era interval it replaces. A commit discovered afterwards that would
open a start behind the observation is nudged past it and says so; nothing
is written backwards and nothing is retracted.
"""

from __future__ import annotations

import hashlib
import json
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

TS_OPEN = "9999-12-31T23:59:59.999999Z"
TS_OPEN_LEGACY = "9999-12-31T00:00:00.000000Z"
TS_OPEN_ENDS = (TS_OPEN, TS_OPEN_LEGACY)

BODY = 'def greet(name):\n    return "hello " + name\n'
EDITED = 'def greet(name):\n    return "hi"\n'


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


def _beliefs(repo, entity_id):
    """The ledger's belief per (id, valid start): the latest row written for
    that start (Macrame D-291's definition, the one INV-15 reads)."""
    with sqlite3.connect(str(_store_path(repo))) as conn:
        payloads = conn.execute(
            "SELECT payload FROM transaction_log"
            " WHERE table_name = 'concepts' AND entity_id = ? ORDER BY seq_id",
            (entity_id,),
        ).fetchall()
    belief = {}
    for (payload,) in payloads:
        row = json.loads(payload)
        belief[row["valid_from"]] = row
    return [belief[start] for start in sorted(belief)]


def _open_beliefs(repo, entity_id):
    return [row for row in _beliefs(repo, entity_id) if row["valid_to"] in TS_OPEN_ENDS]


def _current_extra(repo, entity_id):
    with sqlite3.connect(str(_store_path(repo))) as conn:
        (extra,) = conn.execute(
            "SELECT extra FROM concepts WHERE id = ?", (entity_id,)
        ).fetchone()
    return json.loads(extra)["coderadar"]


class TestAnObservationIsRecorded:
    def test_a_mutation_on_a_git_filled_store_is_recorded_and_reported(self, tmp_path):
        """F5's shape, stated as the report and the ledger row it produced."""
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"app.py": BODY})
        _ingest(repo)
        os.chdir(repo)
        graph = _load(repo)
        plan = graph.plan_replace_body("app.py::greet", 'return "hi"', dry_run=True)
        result = graph.apply(plan)

        assert result.status == "Applied", (result.status, result.failure, result.sync)
        assert result.sync.state == "Synchronized"
        assert len(result.sync.observations) == 1, result.sync.observations
        observed = result.sync.observations[0]
        assert observed["file"] == "app.py"
        assert observed["source"] == "mutation"
        assert observed["observed_at"], "an observation names its instant"

        # The row the write opened carries the tag, the plan and the attempt.
        open_rows = _open_beliefs(repo, "app.py::module")
        assert len(open_rows) == 1, open_rows
        assert open_rows[0]["valid_from"] == observed["observed_at"]
        tag = _current_extra(repo, "app.py::module")
        assert tag["source"] == "mutation"
        assert tag["plan_id"] == plan.id
        assert tag["attempt_id"] == result.attempt_id
        assert tag["tool"] == "replace_entity_body"
        assert tag["source_blob"], "tagging merges; it must not drop the digest"

        # And the recorded fold sees the edit without any `at=`: the newest
        # generation is the observation's.
        generation = _core.recorded_generations(None, None, "main")[-1]
        ref = _core.entity_source_ref_at("app.py::module", generation, "main")
        assert ref["digest"] == hashlib.sha256(EDITED.encode()).hexdigest(), ref

    def test_the_observation_closes_the_commit_interval_it_replaces(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"app.py": BODY})
        _ingest(repo)
        os.chdir(repo)
        graph = _load(repo)
        result = graph.apply(graph.plan_replace_body("app.py::greet", 'return "hi"'))
        assert result.status == "Applied", (result.status, result.sync)
        instant = result.sync.observations[0]["observed_at"]

        rows = _beliefs(repo, "app.py::module")
        assert len(rows) == 2, rows
        (c1_from, c1_to) = (rows[0]["valid_from"], rows[0]["valid_to"])
        assert c1_to == instant, "the commit-era interval ends where the edit was seen"
        assert c1_to != c1_from, "the commit row is closed, not rewritten"
        assert rows[1]["valid_from"] == instant
        assert rows[1]["valid_to"] in TS_OPEN_ENDS
        # INV-15 over the observation: one open interval, and it is the newest.
        assert len(_open_beliefs(repo, "app.py::module")) == 1

    def test_a_commit_after_an_observation_lands_after_it_and_says_so(self, tmp_path):
        """H-26: nothing opens a start behind a recorded belief.

        The commit is dated before the observation on purpose — a rebased,
        imported or backdated commit. Ingest nudges it past the observation
        and counts the nudge; the alternative would be writing backwards,
        which the write law refuses.
        """
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"app.py": BODY})
        _ingest(repo)
        os.chdir(repo)
        graph = _load(repo)
        result = graph.apply(graph.plan_replace_body("app.py::greet", 'return "hi"'))
        assert result.status == "Applied", (result.status, result.sync)
        instant = result.sync.observations[0]["observed_at"]

        _commit(repo, "c2", "2026-01-02T10:00:00+00:00",
                {"app.py": 'def greet(name):\n    return "yo"\n'})
        report = _ingest(repo)
        assert report["commits_ingested"] == 1, report
        assert report["stamps_nudged"] == 1, report

        newest = _beliefs(repo, "app.py::module")[-1]
        assert newest["valid_from"] > instant, (
            "a commit discovered after the observation must not open a start"
            " behind it"
        )
        assert len(_open_beliefs(repo, "app.py::module")) == 1


class TestTheSourceTravels:
    def test_a_bare_update_names_its_caller(self, tmp_path):
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00", {"app.py": BODY})
        _ingest(repo)
        _load(repo)
        report = _core.update_file(
            "app.py", 'def greet(name):\n    return "watched"\n', False,
            source="watcher",
        )
        assert report["source"] == "watcher", report
        assert report["observed_at"]
        assert len(_open_beliefs(repo, "app.py::module")) == 1
        assert _current_extra(repo, "app.py::module")["source"] == "watcher"

    def test_a_full_reindex_on_a_git_filled_store_is_an_analyze_observation(self, tmp_path):
        """An analyze that finds edited bytes records them; identical bytes
        assert nothing new, so the untouched file keeps its one version."""
        repo = _init_repo(tmp_path / "repo")
        _commit(repo, "c1", "2026-01-01T10:00:00+00:00",
                {"app.py": BODY, "b.py": "y = 1" + chr(10)})
        _ingest(repo)
        os.chdir(repo)
        untouched_before = _beliefs(repo, "app.py::module")
        (repo / "b.py").write_text("y = 2" + chr(10), encoding="utf-8", newline=chr(10))

        out = ops.reindex(full=True)
        assert isinstance(out, dict) and "stats" in out, out

        assert _current_extra(repo, "b.py::module")["source"] == "analyze"
        assert len(_open_beliefs(repo, "b.py::module")) == 1
        # The unchanged file is not churned: same stamps, no new version.
        assert _beliefs(repo, "app.py::module") == untouched_before
        assert len(_open_beliefs(repo, "app.py::module")) == 1
