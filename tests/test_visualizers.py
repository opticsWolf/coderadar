"""A visualizer with no data must say so, not draw something plausible.

Every renderer used to answer an empty or unreadable graph with a hardcoded
example — `BaseModel <|-- UserService`, `auth.login --> db.query`,
`app.main --> app.services` — and return it as a normal result. Pointed at a
real codebase with nothing indexed, `coderadar visualize` wrote a confident
diagram of a project that does not exist and exited 0.

Nothing in the suite covered the visualizers, which is why six fabrication
sites survived a release that was otherwise about honest silence.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from coderadar.visualizers import (
    NothingToVisualize,
    generate_call_graph,
    generate_dot,
    generate_mermaid,
)

try:
    import coderadar
    from coderadar._core import analyze as _analyze, search_entities
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False


INVENTED = (
    "BaseModel", "UserService", "auth.login", "db.query", "validate_token",
    "execute_sql", "app.main", "app.services", "lib.utils", "validate_input",
    "db_query", "api_handler", "cron_job",
)


class TestWithoutAGraph:
    """`graph=None` is the shape the fabrications hid behind."""

    @pytest.mark.parametrize("viz", ["hierarchy", "dependencies", "call-graph"])
    def test_mermaid_refuses_to_invent(self, viz):
        with pytest.raises(NothingToVisualize):
            generate_mermaid(viz, [], None)

    @pytest.mark.parametrize("viz", ["hierarchy", "dependencies", "call-graph"])
    def test_dot_refuses_to_invent(self, viz):
        with pytest.raises(NothingToVisualize):
            generate_dot(viz, [], None)

    def test_the_call_graph_helper_refuses_too(self):
        with pytest.raises(NothingToVisualize):
            generate_call_graph(["main"], None)

    def test_an_unknown_type_is_an_error_not_a_diagram(self):
        with pytest.raises(NothingToVisualize):
            generate_dot("not-a-real-type", [], None)


SOURCE = '''\
def helper():
    return 1


def entry():
    return helper()
'''


@pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")
class TestWithARealProject:
    @pytest.fixture
    def project(self, tmp_path):
        (tmp_path / "app.py").write_text(SOURCE, encoding="utf-8")
        previous = Path(os.getcwd())
        os.chdir(tmp_path)
        try:
            _analyze(".")
            yield tmp_path
        finally:
            os.chdir(previous)

    def _entry(self):
        for hit in search_entities("entry", 50):
            if hit.get("name") == "entry":
                return hit["id"]
        raise AssertionError("entry is not in the index")

    def test_the_dot_call_graph_draws_the_real_edge(self, project):
        out = generate_dot("call-graph", [self._entry(), "out", 3],
                           coderadar.CodeGraph())

        assert out.startswith("digraph CallGraph {")
        assert "entry" in out and "helper" in out
        for invented in INVENTED:
            assert invented not in out

    def test_the_mermaid_call_graph_draws_the_same_edge(self, project):
        out = generate_call_graph([self._entry(), "out", 3],
                                  coderadar.CodeGraph())

        assert out.startswith("flowchart TD")
        assert "entry" in out and "helper" in out
        for invented in INVENTED:
            assert invented not in out

    def test_an_entity_with_no_edges_is_reported_not_padded(self, project):
        with pytest.raises(NothingToVisualize):
            generate_dot("call-graph", ["helper", "out", 3],
                         coderadar.CodeGraph())
