"""codegraph_explore — Primary MCP tool (§26.2)

Given symbol names, returns verbatim line-numbered source grouped by file,
call paths between them, and a blast-radius summary. This is the 80%+ tool.

Implements the explore budget system (§26.3): adaptive output caps, file-level
budgeting, and the success-shaped error guidance pattern (§26.1 item 3).
"""

from __future__ import annotations

import os
from typing import Any, Dict, List, Optional, Set, Tuple

import structlog

from .budget import ExploreBudget, get_explore_budget

logger = structlog.get_logger(__name__)

# ── Explore Output ────────────────────────────────────────────────────────

MAX_SOURCE_CHUNK = 150  # lines per contiguous chunk


async def explore(
    graph: Any,
    args: Dict[str, Any],
    file_count: int,
) -> str:
    """Execute codegraph_explore.

    Follows CodeGraph's explore pattern:
    1. Parse args (query or symbols list)
    2. Resolve names → entities
    3. Traverse call graph between them
    4. Retrieve source for each file (budget-gated)
    5. Format output as line-numbered source + relationships
    """
    budget = get_explore_budget(file_count)

    # Success-shaped guidance when no graph is available (§26.1 item 3)
    if graph is None or file_count == 0:
        return _no_index_guidance(args)

    query = args.get("query", "")
    symbols: List[str] = args.get("symbols", [])
    direction = args.get("direction", "both")
    max_files = min(args.get("max_files") or budget.default_max_files, 20)

    # Merge query and symbols into a unified name list
    names = _parse_names(query, symbols)
    if not names:
        return _empty_query_guidance()

    # ── Phase 1: Resolve names → entities ────────────────────────────
    resolved = _resolve_names(graph, names)
    if not resolved:
        return _unresolved_guidance(names)

    # ── Phase 2: Traverse relationships ──────────────────────────────
    all_entities: Dict[str, Dict[str, Any]] = {}
    relationships: Dict[str, List[Dict[str, Any]]] = {}
    for entity in resolved:
        entity_id = entity["id"]
        all_entities[entity_id] = entity

        # Get callers and callees
        try:
            callers = _callers_of(graph, entity_id)
            callees = _callees_of(graph, entity_id)
            if direction in ("upstream", "both") and callers:
                relationships.setdefault(entity_id, []).extend(
                    {"kind": "caller", "entity": c} for c in callers[:10]
                )
            if direction in ("downstream", "both") and callees:
                relationships.setdefault(entity_id, []).extend(
                    {"kind": "callee", "entity": c} for c in callees[:10]
                )
        except Exception:
            pass

    # ── Phase 3: Retrieve source (budget-gated) ──────────────────────
    output = _render_explore_output(
        all_entities, relationships, budget, max_files,
    )

    return output


# ── Name Resolution ──────────────────────────────────────────────────────

def _parse_names(query: str, symbols: List[str]) -> List[str]:
    """Parse a query string or explicit symbols list into candidate names.

    Simple heuristic: split on commas, whitespace; filter common words.
    """
    if symbols:
        return [s.strip() for s in symbols if s.strip()]

    if not query.strip():
        return []

    # Simple: treat as space/comma-separated symbol names
    # More sophisticated NLP parsing deferred to QueryPlanner
    import re
    parts = re.split(r'[,;\s]+', query)
    return [p.strip() for p in parts if p.strip() and len(p.strip()) > 1]


def _resolve_names(graph: Any, names: List[str]) -> List[Dict[str, Any]]:
    """Resolve name strings to entities via find() and search_text()."""
    results: List[Dict[str, Any]] = []
    seen: Set[str] = set()

    for name in names:
        # Try exact ID lookup first
        entity = _find_entity(graph, name)
        if entity and entity.get("id") not in seen:
            results.append(entity)
            seen.add(entity["id"])
            continue

        # Try name substring search
        candidates = _text_search(graph, name, top_k=3)
        for c in candidates:
            if c.get("id") not in seen:
                results.append(c)
                seen.add(c["id"])

    return results


def _find_entity(graph: Any, entity_id: str) -> Optional[Dict[str, Any]]:
    """Look up entity by ID."""
    try:
        from coderadar._core import lookup_entity
        return lookup_entity(entity_id)
    except ImportError:
        return None


def _text_search(graph: Any, query: str, top_k: int = 5) -> List[Dict[str, Any]]:
    """Search entities by name text match."""
    try:
        from coderadar._core import search_entities
        return search_entities(query, top_k)
    except ImportError:
        return []


def _callers_of(graph: Any, entity_id: str) -> List[Dict[str, Any]]:
    """Get callers of an entity."""
    try:
        from coderadar._core import callers_of
        return callers_of(entity_id) or []
    except ImportError:
        return []


def _callees_of(graph: Any, entity_id: str) -> List[Dict[str, Any]]:
    """Get callees of an entity."""
    try:
        from coderadar._core import callees_of
        return callees_of(entity_id) or []
    except ImportError:
        return []


# ── Output Rendering ─────────────────────────────────────────────────────

def _render_explore_output(
    entities: Dict[str, Dict[str, Any]],
    relationships: Dict[str, List[Dict[str, Any]]],
    budget: ExploreBudget,
    max_files: int,
) -> str:
    """Render explore output with budget-gated source and relationships.

    Output shape matches CodeGraph's:
      **file_path** — symbol(kind), symbol(kind), ...
      <n>\t<source line>
      ...

      ## Relationships
      - entity_id ──[calls]──> target_id
    """
    output_parts: List[str] = []
    chars_used = 0

    # Group entities by file
    by_file: Dict[str, List[Dict[str, Any]]] = {}
    for entity_id, entity in entities.items():
        fp = entity.get("file_path", "unknown")
        by_file.setdefault(fp, []).append(entity)

    # Sort files: named files first, then by relevance (entity count)
    sorted_files = sorted(by_file.items(), key=lambda x: -len(x[1]))

    files_rendered = 0
    for file_path, file_entities in sorted_files[:max_files]:
        if chars_used >= budget.max_output_chars:
            break
        files_rendered += 1

        file_block = _render_file_block(
            file_path, file_entities, relationships,
            budget, budget.max_output_chars - chars_used,
        )
        output_parts.append(file_block)
        chars_used += len(file_block)

    # Relationships section
    if budget.include_relationships and relationships:
        rel_block = _render_relationships(relationships, budget)
        if chars_used + len(rel_block) <= budget.max_output_chars:
            output_parts.append(rel_block)
            chars_used += len(rel_block)

    # Additional files note
    if budget.include_additional_files and files_rendered < len(sorted_files):
        remaining = sorted_files[files_rendered:]
        file_list = ", ".join(fp for fp, _ in remaining[:15])
        note = f"\n\n**Additional relevant files (not shown):** {file_list}"
        if len(remaining) > 15:
            note += f" (+{len(remaining) - 15} more)"
        if chars_used + len(note) <= budget.max_output_chars:
            output_parts.append(note)

    # Completeness signal
    if budget.include_completeness_signal:
        signal = (
            "\n\n---\n"
            "Complete source code is included above for the requested symbols. "
            "Use these file paths and line numbers with the Edit tool directly."
        )
        if chars_used + len(signal) <= budget.max_output_chars:
            output_parts.append(signal)

    return "\n".join(output_parts)


def _render_file_block(
    file_path: str,
    entities: List[Dict[str, Any]],
    relationships: Dict[str, List[Dict[str, Any]]],
    budget: ExploreBudget,
    remaining_chars: int,
) -> str:
    """Render a single file's entities with source."""
    lines: List[str] = []

    # File header
    symbols = ", ".join(
        f"{e.get('name', '?')}({e.get('kind', '?')})"
        for e in entities[:budget.max_symbols_in_header]
    )
    header = f"**{file_path}** — {symbols}"
    if len(entities) > budget.max_symbols_in_header:
        header += f", +{len(entities) - budget.max_symbols_in_header} more"
    lines.append(header)
    lines.append("")

    # Source for each entity
    per_entity_budget = min(
        budget.max_chars_per_file // max(len(entities), 1),
        remaining_chars // max(len(entities), 1),
    )

    for entity in entities:
        source = _retrieve_source(entity, per_entity_budget)
        if source:
            lines.append(source)
            lines.append("")

    return "\n".join(lines)


def _retrieve_source(entity: Dict[str, Any], max_chars: int) -> Optional[str]:
    """Retrieve line-numbered source for an entity.

    Returns None if the file can't be read or the entity has no span.
    """
    file_path = entity.get("file_path")
    if not file_path:
        return None

    span = entity.get("span", (0, 0))
    start_line = entity.get("start_line", 1)
    end_line = entity.get("end_line", start_line)

    if not start_line or not end_line:
        return None

    try:
        with open(file_path, "r", encoding="utf-8", errors="replace") as f:
            all_lines = f.readlines()
    except (FileNotFoundError, PermissionError, OSError):
        return None

    # Read the entity's line range, expand to max_chars
    start_idx = max(0, start_line - 1)
    end_idx = min(len(all_lines), end_line)

    # Include surrounding context if budget allows
    entity_lines = all_lines[start_idx:end_idx]
    entity_chars = sum(len(l) for l in entity_lines)

    if entity_chars <= max_chars:
        # Can fit more context
        extra_lines = (max_chars - entity_chars) // 80  # rough
        start_idx = max(0, start_idx - extra_lines // 2)
        end_idx = min(len(all_lines), end_idx + extra_lines // 2)

    result_lines: List[str] = []
    for i in range(start_idx, end_idx):
        result_lines.append(f"{i + 1}\t{all_lines[i].rstrip()}")

    return "\n".join(result_lines)


def _render_relationships(
    relationships: Dict[str, List[Dict[str, Any]]],
    budget: ExploreBudget,
) -> str:
    """Render the relationships section."""
    lines = ["## Relationships", ""]

    # Group by kind for cleaner display
    for source_id, edges in relationships.items():
        source_name = source_id.split("::")[-1]
        for edge in edges[:budget.max_symbols_in_header]:
            target = edge["entity"]
            target_name = target.get("name", target.get("id", "?"))
            kind = edge["kind"]
            arrow = "←──" if kind == "caller" else "──→"
            lines.append(f"- `{target_name}` {arrow}[{kind}] `{source_name}`")

    return "\n".join(lines)


# ── Guidance Messages (§26.1 item 3) ────────────────────────────────────

def _no_index_guidance(args: Dict[str, Any]) -> str:
    """Success-shaped response when no index exists."""
    query = args.get("query", "")
    symbols = args.get("symbols", [])
    wanted = query or ", ".join(symbols) or "symbols"
    return (
        f"This project doesn't have a CodeRadar index yet. "
        f"I can't look up {wanted} through the graph.\n\n"
        "To index this project, run `coderadar init` in the project root. "
        "Indexing typically takes 1-10 seconds depending on project size.\n\n"
        "In the meantime, I'll use built-in tools (Read/Grep/Glob) to find "
        "what you need. Just tell me what you're looking for."
    )


def _empty_query_guidance() -> str:
    """Success-shaped response for empty queries."""
    return (
        "Please provide symbol names or a question to explore. "
        "For example:\n"
        '- `codegraph_explore` with query: "User.save authenticate"\n'
        '- `codegraph_explore` with symbols: ["src/models.py::User"]'
    )


def _unresolved_guidance(names: List[str]) -> str:
    """Success-shaped response when names can't be resolved."""
    name_list = ", ".join(f"`{n}`" for n in names)
    return (
        f"Couldn't find {name_list} in the index. "
        "These might be named differently, in an unindexed file, or external "
        "to the project.\n\n"
        "Try:\n"
        f"- `codegraph_search` with a partial name or keyword\n"
        "- Searching with broader terms"
    )
