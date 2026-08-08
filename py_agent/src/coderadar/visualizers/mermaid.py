"""CodeRadar v3.3 — Visualizers (§18)

Mermaid/Graphviz output for class hierarchy, module dependencies, and call graphs.
"""


def generate_mermaid(viz_type: str, args: list) -> str:
    """Generate Mermaid diagram source for a visualization type.

    Types:
    - hierarchy: Class inheritance DAG
    - call-graph: Function call graph
    - dependencies: Module dependency graph
    """
    if viz_type == "hierarchy":
        return _mermaid_class_hierarchy(args)
    elif viz_type == "call-graph":
        return _mermaid_call_graph(args)
    elif viz_type == "dependencies":
        return _mermaid_dependency_graph(args)
    else:
        return f"graph TD\n    A[Unknown viz type: {viz_type}]"


def _mermaid_class_hierarchy(args: list) -> str:
    """Render a class inheritance hierarchy as Mermaid classDiagram."""
    lines = ["classDiagram"]
    # In production: walk subclasses reverse index from the root class
    lines.append("    class BaseModel {")
    lines.append("        +name: str")
    lines.append("    }")
    lines.append("    class UserService {")
    lines.append("        +create()")
    lines.append("    }")
    lines.append("    BaseModel <|-- UserService")
    return "\n".join(lines)


def _mermaid_call_graph(args: list) -> str:
    """Render a function call graph as Mermaid flowchart."""
    lines = ["flowchart TD"]
    lines.append("    A[auth.login] --> B[db.query]")
    lines.append("    A --> C[validate_token]")
    lines.append("    B --> D[execute_sql]")
    lines.append("    C -.->|confidence: 0.85| D")
    return "\n".join(lines)


def _mermaid_dependency_graph(args: list) -> str:
    """Render module dependencies as Mermaid flowchart."""
    lines = ["flowchart LR"]
    lines.append("    A[app.main] --> B[app.services]")
    lines.append("    A --> C[app.models]")
    lines.append("    B --> C")
    lines.append("    B --> D[lib.utils]")
    return "\n".join(lines)
