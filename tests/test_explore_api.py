"""CodeGraph.explore() walks the call graph and reports real ids.

This existed to be found: `explore()` read `e["target"]` and `e["source"]`
off rows that `callees_of`/`callers_of` never produced — those return
*entities*, keyed `id`/`name`/`kind`. So the method raised KeyError for any
entity with edges, and looked fine only for entities with none. It also
advertised `max_depth` while hardcoding one hop, and filtered `edge_kinds`
against the entity kind ("function", "method"), so asking for "calls"
returned nothing at all.

Every assertion below is one of those four.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

try:
    import coderadar
    from coderadar._core import analyze as _analyze, search_entities
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


SOURCE = '''\
def leaf():
    return 1


def middle():
    return leaf()


def top():
    return middle()
'''


@pytest.fixture
def project(tmp_path):
    (tmp_path / "chain.py").write_text(SOURCE, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


def _id(name: str) -> str:
    for hit in search_entities(name, 50):
        if hit.get("name") == name:
            return hit["id"]
    raise AssertionError(f"{name} is not in the index")


def _names(rows):
    """Bare names of the reached entities.

    explore() reports `entity_id`; traverse() returns whole entity rows
    keyed `id`. Both are handled so the two walks can be asserted alike.
    """
    return {
        (row.get("entity_id") or row["id"]).rsplit("::", 1)[-1]
        for row in rows
    }


class TestExplore:
    def test_it_returns_rows_instead_of_raising(self, project):
        rows = coderadar.CodeGraph().explore(_id("middle"), direction="both")

        assert rows, "an entity with a caller and a callee explored to nothing"
        for row in rows:
            assert set(row) == {"entity_id", "edge_kind", "direction", "depth"}

    def test_it_walks_out_to_the_requested_depth(self, project):
        graph = coderadar.CodeGraph()

        one = graph.explore(_id("top"), direction="out", max_depth=1)
        two = graph.explore(_id("top"), direction="out", max_depth=2)

        assert _names(one) == {"middle"}
        assert _names(two) == {"middle", "leaf"}
        assert {r["depth"] for r in two} == {1, 2}

    def test_it_walks_in_as_well(self, project):
        rows = coderadar.CodeGraph().explore(
            _id("leaf"), direction="in", max_depth=2)

        assert _names(rows) == {"middle", "top"}
        assert all(r["direction"] == "in" for r in rows)

    def test_the_start_is_never_reported_as_its_own_neighbour(self, project):
        start = _id("middle")
        rows = coderadar.CodeGraph().explore(start, direction="both", max_depth=3)

        assert start not in {r["entity_id"] for r in rows}

    def test_asking_for_calls_does_not_come_back_empty(self, project):
        rows = coderadar.CodeGraph().explore(
            _id("top"), direction="out", max_depth=1, edge_kinds=["calls"])

        assert _names(rows) == {"middle"}
        assert all(r["edge_kind"] == "calls" for r in rows)

    def test_asking_for_a_kind_this_walk_cannot_follow_says_so(self, project):
        # Better an honest empty than silently pretending imports were walked.
        rows = coderadar.CodeGraph().explore(
            _id("top"), direction="out", edge_kinds=["imports"])

        assert rows == []

    def test_zero_depth_goes_nowhere(self, project):
        assert coderadar.CodeGraph().explore(_id("top"), max_depth=0) == []


class TestTraverseDefaults:
    """`edge_types=None` is documented as "all kinds" at every layer.

    It reached the BFS as an empty list, which loops over nothing, so the
    default traversal returned the start node and stopped — the same silence
    as an entity with no edges.
    """

    def test_the_default_walk_follows_calls(self, project):
        rows = coderadar.CodeGraph().traverse(_id("top"), max_depth=3)

        assert _names(rows) >= {"top", "middle", "leaf"}

    def test_naming_a_kind_still_narrows_the_walk(self, project):
        rows = coderadar.CodeGraph().traverse(
            _id("top"), max_depth=3, edge_types=["imports"])

        assert _names(rows) == {"top"}, "imports followed a call edge"
