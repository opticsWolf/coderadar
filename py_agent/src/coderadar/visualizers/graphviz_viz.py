"""CodeRadar v3.5 — Visualizers: Graphviz DOT Output

Module dependency graph with cycle highlighting (Kosaraju SCC),
class hierarchy with inheritance edges, wired to real CodeGraph data.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional, Set, Tuple


def generate_dot(viz_type: str, args: list,
                 graph: Optional[Any] = None) -> str:
    """Generate Graphviz DOT source for a visualization type.

    Args:
        viz_type: "dependencies" (module graph) or "hierarchy" (class tree)
        args: Additional arguments (ignored when graph is provided)
        graph: Optional CodeGraph instance for real data

    Types:
    - dependencies: Module dependency graph with SCC cycle clusters
    - hierarchy: Class inheritance styled as DOT
    """
    if viz_type == "dependencies":
        return _dot_dependency_graph(args, graph)
    elif viz_type == "hierarchy":
        return _dot_class_hierarchy(args, graph)
    else:
        return 'digraph G {\n    label="Unknown type";\n}'


# ── Helpers ──────────────────────────────────────────────────────────────

def _safe_id(name: str) -> str:
    """Convert entity name to safe DOT node ID."""
    return name.replace(".", "_").replace(":", "_").replace("::", "__").replace("/", "_").replace("-", "_")


def _short_name(qualified: str) -> str:
    """Extract the last segment of a qualified name for display."""
    return qualified.split("::")[-1] if "::" in qualified else qualified.split(".")[-1]


def _iter_modules(graph) -> List[Dict[str, Any]]:
    """Yield all module entities from the graph."""
    try:
        results = graph.search_entities("module", "", limit=500)
        return [r for r in results if r.get("entity_type") == "Module" or r.get("kind") == "module"]
    except Exception:
        return []


def _iter_classes(graph) -> List[Dict[str, Any]]:
    """Yield all class entities from the graph."""
    try:
        results = graph.search_entities("class", "", limit=500)
        return [r for r in results if r.get("entity_type") == "Class" or r.get("kind") == "class"]
    except Exception:
        return []


# ── Module Dependency Graph ──────────────────────────────────────────────

def _dot_dependency_graph(args: list, graph: Optional[Any] = None) -> str:
    """Module dependency graph with cycle highlighting via Kosaraju SCC.

    When a CodeGraph is provided, edges are extracted from module imports.
    Otherwise falls back to demo data.
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
        edges = [
            ("app.main", "app.services"),
            ("app.main", "app.models"),
            ("app.services", "app.models"),
            ("app.models", "app.services"),  # cycle!
            ("app.services", "lib.utils"),
        ]
        modules = set()
        for src, dst in edges:
            modules.add(src)
            modules.add(dst)

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
    """Extract module→module edges from the graph's import data.

    Uses callees_of on each module to find imported modules,
    then cross-references with the module entity list.
    """
    edges: List[Tuple[str, str]] = []
    modules = _iter_modules(graph)
    module_ids = {m.get("id", ""): m for m in modules}

    for mod in modules:
        mod_id = mod.get("id", "")
        if not mod_id:
            continue
        try:
            imports = mod.get("imports", [])
            if imports:
                for imp in imports:
                    if isinstance(imp, dict):
                        target = imp.get("module") or imp.get("path") or ""
                    else:
                        target = str(imp)
                    if target and target != mod_id:
                        edges.append((mod_id, target))
            else:
                # Fallback: use callees_of
                callees = graph.callees_of(mod_id)
                for c in callees:
                    c_id = c.get("id", "")
                    if c_id in module_ids:
                        edges.append((mod_id, c_id))
        except Exception:
            pass

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

        # Emit inheritance edges (parent → child, reversed for rankdir=BT)
        for cls in classes:
            cls_id = cls.get("id", "")
            parent = cls.get("parent_class") or cls.get("parent")
            if parent:
                parent_id = parent if isinstance(parent, str) else parent.get("id", "")
                if parent_id:
                    lines.append(
                        f"    {_safe_id(parent_id)} -> {_safe_id(cls_id)};"
                    )
            # Also try callees_of for the class
            try:
                callees = graph.callees_of(cls_id)
                for c in callees:
                    c_id = c.get("id", "")
                    if c_id in class_ids and c_id != cls_id:
                        lines.append(
                            f"    {_safe_id(c_id)} -> {_safe_id(cls_id)};"
                        )
            except Exception:
                pass
    else:
        # Demo fallback
        lines.append('    BaseModel [label="{BaseModel|+name: str}"];')
        lines.append('    UserService [label="{UserService|+create(): User}"];')
        lines.append("    BaseModel -> UserService;")

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
