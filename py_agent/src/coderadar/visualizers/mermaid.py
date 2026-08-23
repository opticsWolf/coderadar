"""CodeRadar v3.6 — Visualizers: Mermaid / Graphviz (§18)

Class hierarchy, module dependencies, and call graph visualization
with real data from the CodeGraph projection.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional

from . import NothingToVisualize


def generate_mermaid(viz_type: str, args: List[str],
                     graph: Optional[Any] = None) -> str:
    """Generate Mermaid diagram source for a visualization type.

    Args:
        viz_type: "hierarchy", "call-graph", or "dependencies"
        args: Positional arguments (e.g., entity IDs, filters)
        graph: Optional CodeGraph instance for real data

    Types:
    - hierarchy: Class inheritance DAG
    - call-graph: Function call graph (by entity ID or name)
    - dependencies: Module dependency graph
    """
    if viz_type == "hierarchy":
        return _mermaid_class_hierarchy(args, graph)
    elif viz_type == "call-graph":
        return _mermaid_call_graph(args, graph)
    elif viz_type == "dependencies":
        return _mermaid_dependency_graph(args, graph)
    raise NothingToVisualize(f"Unknown visualization type: {viz_type}")


def _safe_id(name: str) -> str:
    """A Mermaid-safe node id.

    Entity ids carry path separators — backslashes on Windows — which the
    chained `.replace` calls this used to do left untouched.
    """
    return "n_" + "".join(
        ch if (ch.isalnum() or ch == "_") else "_" for ch in name
    )


def _truncated_label(qualified: str, max_len: int = 40) -> str:
    """Create a short display label from a qualified name."""
    name = qualified.split("::")[-1] if "::" in qualified else qualified
    if len(name) > max_len:
        return name[:max_len-3] + "..."
    return name


# ── Class Hierarchy ──────────────────────────────────────────────────────

def _mermaid_class_hierarchy(args: List[str],
                              graph: Optional[Any] = None) -> str:
    """Render a class inheritance hierarchy as Mermaid classDiagram."""
    lines = ["classDiagram"]

    if graph:
        try:
            from coderadar._core import graph_stats
            stats = graph_stats()
            # Walk all classes and their subclasses
            visited = set()

            classes = list(_iter_classes(graph))
            by_name = {c.get("name", ""): cid for cid, c in classes}
            for cls_id, cls_data in classes:
                safe = _safe_id(cls_id)
                label = _truncated_label(cls_data.get("name", cls_id))
                lines.append(f"    class {safe} {{")
                lines.append(f"        +{label}")
                lines.append(f"    }}")
                visited.add(cls_id)

            # Inheritance comes from the resolved `bases` names; callees_of
            # returns call edges, which are not inheritance.
            for cls_id, cls_data in classes:
                for base in cls_data.get("bases") or []:
                    base_id = by_name.get(base)
                    if base_id and base_id != cls_id:
                        lines.append(
                            f"    {_safe_id(base_id)} <|-- {_safe_id(cls_id)}"
                        )

            if len(visited) > 0:
                return "\n".join(lines)
        except Exception as exc:
            raise NothingToVisualize(
                f"Could not read the class hierarchy: {exc}") from exc

    raise NothingToVisualize(
        "No classes in the index. Run `coderadar analyze` in this process "
        "first — the CLI does not yet load a stored graph."
    )


# ── Call Graph ──────────────────────────────────────────────────────────

def _mermaid_call_graph(args: List[str],
                         graph: Optional[Any] = None) -> str:
    """Render a function call graph as Mermaid flowchart.

    Uses real callers_of/callees_of data when graph is available.
    """
    lines = ["flowchart TD"]

    if graph and args:
        entity_id = args[0]
        depth_limit = int(args[1]) if len(args) > 1 else 3
        direction = args[2] if len(args) > 2 else "both"

        visited: set = set()
        edges: List[tuple] = []
        _gather_call_edges(graph, entity_id, depth_limit, direction, visited, edges)

        if edges:
            for src, dst, conf in edges:
                safe_src = _safe_id(src)
                safe_dst = _safe_id(dst)
                if conf < 0.8:
                    lines.append(f"    {safe_src} -.->|{conf:.0%}| {safe_dst}")
                else:
                    lines.append(f"    {safe_src} --> {safe_dst}")
            return "\n".join(lines)

    raise NothingToVisualize(
        "No call edges found for that entity. Either the index is empty "
        "(run `coderadar analyze` in this process first) or nothing indexed "
        "calls it and it calls nothing indexed."
    )


def _gather_call_edges(graph, entity_id: str, depth: int,
                        direction: str, visited: set,
                        edges: list) -> None:
    """Recursively gather call edges from the graph."""
    if depth <= 0 or entity_id in visited:
        return
    visited.add(entity_id)

    if direction in ("out", "both"):
        for callee in graph.callees_of(entity_id):
            edges.append((entity_id, callee.get("id", callee.get("name", "?")), 1.0))
            _gather_call_edges(graph, callee.get("id", ""), depth - 1,
                              direction, visited, edges)

    if direction in ("in", "both"):
        for caller in graph.callers_of(entity_id):
            edges.append((caller.get("id", caller.get("name", "?")), entity_id, 1.0))
            _gather_call_edges(graph, caller.get("id", ""), depth - 1,
                              direction, visited, edges)


# ── Dependency Graph ────────────────────────────────────────────────────

def _mermaid_dependency_graph(args: List[str],
                                graph: Optional[Any] = None) -> str:
    """Render module dependencies as Mermaid flowchart.

    Shows import relationships between modules.
    """
    lines = ["flowchart LR"]

    if graph:
        try:
            from coderadar._core import search_entities, callees_of
            # Get all modules
            modules = search_entities("module", 100)
            module_names = {}
            for m in modules:
                module_names[m.get("id", "")] = m.get("name", m.get("id", "?"))

            for mod_id in list(module_names.keys())[:50]:
                safe = _safe_id(mod_id)
                label = _truncated_label(module_names.get(mod_id, mod_id))
                lines.append(f"    {safe}[\"{label}\"]")

                # Show imports from this module
                callees = callees_of(mod_id)
                for callee in callees[:5]:
                    callee_id = callee.get("id", "")
                    if callee_id in module_names and callee_id != mod_id:
                        lines.append(
                            f"    {_safe_id(mod_id)} --> {_safe_id(callee_id)}"
                        )

            if len(module_names) > 0:
                return "\n".join(lines)
        except NothingToVisualize:
            raise
        except Exception as exc:
            raise NothingToVisualize(f"Could not read the graph: {exc}") from exc

    raise NothingToVisualize(
        "No module dependencies in the index. Run `coderadar analyze` in "
        "this process first — the CLI does not yet load a stored graph."
    )


def _iter_classes(graph):
    """Iterate over all class entities in the graph.

    This used to *text-search* for the word "class", which matches
    `from dataclasses import dataclass` and every docstring mentioning one,
    and misses any class whose name does not contain it. An empty query with
    a kind filter enumerates the real thing.
    """
    from coderadar._core import search_entities
    for cls in search_entities("", 500, "class"):
        yield cls.get("id", ""), cls
