"""CodeRadar v3.6 — Visualizers: Graphviz DOT Output

Module dependency graph with cycle highlighting (Kosaraju SCC),
class hierarchy with inheritance edges, wired to real CodeGraph data.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional, Set, Tuple

from . import NothingToVisualize


def generate_dot(viz_type: str, args: list,
                 graph: Optional[Any] = None) -> str:
    """Generate Graphviz DOT source for a visualization type.

    Args:
        viz_type: "dependencies", "hierarchy", or "call-graph"
        args: Additional arguments (ignored when graph is provided)
        graph: Optional CodeGraph instance for real data

    Types:
    - dependencies: Module dependency graph with SCC cycle clusters
    - hierarchy: Class inheritance styled as DOT
    - call-graph: Fan-out/fan-in around one function
    """
    if viz_type == "dependencies":
        return _dot_dependency_graph(args, graph)
    elif viz_type == "hierarchy":
        return _dot_class_hierarchy(args, graph)
    elif viz_type == "call-graph":
        return _dot_call_graph(args, graph)
    raise NothingToVisualize(f"Unknown visualization type: {viz_type}")


# ── Helpers ──────────────────────────────────────────────────────────────

def _safe_id(name: str) -> str:
    """A DOT-safe node id.

    Backslashes are escape characters in DOT, so an unescaped Windows path
    in an entity id produced a file that Graphviz could not parse.
    """
    out = []
    for ch in name:
        out.append(ch if (ch.isalnum() or ch == "_") else "_")
    return "n_" + "".join(out)


def _short_name(qualified: str) -> str:
    """A readable label for an entity id.

    Module ids all end `::module`, so taking the last segment labelled every
    module node "module". Fall back to the file stem in that case.
    """
    if "::" not in qualified:
        return qualified.split(".")[-1]
    head, _, tail = qualified.rpartition("::")
    if tail == "module":
        stem = head.replace(chr(92), "/").rsplit("/", 1)[-1]
        return stem.rsplit(".", 1)[0] or stem
    return tail


def _entities_of_kind(kind: str, limit: int = 500) -> List[Dict[str, Any]]:
    """Every indexed entity of one kind.

    Both callers used to ask `graph.search_entities(...)` — a method
    `CodeGraph` does not have — and swallow the AttributeError, so they
    always returned an empty list and every DOT diagram fell through to the
    demo data. An empty query with a kind filter is the enumeration the core
    actually offers.
    """
    from coderadar._core import search_entities
    return list(search_entities("", limit, kind))


def _iter_modules(graph) -> List[Dict[str, Any]]:
    """Yield all module entities from the graph."""
    return _entities_of_kind("module")


def _iter_classes(graph) -> List[Dict[str, Any]]:
    """Yield all class entities from the graph."""
    return _entities_of_kind("class")


# ── Module Dependency Graph ──────────────────────────────────────────────

def _dot_dependency_graph(args: list, graph: Optional[Any] = None) -> str:
    """Module dependency graph with cycle highlighting via Kosaraju SCC.

    Edges are extracted from module imports. An empty graph is an error,
    not an occasion for demo data.
    """
    lines = [
        "digraph Dependencies {",
        '    rankdir=LR;',
        '    node [shape=box, style=rounded, fontname="Helvetica"];',
        '    label="Module Dependencies";',
    ]

    if graph:
        edges = _extract_module_edges(graph)
        modules = set()
        for src, dst in edges:
            modules.add(src)
            modules.add(dst)
    else:
        raise NothingToVisualize(
            "No graph was provided to the dependency renderer.")

    if not edges:
        raise NothingToVisualize(
            "No module dependencies in the index. Run `coderadar analyze` "
            "in this process first — the CLI does not yet load a stored "
            "graph."
        )

    # Find SCCs and highlight cycles
    sccs = _find_sccs(edges)
    cluster_idx = 0
    nodes_in_cycles: Set[str] = set()

    for scc in sccs:
        if len(scc) > 1:
            cluster_idx += 1
            nodes_in_cycles.update(scc)
            lines.append(f"    subgraph cluster_{cluster_idx} {{")
            lines.append(f'        label="SCC (cycle)";')
            lines.append('        style=filled;')
            lines.append('        color=lightcoral;')
            lines.append('        fontcolor=darkred;')
            for node in sorted(scc):
                lines.append(f"        {_safe_id(node)};")
            lines.append("    }")

    # Emit standalone nodes with tooltips
    for module in sorted(modules):
        safe = _safe_id(module)
        label = _short_name(module)
        if module not in nodes_in_cycles:
            lines.append(f'    {safe} [label="{label}", tooltip="{module}"];')

    # Emit edges
    for src, dst in edges:
        lines.append(f"    {_safe_id(src)} -> {_safe_id(dst)};")

    lines.append("}")
    return "\n".join(lines)


def _extract_module_edges(graph) -> List[Tuple[str, str]]:
    """Resolved module→module import edges.

    The `imports` list on a module entity holds import-statement *entity
    ids* (`...::import@3`), whose only payload is the raw source line — so
    using them as edge targets drew one node per import statement and no
    dependency at all. The resolved edges live in the core's importer
    index, which a depth-1 `imports` traversal reads.
    """
    from coderadar._core import traverse as _traverse

    edges: List[Tuple[str, str]] = []
    module_ids = {m.get("id", "") for m in _iter_modules(graph)}

    for mod_id in sorted(module_ids):
        if not mod_id:
            continue
        for row in _traverse(mod_id, 1, ["imports"], "out", None):
            target = row.get("id", "")
            if target and target != mod_id and target in module_ids:
                edges.append((mod_id, target))
    return edges


# ── Class Hierarchy ──────────────────────────────────────────────────────

def _dot_class_hierarchy(args: list, graph: Optional[Any] = None) -> str:
    """Class hierarchy as Graphviz DOT with real inheritance edges.

    When a CodeGraph is provided, walks class entities and their
    parent classes to build the inheritance DAG.
    """
    lines = [
        "digraph Hierarchy {",
        '    rankdir=BT;',
        '    node [shape=record, fontname="Helvetica", style=rounded];',
        '    edge [arrowhead=empty];',
    ]

    if graph:
        classes = _iter_classes(graph)
        class_ids = {c.get("id", ""): c for c in classes}

        # Emit class nodes
        for cls in classes:
            cls_id = cls.get("id", "")
            name = cls.get("name", _short_name(cls_id))
            methods = cls.get("methods") or cls.get("function_ids") or []
            method_str = "|".join(
                [f"+ {m if isinstance(m, str) else m.get('name', '?')}()"
                 for m in methods[:6]]
            )
            label = f"{{{name}|{method_str}}}" if method_str else f"{{{name}}}"
            lines.append(f'    {_safe_id(cls_id)} [label="{label}"];')

        # Emit inheritance edges (parent → child, reversed for rankdir=BT).
        # The class dict exposes resolved base *names* under `bases`; the
        # keys this read before (`parent_class`, `parent`) are not on it, so
        # no hierarchy edge was ever drawn.
        by_name = {c.get("name", ""): c.get("id", "") for c in classes}
        for cls in classes:
            cls_id = cls.get("id", "")
            for base in cls.get("bases") or []:
                base_id = by_name.get(base)
                if base_id and base_id != cls_id:
                    lines.append(
                        f"    {_safe_id(base_id)} -> {_safe_id(cls_id)};"
                    )
    else:
        raise NothingToVisualize(
            "No graph was provided to the hierarchy renderer.")

    if len(lines) == 4:
        raise NothingToVisualize(
            "No classes in the index. Run `coderadar analyze` in this "
            "process first — the CLI does not yet load a stored graph."
        )

    lines.append("}")
    return "\n".join(lines)


# ── Kosaraju SCC ─────────────────────────────────────────────────────────

def _find_sccs(edges: List[Tuple[str, str]]) -> List[Set[str]]:
    """Find strongly-connected components using Kosaraju's algorithm."""
    # Build adjacency list
    adj: Dict[str, Set[str]] = {}
    for src, dst in edges:
        adj.setdefault(src, set()).add(dst)
        adj.setdefault(dst, set())

    # First pass: compute finish order
    visited: Set[str] = set()
    order: List[str] = []

    def dfs1(node: str) -> None:
        stack = [(node, False)]
        while stack:
            n, done = stack.pop()
            if done:
                order.append(n)
                continue
            if n in visited:
                continue
            visited.add(n)
            stack.append((n, True))
            for neighbor in sorted(adj.get(n, set()), reverse=True):
                if neighbor not in visited:
                    stack.append((neighbor, False))

    for node in sorted(adj):
        if node not in visited:
            dfs1(node)

    # Reverse graph
    rev_adj: Dict[str, Set[str]] = {}
    for src, dst in edges:
        rev_adj.setdefault(dst, set()).add(src)
        rev_adj.setdefault(src, set())

    visited.clear()
    components: List[Set[str]] = []

    def dfs2(start: str) -> Set[str]:
        comp: Set[str] = set()
        stack = [start]
        while stack:
            n = stack.pop()
            if n in visited:
                continue
            visited.add(n)
            comp.add(n)
            for neighbor in rev_adj.get(n, set()):
                if neighbor not in visited:
                    stack.append(neighbor)
        return comp

    for node in reversed(order):
        if node not in visited:
            components.append(dfs2(node))

    return components


# ── Call Graph ───────────────────────────────────────────────────────────

def _dot_call_graph(args: list, graph: Optional[Any] = None) -> str:
    """Fan-out (or fan-in) around one function, as DOT.

    The Mermaid renderer in `call_graph.py` already walks the graph; this
    reuses that walk so the two formats cannot disagree about the edges,
    and only the rendering differs.
    """
    from .call_graph import _gather_fan_in, _gather_fan_out

    if graph is None:
        raise NothingToVisualize(
            "No graph was provided to the call-graph renderer.")

    func_name = args[0] if args else ""
    direction = args[1] if len(args) > 1 else "out"
    max_depth = int(args[2]) if len(args) > 2 else 5

    if not func_name:
        raise NothingToVisualize(
            "call-graph needs a function to start from, e.g. "
            "`coderadar visualize call-graph src/app.py::main`."
        )

    entity_id = func_name
    if "::" not in func_name:
        from coderadar._core import search_entities
        hits = search_entities(func_name, 1)
        if hits:
            entity_id = hits[0].get("id", func_name)

    visited: Set[str] = set()
    edges: List[Tuple[str, str, float]] = []
    if direction == "out":
        _gather_fan_out(graph, entity_id, max_depth, 0.0, visited, edges)
    else:
        _gather_fan_in(graph, entity_id, max_depth, 0.0, visited, edges)

    if not edges:
        raise NothingToVisualize(
            f"No call edges found {'from' if direction == 'out' else 'to'} "
            f"`{func_name}`. Either the index is empty (run `coderadar "
            f"analyze` in this process first) or that function neither "
            f"calls nor is called by anything indexed."
        )

    lines = [
        "digraph CallGraph {",
        '    rankdir=LR;',
        '    node [shape=box, fontname="Helvetica", style=rounded];',
    ]
    for node in sorted({e for src, dst, _ in edges for e in (src, dst)}):
        lines.append(f'    {_safe_id(node)} [label="{_short_name(node)}"];')
    for src, dst, _ in edges:
        lines.append(f"    {_safe_id(src)} -> {_safe_id(dst)};")
    lines.append("}")
    return "\n".join(lines)
