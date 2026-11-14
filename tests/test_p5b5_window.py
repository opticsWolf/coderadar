"""P5b.5's ingest half: the backfill window (`docs/temporal-p5b-and-beyond.md`).

Doctrine VIII as a test. One history, two stores: A ingests it in order; B
starts at a later commit and backfills the prefix as contiguous chunks,
newest first. Every commit's **valid-axis** answers must agree between the
two stores, and the recorded axis must differ exactly where the chunk order
did. The window writes closed intervals that abut the belief they precede:
no sweep, no edges, no cursor movement, and every interval's end is its
successor's start.

The refusals are part of the contract: a window must end at the belief's
start (anything else leaves a hole), must have a belief to abut, and must
find its end on the walked chain.
"""

from __future__ import annotations

import os
import subprocess

import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover
    _core = None  # type: ignore[assignment]
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

A1 = "def f():\n    return 1\n"
A2 = "def f():\n    return 2\n"
A3 = "def f():\n    return 3\n"
B1 = "def g():\n    return 1\n"
B2 = "def g():\n    return 2\n"
B3 = "def g():\n    return 3\n"
D1 = "def h():\n    return 1\n"
C1 = "def k():\n    return 1\n"


def _git(repo, *args, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    subprocess.run(["git", *args], cwd=str(repo), env=merged,
                   check=True, capture_output=True)


def _env(date):
    return {
        "GIT_AUTHOR_NAME": "T Author",
        "GIT_AUTHOR_EMAIL": "author@t.t",
        "GIT_COMMITTER_NAME": "T Committer",
        "GIT_COMMITTER_EMAIL": "committer@t.t",
        "GIT_AUTHOR_DATE": date,
        "GIT_COMMITTER_DATE": date,
    }


def _init_repo(path):
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main", ".")
    _git(path, "config", "user.name", "T")
    _git(path, "config", "user.email", "t@t.t")
    _git(path, "config", "core.autocrlf", "false")
    return path


def _commit(repo, message, date, files=None, delete=()):
    for name, content in (files or {}).items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline="\n")
    for name in delete:
        _git(repo, "rm", "-q", name)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env=_env(date))
    out = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(repo),
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


def _store_path(repo, name="coderadar.db"):
    return repo / ".coderadar" / name


# c1: a, b, d (d never touched again)
# c2: a changed
# c3: b changed, c added
# c4: c deleted
# c5: a changed again
# c6: b changed again
TIMES = [
    "2026-01-01T10:00:00+00:00",
    "2026-01-01T11:00:00+00:00",
    "2026-01-01T12:00:00+00:00",
    "2026-01-01T13:00:00+00:00",
    "2026-01-01T14:00:00+00:00",
    "2026-01-01T15:00:00+00:00",
]


def _history(path):
    repo = _init_repo(path)
    c1 = _commit(repo, "c1", TIMES[0],
                 {"a.py": A1, "b.py": B1, "d.py": D1})
    c2 = _commit(repo, "c2", TIMES[1], {"a.py": A2})
    c3 = _commit(repo, "c3", TIMES[2], {"b.py": B2, "c.py": C1})
    c4 = _commit(repo, "c4", TIMES[3], delete=["c.py"])
    c5 = _commit(repo, "c5", TIMES[4], {"a.py": A3})
    c6 = _commit(repo, "c6", TIMES[5], {"b.py": B3})
    return repo, (c1, c2, c3, c4, c5, c6)


def _load(repo):
    """Attach `repo`'s store as the ambient one, the way every op reads it."""
    import coderadar
    coderadar.load(str(_store_path(repo)), str(repo))
    return coderadar.CodeGraph(str(_store_path(repo)))


def _authors_write(ts: str) -> str:
    """A commit's own stamp: the instant every row of that commit starts at."""
    return ts.replace("+00:00", ".000000Z")


IDS = ["a.py::module", "a.py::f", "b.py::module", "b.py::g",
       "c.py::module", "c.py::k", "d.py::module", "d.py::h"]


def _digests_at(ts: str) -> dict:
    from coderadar._core import entity_source_ref_at
    out = {}
    for ident in IDS:
        ref = entity_source_ref_at(ident, ts, "main", valid=True)
        out[ident] = (ref or {}).get("digest")
    return out


class TestDoctrineEight:
    def test_chunked_backfill_answers_the_same_valid_axis(self, tmp_path):
        repo, shas = _history(tmp_path / "repo")

        # The two stores must sit at each repo's default path: the axis
        # reads resolve the ambient store, and a store path of the caller's
        # choosing is not one the ambient loader knows.
        store_a = _store_path(repo)
        ops.ingest_history(str(repo), store_path=str(store_a))

        repo_b = tmp_path / "repo-b"
        subprocess.run(["git", "clone", "-q", str(repo), str(repo_b)],
                       check=True, capture_output=True)
        store_b = _store_path(repo_b)
        # B starts at c4: its belief begins at c5, so a.py and b.py are
        # recorded from c5/c6 on and the prefix is missing.
        first = ops.ingest_history(str(repo), from_sha=shas[3],
                                   store_path=str(store_b))
        assert first["belief_start"] == shas[4], first
        assert first["head"] == shas[5], first

        # Chunk 1: (c2, c5) = c3 and c4.
        second = ops.ingest_history(str(repo), from_sha=shas[1],
                                    to_sha=first["belief_start"],
                                    store_path=str(store_b))
        assert second["window"] == {
            "from": shas[1], "to": shas[4], "commits": 2,
            "edges_skipped": True,
        }, second
        assert second["commits_ingested"] == 2, second
        assert second["head"] == shas[5], (
            "a window moves no cursor", second)
        assert second["belief_start"] == shas[2], second

        # Chunk 2: the root through c2, contiguous with chunk 1.
        third = ops.ingest_history(str(repo), to_sha=second["belief_start"],
                                   store_path=str(store_b))
        assert third["window"] == {
            "from": None, "to": shas[2], "commits": 2,
            "edges_skipped": True,
        }, third
        assert third["belief_start"] == shas[0], third

        # The pair of assertions Doctrine VIII states: the valid axis is
        # identical at every commit in both stores, and the current state is
        # identical too. (The recorded axis differs by construction — the
        # window's completions were written after the newer commits' — which
        # is what the report's `window` states, not something to measure in
        # rows.)
        _load(repo)
        recorded_a = [_digests_at(_authors_write(stamp)) for stamp in TIMES]
        present_a = ops.as_of("2027-01-01T00:00:00.000000Z", symbols=IDS,
                              valid=True)["entities"]
        _load(repo_b)
        recorded_b = [_digests_at(_authors_write(stamp)) for stamp in TIMES]
        present_b = ops.as_of("2027-01-01T00:00:00.000000Z", symbols=IDS,
                              valid=True)["entities"]
        assert recorded_a == recorded_b, (
            "chunked newest-first must answer every commit's valid axis "
            "exactly as one in-order ingest does")
        assert present_a == present_b, (
            "the current state must agree: a closed window row may never "
            "leave a deleted entity in the live projection")
        assert present_b["c.py::k"] is None
        assert present_b["d.py::h"] is not None

        # The contents really do differ across commits — otherwise the
        # equality above would be vacuous.
        a_of = [row["a.py::f"] for row in recorded_a]
        assert len(set(a_of)) == 3, a_of
        assert recorded_a[2]["c.py::k"] is not None
        assert recorded_a[3]["c.py::k"] is None, (
            "c.py died at c4 and must be absent after it")
        assert recorded_a[5]["d.py::h"] is not None, (
            "d.py was never touched again: its version stays open")


    def test_a_window_is_dry_runnable(self, tmp_path):
        repo, shas = _history(tmp_path / "repo")
        store = tmp_path / "dry.db"
        ops.ingest_history(str(repo), from_sha=shas[3], store_path=str(store))
        plan = ops.ingest_history(str(repo), from_sha=shas[1],
                                  to_sha=shas[4], dry_run=True,
                                  store_path=str(store))
        assert plan["dry_run"] is True
        assert plan["commits_ingested"] == 0
        assert [c["sha"] for c in plan["commits_skipped"]] == [shas[2], shas[3]]
        assert plan["window"]["commits"] == 2, plan
        # Nothing was written: the belief start is still c5, and a real run
        # afterwards is still accepted.
        stored = ops.ingest_history(str(repo), from_sha=shas[1],
                                    to_sha=shas[4], store_path=str(store))
        assert stored["commits_ingested"] == 2, stored


class TestTheWindowRefusesWhatItCannotDo:
    def test_the_end_must_be_the_belief_start(self, tmp_path):
        repo, shas = _history(tmp_path / "repo")
        store = tmp_path / "wrong-end.db"
        ops.ingest_history(str(repo), from_sha=shas[3], store_path=str(store))
        with pytest.raises(ops.InvalidRequest, match="window-end"):
            ops.ingest_history(str(repo), from_sha=shas[1], to_sha=shas[3],
                               store_path=str(store))

    def test_a_store_with_no_belief_refuses_a_window(self, tmp_path):
        repo, shas = _history(tmp_path / "repo")
        with pytest.raises(ops.InvalidRequest, match="window-needs-belief"):
            ops.ingest_history(str(repo), from_sha=shas[1], to_sha=shas[2],
                               store_path=str(tmp_path / "fresh.db"))

    def test_the_end_must_lie_on_the_walked_chain(self, tmp_path):
        repo, shas = _history(tmp_path / "repo")
        store = tmp_path / "off-chain.db"
        ops.ingest_history(str(repo), from_sha=shas[3], store_path=str(store))
        # A branch commit is not on main's chain: the walk from c4 never
        # reaches it.
        _git(repo, "checkout", "-q", "-b", "side", shas[0])
        side = _commit(repo, "side", "2026-01-02T10:00:00+00:00",
                       {"s.py": "x = 1\n"})
        _git(repo, "checkout", "-q", "main")
        with pytest.raises(ops.InvalidRequest, match="window-off-chain"):
            ops.ingest_history(str(repo), from_sha=shas[3], to_sha=side,
                               store_path=str(store))

    def test_a_window_at_the_root_has_nothing_to_write(self, tmp_path):
        repo, shas = _history(tmp_path / "repo")
        store = tmp_path / "at-root.db"
        ops.ingest_history(str(repo), store_path=str(store))
        with pytest.raises(ops.InvalidRequest, match="window-empty"):
            ops.ingest_history(str(repo), to_sha=shas[0],
                               store_path=str(store))
