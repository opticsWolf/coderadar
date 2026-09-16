"""CodeRadar — visualizer cold-start regressions (v0.9.3).

Field report: query/traverse/call-graph reads were fine, but
`visualize dependencies` (Mermaid) rendered nothing and bare
`visualize call-graph` failed on projects without a `main` — both on a
graph cold-loaded from the store, exactly the CLI path.

  - Mermaid dependencies text-searched `search_entities("module")` for the
    word "module" instead of enumerating kind="module", and followed CALLS
    (`callees_of`) instead of IMPORTS — so any project without a module
    literally named *module* got "No module dependencies".
  - Bare `visualize call-graph` assumed a function named `main`; without
    one the failure read as an empty index rather than a missing argument.
  - Four "the CLI does not yet load a stored graph" messages were stale
    since the v0.8 cold-start work — the CLI demonstrably does.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))


def _write_proj(proj: Path) -> None:
    (proj / "db.py").write_text(
        '"""Database helpers."""\n'
        "\n\n"
        "def fetch_user(uid):\n"
        '    """Fetch a user row."""\n'
        "    return uid\n",
        encoding="utf-8",
    )
    (proj / "api.py").write_text(
        '"""HTTP layer."""\n'
        "from db import fetch_user\n"
        "\n\n"
        "def get_user(uid):\n"
        '    """GET /user."""\n'
        "    return fetch_user(uid)\n",
        encoding="utf-8",
    )


@pytest.fixture()
def cold_graph(tmp_path, monkeypatch):
    """Analyze a tiny two-module project, then cold-load it like the CLI."""
    import coderadar

    proj = tmp_path / "shop"
    proj.mkdir()
    _write_proj(proj)
    monkeypatch.chdir(proj)
    coderadar.analyze(".", create_store=True)
    db = proj / ".coderadar" / "store" / "coderadar.db"
    assert db.is_file(), "analyze with create_store must leave a store behind"
    coderadar.load(str(db), ".")
    return coderadar.CodeGraph()


class TestMermaidDependenciesCold:
    def test_lists_modules_that_are_not_named_module(self, cold_graph):
        from coderadar.visualizers.mermaid import generate_mermaid

        text = generate_mermaid("dependencies", [], cold_graph)
        assert "flowchart LR" in text
        assert "api" in text and "db" in text

    def test_draws_the_import_edge(self, cold_graph):
        from coderadar.visualizers.mermaid import generate_mermaid

        text = generate_mermaid("dependencies", [], cold_graph)
        assert "-->" in text, f"expected an api→db edge:\n{text}"

    def test_agrees_with_dot(self, cold_graph):
        from coderadar.visualizers.graphviz_viz import generate_dot
        from coderadar.visualizers.mermaid import generate_mermaid

        mermaid = generate_mermaid("dependencies", [], cold_graph)
        dot = generate_dot("dependencies", [], cold_graph)
        assert "api" in mermaid and "db" in mermaid
        assert "-->" in mermaid  # Mermaid arrow
        assert "->" in dot  # DOT arrow


class TestCallGraphNoArg:
    def test_names_the_missing_argument_not_an_empty_index(self, cold_graph):
        from coderadar.visualizers import NothingToVisualize
        from coderadar.visualizers.call_graph import generate_call_graph

        with pytest.raises(NothingToVisualize, match="pass a function"):
            generate_call_graph([], cold_graph)

    def test_explicit_id_still_renders_cold(self, cold_graph):
        from coderadar._core import search_entities
        from coderadar.visualizers.call_graph import generate_call_graph

        callee = next(e for e in search_entities("", 100, "function")
                      if e["name"] == "get_user")
        text = generate_call_graph([callee["id"]], cold_graph)
        assert "flowchart TD" in text
        assert "-->" in text

    def test_dot_format_agrees_on_no_arg(self, cold_graph):
        from coderadar.visualizers import NothingToVisualize
        from coderadar.visualizers.graphviz_viz import generate_dot

        with pytest.raises(NothingToVisualize, match="pass a function"):
            generate_dot("call-graph", [], cold_graph)


class TestStaleColdStartMessages:
    def test_no_renderer_claims_the_cli_cannot_load_a_store(self, cold_graph):
        from coderadar.visualizers import NothingToVisualize
        from coderadar.visualizers.graphviz_viz import generate_dot
        from coderadar.visualizers.mermaid import generate_mermaid

        for render in (lambda: generate_mermaid("hierarchy", [], cold_graph),
                       lambda: generate_dot("hierarchy", [], cold_graph)):
            with pytest.raises(NothingToVisualize) as exc:
                render()
            assert "does not yet load a stored graph" not in str(exc.value)
