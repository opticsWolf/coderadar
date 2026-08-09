"""CodeRadar MCP Server — §26 Agent Interface Design

Four-tool MCP surface over stdio using the MCP v2 MCPServer decorator API.
Type hints ARE the JSON Schema — no manual Tool/schema boilerplate.

Usage:
  server = create_server(graph)
  server.run(transport="stdio")  # blocking
"""

from __future__ import annotations

from collections import deque
from typing import Any, Literal, Optional

from mcp.server import MCPServer

import structlog

logger = structlog.get_logger(__name__)

# ── Instructions (§26.1 item 2) ──────────────────────────────────────────

SERVER_INSTRUCTIONS = """# CodeRadar — live semantic graph of your codebase

CodeRadar is a pre-computed knowledge graph of every symbol, edge, and file
in the workspace — cached intelligence for thousands of parse/trace decisions
you'd otherwise re-derive by reading files. Indexes Python + TypeScript;
reads are sub-millisecond.

## Primary tool: codegraph_explore

Call `codegraph_explore` for ANY structural or flow question. It returns
verbatim, line-numbered source of the relevant symbols grouped by file —
the same shape `Read` gives you, safe to `Edit` from — PLUS the call paths
between them and a blast-radius summary.

ONE call usually answers the whole question. CodeRadar IS the pre-built search
index — running your own grep + read loop repeats work already done and costs
more for the same answer.

## How to query

- **"How does X work?" / architecture / bug / "what is X"** → `codegraph_explore` with the symbol name(s)
- **"The flow from X to Y"** → `codegraph_explore` with both endpoints
- **Reading/editing a specific symbol** → name it in `codegraph_explore`, get line-numbered source back
- **Need more?** Call `codegraph_explore` again with more specific names

## Other tools

- `codegraph_node` — full details for an entity identified via explore
- `codegraph_search` — find symbols by keyword when you don't know the exact name
- `codegraph_affected` — transitive impact: "what calls this, all the way up?"

## Anti-patterns

- **Trust codegraph's results — don't re-verify with grep.** They come from a full AST parse.
- **Don't grep or Read first** to find indexed code — ONE explore call returns source together.
- **Don't reconstruct a flow by hand** — name the endpoints and explore surfaces the path.
- **If a project isn't indexed**, stop calling codegraph tools for that project and use built-in tools.
"""


# ── Server Factory ────────────────────────────────────────────────────────

def create_server(graph: Any) -> MCPServer:
    """Create an MCP v2 server wrapping the given CodeGraph instance.

    Tools capture `graph` via closure — no globals, no thread-locals.
    Every tool is a plain, type-hinted Python function; MCPServer derives
    JSON Schema and routing from the function signature.
    """
    mcp = MCPServer(
        "CodeRadar",
                version="0.3.15",
        instructions=SERVER_INSTRUCTIONS,
    )

    # ── codegraph_explore (§26.2 primary tool) ─────────────────────────

    @mcp.tool(description=(
        "Explore the code graph: given symbol or file names (or a natural-language "
        "question), returns the verbatim line-numbered source of the relevant "
        "symbols grouped by file, PLUS the call paths between them and a blast-radius "
        "summary of what depends on them. Use this instead of grep + Read for any "
        "structural or flow question. For multiple symbols, pass them together in "
        "one call to get the relationships between them."
    ))
    def codegraph_explore(
        query: str = "",
        symbols: Optional[list[str]] = None,
        direction: Literal["downstream", "upstream", "both"] = "both",
        max_files: int = 8,
    ) -> str:
        """Primary code exploration tool."""
        return _explore(graph, query, symbols or [], direction, max_files)

    # ── codegraph_node (§26.2 depth tool) ──────────────────────────────

    @mcp.tool(description=(
        "Get full details for a specific entity identified via codegraph_explore. "
        "Returns complete metadata, source location, docstring, decorators, "
        "and optionally immediate neighbors (callers and callees)."
    ))
    def codegraph_node(
        id: str,
        include_neighbors: bool = False,
    ) -> str:
        """Depth drill-down for a single entity."""
        return _node_detail(graph, id, include_neighbors)

    # ── codegraph_search (§26.2 discovery tool) ────────────────────────

    @mcp.tool(description=(
        "Search for symbols by keyword or natural-language description when "
        "you don't know the exact symbol name. Returns ranked results with "
        "snippets. Use this to discover what's available before calling explore."
    ))
    def codegraph_search(
        query: str,
        kind: Optional[str] = None,
        top_k: int = 10,
    ) -> str:
        """Symbol discovery via keyword search."""
        return _search(graph, query, kind, top_k)

    # ── codegraph_affected (§26.2 impact tool) ─────────────────────────

    @mcp.tool(description=(
        "Find all entities transitively affected by a given entity — the "
        "blast radius. Traverses upstream through callers to show the full "
        "dependency tree. Use this before editing to understand the impact."
    ))
    def codegraph_affected(
        id: str,
        max_depth: int = 5,
    ) -> str:
        """Transitive impact analysis."""
        return _affected(graph, id, max_depth)

    return mcp


# ── Entry Point ──────────────────────────────────────────────────────────

def serve(graph: Any) -> None:
    """Run the MCP server over stdio (blocking)."""
    server = create_server(graph)
    server.run(transport="stdio")


# ── Tool Implementations ─────────────────────────────────────────────────

def _explore(
    graph: Any, query: str, symbols: list[str],
    direction: str, max_files: int,
) -> str:
    """Execute codegraph_explore."""
    try:
        from coderadar._core import graph_stats
        stats = graph_stats()
        if stats.get("modules", 0) == 0:
            return "No index available. Run `coderadar init` in the project root first."
    except ImportError:
        return "CodeRadar extension not available."

    names = _parse_names(query, symbols)
    if not names:
        return (
            "Please provide symbol names or a question to explore. "
            'For example: codegraph_explore(query="User.save authenticate")'
        )

    # Resolve names → entities
    resolved = _resolve_names(graph, names)
    if not resolved:
        name_list = ", ".join(f"`{n}`" for n in names)
        return (
            f"Couldn't find {name_list} in the index. "
            "Try codegraph_search with broader terms."
        )

    # Group by file
    by_file: dict[str, list[dict]] = {}
    for entity in resolved:
        fp = entity.get("file_path", "unknown")
        by_file.setdefault(fp, []).append(entity)

    # Render output
    lines: list[str] = []
    for file_path, entities in list(by_file.items())[:max_files]:
        names_str = ", ".join(
            f"{e.get('name', '?')}({e.get('kind', '?')})"
            for e in entities[:10]
        )
        lines.append(f"**{file_path}** — {names_str}")
        lines.append("")

        for entity in entities:
            source = _read_source(entity)
            if source:
                lines.append(source)
                lines.append("")

    # Relationships
    rel_lines = _render_relationships(graph, resolved, direction)
    if rel_lines:
        lines.append("## Relationships")
        lines.extend(rel_lines)

    return "\n".join(lines)


def _node_detail(graph: Any, entity_id: str, include_neighbors: bool) -> str:
    """Get full entity details."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run `coderadar init` first."
    except ImportError:
        return "CodeRadar extension not available."

    entity = _find_entity(graph, entity_id)
    if not entity:
        return (
            f"Entity `{entity_id}` not found. "
            "Try codegraph_search to locate it."
        )

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

    docstring = entity.get("docstring")
    if docstring:
        lines.append(f"\n```\n{docstring}\n```")

    signature = entity.get("signature")
    if signature:
        lines.append(f"\n**Signature:** `{signature}`")

    decorators = entity.get("decorators", [])
    if decorators:
        lines.append(f"\n**Decorators:** {', '.join(f'`{d}`' for d in decorators)}")

    if include_neighbors:
        callers = _get_callers(graph, entity_id)
        callees = _get_callees(graph, entity_id)
        if callers:
            lines.append(f"\n## Callers ({len(callers)})")
            for c in callers[:15]:
                lines.append(f"- `{c.get('name', c.get('id', '?'))}` ({c.get('kind', '?')})")
        if callees:
            lines.append(f"\n## Callees ({len(callees)})")
            for c in callees[:15]:
                lines.append(f"- `{c.get('name', c.get('id', '?'))}` ({c.get('kind', '?')})")

    return "\n".join(lines)


def _search(graph: Any, query: str, kind: str | None, top_k: int) -> str:
    """Keyword search for symbols."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run `coderadar init` first."
    except ImportError:
        return "CodeRadar extension not available."

    if not query.strip():
        return "Please provide a query to search for."

    results = _text_search(graph, query, min(top_k, 20))
    if kind:
        # kind filtering now happens in Rust search_entities via the kind param
        results = [r for r in results if r.get("kind") == kind or r.get("entity_type") == kind]

    if not results:
        return (
            f"No results found for '{query}'"
            + (f" (kind: {kind})" if kind else "")
            + ". Try broader terms."
        )

    lines = [f"## Search: `{query}`", f"Found {len(results)} result(s)", ""]
    for i, entity in enumerate(results[:top_k], 1):
        name = entity.get("name", "?")
        ek = entity.get("kind", "?")
        eid = entity.get("id", "?")
        fp = entity.get("file_path", "?")
        sl = entity.get("start_line")

        lines.append(f"### {i}. `{name}` ({ek})")
        lines.append(f"- **ID:** `{eid}`")
        lines.append(f"- **File:** `{fp}`")
        if sl:
            lines.append(f"- **Line:** {sl}")
        doc = entity.get("docstring")
        if doc:
            lines.append(f"- **Docstring:** {doc[:200]}{'...' if len(doc) > 200 else ''}")
        sig = entity.get("signature")
        if sig:
            lines.append(f"- **Signature:** `{sig}`")
        lines.append("")

    return "\n".join(lines)


def _affected(graph: Any, entity_id: str, max_depth: int) -> str:
    """Transitive impact analysis."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run `coderadar init` first."
    except ImportError:
        return "CodeRadar extension not available."

    entity = _find_entity(graph, entity_id)
    if not entity:
        return f"Entity `{entity_id}` not found. Try codegraph_search."

    # BFS upstream
    tree: dict[int, list[dict]] = {}
    visited = {entity_id}
    queue: deque[tuple[str, int]] = deque([(entity_id, 0)])

    while queue:
        current_id, depth = queue.popleft()
        if depth >= min(max_depth, 20):
            continue
        callers = _get_callers(graph, current_id)
        for caller in callers:
            cid = caller.get("id", "")
            if cid and cid not in visited:
                visited.add(cid)
                tree.setdefault(depth + 1, []).append(caller)
                queue.append((cid, depth + 1))

    entity_name = entity.get("name", "?")
    total = sum(len(v) for v in tree.values())

    lines = [
        f"## Affected by `{entity_name}`",
        "",
        f"Transitive impact for `{entity_id}` (max depth: {max_depth})",
        f"**Total dependents:** {total}",
        "",
    ]

    if total == 0:
        lines.append("No dependents found. Nothing calls this entity.")
        return "\n".join(lines)

    for depth in sorted(tree.keys()):
        entities = tree[depth]
        indent = "  " * depth
        lines.append(f"**Depth {depth}** ({len(entities)}):")
        for e in entities[:20]:
            n = e.get("name", "?")
            k = e.get("kind", "?")
            ei = e.get("id", "?")
            lines.append(f"{indent}- `{n}` ({k}) — `{ei}`")
        if len(entities) > 20:
            lines.append(f"{indent}  ... and {len(entities) - 20} more")
        lines.append("")

    return "\n".join(lines)


# ── Helpers ──────────────────────────────────────────────────────────────

def _parse_names(query: str, symbols: list[str]) -> list[str]:
    """Parse query string or explicit symbols into candidate names."""
    if symbols:
        return [s.strip() for s in symbols if s.strip()]
    if not query.strip():
        return []
    import re
    parts = re.split(r'[,;\s]+', query)
    return [p.strip() for p in parts if p.strip() and len(p.strip()) > 1]


def _resolve_names(graph: Any, names: list[str]) -> list[dict]:
    """Resolve names to entities via find + search fallback."""
    results: list[dict] = []
    seen: set[str] = set()
    for name in names:
        entity = _find_entity(graph, name)
        if entity and entity.get("id") not in seen:
            results.append(entity)
            seen.add(entity["id"])
            continue
        for c in _text_search(graph, name, 3):
            if c.get("id") not in seen:
                results.append(c)
                seen.add(c["id"])
    return results


def _read_source(entity: dict) -> str | None:
    """Read line-numbered source for an entity from disk."""
    file_path = entity.get("file_path")
    start_line = entity.get("start_line", 1)
    end_line = entity.get("end_line", start_line)
    if not file_path or not start_line:
        return None
    try:
        with open(file_path, encoding="utf-8", errors="replace") as f:
            all_lines = f.readlines()
    except OSError:
        return None
    si = max(0, start_line - 1)
    ei = min(len(all_lines), end_line)
    return "".join(f"{i + 1}\t{all_lines[i]}" for i in range(si, ei))


def _render_relationships(
    graph: Any, entities: list[dict], direction: str,
) -> list[str]:
    """Render callers/callees relationships."""
    lines: list[str] = []
    for entity in entities:
        entity_id = entity["id"]
        name = entity.get("name", entity_id)
        if direction in ("upstream", "both"):
            for c in _get_callers(graph, entity_id)[:5]:
                cn = c.get("name", c.get("id", "?"))
                lines.append(f"- `{cn}` ←──[caller] `{name}`")
        if direction in ("downstream", "both"):
            for c in _get_callees(graph, entity_id)[:5]:
                cn = c.get("name", c.get("id", "?"))
                lines.append(f"- `{name}` ──→[callee] `{cn}`")
    return lines


def _find_entity(graph: Any, entity_id: str) -> dict | None:
    try:
        from coderadar._core import lookup_entity
        return lookup_entity(entity_id)
    except ImportError:
        return None


def _text_search(graph: Any, query: str, top_k: int, kind: str | None = None) -> list[dict]:
    try:
        from coderadar._core import search_entities
        return search_entities(query, top_k, kind) or []
    except ImportError:
        return []


def _get_callers(graph: Any, entity_id: str) -> list[dict]:
    try:
        from coderadar._core import callers_of
        return callers_of(entity_id) or []
    except ImportError:
        return []


def _get_callees(graph: Any, entity_id: str) -> list[dict]:
    try:
        from coderadar._core import callees_of
        return callees_of(entity_id) or []
    except ImportError:
        return []
