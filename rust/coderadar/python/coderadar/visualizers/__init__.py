"""Diagram rendering, executed by the Rust binary."""

from .. import visualize as _visualize


class NothingToVisualize(Exception):
    """The graph has no rows for that diagram."""


def generate_mermaid(kind="hierarchy", args=None):
    """Render `kind` as Mermaid text."""
    return _visualize(kind, "mermaid", args or [])


def generate_dot(kind="dependencies", args=None):
    """Render `kind` as Graphviz DOT text."""
    return _visualize(kind, "graphviz", args or [])


def generate_call_graph(entity_id):
    """Render one entity's call graph as Mermaid."""
    return _visualize("call-graph", "mermaid", [entity_id])


__all__ = [
    "NothingToVisualize", "generate_call_graph", "generate_dot", "generate_mermaid",
]
