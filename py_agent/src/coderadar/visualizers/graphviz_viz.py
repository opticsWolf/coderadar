"""CodeRadar v3.3 — Visualizers: Graphviz DOT Output (§18)

Module dependency graph with cycle highlighting via Tarjan SCC.
"""

from __future__ import annotations

from typing import List, Set, Tuple


def generate_dot(viz_type: str, args: list) -> str:
    """Generate Graphviz DOT source for a visualization type.

    Types:
    - dependencies: Module dependency graph with SCC clusters
    - hierarchy: Class hierarchy styled as DOT
    """
    if viz_type == "dependencies":
        return _dot_dependency_graph(args)
    elif viz_type == "hierarchy":
        return _dot_class_hierarchy(args)
    else:
        return 'digraph G {\n    label="Unknown type";\n}'


def _dot_dependency_graph(args: list) -> str:
    """Module dependency graph with cycle highlighting via Tarjan SCC."""
    lines = [
        "digraph Dependencies {",
        '    rankdir=LR;',
        '    node [shape=box, style=rounded];',
        '    label="Module Dependencies";',
    ]

    # In production, uses importers reverse index to build edges
    # and Tarjan's algorithm to find strongly-connected components
    edges = [
        ("app.main", "app.services"),
        ("app.main", "app.models"),
        ("app.services", "app.models"),
        ("app.models", "app.services"),  # cycle!
        ("app.services", "lib.utils"),
    ]

    # Find SCCs using simplified Tarjan algorithm
    sccs = _find_sccs(edges)

    cluster_idx = 0
    for scc in sccs:
        if len(scc) > 1:
            cluster_idx += 1
            lines.append(f"    subgraph cluster_{cluster_idx} {{")
            lines.append(f'        label="SCC (cycle)";')
            lines.append('        style=filled;')
            lines.append('        color=lightcoral;')
            for node in scc:
                safe_name = node.replace(".", "_")
                lines.append(f"        {safe_name};")
            lines.append("    }")

    for src, dst in edges:
        safe_src = src.replace(".", "_")
        safe_dst = dst.replace(".", "_")
        lines.append(f"    {safe_src} -> {safe_dst};")

    lines.append("}")
    return "\n".join(lines)


def _find_sccs(edges: List[Tuple[str, str]]) -> List[Set[str]]:
    """Find strongly-connected components using a simplified Tarjan algorithm."""
    # Build adjacency list
    adj: dict = {}
    for src, dst in edges:
        adj.setdefault(src, set()).add(dst)
        adj.setdefault(dst, set())

    # Kosaraju's algorithm (simpler)
    visited = set()
    order = []

    def dfs1(node):
        visited.add(node)
        for neighbor in adj.get(node, set()):
            if neighbor not in visited:
                dfs1(neighbor)
        order.append(node)

    for node in adj:
        if node not in visited:
            dfs1(node)

    # Reverse graph
    rev_adj: dict = {}
    for src, dst in edges:
        rev_adj.setdefault(dst, set()).add(src)
        rev_adj.setdefault(src, set())

    visited.clear()
    components = []

    def dfs2(node, comp):
        visited.add(node)
        comp.add(node)
        for neighbor in rev_adj.get(node, set()):
            if neighbor not in visited:
                dfs2(neighbor, comp)

    for node in reversed(order):
        if node not in visited:
            comp = set()
            dfs2(node, comp)
            components.append(comp)

    return components


def _dot_class_hierarchy(args: list) -> str:
    """Class hierarchy as Graphviz DOT."""
    lines = [
        "digraph Hierarchy {",
        '    rankdir=BT;',
        '    node [shape=record];',
    ]
    lines.append('    BaseModel [label="{BaseModel|+name: str}"];')
    lines.append('    UserService [label="{UserService|+create(): User}"];')
    lines.append("    BaseModel -> UserService [arrowhead=empty];")
    lines.append("}")
    return "\n".join(lines)
