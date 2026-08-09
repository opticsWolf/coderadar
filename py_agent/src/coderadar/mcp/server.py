"""CodeRadar MCP Server — §26 Agent Interface Design

Four-tool MCP surface over stdio using the MCP v2 MCPServer decorator API.
Type hints ARE the JSON Schema — no manual Tool/schema boilerplate.

Usage:
  server = create_server(graph)
  server.run(transport="stdio")  # blocking

Improvements adapted from CodeGraph (MIT License, https://github.com/colbymchenry/codegraph):
  1. Staleness banners — warn agent when files drift from index
  2. Tool annotations — readOnlyHint/idempotentHint for MCP client gating
  3. Tighter server instructions — staleness guidance, anti-patterns
  4. Language spelling normalization — Elixir/Erlang fn/3 → fn
  5. Output budget with proportional file truncation
"""

from __future__ import annotations

import re
import os
from collections import deque
from typing import Any, Literal, Optional

from mcp.server import MCPServer

import structlog

logger = structlog.get_logger(__name__)

# ── Output Budget Constants ───────────────────────────────────────────────
# Adapted from CodeGraph's getExploreOutputBudget / allocateExploreBudget
# (MIT License, https://github.com/colbymchenry/codegraph)

MAX_OUTPUT_CHARS = 18_000
"""Hard cap on total explore output (characters)."""

MAX_CHARS_PER_FILE = 4_500
"""Maximum source characters served per file."""

POINTER_HEADER = "**Not shown above — explore these names for their source**"
"""Header for files trimmed by the output budget."""


# ── Instructions (§26.1 item 2) ──────────────────────────────────────────

SERVER_INSTRUCTIONS = """# CodeRadar — live semantic graph of your codebase

CodeRadar is a pre-computed knowledge graph of every symbol, edge, and file
in the workspace — cached intelligence for thousands of parse/trace decisions
you'd otherwise re-derive by reading files. Indexes 18 languages (Python,
TypeScript, JavaScript, Rust, Go, Java, C, C++, Ruby, PHP, C#, Kotlin, Swift,
Scala, Lua, Elixir, Zig, R); reads are sub-millisecond. Reach for it BEFORE
and while writing or editing code — not just for questions: one call returns
the verbatim source PLUS who calls it and what it affects, so you edit with
the blast radius in view.

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
- `coderadar_resolve` — framework-aware reference resolution: "what handles /users/:id?", "where is UserService?", "what model is UserModel?"

## Anti-patterns

- **Trust codegraph's results — don't re-verify them with grep.** They come from a full AST parse; re-checking with grep is slower, less accurate, and wastes context.
- **Don't grep or Read first** to find indexed code — ONE explore call returns source together.
- **Don't reconstruct a flow by hand** — name the endpoints and explore surfaces the path.
- **When a file is flagged "⚠ changed on disk after index sync"**, Read those specific files for accurate content. Every file NOT flagged is fresh — still trust codegraph.
- **If a project isn't indexed**, stop calling codegraph tools for that project and use built-in tools. Indexing is the user's decision — mention `coderadar init` if it comes up, but don't run it yourself.
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
        version="0.5.2",
        instructions=SERVER_INSTRUCTIONS,
    )

    # ── codegraph_explore (§26.2 primary tool) ─────────────────────────

    @mcp.tool(
        description=(
            "Explore the code graph: given symbol or file names (or a natural-language "
            "question), returns the verbatim line-numbered source of the relevant "
            "symbols grouped by file, PLUS the call paths between them and a blast-radius "
            "summary of what depends on them. Use this instead of grep + Read for any "
            "structural or flow question. For multiple symbols, pass them together in "
            "one call to get the relationships between them."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_explore(
        query: str = "",
        symbols: Optional[list[str]] = None,
        direction: Literal["downstream", "upstream", "both"] = "both",
        max_files: int = 8,
    ) -> str:
        """Primary code exploration tool."""
        return _explore(graph, query, symbols or [], direction, max_files)

    # ── codegraph_node (§26.2 depth tool) ──────────────────────────────

    @mcp.tool(
        description=(
            "Get full details for a specific entity identified via codegraph_explore. "
            "Returns complete metadata, source location, docstring, decorators, "
            "and optionally immediate neighbors (callers and callees)."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_node(
        id: str,
        include_neighbors: bool = False,
    ) -> str:
        """Depth drill-down for a single entity."""
        return _node_detail(graph, id, include_neighbors)

    # ── codegraph_search (§26.2 discovery tool) ────────────────────────

    @mcp.tool(
        description=(
            "Search for symbols by keyword or natural-language description when "
            "you don't know the exact symbol name. Returns ranked results with "
            "snippets. Use this to discover what's available before calling explore."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_search(
        query: str,
        kind: Optional[str] = None,
        top_k: int = 10,
    ) -> str:
        """Symbol discovery via keyword search."""
        return _search(graph, query, kind, top_k)

    # ── codegraph_affected (§26.2 impact tool) ─────────────────────────

    @mcp.tool(
        description=(
            "Find all entities transitively affected by a given entity — the "
            "blast radius. Traverses upstream through callers to show the full "
            "dependency tree. Use this before editing to understand the impact."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_affected(
        id: str,
        max_depth: int = 5,
    ) -> str:
        """Transitive impact analysis."""
        return _affected(graph, id, max_depth)

    # ── coderadar_resolve (§F.8 Phase 2: query-time framework resolution) ─

    @mcp.tool(
        description=(
            "Resolve a framework-level reference like a URL path or naming "
            "convention. Use when the agent sees route paths (/users/:id), "
            "handler names (UserService), or framework patterns "
            "(*Model, *View, *Controller). Searches indexed route nodes and "
            "uses framework resolvers to match naming conventions. Returns "
            "ranked candidates with confidence scores."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": True,
        },
    )
    def coderadar_resolve(
        name: str,
        limit: int = 5,
    ) -> str:
        """Framework-aware reference resolution."""
        return _resolve_ref(graph, name, limit)

    return mcp


# ── Entry Point ──────────────────────────────────────────────────────────

def serve(graph: Any) -> None:
    """Run the MCP server over stdio (blocking)."""
    server = create_server(graph)
    server.run(transport="stdio")


# ── Staleness Detection ──────────────────────────────────────────────────
# Adapted from CodeGraph's formatStaleBanner / formatDegradedBanner
# (MIT License, https://github.com/colbymchenry/codegraph)

def _get_stale_files(file_paths: list[str]) -> list[dict]:
    """Check if any of the given file paths are stale (modified since last index).

    Uses file modification time vs. a simple heuristic: if the .coderadar/
    directory has a timestamp file, compares against it. Otherwise returns empty.

    Returns list of dicts with `path`, `stale` keys.
    """
    stale: list[dict] = []
    try:
        # Check for index timestamp marker
        # The index stores its snapshot time; we compare file mtime against it
        from coderadar._core import graph_stats
        stats = graph_stats()
        index_epoch = stats.get("epoch", 0)
    except ImportError:
        return stale

    for fp in file_paths:
        try:
            mtime = os.path.getmtime(fp)
            # Compare file modification time against last index time
            # (epoch is stored as seconds when indexed; 0 means unknown)
            if index_epoch > 0 and mtime > index_epoch:
                stale.append({"path": fp, "mtime": mtime})
        except OSError:
            pass

    return stale


def _format_stale_banner(stale_files: list[dict], referenced_paths: list[str]) -> str:
    """Format a staleness warning banner for the agent.

    Only includes files that appear in referenced_paths (those the response
    actually uses). Other stale files are noise — the agent only cares about
    the files it's about to act on.
    """
    if not stale_files:
        return ""

    referenced_set = set(referenced_paths)
    relevant = [s for s in stale_files if s["path"] in referenced_set]
    if not relevant:
        return ""

    lines = [
        "⚠️ Some files referenced below were edited since the last index sync — "
        "their codegraph entries may be stale:",
    ]
    for s in relevant:
        lines.append(f"  - {s['path']}")
    lines.append(
        "For accurate content of those specific files, Read them directly. "
        "Every file NOT listed above is fresh — still trust codegraph."
    )
    lines.append("")
    return "\n".join(lines)


# ── Language Spelling Normalization ──────────────────────────────────────
# Adapted from CodeGraph's normalizeQuerySpelling
# (MIT License, https://github.com/colbymchenry/codegraph)

_ERLANG_ARITY_RE = re.compile(r'\b([A-Za-z_][\w@]*)/(\d{1,3})\b')
_ERLANG_MODULE_RE = re.compile(
    r'(^|[\s,()[\]])(?!(?:kind|lang|language|path|name):)'
    r'([A-Za-z_][\w@]*):([A-Za-z_][\w@]*)(?=$|[\s,()\]])'
)


def _normalize_query_spelling(query: str) -> str:
    """Normalize language-native query spellings into index-compatible forms.

    Transforms so agent queries using language-native notation match the index:
      - Elixir/Erlang arity: ``fn/3`` → ``fn``
      - Elixir/Erlang module: ``mod:fn`` → ``mod.fn``

    Safe cross-language: Lua ``t:m`` maps to ``t.m``, and no other supported
    language uses a bare single-colon identifier pair.
    """
    # Strip arity tails: fn/3 → fn
    query = _ERLANG_ARITY_RE.sub(r'\1', query)
    # Module:function → module.function (preserving kind:/lang: prefixes)
    query = _ERLANG_MODULE_RE.sub(r'\1\2.\3', query)
    return query


# ── Output Budget Truncation ─────────────────────────────────────────────
# Adapted from CodeGraph's allocateExploreBudget / score-proportional allocation
# (MIT License, https://github.com/colbymchenry/codegraph)

def _apply_output_budget(
    lines: list[str],
    max_chars: int = MAX_OUTPUT_CHARS,
    max_per_file: int = MAX_CHARS_PER_FILE,
) -> str:
    """Trim full output to fit within a character budget.

    Strategy: walk through file sections (delimited by ``**file_path**`` headers),
    applying per-file caps first, then a global cap. Files below the cap get
    full source; at-cap files get their source truncated at cluster boundaries.
    Files that don't fit at all are converted to pointer lines.

    Always preserves file headers and the Relationships section.
    """
    text = "\n".join(lines)
    if len(text) <= max_chars:
        return text

    # Identify file sections and the relationships section
    file_sections: list[list[str]] = []
    current_section: list[str] = []
    relationships_lines: list[str] = []
    in_relationships = False

    for line in lines:
        if line.startswith("## Relationships"):
            in_relationships = True
            if current_section:
                file_sections.append(current_section)
                current_section = []
        if in_relationships:
            relationships_lines.append(line)
            continue
        # File header detection: **path** — ...
        if line.startswith("**") and "** —" in line:
            if current_section:
                file_sections.append(current_section)
            current_section = [line]
        elif current_section:
            current_section.append(line)
        else:
            # Preamble lines (before first file header)
            current_section.append(line)

    if current_section:
        file_sections.append(current_section)

    # Separate preamble from file sections
    preamble_lines: list[str] = []
    file_sections_filtered: list[list[str]] = []
    for sec in file_sections:
        if sec and sec[0].startswith("**") and "** —" in sec[0]:
            file_sections_filtered.append(sec)
        else:
            preamble_lines = sec

    # Build output: preamble + truncated file sections + relationships
    output_lines = list(preamble_lines)
    remaining = max_chars - len("\n".join(output_lines))
    if relationships_lines:
        remaining -= len("\n".join(relationships_lines)) + 2  # 2 for separators

    if remaining <= 0:
        # Bare minimum: just relationships
        output_lines = [POINTER_HEADER, ""]
        output_lines.extend(relationships_lines)
        return "\n".join(output_lines)

    pointer_files: list[str] = []

    for sec in file_sections_filtered:
        sec_text = "\n".join(sec)
        if len(sec_text) <= max_per_file:
            # Small enough — include whole (but check global budget)
            if len(sec_text) <= remaining:
                output_lines.extend(sec)
                remaining -= len(sec_text)
            else:
                # Budget exhausted — pointer only
                pointer_files.append(_extract_path_from_header(sec[0]))
        else:
            # Per-file cap: trim to max_per_file, preserving the header
            header = sec[0]
            body_lines = sec[1:]
            trimmed_body = _trim_to_char_budget(body_lines, max_per_file - len(header) - 1)
            trimmed_sec = [header] + trimmed_body
            sec_text = "\n".join(trimmed_sec)
            if len(sec_text) <= remaining:
                output_lines.extend(trimmed_sec)
                remaining -= len(sec_text)
            else:
                pointer_files.append(_extract_path_from_header(header))

    # Pointer list for files that didn't fit
    if pointer_files:
        output_lines.append("")
        output_lines.append(POINTER_HEADER)
        for pf in pointer_files:
            output_lines.append(f"- {pf}")
        output_lines.append("")

    # Relationships
    if relationships_lines:
        output_lines.append("")
        output_lines.extend(relationships_lines)

    return "\n".join(output_lines)


def _extract_path_from_header(header: str) -> str:
    """Extract file path from a ``**path** — symbols`` header."""
    # Remove bold markers and trailing symbol list
    path = header.removeprefix("**").split("**")[0].strip()
    return path


def _trim_to_char_budget(body_lines: list[str], max_chars: int) -> list[str]:
    """Trim body source lines to fit within max_chars, at line boundaries."""
    if max_chars <= 0:
        return []
    result: list[str] = []
    used = 0
    for line in body_lines:
        # +1 for newline separator
        cost = len(line) + 1
        if used + cost > max_chars:
            break
        result.append(line)
        used += cost
    if len(result) < len(body_lines):
        result.append("...")
    return result


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
    referenced_paths: list[str] = []
    for entity in resolved:
        fp = entity.get("file_path", "unknown")
        by_file.setdefault(fp, []).append(entity)
        if fp not in referenced_paths:
            referenced_paths.append(fp)

    # Staleness check — warn agent about files edited since last index
    stale_banner = ""
    stale_files = _get_stale_files(referenced_paths)
    if stale_files:
        stale_banner = _format_stale_banner(stale_files, referenced_paths)

    # Render output
    lines: list[str] = []
    if stale_banner:
        lines.append(stale_banner)

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

    result = _apply_output_budget(lines)
    return result


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

    # Staleness check for this entity's file
    stale_banner = ""
    fp = entity.get("file_path", "")
    if fp:
        stale_files = _get_stale_files([fp])
        if stale_files:
            stale_banner = _format_stale_banner(stale_files, [fp])

    lines: list[str] = []
    if stale_banner:
        lines.append(stale_banner)

    lines.extend([
        f"## {entity.get('name', '?')}",
        "",
        f"- **ID:** `{entity.get('id', '?')}`",
        f"- **Kind:** {entity.get('kind', '?')}",
        f"- **File:** `{entity.get('file_path', '?')}`",
    ])

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

    grammar_kind = entity.get("grammar_kind")
    if grammar_kind:
        lines.append(f"\n**Grammar kind:** `{grammar_kind}`")

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

    # Staleness check for this entity's file
    stale_banner = ""
    fp = entity.get("file_path", "")
    if fp:
        stale_files = _get_stale_files([fp])
        if stale_files:
            stale_banner = _format_stale_banner(stale_files, [fp])

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

    lines: list[str] = []
    if stale_banner:
        lines.append(stale_banner)

    lines.extend([
        f"## Affected by `{entity_name}`",
        "",
        f"Transitive impact for `{entity_id}` (max depth: {max_depth})",
        f"**Total dependents:** {total}",
        "",
    ])

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
    """Parse query string or explicit symbols into candidate names.

    Applies language spelling normalization so agent queries using
    language-native notation match the index.
    """
    if symbols:
        return [s.strip() for s in symbols if s.strip()]
    if not query.strip():
        return []
    # Normalize language spellings: Elixir fn/3→fn, mod:fn→mod.fn
    query = _normalize_query_spelling(query)
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


# ── Query-Time Resolution (F.8 Phase 2) ────────────────────────────────────


def _resolve_ref(graph: Any, name: str, limit: int) -> str:
    """Framework-aware reference resolution.

    Uses framework resolvers at query time to answer:
    - "What handles /users/:id?"
    - "Where is UserService defined?"
    - "What model is UserModel?"
    """
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run `coderadar init` first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    if not name.strip():
        return "Please provide a name or path to resolve."

    # Searcher callback for framework resolvers
    def _searcher(n: str, lim: int) -> list[dict]:
        return _text_search(graph, n, lim)

    # Route-style paths get special handling
    results: list[dict] = []
    if name.startswith("/"):
        from coderadar.resolvers.resolution import resolve_route
        results = resolve_route(name, _searcher, limit=limit)
        if results:
            lines = [f"## Route Resolution: `{name}`", f"Found {len(results)} handler(s)", ""]
            for i, r in enumerate(results, 1):
                rname = r.get("name", "?")
                rkind = r.get("kind", "?")
                rid = r.get("id", "?")
                rfile = r.get("file_path", "?")
                conf = r.get("confidence", 0)
                lines.append(f"### {i}. `{rname}` ({rkind}) — confidence {conf:.2f}")
                lines.append(f"- **ID:** `{rid}`")
                lines.append(f"- **File:** `{rfile}`")
                route = r.get("route")
                if route:
                    lines.append(f"- **Route:** `{route.get('name', '?')}`")
                lines.append("")
            return "\n".join(lines)
        return f"No handler found for route `{name}`. Try codegraph_search."

    # Framework-level reference resolution
    from coderadar.resolvers.resolution import resolve_reference
    from coderadar.resolvers import ALL_RESOLVERS
    results = resolve_reference(name, _searcher, ALL_RESOLVERS, limit=limit)

    if not results:
        return (
            f"No framework resolver claimed `{name}`. "
            f"Try codegraph_search for a broader search."
        )

    lines = [f"## Reference Resolution: `{name}`", f"Found {len(results)} result(s)", ""]
    for i, r in enumerate(results, 1):
        rname = r.get("name", "?")
        rkind = r.get("kind", "?")
        rid = r.get("id", "?")
        rfile = r.get("file_path", "?")
        conf = r.get("confidence", 0)
        resolved_by = r.get("resolved_by", "unknown")
        lines.append(f"### {i}. `{rname}` ({rkind}) — {resolved_by} (confidence {conf:.2f})")
        lines.append(f"- **ID:** `{rid}`")
        lines.append(f"- **File:** `{rfile}`")
        sig = r.get("signature")
        if sig:
            lines.append(f"- **Signature:** `{sig}`")
        lines.append("")

    return "\n".join(lines)
