"""CodeRadar v3.6 — Visualizers: Call Graph

Fan-out (callees_by_caller) and fan-in (callers_by_callee) visualization
for a single function, with real data from the CodeGraph.
"""

from __future__ import annotations

from typing import Any, List, Optional


def generate_call_graph(args: list, graph: Optional[Any] = None) -> str:
    """Generate a call graph visualization for a function.

    Args:
        args: [function_name_or_id, direction, max_depth, min_confidence]
            direction: "out" (fan-out) or "in" (fan-in), default "out"
            max_depth: default 5
            min_confidence: default 0.7
        graph: Optional CodeGraph for real data

    Returns:
        Mermaid flowchart source.
    """
    func_name = args[0] if args else "main"
    direction = args[1] if len(args) > 1 else "out"
    max_depth = int(args[2]) if len(args) > 2 else 5
    min_confidence = float(args[3]) if len(args) > 3 else 0.7

    lines = ["flowchart TD"]

    if graph and func_name:
        visited: set = set()
        edges: List[tuple] = []

        # Try to look up by name first, then by ID
        entity_id = func_name
        if "::" not in func_name:
            try:
                from coderadar._core import search_entities
                results = search_entities(func_name, 1)
                if results:
                    entity_id = results[0].get("id", func_name)
            except Exception:
                pass

        if direction == "out":
            _gather_fan_out(graph, entity_id, max_depth, min_confidence,
                           visited, edges)
        else:
            _gather_fan_in(graph, entity_id, max_depth, min_confidence,
                          visited, edges)

        if edges:
            for src, dst, conf in edges:
                safe_src = _safe_id(src)
                safe_dst = _safe_id(dst)
                if conf < min_confidence:
                    lines.append(
                        f"    {safe_src} -.->|{conf:.0%}| {safe_dst}"
                    )
                else:
                    lines.append(f"    {safe_src} --> {safe_dst}")
            return "\n".join(lines)

    from . import NothingToVisualize

    if graph is None:
        raise NothingToVisualize(
            "No graph was provided to the call-graph renderer.")
    raise NothingToVisualize(
        f"No call edges found {'from' if direction == 'out' else 'to'} "
        f"`{func_name}`. Either the index is empty (run `coderadar analyze` "
        f"in this process first — the CLI does not yet load a stored graph) "
        f"or that function neither calls nor is called by anything indexed."
    )


def _gather_fan_out(graph, entity_id: str, depth: int,
                     min_conf: float, visited: set,
                     edges: list) -> None:
    """Gather fan-out edges (callees)."""
    if depth <= 0 or entity_id in visited:
        return
    visited.add(entity_id)

    try:
        for callee in graph.callees_of(entity_id):
            callee_id = callee.get("id", "")
            if callee_id:
                edges.append((entity_id, callee_id, 1.0))
                _gather_fan_out(graph, callee_id, depth - 1,
                               min_conf, visited, edges)
    except Exception:
        pass


def _gather_fan_in(graph, entity_id: str, depth: int,
                    min_conf: float, visited: set,
                    edges: list) -> None:
    """Gather fan-in edges (callers)."""
    if depth <= 0 or entity_id in visited:
        return
    visited.add(entity_id)

    try:
        for caller in graph.callers_of(entity_id):
            caller_id = caller.get("id", "")
            if caller_id:
                edges.append((caller_id, entity_id, 1.0))
                _gather_fan_in(graph, caller_id, depth - 1,
                              min_conf, visited, edges)
    except Exception:
        pass


def _safe_id(name: str) -> str:
    """A Mermaid-safe node id (see `mermaid._safe_id`)."""
    from .mermaid import _safe_id as _shared
    return _shared(name)
