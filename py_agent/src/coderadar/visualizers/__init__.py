"""CodeRadar v3.6 — Visualizers Package (§18)

Mermaid/Graphviz output for class hierarchy, module dependencies, and call graphs.
"""


class NothingToVisualize(RuntimeError):
    """The renderer had no graph data to draw.

    Every renderer used to answer this case with a hardcoded example —
    `BaseModel <|-- UserService`, `auth.login --> db.query` — and exit 0.
    Pointed at a real codebase with no index loaded, `coderadar visualize`
    wrote a confident diagram of a project that does not exist. A drawing
    that cannot be told apart from analysis is the one output worse than
    an error.
    """


from .call_graph import generate_call_graph
from .graphviz_viz import generate_dot
from .mermaid import generate_mermaid

__all__ = [
    "NothingToVisualize",
    "generate_call_graph",
    "generate_dot",
    "generate_mermaid",
]
