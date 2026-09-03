"""With no graph loaded, every backend answers with guidance.

These assertions used to be `isinstance(result, str)` and `len(result) > 0`,
spread across a dozen classes — true of literally any string the code could
return, including a real answer, because other tests in the same process
leave a graph indexed. Nothing about a no-index response was actually being
checked.

Forcing the no-graph state is what makes the assertion mean something: the
backend must not raise, must not answer, and must name the action that would
fix it.
"""

from __future__ import annotations

import pytest

from coderadar.mcp import lazy


@pytest.fixture(autouse=True)
def no_graph(monkeypatch):
    """The state before the first index, deterministically.

    `with_graph` raises PyRuntimeError when nothing is loaded — not
    ImportError, which is what the per-tool guards used to catch, and why
    these paths raised at the agent instead of returning the message written
    for exactly this case.
    """
    import coderadar._core as core

    def raise_no_graph():
        raise RuntimeError("No graph loaded — run coderadar init first")

    monkeypatch.setattr(core, "graph_stats", raise_no_graph)
    lazy.configure(None)
    yield
    lazy.configure(None)


def _backends():
    from coderadar.mcp import server as s

    return [
        ("explore", lambda: s._explore(None, "User", [], "both", 8)),
        ("node", lambda: s._node_detail(None, "test.py::func", False)),
        ("search", lambda: s._search(None, "test", None, 5)),
        ("affected", lambda: s._affected(None, "test.py::func", 3)),
        ("resolve", lambda: s._resolve_ref(None, "UserService", 5)),
        ("query", lambda: s._query_graph(None, "functions where name contains 'x'")),
        ("module_children", lambda: s._module_children(None, "test.py::module")),
        ("traverse", lambda: s._traverse(None, "test.py::func", "both", None, 3)),
        ("search_similar", lambda: s._search_similar(None, "auth logic", 5)),
        ("compute_embeddings", lambda: s._compute_embeddings(None)),
        ("as_of", lambda: s._as_of(None, "2025-01-01T00:00:00Z", "", [])),
        ("as_of_symbols", lambda: s._as_of(None, "2025-01-01T00:00:00Z", "", ["User"])),
        ("update_file", lambda: s._update_file(None, "test.py", "def foo(): pass")),
        ("replace_body",
         lambda: s._replace_body(None, "test.py::fn", "return 42", None, True)),
        ("update_signature",
         lambda: s._update_signature(None, "test.py::fn", "def fn(x, y):", False, True)),
        ("rename", lambda: s._rename(None, "test.py::fn", "new_fn", True)),
        ("create_entity", lambda: s._create_entity(
            None, "test.py", "python", "function", "new_fn", "return 1",
            None, "end", None, True)),
    ]


IDS = [name for name, _ in _backends()]


@pytest.mark.parametrize("index", range(len(IDS)), ids=IDS)
class TestEveryBackendWithoutAnIndex:
    def test_it_returns_a_message_instead_of_raising(self, index):
        name, call = _backends()[index]
        result = call()
        assert isinstance(result, str) and result.strip(), name

    def test_it_names_something_the_agent_can_do(self, index):
        name, call = _backends()[index]
        result = call()
        # A dead end is worse than an error: the agent has to be told which
        # of the two states it is in — no index here, or the wrong project.
        assert any(
            hint in result
            for hint in ("coderadar init", "codegraph_reindex", "--path")
        ), f"{name} gave no next step: {result!r}"

    def test_it_does_not_claim_to_have_answered(self, index):
        name, call = _backends()[index]
        result = call()
        assert "Mutation Applied" not in result, name
        assert "Reindex Complete" not in result, name
