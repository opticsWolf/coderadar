"""codegraph_affected — Impact analysis tool (§26.2)

Transitive upstream traversal: "what calls this, all the way up?"
Returns a tree of dependent callers (blast radius).
"""

from __future__ import annotations

from collections import deque
from typing import Any, Dict, List, Set


async def affected(graph: Any, args: Dict[str, Any]) -> str:
    """Find all entities transitively affected by a change to the given entity.

    Args:
        graph: CodeGraph instance.
        args: {id: str, max_depth?: int}

    Returns:
        Tree of dependent callers.
    """
    entity_id = args.get("id", "")
    if not entity_id:
        return "Please provide an entity `id` to analyze."

    if graph is None:
        return "No index available. Run `coderadar init` first."

    max_depth = min(args.get("max_depth") or 5, 20)

    # Verify entity exists
    entity = _find_entity(graph, entity_id)
    if not entity:
        return (
            f"Entity `{entity_id}` not found. "
            "Try `codegraph_search` to locate it."
        )

    # BFS upstream traversal
    tree = _trace_affected(graph, entity_id, max_depth)

    return _render_tree(entity, tree, max_depth)


def _trace_affected(
    graph: Any, start_id: str, max_depth: int,
) -> Dict[int, List[Dict[str, Any]]]:
    """BFS upstream: depth → list of entities at that depth."""
    tree: Dict[int, List[Dict[str, Any]]] = {}
    visited: Set[str] = {start_id}
    queue: deque = deque([(start_id, 0)])

    while queue:
        current_id, depth = queue.popleft()
        if depth >= max_depth:
            continue

        callers = _get_callers(graph, current_id)
        for caller in callers:
            caller_id = caller.get("id", "")
            if caller_id and caller_id not in visited:
                visited.add(caller_id)
                tree.setdefault(depth + 1, []).append(caller)
                queue.append((caller_id, depth + 1))

    return tree


def _render_tree(
    entity: Dict[str, Any],
    tree: Dict[int, List[Dict[str, Any]]],
    max_depth: int,
) -> str:
    """Format the affected tree."""
    entity_name = entity.get("name", "?")
    entity_id = entity.get("id", "?")

    lines = [
        f"## Affected by `{entity_name}`",
        "",
        f"Transitive impact analysis for `{entity_id}` (max depth: {max_depth})",
        "",
    ]

    total = sum(len(v) for v in tree.values())
    if total == 0:
        lines.append("**No dependents found.** Nothing calls this entity.")
        return "\n".join(lines)

    lines.append(f"**Total dependents:** {total}")
    lines.append("")

    for depth in sorted(tree.keys()):
        entities = tree[depth]
        indent = "  " * depth
        lines.append(f"**Depth {depth}** ({len(entities)}):")
        for e in entities[:20]:
            name = e.get("name", "?")
            kind = e.get("kind", "?")
            eid = e.get("id", "?")
            lines.append(f"{indent}- `{name}` ({kind}) — `{eid}`")
        if len(entities) > 20:
            lines.append(f"{indent}  ... and {len(entities) - 20} more")
        lines.append("")

    if _max_depth_reached(tree, max_depth):
        lines.append(
            "⚠️ Max depth reached — there may be more dependents beyond "
            f"depth {max_depth}. Increase `max_depth` to see more."
        )

    return "\n".join(lines)


def _max_depth_reached(tree: Dict[int, List[Dict[str, Any]]], max_depth: int) -> bool:
    """Check if max depth was reached."""
    return any(d >= 20 for d in tree.keys()) or max(tree.keys(), default=0) >= max_depth


def _find_entity(graph: Any, entity_id: str):
    """Look up entity by ID."""
    try:
        from coderadar._core import lookup_entity
        return lookup_entity(entity_id)
    except ImportError:
        return None


def _get_callers(graph: Any, entity_id: str):
    """Get callers of an entity."""
    try:
        from coderadar._core import callers_of
        return callers_of(entity_id) or []
    except ImportError:
        return []
