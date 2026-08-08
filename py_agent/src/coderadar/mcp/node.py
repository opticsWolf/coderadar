"""codegraph_node — Depth drill-down tool (§26.2)

Returns full entity details with optional neighbor expansion. Called after
codegraph_explore identifies a specific entity of interest.
"""

from __future__ import annotations

from typing import Any, Dict


async def node_detail(graph: Any, args: Dict[str, Any]) -> str:
    """Get full details for an entity.

    Args:
        graph: CodeGraph instance.
        args: {id: str, include_neighbors?: bool}

    Returns:
        Formatted entity detail string.
    """
    entity_id = args.get("id", "")
    if not entity_id:
        return "Please provide an entity `id` to look up."

    if graph is None:
        return "No index available. Run `coderadar init` first."

    entity = _find_entity(graph, entity_id)
    if not entity:
        return (
            f"Entity `{entity_id}` not found in the index. "
            "It may have been renamed or removed. Try `codegraph_search` to find it."
        )

    lines = [_render_entity_detail(entity)]

    include_neighbors = args.get("include_neighbors", False)
    if include_neighbors:
        callers = _get_callers(graph, entity_id)
        callees = _get_callees(graph, entity_id)

        if callers:
            lines.append("\n## Callers")
            for c in callers[:15]:
                lines.append(f"- `{c.get('name', c.get('id', '?'))}` ({c.get('kind', '?')})")
            if len(callers) > 15:
                lines.append(f"  ... and {len(callers) - 15} more")

        if callees:
            lines.append("\n## Callees")
            for c in callees[:15]:
                lines.append(f"- `{c.get('name', c.get('id', '?'))}` ({c.get('kind', '?')})")
            if len(callees) > 15:
                lines.append(f"  ... and {len(callees) - 15} more")

    return "\n".join(lines)


def _render_entity_detail(entity: Dict[str, Any]) -> str:
    """Format a single entity's details."""
    lines = [
        f"## {entity.get('name', '?')}",
        "",
        f"- **ID:** `{entity.get('id', '?')}`",
        f"- **Kind:** {entity.get('kind', '?')}",
        f"- **File:** `{entity.get('file_path', '?')}`",
    ]

    start = entity.get("start_line")
    end = entity.get("end_line")
    if start and end:
        lines.append(f"- **Lines:** {start}–{end} ({end - start + 1} lines)")

    span = entity.get("span", (0, 0))
    if span[0] and span[1]:
        lines.append(f"- **Bytes:** {span[0]}–{span[1]}")

    if entity.get("visibility"):
        vis_map = {1: "public", 2: "private", 3: "protected", 4: "internal"}
        lines.append(f"- **Visibility:** {vis_map.get(entity['visibility'], 'unknown')}")

    docstring = entity.get("docstring")
    if docstring:
        lines.append(f"\n```\n{docstring}\n```")

    signature = entity.get("signature")
    if signature:
        lines.append(f"\n**Signature:** `{signature}`")

    decorators = entity.get("decorators", [])
    if decorators:
        lines.append(f"\n**Decorators:** {', '.join(f'`{d}`' for d in decorators)}")

    parent = entity.get("parent_id")
    if parent:
        lines.append(f"\n**Parent:** `{parent}`")

    return "\n".join(lines)


def _find_entity(graph: Any, entity_id: str):
    """Look up entity by ID."""
    try:
        from coderadar._core import lookup_entity
        return lookup_entity(entity_id)
    except ImportError:
        return None


def _get_callers(graph: Any, entity_id: str):
    """Get callers."""
    try:
        from coderadar._core import callers_of
        return callers_of(entity_id) or []
    except ImportError:
        return []


def _get_callees(graph: Any, entity_id: str):
    """Get callees."""
    try:
        from coderadar._core import callees_of
        return callees_of(entity_id) or []
    except ImportError:
        return []
