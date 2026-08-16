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

import functools
import re
import os
from collections import deque
from typing import Any, Literal, Optional

from mcp.server import MCPServer

import structlog

logger = structlog.get_logger(__name__)

# Cached fastembed model for semantic search (lazy-loaded, reused across queries)
_EMBED_MODEL = None

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
- `codegraph_query` — structured Pest graph queries ("functions where name contains 'test'")
- `codegraph_search_similar` — semantic/embedding search across all entity types
- `codegraph_compute_embeddings` — generate embedding vectors for semantic search
- `codegraph_module_children` — list classes/functions/imports in a module
- `codegraph_as_of` — query the graph at a past timestamp ("what did X look like at commit Y?")
- `codegraph_traverse` — generic edge traversal with direction and depth control
- `codegraph_get_smells` — detect architectural code smells (god-class, long-method, long-parameter-list, deep-nesting, data-class, high-cyclomatic-complexity, brain-method, excessive-returns, too-many-fields)

## Mutation pipeline (LLM-writable code)

After editing code, use the mutation pipeline to keep the graph in sync:

1. `coderadar_replace_body` — replace a function body
2. `coderadar_update_signature` — change a function signature (with call-site cascade)
3. `coderadar_rename` — rename an entity and all references
4. `coderadar_create_entity` — create a new entity in a file

**Always dry-run first, review the diff, then apply with dry_run=False.**

## Keeping the graph fresh

- `codegraph_update_file` — after editing a single file via Read/Edit, sync just that file
- `codegraph_reindex` — after batch edits, full guaranteed-fresh reindex

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
        version="0.6.22",
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

    # ── codegraph_query — Pest graph query language ───────────────────

    @mcp.tool(
        description=(
            "Execute a structured Pest graph query against the indexed codebase. "
            "Supports: 'functions where name contains X', 'classes where inherits_from "
            "contains Y', 'imports where module contains Z', 'entities where kind = function'. "
            "Returns matched entities with metadata. Use for precise structural queries "
            "that go beyond keyword search."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_query(
        query: str,
    ) -> str:
        """Execute a Pest graph query."""
        return _query_graph(graph, query)

    # ── codegraph_search_similar — embedding/semantic search ──────────

    @mcp.tool(
        description=(
            "Find symbols semantically similar to a natural-language query. "
            "Scans ALL entity types (functions, classes, modules, imports, constants, "
            "type aliases) with stored embeddings. "
            "Returns ranked results with cosine similarity scores and entity kind. "
            "Use for conceptual search: 'authentication logic', 'error handling', etc. "
            "Auto-computes embeddings on first call if none exist."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_search_similar(
        query: str,
        top_k: int = 10,
    ) -> str:
        """Semantic similarity search."""
        return _search_similar(graph, query, top_k)

    # ── codegraph_compute_embeddings — generate embedding vectors ────

    @mcp.tool(
        description=(
            "Compute and store embedding vectors for all entities in the index "
            "(functions, classes, modules, imports, constants, type aliases). "
            "Uses fastembed (BAAI/bge-small-en-v1.5) for local embedding generation. "
            "This is a prerequisite for codegraph_search_similar — without embeddings, "
            "semantic search returns 'no embeddings found'. Run once after indexing. "
            "Subsequent runs skip unchanged functions via content hash dedup. "
            "Returns: {generated, cached, total, errors}."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_compute_embeddings(
    ) -> str:
        """Compute embeddings for semantic search."""
        return _compute_embeddings(graph)

    # ── codegraph_module_children — structural discovery ─────────────

    @mcp.tool(
        description=(
            "List all children (classes, functions, imports, constants) of a module. "
            "The module ID is typically '{file_path}::module' — get it from codegraph_explore "
            "or codegraph_search results. Use to understand a module's structure before editing."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_module_children(
        module_id: str,
    ) -> str:
        """List module children."""
        return _module_children(graph, module_id)

    # ── codegraph_as_of — temporal query ─────────────────────────────

    @mcp.tool(
        description=(
            "Query the code graph as it existed at a specific point in time. "
            "Macrame's bitemporal ledger stores every version of every entity and edge, "
            "so this reconstructs the graph at any past timestamp. "
            "Use for: 'what did this look like last week?', 'when was X introduced?'."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": True,
        },
    )
    def codegraph_as_of(
        timestamp: str,
        query: str = "",
        symbols: Optional[list[str]] = None,
    ) -> str:
        """Temporal graph query."""
        return _as_of(graph, timestamp, query, symbols or [])

    # ── codegraph_traverse — edge traversal ──────────────────────────

    @mcp.tool(
        description=(
            "Traverse the graph from a starting entity along specified edge kinds. "
            "Direction: 'downstream' (callees), 'upstream' (callers), 'both'. "
            "Edge kinds: 'calls', 'imports', 'inherits', 'overrides', 'handles', "
            "'declares', 'references', 'navigation'. Returns a tree of linked entities. "
            "Use for custom flow analysis beyond explore/affected."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_traverse(
        entity_id: str,
        direction: Literal["downstream", "upstream", "both"] = "both",
        edge_kinds: Optional[list[str]] = None,
        max_depth: int = 3,
    ) -> str:
        """Generic edge traversal."""
        return _traverse(graph, entity_id, direction, edge_kinds, max_depth)

    # ── codegraph_get_smells — code smell detection ─────────────────

    @mcp.tool(
        description=(
            "Detect code smells (architectural issues) across the indexed codebase. "
            "Run without arguments to list all findings, or filter by entity_id "
            "(exact match) and/or rule_id (one of: god-class, long-method, "
            "long-parameter-list, deep-nesting, data-class, "
            "high-cyclomatic-complexity, brain-method, excessive-returns, "
            "too-many-fields). Each finding carries a severity, a human message, "
            "and the metric signals (WMC, CBO, LOC, cyclomatic, nesting_depth, "
            "param_count, field_count, return_count, max_method_cyclomatic) that "
            "triggered it."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_get_smells(
        entity_id: Optional[str] = None,
        rule_id: Optional[str] = None,
    ) -> str:
        """Detect architectural code smells."""
        return _get_smells(graph, entity_id, rule_id)

    # ── coderadar_replace_body ────────────────────────────────────

    @mcp.tool(
        description=(
            "Replace the body of a function/method. "
            "With dry_run=True (default): returns a diff preview for review. "
            "With dry_run=False: writes the file AND updates the graph atomically. "
            "Best practice: call first with dry_run=True to review, then call "
            "again with dry_run=False to apply. Use expected_hash to verify the "
            "current body matches before replacing (safety check)."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_replace_body(
        entity_id: str,
        new_body: str,
        expected_hash: Optional[str] = None,
        dry_run: bool = True,
    ) -> str:
        """Replace a function body."""
        return _replace_body(graph, entity_id, new_body, expected_hash, dry_run)

    # ── coderadar_update_signature ────────────────────────────────

    @mcp.tool(
        description=(
            "Change a function/method signature. "
            "With dry_run=True: shows the signature change and all affected call sites. "
            "With dry_run=False: writes the definition change AND updates the graph. "
            "NOTE: the definition signature is edited automatically, but call sites are "
            "returned as unverified_sites (with line numbers) for manual review — "
            "call-site argument spans are not indexed, so they cannot be auto-edited safely."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_update_signature(
        entity_id: str,
        new_signature: str,
        inject_defaults: bool = False,
        dry_run: bool = True,
    ) -> str:
        """Change a function signature."""
        return _update_signature(graph, entity_id, new_signature, inject_defaults, dry_run)

    # ── coderadar_rename ────────────────────────────────────────────

    @mcp.tool(
        description=(
            "Rename an entity (function, class, variable) and all references. "
            "With dry_run=True: shows all files and references that need updating. "
            "With dry_run=False: renames the definition and ALL references, "
            "updates the graph. Covers definition site and all usages."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_rename(
        entity_id: str,
        new_name: str,
        dry_run: bool = True,
    ) -> str:
        """Rename an entity."""
        return _rename(graph, entity_id, new_name, dry_run)

    # ── coderadar_create_entity ─────────────────────────────────────

    @mcp.tool(
        description=(
            "Create a new entity (function, class, constant) in a file. "
            "With dry_run=True: shows where the entity would be inserted. "
            "With dry_run=False: inserts the entity into the file AND indexes it. "
            "anchor='end' appends at file end, 'top' inserts at file top, "
            "or pass an entity ID to insert after that entity. "
            "The code is rendered from name/body/decorators using language-aware "
            "syntax for common languages."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_create_entity(
        file_path: str,
        language: str,
        kind: str,
        name: str,
        body: str,
        decorators: Optional[list[str]] = None,
        anchor: str = "end",
        dry_run: bool = True,
    ) -> str:
        """Create a new entity."""
        return _create_entity(graph, file_path, language, kind, name, body, decorators, anchor, dry_run)

    # ── codegraph_reindex — full graph refresh ───────────────────────

    @mcp.tool(
        description=(
            "Re-index the entire project to refresh the code graph. "
            "Use after batch edits when you've changed many files and want "
            "a guaranteed-fresh index. Slower than codegraph_update_file but "
            "always correct. Set with_embeddings=True to also compute embedding "
            "vectors for semantic search (adds 8-10s for small projects). "
            "Returns index statistics."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_reindex(
        with_embeddings: bool = False,
    ) -> str:
        """Full reindex with optional embeddings."""
        return _reindex(graph, with_embeddings)

    # ── codegraph_update_file — incremental single-file sync ────────

    @mcp.tool(
        description=(
            "Incrementally update the graph after editing a single file. "
            "Call this after using Read/Edit to modify source code, before "
            "the next codegraph_explore or codegraph_affected call. "
            "Faster than codegraph_reindex — only re-parses one file. "
            "Pass the file path and optionally the new content; if content "
            "is omitted, the file is read from disk."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def codegraph_update_file(
        file_path: str,
        content: Optional[str] = None,
    ) -> str:
        """Incrementally sync one file."""
        return _update_file(graph, file_path, content)

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

NO_INDEX_MESSAGE = (
    "No index available. Run `coderadar init` in the project root first."
)
NO_EXTENSION_MESSAGE = "CodeRadar extension not available."


def requires_index(func):
    """Return a message instead of raising when there is no index yet.

    Every tool carried its own copy of this guard, and every copy caught only
    ImportError — but `with_graph` raises PyRuntimeError ("No graph loaded"),
    so before the first index the tools raised a RuntimeError at the agent
    instead of the message written for exactly that case. The friendly path
    was reachable only when a graph was loaded *and* empty, which never
    happens.
    """
    @functools.wraps(func)
    def wrapper(*args: Any, **kwargs: Any) -> str:
        try:
            from coderadar._core import graph_stats
            if graph_stats().get("modules", 0) == 0:
                return NO_INDEX_MESSAGE
        except ImportError:
            return NO_EXTENSION_MESSAGE
        except RuntimeError:
            # No graph loaded — the state this guard exists for.
            return NO_INDEX_MESSAGE
        return func(*args, **kwargs)

    return wrapper


@requires_index
def _explore(
    graph: Any, query: str, symbols: list[str],
    direction: str, max_files: int,
) -> str:
    """Execute codegraph_explore."""
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


@requires_index
def _node_detail(graph: Any, entity_id: str, include_neighbors: bool) -> str:
    """Get full entity details."""
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


@requires_index
def _search(graph: Any, query: str, kind: str | None, top_k: int) -> str:
    """Keyword search for symbols."""
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


@requires_index
def _affected(graph: Any, entity_id: str, max_depth: int) -> str:
    """Transitive impact analysis."""
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


# ── New Tool Implementations (v0.5.9) ─────────────────────────────────

def _query_graph(graph: Any, query: str) -> str:
    """Execute a Pest query against the graph."""
    # An empty query is a usage error regardless of graph state, so prompt
    # for it before touching the (possibly unloaded) in-memory graph.
    if not query.strip():
        return "Please provide a Pest query. Examples:\n" \
               "  - functions where name contains 'test'\n" \
               "  - classes where inherits_from contains 'BaseModel'\n" \
               "  - imports where module contains 'os'"

    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run `coderadar init` first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    try:
        from coderadar._core import query_graph as _qg
        rows = _qg(query)
        if not rows:
            return f"Query `{query}` returned no results."
        lines = [f"## Query: `{query}`", f"Found {len(rows)} result(s)", ""]
        for i, row in enumerate(rows[:30], 1):
            name = row.get("name", row.get("id", "?"))
            kind = row.get("kind", row.get("entity_type", "?"))
            fp = row.get("file_path", "?")
            sl = row.get("start_line", row.get("line", ""))
            lines.append(f"{i}. `{name}` ({kind}) — `{fp}`")
            if sl:
                lines.append(f"   Line: {sl}")
            sig = row.get("signature")
            if sig:
                lines.append(f"   Signature: `{sig}`")
        if len(rows) > 30:
            lines.append(f"... and {len(rows) - 30} more")
        return "\n".join(lines)
    except Exception as e:
        return f"Query failed: {e}"


def _compute_embeddings(graph: Any) -> str:
    """Compute embeddings for all functions."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run codegraph_reindex first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    try:
        metrics = graph.compute_embeddings()
        return (
            f"## Embeddings Complete\n\n"
            f"- **Generated:** {metrics.get('generated', 0)}\n"
            f"- **Cached (unchanged):** {metrics.get('cached', 0)}\n"
            f"- **Total entities:** {metrics.get('total', 0)}\n"
            f"- **Errors:** {metrics.get('errors', 0)}\n\n"
            f"Semantic search (codegraph_search_similar) is now available."
        )
    except Exception as e:
        return f"Embedding generation failed: {e}\n\nEnsure fastembed is installed: pip install fastembed"


def _get_embedding_model():
    """Lazily load and cache the fastembed model (avoid reload per query)."""
    global _EMBED_MODEL
    if _EMBED_MODEL is None:
        from fastembed import TextEmbedding
        _EMBED_MODEL = TextEmbedding(model_name="BAAI/bge-small-en-v1.5")
    return _EMBED_MODEL


@requires_index
def _search_similar(graph: Any, query: str, top_k: int) -> str:
    """Semantic/embedding similarity search."""
    if not query.strip():
        return "Please provide a natural-language query for semantic search."

    # Try to embed the query using a cached fastembed model
    try:
        embedding = list(_get_embedding_model().embed([query]))[0]
    except ImportError:
        return (
            "Semantic search requires `fastembed` to be installed. "
            "Run: pip install fastembed\n"
            "Then run compute_embeddings() to index all entities."
        )
    except Exception as e:
        return f"Embedding failed: {e}"

    try:
        from coderadar._core import search_similar as _ss
        results = _ss(list(embedding), min(top_k, 20))
    except RuntimeError:
        # No embeddings in index — try to auto-compute
        try:
            metrics = graph.compute_embeddings()
            results = _ss(list(embedding), min(top_k, 20))
        except Exception:
            return (
                "No embeddings found and auto-computation failed. "
                "Run codegraph_compute_embeddings first, or "
                "codegraph_reindex with_embeddings=True."
            )

    if not results:
        return f"No semantically similar results found for '{query}'."

    lines = [f"## Semantic Search: `{query}`", f"Found {len(results)} result(s)", ""]
    for i, r in enumerate(results, 1):
        name = r.get("name", "?")
        kind = r.get("kind", "?")
        fp = r.get("file_path", "?")
        sim = r.get("similarity", 0.0)
        lines.append(f"{i}. `{name}` ({kind}) — similarity {sim:.3f}")
        lines.append(f"   File: `{fp}`")
        doc = r.get("docstring")
        if doc:
            lines.append(f"   {doc[:120]}{'...' if len(doc) > 120 else ''}")
        lines.append("")
    return "\n".join(lines)


def _module_children(graph: Any, module_id: str) -> str:
    """List children of a module."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run `coderadar init` first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    if not module_id.strip():
        return "Please provide a module ID (e.g. 'src/main.py::module')."

    module_id = _canonical_entity_id(module_id)

    try:
        from coderadar._core import module_children as _mc
        children = _mc(module_id)
    except Exception as e:
        return f"Module `{module_id}` not found or error: {e}"

    total = sum(len(children.get(k, [])) for k in ("classes", "functions", "imports", "constants"))
    lines = [f"## Module: `{module_id}`", f"{total} children", ""]

    for category in ("classes", "functions", "imports", "constants"):
        items = children.get(category, [])
        if not items:
            continue
        lines.append(f"### {category.title()} ({len(items)})")
        for item in items:
            name = item.get("name", item.get("id", "?"))
            item_id = item.get("id", "")
            line_no = item.get("line", item.get("start_line", ""))
            extra = f" (line {line_no})" if line_no else ""
            lines.append(f"- `{name}`{extra} — `{item_id}`")
        lines.append("")

    return "\n".join(lines)


def _as_of(graph: Any, timestamp: str, query: str, symbols: list[str]) -> str:
    """Query the graph at a past timestamp."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run `coderadar init` first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    if not timestamp:
        return "Please provide an ISO 8601 timestamp (e.g. '2025-01-15T10:00:00Z')."

    try:
        snapshot = graph.as_of(timestamp)
    except Exception as e:
        return f"Temporal query failed: {e}. Ensure Macrame snapshots are enabled."

    names = _parse_names(query, symbols)
    if not names:
        lines = [
            f"## Snapshot at `{timestamp}`",
            "",
            f"Graph loaded at {timestamp}. Provide symbols to explore, or use:",
            f"- `codegraph_query` with timestamp to run Pest queries",
            f"- `search_entities` to find symbols at this point in time",
        ]
        return "\n".join(lines)

    lines = [f"## Snapshot at `{timestamp}`", ""]
    for name in names:
        entity = snapshot.find(name) if hasattr(snapshot, "find") else None
        if entity:
            lines.append(f"**{entity.get('name', name)}** ({entity.get('kind', '?')})")
            lines.append(f"- File: `{entity.get('file_path', '?')}`")
            sig = entity.get("signature")
            if sig:
                lines.append(f"- Signature: `{sig}`")
        else:
            lines.append(f"`{name}` — not found at {timestamp}")
        lines.append("")

    return "\n".join(lines)


def _get_smells(graph: Any, entity_id: str | None, rule_id: str | None) -> str:
    """Run the native smell engine and render findings as markdown."""
    try:
        from coderadar._core import get_smells as _get_smells_rust, graph_stats
        stats = graph_stats()
        if stats.get("functions", 0) == 0 and stats.get("classes", 0) == 0:
            return "No index available. Run codegraph_reindex first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    try:
        findings = _get_smells_rust(entity_id, rule_id)
    except Exception as e:
        return f"Smell detection failed: {e}"

    if not findings:
        scope = []
        if entity_id:
            scope.append(f"entity `{entity_id}`")
        if rule_id:
            scope.append(f"rule={rule_id}")
        suffix = f" for {' and '.join(scope)}" if scope else ""
        return f"## Code smells\n\nNo findings{suffix}."

    lines = ["## Code smells", f"Found {len(findings)} finding(s)", ""]
    for f in findings:
        sev = f.get("severity", "?")
        name = f.get("entity_name") or f.get("entity_id", "?")
        lines.append(
            f"- **[{sev}]** `{f.get('rule_id', '?')}` — "
            f"{name}: {f.get('message', '')}"
        )
        signals = f.get("signals") or {}
        if signals:
            sig = ", ".join(f"{k}={v:g}" for k, v in signals.items())
            lines.append(f"  - signals: {sig}")
    return "\n".join(lines)


def _traverse(
    graph: Any, entity_id: str, direction: str,
    edge_kinds: list[str] | None, max_depth: int,
) -> str:
    """Proper multi-depth BFS edge traversal via MacrameQuery."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run codegraph_reindex first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    if not entity_id.strip():
        return "Please provide an entity ID to traverse from."

    # Production: `graph` is the CodeGraph captured by create_server's
    # closure. When invoked without it (harness / suite) fall back to a
    # CodeGraph attached to the already-analysed global graph.
    if graph is None:
        from coderadar import CodeGraph
        graph = CodeGraph()

    entity = _find_entity(graph, entity_id)
    if not entity:
        return f"Entity `{entity_id}` not found. Try codegraph_search."
    entity_id = _canonical_entity_id(entity_id)

    depth = min(max_depth, 10)
    # Map MCP direction names to MacrameQuery direction
    macrame_direction = {"downstream": "out", "upstream": "in", "both": "both"}.get(direction, "both")

    try:
        results = graph.traverse(entity_id, depth, edge_kinds, macrame_direction)
    except Exception as e:
        return f"Traversal failed: {e}"

    # 2.3: surface silent truncation — count targets the walk could not follow.
    try:
        from coderadar._core import traverse_unresolved
        unresolved = traverse_unresolved(entity_id, depth, edge_kinds or [], macrame_direction)
    except Exception:
        unresolved = 0

    if not results:
        return (
            f"## Traverse from `{entity.get('name', entity_id)}`\n\n"
            f"No neighbors found (direction={direction}, max_depth={depth})"
        )

    # Group by depth
    by_depth: dict[int, list[dict]] = {}
    for r in results:
        d = r.get("depth", 1)
        by_depth.setdefault(d, []).append(r)

    lines = [
        f"## Traverse from `{entity.get('name', entity_id)}`",
        f"Direction: {direction}, max depth: {depth}, "
        f"edge kinds: {edge_kinds or 'all'}",
        f"Found {len(results)} reachable entities",
        "",
    ]
    if unresolved > 0:
        lines.append(
            f"⚠️ Traversal incomplete: {unresolved} outgoing target(s) "
            f"could not be resolved and were excluded from the walk."
        )

    for d in sorted(by_depth.keys()):
        items = by_depth[d]
        lines.append(f"### Depth {d} ({len(items)})")
        for item in items[:15]:
            name = item.get("name", item.get("id", item.get("entity_id", "?")))
            ek = item.get("kind", item.get("edge_type", "?"))
            eid = item.get("id", item.get("entity_id", ""))
            fp = item.get("file_path", "")
            fp_str = f" — `{fp}`" if fp else ""
            lines.append(f"- `{name}` ({ek}){fp_str}")
        if len(items) > 15:
            lines.append(f"  ... and {len(items) - 15} more")
        lines.append("")

    return "\n".join(lines)


def _replace_body(
    graph: Any, entity_id: str, new_body: str,
    expected_hash: str | None, dry_run: bool,
) -> str:
    """Replace a function body."""
    try:
        entity_id = _canonical_entity_id(entity_id)
        plan = graph.plan_body_replacement(entity_id, new_body, expected_hash, dry_run=True)
        if dry_run:
            return _format_mutation_plan(plan) + "\n**To apply:** call again with `dry_run=False`."
        result = graph.apply(plan)
        return _format_mutation_applied(result, plan.unverified_sites)
    except Exception as e:
        return f"Mutation failed: {e}"


def _update_signature(
    graph: Any, entity_id: str, new_signature: str,
    inject_defaults: bool, dry_run: bool,
) -> str:
    """Change a function signature."""
    try:
        entity_id = _canonical_entity_id(entity_id)
        plan = graph.plan_signature_update(
            entity_id, new_signature, inject_defaults=inject_defaults, dry_run=True,
        )
        if dry_run:
            return _format_mutation_plan(plan) + "\n**To apply:** call again with `dry_run=False`."
        result = graph.apply(plan)
        return _format_mutation_applied(result, plan.unverified_sites)
    except Exception as e:
        return f"Mutation failed: {e}"


def _rename(graph: Any, entity_id: str, new_name: str, dry_run: bool) -> str:
    """Rename an entity."""
    try:
        entity_id = _canonical_entity_id(entity_id)
        plan = graph.plan_rename(entity_id, new_name, dry_run=True)
        if dry_run:
            return _format_mutation_plan(plan) + "\n**To apply:** call again with `dry_run=False`."
        result = graph.apply(plan)
        return _format_mutation_applied(result, plan.unverified_sites)
    except Exception as e:
        return f"Mutation failed: {e}"


def _render_entity_code(
    language: str, kind: str, name: str, body: str, decorators: list[str] | None,
) -> str:
    """Render a source snippet for a new entity using language-aware syntax."""
    lang = (language or "").lower()
    kind_norm = (kind or "function").lower()
    body = (body or "").rstrip("\n")
    dec = "\n".join(decorators or [])
    dec_block = (dec + "\n") if dec else ""

    def indent(text: str, spaces: int = 4) -> str:
        pad = " " * spaces
        return "\n".join((pad + line) if line.strip() else line for line in text.split("\n"))

    if kind_norm in ("function", "method", "fn"):
        if lang in ("python", "py"):
            return f"{dec_block}def {name}():\n{indent(body)}\n"
        if lang in ("rust", "rs"):
            return f"{dec_block}pub fn {name}() {{\n{body}\n}}\n"
        if lang == "go":
            return f"{dec_block}func {name}() {{\n{body}\n}}\n"
        if lang in ("javascript", "typescript", "js", "ts", "jsx", "tsx"):
            return f"{dec_block}function {name}() {{\n{body}\n}}\n"
        if lang in ("java",):
            return f"{dec_block}public void {name}() {{\n{body}\n}}\n"
        if lang in ("csharp", "cs"):
            return f"{dec_block}public void {name}() {{\n{body}\n}}\n"
        if lang in ("php",):
            return f"{dec_block}function {name}() {{\n{body}\n}}\n"
        if lang in ("ruby", "rb"):
            return f"{dec_block}def {name}\n{body}\nend\n"
        # generic brace language fallback
        return f"{dec_block}{name}() {{\n{body}\n}}\n"

    if kind_norm in ("class", "struct"):
        if lang in ("python", "py"):
            inner = indent(body) or "    pass"
            return f"{dec_block}class {name}:\n{inner}\n"
        if lang in ("ruby", "rb"):
            return f"{dec_block}class {name}\n{body}\nend\n"
        return f"{dec_block}class {name} {{\n{body}\n}}\n"

    if kind_norm in ("constant", "variable", "const", "var"):
        if lang in ("python", "py"):
            return f"{dec_block}{name} = {body or 'None'}\n"
        if lang == "go":
            return f"{dec_block}const {name} = {body or 'nil'}\n"
        if lang in ("javascript", "typescript", "js", "ts"):
            return f"{dec_block}const {name} = {body or 'null'};\n"
        return f"{dec_block}{name} = {body or 'null'}\n"

    # Unknown kind: emit the body verbatim
    return (body + "\n") if body else ""


def _canonical_file_path(file_path: str) -> str:
    r"""Resolve a file path to the project-relative form the graph stores.

    The graph stores entity IDs as `.\relative\path::name` (Windows
    backslashes, `./`-style prefix). Convert absolute paths to that form so
    create_entity's reindex step matches existing entities instead of
    creating duplicates.
    """
    import os
    if os.path.isabs(file_path):
        try:
            return '.' + os.sep + os.path.relpath(file_path, os.getcwd())
        except ValueError:
            return file_path
    if file_path.startswith('./') or file_path.startswith('.\\'):
        return file_path
    return '.' + os.sep + file_path


def _create_entity(
    graph: Any, file_path: str, language: str, kind: str,
    name: str, body: str, decorators: list[str] | None,
    anchor: str, dry_run: bool,
) -> str:
    """Create a new entity."""
    try:
        code = _render_entity_code(language, kind, name, body, decorators)
        if not code.strip():
            return "Cannot render entity: provide a non-empty body or kind."
        target = _canonical_file_path(file_path)
        # If the anchor is an entity ID (not 'top'/'end'), canonicalize it too
        anchor_norm = anchor or "end"
        if anchor_norm not in ("top", "end"):
            anchor_norm = _canonical_entity_id(anchor_norm)
        plan = graph.plan_create_entity(
            target, anchor_norm, code, dry_run=True,
        )
        if dry_run:
            return _format_mutation_plan(plan) + "\n**To apply:** call again with `dry_run=False`."
        result = graph.apply(plan)
        return _format_mutation_applied(result, plan.unverified_sites)
    except Exception as e:
        return f"Mutation failed: {e}"


def _format_mutation_plan(plan: Any) -> str:
    """Format a MutationPlan for MCP output (dry-run)."""
    lines = [f"## Mutation Plan: `{plan.tool}` (DRY RUN)", ""]
    lines.append(f"- **Plan ID:** `{plan.id}`")
    lines.append(f"- **Affected files:** {len(plan.affected_files)}")

    if plan.diff_preview:
        lines.append("")
        lines.append("### Diff Preview")
        lines.append("```diff")
        for line in plan.diff_preview.split("\n")[:60]:
            lines.append(line)
        if len(plan.diff_preview.split("\n")) > 60:
            lines.append("...")
        lines.append("```")

    if plan.unverified_sites:
        lines.append("")
        lines.append(
            f"⚠️ **WARNING: {len(plan.unverified_sites)} call site(s) could not be "
            f"verified/rewritten. Manual review required.**"
        )
        for site in plan.unverified_sites[:10]:
            lines.append(f"- `{site}`")

    if plan.warnings:
        lines.append("")
        lines.append("### Warnings")
        for w in plan.warnings:
            lines.append(f"- ⚠ {w}")

    return "\n".join(lines)


def _format_mutation_applied(result: Any, unverified_sites: list | None = None) -> str:
    """Format a MutationResult after application."""
    lines = ["## Mutation Applied", ""]
    lines.append(f"- **Status:** {result.status}")
    if result.files_written:
        lines.append(f"- **Files written:** {len(result.files_written)}")
        for f in result.files_written:
            lines.append(f"  - `{f}`")
    if result.syntax_errors:
        lines.append(f"- **Syntax errors:** {len(result.syntax_errors)}")
    if result.backup_path:
        lines.append(f"- **Backup:** `{result.backup_path}`")
    if unverified_sites:
        lines.append("")
        lines.append(
            f"⚠️ **WARNING: {len(unverified_sites)} call site(s) could not be "
            f"verified/rewritten. Manual review required.**"
        )
        for site in unverified_sites[:10]:
            lines.append(f"- `{site}`")
    lines.append("")
    lines.append("Graph has been updated — subsequent queries will reflect the change.")
    return "\n".join(lines)


def _reindex(graph: Any, with_embeddings: bool = False) -> str:
    """Full reindex of the project."""
    try:
        from coderadar._core import analyze as _analyze, graph_stats
        # Use relative root ('.') to keep entity IDs consistent with startup
        # (analyze('.')) — absolute os.getcwd() would change ID prefixes.
        _analyze('.')
        stats = graph_stats()
        lines = [
            "## Reindex Complete",
            "",
            f"- **Files:** {stats.get('file_count', 0)}",
            f"- **Modules:** {stats.get('modules', 0)}",
            f"- **Classes:** {stats.get('classes', 0)}",
            f"- **Functions:** {stats.get('functions', 0)}",
            f"- **Call edges:** {stats.get('call_edges', 0)}",
        ]
        if with_embeddings:
            lines.append("")
            try:
                emb_metrics = graph.compute_embeddings()
                lines.append(f"- **Embeddings generated:** {emb_metrics.get('generated', 0)}")
                lines.append(f"- **Embeddings cached:** {emb_metrics.get('cached', 0)}")
            except Exception as e:
                lines.append(f"- **Embeddings:** failed — {e}")
        return "\n".join(lines)
    except ImportError:
        return "CodeRadar extension not available."
    except Exception as e:
        return f"Reindex failed: {e}"


def _update_file(graph: Any, file_path: str, content: str | None) -> str:
    """Incremental single-file update."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return "No index available. Run codegraph_reindex first."
    except (ImportError, RuntimeError):
        return "CodeRadar extension not available."

    if not file_path.strip():
        return "Please provide a file path."

    try:
        report = graph.update_file(file_path, content)
        if not report.fully_applied:
            # This branch was dead until the Rust side stopped hardcoding a
            # clean parse. tree-sitter recovers rather than failing, so the
            # graph did take entities from the file — just not reliably the
            # ones inside the region it had to recover from.
            return (
                f"## Update Incomplete\n\n"
                f"- **File:** `{file_path}`\n"
                f"- **Parse quality:** {report.parse_quality}\n"
                f"- **Parse errors:** {report.parse_errors}\n"
                f"\nThe file was indexed from a recovered parse — entities in "
                f"the broken region may be missing or wrong. Fix the syntax "
                f"and update again.\n"
            )
        return (
            f"## File Updated\n\n"
            f"- **File:** `{file_path}`\n"
            f"- Graph refreshed from {'provided content' if content else 'disk'}.\n"
        )
    except Exception as e:
        return f"Update failed: {e}"


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


def _canonical_entity_id(entity_id: str) -> str:
    """Resolve an entity ID to its canonical in-graph form.

    Handles both relative (`.\\...`) and absolute (`D:\\...`) path prefixes
    by probing the graph with a few candidate forms.
    """
    try:
        from coderadar._core import lookup_entity
    except ImportError:
        return entity_id
    if lookup_entity(entity_id):
        return entity_id

    import os
    candidates: list[str] = []

    # Relative → absolute
    if entity_id.startswith('.\\') or entity_id.startswith('./'):
        candidates.append(os.path.join(os.getcwd(), entity_id[2:]))

    # Absolute → relative (with ./ prefix and bare)
    if os.path.isabs(entity_id):
        try:
            rel = os.path.relpath(entity_id, os.getcwd())
            candidates.append('.' + os.sep + rel)
            candidates.append(rel)
        except ValueError:
            pass

    # Normalize separators both ways
    normalized: list[str] = []
    for c in candidates:
        normalized.append(c.replace('/', '\\'))
        normalized.append(c.replace('\\', '/'))

    for c in normalized:
        if lookup_entity(c):
            return c
    return entity_id


def _find_entity(graph: Any, entity_id: str) -> dict | None:
    try:
        from coderadar._core import lookup_entity
        return lookup_entity(_canonical_entity_id(entity_id))
    except (ImportError, RuntimeError):
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
