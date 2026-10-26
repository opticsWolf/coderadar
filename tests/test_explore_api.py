"""CodeGraph.traverse() walks any edge kinds and reports real ids.

The legacy `explore(start_id=...)` call-graph walk was removed in 0.13:
it only followed call edges, reported `{entity_id, edge_kind, direction,
depth}` rows, and filtered `edge_kinds` against entity kinds. What remains
is the canonical `traverse(entity_id, direction, edge_kinds, max_depth)`
returning whole entity rows keyed `id` plus `depth` and `edge_type`.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

try:
    import coderadar
    from coderadar._core import analyze as _analyze
    from coderadar._core import search_entities
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
    """Bare names of the reached entities (traverse rows keyed `id`)."""
    return {
        (row.get("entity_id") or row["id"]).rsplit("::", 1)[-1]
        for row in rows
    }


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
            _id("top"), max_depth=3, edge_kinds=["imports"])

        assert _names(rows) == {"top"}, "imports followed a call edge"
