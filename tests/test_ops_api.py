"""The shared ops layer, reached through the CodeGraph API.

MCP tools, CLI commands and CodeGraph methods all answer from
`coderadar.ops`; these tests pin the API side: the methods exist, return
data (not rendered text), and fail with typed `ops` errors that callers
can branch on.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

try:
    import coderadar
    from coderadar import ops
    from coderadar._core import analyze as _analyze
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


SOURCE = '''\
TIMEOUT = 30


class Store:
    def load(self):
        return helper()


def helper():
    return 1


def entry():
    return Store().load()
'''


@pytest.fixture
def graph(tmp_path):
    (tmp_path / "app.py").write_text(SOURCE, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield coderadar.CodeGraph()
    finally:
        os.chdir(previous)


def _id(graph, name: str) -> str:
    for hit in graph.search(name):
        if hit.get("name") == name:
            return hit["id"]
    raise AssertionError(f"{name} is not in the index")


def test_search_finds_and_narrows_by_kind(graph):
    assert any(h.get("name") == "helper" for h in graph.search("helper"))
    assert all(h.get("kind") == "class" or h.get("entity_type") == "class"
               for h in graph.search("Store", kind="class"))


@pytest.mark.parametrize("query, kind", [("   ", None), ("helper", "bogus")])
def test_search_rejects_bad_requests(graph, query, kind):
    with pytest.raises(ops.InvalidRequest):
        graph.search(query, kind=kind)


def test_node_with_neighbors(graph):
    helper = _id(graph, "helper")
    plain = graph.node(helper)
    assert plain["id"] == helper and "callers" not in plain
    full = graph.node(helper, include_neighbors=True)
    assert any(c["id"].endswith("Store.load") for c in full["callers"])


def test_node_miss_offers_candidates(graph):
    with pytest.raises(ops.NotFound) as info:
        graph.node(_id(graph, "helper") + "x")
    assert any(c.get("name") == "helper" for c in info.value.candidates)


def test_affected_groups_callers_by_depth(graph):
    result = graph.affected(_id(graph, "helper"), max_depth=5)
    names = {d: {e["id"].rsplit("::", 1)[-1] for e in es}
             for d, es in result["depths"].items()}
    assert "Store.load" in names[1]
    assert "entry" in names[2]


def test_module_children(graph):
    module = _id(graph, "helper").split("::")[0] + "::module"
    result = graph.module_children(module)
    assert {c["name"] for c in result["classes"]} == {"Store"}
    assert {"helper", "entry"} <= {f["name"] for f in result["functions"]}
    with pytest.raises(ops.InvalidRequest):
        graph.module_children("  ")
    with pytest.raises(ops.NotFound):
        graph.module_children("nope.py::module")


def test_resolve_returns_a_mode(graph):
    assert graph.resolve("Store")["mode"] in ("route", "reference")
    assert graph.resolve("/users/:id")["mode"] == "route"


def test_analyses_return_lists(graph):
    assert isinstance(graph.dead_code(min_confidence=0.0), list)
    assert isinstance(graph.get_smells(), list)
    assert isinstance(graph.find_clones(min_lines=2), list)
    assert isinstance(graph.find_scaffolding(), list)


def test_bad_strictness_is_an_invalid_request(graph):
    with pytest.raises(ops.InvalidRequest):
        graph.get_smells(strictness="bogus")
