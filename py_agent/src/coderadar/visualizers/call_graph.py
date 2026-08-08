"""CodeRadar v3.3 — Visualizers: Call Graph (§18)

Fan-out (callees_by_caller) and fan-in (callers_by_callee) visualization
for a single function, with confidence-based edge styling.
"""

from __future__ import annotations

from typing import List, Optional, Tuple


def generate_call_graph(args: list) -> str:
    """Generate a call graph visualization for a function.

    Args:
        args: [function_name, direction, max_depth, min_confidence]
            direction: "out" (fan-out) or "in" (fan-in)
            max_depth: default 5
            min_confidence: default 0.7

    Returns:
        Mermaid flowchart source.
    """
    func_name = args[0] if args else "main"
    direction = args[1] if len(args) > 1 else "out"
    max_depth = int(args[2]) if len(args) > 2 else 5
    min_confidence = float(args[3]) if len(args) > 3 else 0.7

    lines = ["flowchart TD"]

    if direction == "out":
        # Fan-out: what does this function call?
        lines.append(f"    {_safe_id(func_name)}[\"{func_name}\"]")
        lines.extend(_build_fan_out_edges(func_name, max_depth, min_confidence))
    else:
        # Fan-in: who calls this function?
        lines.append(f"    {_safe_id(func_name)}[\"{func_name}\"]")
        lines.extend(_build_fan_in_edges(func_name, max_depth, min_confidence))

    return "\n".join(lines)


def _build_fan_out_edges(root: str, max_depth: int,
                          min_confidence: float) -> List[str]:
    """Build fan-out edges from the root function."""
    # In production: walk callees_by_caller reverse index
    # For now, return stub edges
    edges = [
        (root, "validate_input", 0.95),
        (root, "db.query", 0.92),
        (root, "format_response", 0.88),
        ("db.query", "execute_sql", 0.97),
        ("validate_input", "sanitize", 0.94),
    ]

    lines = []
    for src, dst, conf in edges:
        safe_src = _safe_id(src)
        safe_dst = _safe_id(dst)
        if conf < min_confidence:
            lines.append(
                f"    {safe_src} -.->|confidence: {conf:.2f}| {safe_dst}"
            )
        else:
            lines.append(f"    {safe_src} --> {safe_dst}")
    return lines


def _build_fan_in_edges(root: str, max_depth: int,
                         min_confidence: float) -> List[str]:
    """Build fan-in edges showing callers of the root function."""
    edges = [
        ("api.handler", root, 0.96),
        ("cron.job", root, 0.91),
        ("cli.main", root, 0.85),
        ("api.handler", "parse_request", 0.93),
    ]

    lines = []
    for src, dst, conf in edges:
        safe_src = _safe_id(src)
        safe_dst = _safe_id(dst)
        if conf < min_confidence:
            lines.append(
                f"    {safe_src} -.->|confidence: {conf:.2f}| {safe_dst}"
            )
        else:
            lines.append(f"    {safe_src} --> {safe_dst}")
    return lines


def _safe_id(name: str) -> str:
    """Convert a function name to a safe Mermaid node ID."""
    return name.replace(".", "_").replace("::", "__")
