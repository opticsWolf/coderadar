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
import os
from pathlib import Path
from typing import Any, Literal

import structlog
from mcp.server import MCPServer

from coderadar import ops, render

logger = structlog.get_logger(__name__)

# ── Instructions (§26.1 item 2) ──────────────────────────────────────────

SERVER_INSTRUCTIONS = """# CodeRadar — live semantic graph of your codebase

A pre-built graph of every symbol, call edge and file in the workspace
(41 languages). Use it before and while editing, not only for questions: one
call returns verbatim source plus who calls it and what it affects.

## Start with coderadar_explore

Any structural or flow question — "how does X work?", "the flow from X to
Y", reading a symbol before editing it — is one `coderadar_explore` call
naming the symbol(s). It returns line-numbered source grouped by file (safe
to Edit from), the call paths between the symbols and a blast-radius summary.
Need more? Call it again with more specific names.

## Other read tools

- `coderadar_search` — find symbols by keyword when you don't know the name
- `coderadar_search_similar` — semantic search (embeddings via `coderadar_compute_embeddings`, computed on first use)
- `coderadar_node` — one entity's details and neighbours; `coderadar_module_children` — a module's contents
- `coderadar_affected` — transitive callers (blast radius), centrality-ranked; `coderadar_callers` / `coderadar_callees` — direct 1-hop neighbours
- `coderadar_traverse` — any edge kind, upstream or downstream
- `coderadar_query` — structured queries, e.g. `functions where caller_count == 0` (docs/query-language.md)
- `coderadar_as_of` — the graph at a past timestamp
- `coderadar_resolve` — framework references: routes (`/users/:id`), `*Model` / `*View` names
- `coderadar_get_smells`, `coderadar_dead_code`, `coderadar_find_clones`, `coderadar_find_scaffolding` — quality findings. Dead code is ranked evidence, not proof: check `coderadar_affected` before deleting.

## Editing

`coderadar_replace_body`, `coderadar_update_signature`, `coderadar_rename`
and `coderadar_create_entity` edit the file and the graph together. They
dry-run by default: review the diff, then apply with dry_run=False.
After editing with Read/Edit instead, sync with `coderadar_update_file` (one
file) or `coderadar_reindex` (many files). `coderadar_status` shows what is
served and how fresh the index is.

## Anti-patterns

- **Don't re-verify results with grep.** They come from a full parse.
- **Don't grep or Read first** to find indexed code, and don't reconstruct a flow by hand: name the endpoints and explore.
- **Files flagged "⚠ changed on disk after index sync"** are stale: Read those. Every other file is fresh.
- **No index for a project?** Use built-in tools. Indexing is the user's decision: mention `coderadar init`, don't run it.
- **"Indexing in progress" is not an error.** Retry in a few seconds rather than falling back to grep.

## One project at a time

The server serves one project. Every tool takes an optional `project_path`:
a path inside the served project is accepted, another project is refused
with the reason. `coderadar_set_project` switches wholesale (config re-read,
background re-index, earlier event ids invalid).

The same operations exist on the shell under the same names, hyphenated
(`coderadar explore`, `coderadar get-smells`, `coderadar status`, ...).
"""


# The directory the MCP client launched this process from, captured before
# any chdir (cli.py's `mcp serve` sets it). It keys the last-project record
# (P2-3): `set_project` records the switch against it, so the next launch
# from the same directory can resume the chosen project. None in tests and
# directly-constructed servers, where recording is skipped.
_LAUNCH_CWD: Path | None = None


def set_launch_cwd(directory: Path | None) -> None:
    """Record the launch directory for the lifetime of this process."""
    global _LAUNCH_CWD
    _LAUNCH_CWD = directory


# ── Server Factory ────────────────────────────────────────────────────────

def create_server(graph: Any) -> MCPServer:
    """Create an MCP v2 server wrapping the given CodeGraph instance.

    Tools capture `graph` via closure — no globals, no thread-locals.
    Every tool is a plain, type-hinted Python function; MCPServer derives
    JSON Schema and routing from the function signature.
    """
    from coderadar import __version__

    mcp = MCPServer(
        "CodeRadar",
        version=__version__,
        instructions=SERVER_INSTRUCTIONS,
    )

    # `roots/list` is a server-to-client request and MCP gives the server no
    # `initialize` hook to send one from — awaiting one during the handshake
    # deadlocks. Middleware is the one place that sees every inbound request
    # and holds the session, so the top rung of the path ladder is climbed
    # here, on the first tool call, and only if startup's answer was a guess.
    from .lazy import make_middleware
    mcp.middleware.append(make_middleware())

    # Any inbound message means the client is there, which is what the
    # handshake timeout is waiting to learn.
    from .lifecycle import make_middleware as make_lifecycle_middleware
    mcp.middleware.append(make_lifecycle_middleware())

    # ── coderadar_explore (§26.2 primary tool) ─────────────────────────

    @mcp.tool(
        description=(
            "Explore the code graph: given symbol or file names (or a natural-language "
            "question), returns the verbatim line-numbered source of the relevant "
            "symbols grouped by file, PLUS the call paths between them and a blast-radius "
            "summary of what depends on them. Use this instead of grep + Read for any "
            "structural or flow question. For multiple symbols, pass them together in "
            "one call to get the relationships between them."
            " " + _ENTITY_ID_GRAMMAR
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_explore(
        query: str = "",
        symbols: list[str] | None = None,
        direction: Literal["downstream", "upstream", "both"] = "both",
        max_files: int = 8,
        project_path: str | None = None,
    ) -> str:
        """Primary code exploration tool."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _explore(graph, query, symbols or [], direction, max_files)

    # ── coderadar_node (§26.2 depth tool) ──────────────────────────────

    @mcp.tool(
        description=(
            "Get full details for a specific entity identified via coderadar_explore. "
            "Returns complete metadata, source location, docstring, decorators, "
            "and optionally immediate neighbors (callers and callees)."
            " " + _ENTITY_ID_GRAMMAR
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_node(
        entity_id: str,
        include_neighbors: bool = False,
        project_path: str | None = None,
    ) -> str:
        """Depth drill-down for a single entity."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _node_detail(graph, entity_id, include_neighbors)

    # ── coderadar_search (§26.2 discovery tool) ────────────────────────

    @mcp.tool(
        description=(
            "Search for symbols by keyword or natural-language description when "
            "you don't know the exact symbol name. Returns ranked results with "
            "snippets. Use this to discover what's available before calling explore. "
            "kind filters to one of: function | class | type_alias | constant | module | import."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_search(
        query: str,
        kind: str | None = None,
        top_k: int = 10,
        project_path: str | None = None,
    ) -> str:
        """Symbol discovery via keyword search."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _search(graph, query, kind, top_k)

    # ── coderadar_affected (§26.2 impact tool) ─────────────────────────

    @mcp.tool(
        description=(
            "Find all entities transitively affected by a given entity — the "
            "blast radius. Traverses upstream through callers to show the full "
            "dependency tree, ordered within each depth by harmonic centrality "
            "(the three most-depended-on ids carry a star marker). "
            "Use this before editing to understand the impact."
            " " + _ENTITY_ID_GRAMMAR
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_affected(
        entity_id: str,
        max_depth: int = 5,
        project_path: str | None = None,
    ) -> str:
        """Transitive impact analysis."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _affected(graph, entity_id, max_depth)

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
        project_path: str | None = None,
    ) -> str:
        """Framework-aware reference resolution."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _resolve_ref(graph, name, limit)

    # ── coderadar_query — Pest graph query language ───────────────────

    @mcp.tool(
        description=(
            "Execute a structured graph query against the indexed codebase. "
            "Shape: '<entity> [select ...] [where ...] [group by ...] [order by ...] "
            "[limit N]'. Entities: modules, classes, functions, methods, constants, "
            "entities, imports, calls, fields. Operators: ==, !=, <, <=, >, >=, "
            "contains, matches (regex), starts_with, ends_with, in — combined with "
            "and/or/not. Examples: 'classes where inherits_from contains \"BaseModel\"', "
            "'methods where is_async == true', 'functions where name starts_with \"test_\"', "
            "'functions where caller_count == 0 and not name matches \"^test_\"'. "
            "Every row carries id, file_path, kind and parent_id, so a hit can be "
            "fed to coderadar_affected / coderadar_rename. Unknown fields are "
            "rejected with the available list instead of returning nothing. "
            "Full field reference: docs/query-language.md."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_query(
        query: str,
        project_path: str | None = None,
    ) -> str:
        """Execute a Pest graph query."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _query_graph(graph, query)

    # ── coderadar_search_similar — embedding/semantic search ──────────

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
    def coderadar_search_similar(
        query: str,
        top_k: int = 10,
        project_path: str | None = None,
    ) -> str:
        """Semantic similarity search."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _search_similar(graph, query, top_k)

    # ── coderadar_compute_embeddings — generate embedding vectors ────

    @mcp.tool(
        description=(
            "Compute and store embedding vectors for all entities in the index "
            "(functions, classes, modules, imports, constants, type aliases). "
            "Uses fastembed (BAAI/bge-small-en-v1.5) for local embedding generation. "
            "This is a prerequisite for coderadar_search_similar — without embeddings, "
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
    def coderadar_compute_embeddings(
        model_name: str | None = None,
        project_path: str | None = None,
    ) -> str:
        """Compute embeddings for semantic search."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _compute_embeddings(graph, model_name)

    # ── coderadar_module_children — structural discovery ─────────────

    @mcp.tool(
        description=(
            "List all children (classes, functions, imports, constants) of a module. "
            "The module ID is typically '{file_path}::module' — get it from coderadar_explore "
            "or coderadar_search results. Use to understand a module's structure before editing."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_module_children(
        module_id: str,
        project_path: str | None = None,
    ) -> str:
        """List module children."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _module_children(graph, module_id)

    # ── coderadar_callers / coderadar_callees — 1-hop neighbourhood ──

    @mcp.tool(
        description=(
            "List the direct callers of one entity (1 hop upstream). "
            "For the transitive blast radius use coderadar_affected instead. "
            "Unknown ids are reported as unknown, not as callerless."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_callers(
        entity_id: str,
        project_path: str | None = None,
    ) -> str:
        """List direct callers."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _callers(graph, entity_id)

    @mcp.tool(
        description=(
            "List the direct callees of one entity (1 hop downstream). "
            "An entity with no callees reports empty; unknown ids report unknown."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_callees(
        entity_id: str,
        project_path: str | None = None,
    ) -> str:
        """List direct callees."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _callees(graph, entity_id)

    # ── coderadar_as_of — temporal query ─────────────────────────────

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
    def coderadar_as_of(
        timestamp: str,
        query: str = "",
        symbols: list[str] | None = None,
        project_path: str | None = None,
    ) -> str:
        """Temporal graph query."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _as_of(graph, timestamp, query, symbols or [])

    # ── coderadar_traverse — edge traversal ──────────────────────────

    @mcp.tool(
        description=(
            "Traverse the graph from a starting entity along specified edge kinds. "
            "Direction: 'downstream' (callees), 'upstream' (callers), 'both'. "
            "Edge kinds: 'calls', 'imports', 'inherits' (alias 'extends'), 'overrides'; "
            "default all. Returns a tree of linked entities. "
            "Use for custom flow analysis beyond explore/affected."
            " " + _ENTITY_ID_GRAMMAR
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_traverse(
        entity_id: str,
        direction: Literal["downstream", "upstream", "both"] = "both",
        edge_kinds: list[str] | None = None,
        max_depth: int = 3,
        project_path: str | None = None,
    ) -> str:
        """Generic edge traversal."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _traverse(graph, entity_id, direction, edge_kinds, max_depth)

    # ── coderadar_get_smells — code smell detection ─────────────────

    @mcp.tool(
        description=(
            "Detect code smells (architectural issues) across the indexed codebase. "
            "Run without arguments to list all findings, or filter by entity_id "
            "(exact match) and/or rule_id (one of: god-class, long-method, "
            "long-parameter-list, deep-nesting, data-class, "
            "high-cyclomatic-complexity, brain-method, excessive-returns, "
            "too-many-fields, dead-code, dead-branch, intra-dead-statements). "
            "Each finding carries a severity, a human message, "
            "and the metric signals (WMC, CBO, LOC, cyclomatic, nesting_depth, "
            "param_count, field_count, return_count, max_method_cyclomatic) that "
            "triggered it. strictness selects the threshold profile: 'strict' "
            "catches more (thresholds drop ~40%), 'loose' only egregious cases "
            "(thresholds rise ~80%); default 'normal'."
            " " + _ENTITY_ID_GRAMMAR
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_get_smells(
        entity_id: str | None = None,
        rule_id: str | None = None,
        project_path: str | None = None,
        strictness: str = "normal",
    ) -> str:
        """Detect architectural code smells."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _get_smells(graph, entity_id, rule_id, strictness)

    # ── coderadar_dead_code — unreachable-code detection ───────────────

    @mcp.tool(
        description=(
            "Find dead code: functions unreachable from any entry point "
            "(the mirror image of coderadar_affected). Entry points: mains, "
            "framework handlers (routes, CLIs, pytest, Qt slots), dunder methods, "
            "overrides of external bases (e.g. Qt paintEvent), Protocol/ABC "
            "members, __all__ and package exports, project scripts, and tests. "
            "Calls, callbacks passed as values and virtual dispatch all extend "
            "liveness. Each finding carries kind (unreachable | "
            "transitively-dead | test-only | rta-dead), tier, confidence, "
            "removable lines and the evidence behind it, ranked "
            "most-safely-deletable first. rta-dead is the weakest kind: the "
            "method lives only through dispatch on a class never constructed "
            "in the indexed root. Verify with coderadar_affected before "
            "removing anything."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    @requires_index
    def coderadar_dead_code(
        project_path: str | None = None,
        min_confidence: float = 0.6,
        include_test_reachable: bool = False,
        max_findings: int = 100,
    ) -> str:
        """Find functions unreachable from any entry point."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _dead_code(graph, min_confidence, include_test_reachable, max_findings)

    # ── coderadar_find_clones — token-level clone detection ───────────

    @mcp.tool(
        description=(
            "Detect syntactic code clones (Types 1-3): identical bodies, "
            "renamed bodies, and near-duplicates above a similarity floor. "
            "Each group lists its instances (entity_id, file, byte span) with "
            "a confidence tier; groups are ranked largest first. Type-4 "
            "(semantic-only) similarity is covered by coderadar_search_similar. "
            "min_lines filters trivially short bodies; min_similarity applies "
            "to Type-3 candidates."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    @requires_index
    def coderadar_find_clones(
        project_path: str | None = None,
        min_lines: int = 10,
        min_similarity: float = 0.8,
        max_groups: int = 100,
    ) -> str:
        """Find duplicated code across the indexed codebase."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _find_clones(graph, min_lines, min_similarity, max_groups)

    # ── coderadar_find_scaffolding — AI-scaffolding & secrets scan ────

    @mcp.tool(
        description=(
            "Detect AI-coding scaffolding debt: TODO/FIXME/HACK/WIP/implement-me "
            "comment markers, placeholder bodies (pass/todo!/NotImplementedError), "
            "temp-file naming (temp_*/backup_*/old_*), and — opt-in — hardcoded secrets. "
            "Secret findings are always redacted to their first 8 characters plus '***'; "
            "full matches never leave this process. Gitignored files are skipped. "
            "max_findings applies PER finding kind so one noisy kind cannot crowd out "
            "the others; the result ends with a scan-coverage footer."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    @requires_index
    def coderadar_find_scaffolding(
        project_path: str | None = None,
        include_secrets: bool = False,
        max_findings: int = 100,
    ) -> str:
        """Scan for AI scaffolding markers, stubs, temp files and secrets."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _find_scaffolding(include_secrets, max_findings)

    # ── coderadar_replace_body ────────────────────────────────────

    @mcp.tool(
        description=(
            "Replace the body of a function/method. "
            "With dry_run=True (default): returns a diff preview for review. "
            "With dry_run=False: writes the file AND updates the graph atomically. "
            "Best practice: call first with dry_run=True to review, then call "
            "again with dry_run=False to apply. Use expected_hash to verify the "
            "current body matches before replacing (safety check). Indentation: "
            "the replacement is re-based to the function's body column — pass it "
            "either unindented or copied verbatim from the source; both produce "
            "identical files. Do not include the def/signature line or decorators."
            " " + _ENTITY_ID_GRAMMAR
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
        expected_hash: str | None = None,
        dry_run: bool = True,
        project_path: str | None = None,
    ) -> str:
        """Replace a function body."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
            " " + _ENTITY_ID_GRAMMAR
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
        project_path: str | None = None,
    ) -> str:
        """Change a function signature."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _update_signature(graph, entity_id, new_signature, inject_defaults, dry_run)

    # ── coderadar_rename ────────────────────────────────────────────

    @mcp.tool(
        description=(
            "Rename an entity (function, class, variable) and all references. "
            "With dry_run=True: shows all files and references that need updating. "
            "With dry_run=False: renames the definition and ALL references, "
            "updates the graph. Covers definition site and all usages."
            " " + _ENTITY_ID_GRAMMAR
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
        project_path: str | None = None,
    ) -> str:
        """Rename an entity."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
            "syntax for common languages. For function-like kinds, pass `signature` "
            "with the complete header to write verbatim, e.g. "
            "`fn sync_status_text(store: &Store) -> String` or "
            "`def save(self, name: str) -> None` — omit it only for parameter-less "
            "helpers, whose header is rendered from `name` alone."
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
        decorators: list[str] | None = None,
        anchor: str = "end",
        signature: str | None = None,
        dry_run: bool = True,
        project_path: str | None = None,
    ) -> str:
        """Create a new entity."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _create_entity(graph, file_path, language, kind, name, body,
                              decorators, anchor, signature, dry_run)

    # ── coderadar_reindex — full graph refresh ───────────────────────

    @mcp.tool(
        description=(
            "Re-index the entire project to refresh the code graph. "
            "Use after batch edits when you've changed many files and want "
            "a guaranteed-fresh index. Slower than coderadar_update_file but "
            "always correct. Cheap by default (only changed files); set full=True "
            "to walk the whole tree (e.g. after config changes). "
            "Set with_embeddings=True to also compute embedding "
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
    def coderadar_reindex(
        with_embeddings: bool = False,
        full: bool = False,
        project_path: str | None = None,
    ) -> str:
        """Bring the index up to date; `full` walks the whole tree."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _reindex(graph, with_embeddings, full)

    # ── coderadar_update_file — incremental single-file sync ────────

    @mcp.tool(
        description=(
            "Incrementally update the graph after editing a single file. "
            "Call this after using Read/Edit to modify source code, before "
            "the next coderadar_explore or coderadar_affected call. "
            "Faster than coderadar_reindex — only re-parses one file. "
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
    def coderadar_update_file(
        file_path: str,
        content: str | None = None,
        project_path: str | None = None,
    ) -> str:
        """Incrementally sync one file."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _update_file(graph, file_path, content)

    # ── coderadar_set_project — switch the served project ───────────

    @mcp.tool(
        description=(
            "Switch this server to a different project. All other tools then "
            "verify their project_path against this root. Pass any directory, "
            "subdirectory or file inside the target project — the nearest "
            ".coderadar marker at or above it defines the root; pass "
            "confirm=true only if the directory has no marker and you want it "
            "served anyway. Config and mutation policy are re-read from the "
            "new project, and indexing restarts in the background. Switching "
            "replaces the graph wholesale: calls racing a switch get the old "
            "or new answer, never a mix."
        ),
        annotations={
            "read_only_hint": False,
            "destructive_hint": False,
            "idempotent_hint": False,
            "open_world_hint": True,
        },
    )
    def coderadar_set_project(project_path: str, confirm: bool = False) -> str:
        """Switch the served project."""
        return _set_project(project_path, confirm)

    # ── coderadar_status — what is served, how fresh ────────────────

    @mcp.tool(
        description=(
            "Report the project this server serves, its config and store, "
            "whether the index is loaded, how long ago it was synced, and "
            "its counts (files, modules, classes, functions, call edges). "
            "Answers even while indexing is in progress or no index exists."
        ),
        annotations={
            "read_only_hint": True,
            "destructive_hint": False,
            "idempotent_hint": True,
            "open_world_hint": False,
        },
    )
    def coderadar_status(project_path: str | None = None) -> str:
        """Served project and index freshness."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _status()

    return mcp


# ── Entry Point ──────────────────────────────────────────────────────────

def serve(graph: Any) -> None:
    """Run the MCP server over stdio (blocking).

    Returns when the transport ends — stdin closing is the client hanging
    up, and there is nothing to serve after that. The two lifecycle guards
    cover the cases where the transport never ends on its own: a client that
    connects and never speaks, and a client that is killed rather than
    closed.
    """
    from .lifecycle import install

    handshake, watchdog = install()
    server = create_server(graph)
    try:
        server.run(transport="stdio")
    finally:
        # stdin closed, or the transport failed. Either way the client is
        # gone; stop the watchdog so a test or an embedding caller is not
        # left with a thread that outlives the server it was watching.
        handshake.disarm()
        watchdog.stop()


# ── Tool Implementations ─────────────────────────────────────────────────

def _served_root() -> Path:
    """The root this server currently serves.

    The lazy retry handle holds it once startup ran; a directly constructed
    server (tests, embedders) falls back to the process cwd, by the same
    reasoning that made serve chdir onto the root.
    """
    from coderadar.mcp import lazy

    retry = lazy.current()
    return retry.resolved.path if retry is not None else Path.cwd().resolve()


def _wrong_project(project_path: str | None) -> str | None:
    """Answer honestly when a tool is asked about a project we are not serving.

    Every tool takes an optional `project_path` because agents working across
    repositories will pass one, and a tool that silently ignores it answers a
    question nobody asked — about the wrong codebase, with no sign that
    anything went wrong.

    This build serves one project at a time: the core keeps a single
    GLOBAL_GRAPH and `analyze` replaces it wholesale rather than merging. But
    the argument is read the way agents mean it: the selector walks up from
    whatever was passed — a file, a subdirectory, or the root itself — to the
    nearest `.coderadar` marker, so anything inside the served project is
    accepted. Only a path that lands in some other project is refused, with
    the reason and the way out rather than a quietly wrong answer.

    Returns None when the call may proceed, or the message to return instead.
    """
    if not project_path:
        return None

    from coderadar.mcp.roots import resolve_selector

    selected = resolve_selector(project_path)
    if selected is None:
        return (
            f"`{project_path}` names no readable directory, so no project "
            "can be selected from it. Pass a directory (or any file inside "
            "one) and retry."
        )

    served = _served_root()
    # Windows paths keep their drive-letter casing through resolve(); compare
    # case-insensitively so D:/User and d:/user are one directory everywhere.
    if os.path.normcase(str(selected.path)) == os.path.normcase(str(served)):
        return None

    return (
        f"This server is serving `{served}` and cannot answer for "
        f"`{selected.path}`.\n\n"
        "One project at a time in this build: the index is a single "
        "in-process graph. To ask about that other project instead, call "
        f"`coderadar_set_project` with `{selected.path}` — this server "
        "switches there and re-indexes. Or drop the `project_path` argument "
        f"to keep asking about `{served}`."
    )


def _no_index_message() -> str:
    """Say where we looked, so the agent can tell us we looked in the wrong place.

    "No index available. Run `coderadar init`" was a dead end: it named no
    directory, so an agent served the wrong project had no way to notice, and
    an agent in the right project had no way to tell whether the index was
    missing or merely empty. This states the root, how it was chosen, and the
    one action that fixes each case.
    """
    from coderadar.mcp import lazy

    retry = lazy.current()
    if retry is None:
        return (
            "No index available for this project.\n\n"
            "Run `coderadar init` in the project root, then retry this call."
        )

    resolved = retry.resolved
    lines = [
        (
            f"No code was indexed under `{resolved.path}`, which is where this "
            f"server is serving from (chosen from: {resolved.source})."
        ),
        "",
    ]
    if resolved.confirmed:
        lines += [
            (
                "That directory does carry a `.coderadar` marker, so it is very "
                "likely the right project and simply has no indexed code yet."
            ),
            "",
            "Run `coderadar_reindex` to index it now.",
        ]
    else:
        lines += [
            (
                "Nothing on disk confirmed that directory as a project root — no "
                "`.coderadar/` or `.coderadar.toml` was found at or above it."
            ),
            "",
            (
                "If that is the wrong project, call `coderadar_set_project` with "
                "the right directory to switch this server there, restart with "
                "`coderadar mcp serve --path <project root>`, or run "
                "`coderadar init` in the right directory so it can be found "
                "automatically."
            ),
            "",
            "If it is the right project, run `coderadar_reindex` to index it.",
        ]
    return "\n".join(lines)


def _set_project(project_path: str, confirm: bool = False) -> str:
    """Switch the served project, and say what happened.

    One project at a time is the shape of the core — a single GLOBAL_GRAPH
    that `analyze` replaces wholesale — so switching means re-running the
    whole start-up sequence against the new root: config in, process moved,
    index restarted. Every step already exists from launch; this orders them
    for a tool call instead.

    The order matters. Config first, so `[mutation]` policy and excludes are
    read from the *new* project before anything walks it; chdir second, so
    entity ids and every cwd-relative helper agree with the root; restart
    last, so the generation bump cannot race the config swap.

    An explicit tool call outranks everything: the lazy roots/list retry is
    retired for the rest of the connection, because an agent that named the
    project it wants must not be second-guessed by whatever the host declares
    as its workspace.
    """
    from coderadar.config import activate_config
    from coderadar.mcp import lazy, startup
    from coderadar.mcp.roots import adopt_project_root, resolve_selector

    selected = resolve_selector(project_path)
    if selected is None:
        return (
            f"`{project_path}` names no readable directory, so no project "
            "can be selected from it. Pass a directory (or any file inside "
            "one) and retry."
        )

    if not selected.confirmed and not confirm:
        return (
            f"No `.coderadar/` or `.coderadar.toml` was found at or above "
            f"`{selected.path}`, so nothing confirms that directory as a "
            "project root.\n\n"
            "Run `coderadar init` there if it should be one, or re-call "
            "with confirm=true to serve it anyway (unmarked roots are "
            "served as bare guesses, same as at startup)."
        )

    # Already there? Say so rather than silently re-indexing the same tree.
    served_now = _served_root()
    if os.path.normcase(str(selected.path)) == os.path.normcase(str(served_now)):
        return (
            f"Already serving `{selected.path}` — nothing to switch. Use "
            "`coderadar_reindex` to refresh the index."
        )

    lines = [f"Switched to `{selected.path}`"]
    if selected.confirmed:
        lines[0] += f" (marker {selected.marker.name})"
    else:
        lines[0] += " — unconfirmed: no marker found, serving on your say-so"

    # 1. Config from the NEW project, before anything reads it.
    try:
        activated = activate_config(selected.path)
        if activated.ignored:
            lines.append(
                f"Config: {len(activated.ignored)} setting(s) with no "
                f"consumer were ignored ({', '.join(activated.ignored[:3])}" +
                (", ..." if len(activated.ignored) > 3 else "") + ")."
            )
    except Exception as exc:  # noqa: BLE001 — a broken config must not strand the server on the old project
        lines.append(
            f"WARNING: config not applied ({type(exc).__name__}: {exc}) — "
            "this project runs on defaults, including default mutation "
            "policy."
        )
        structlog.get_logger(__name__).warning(
            "mcp.set_project.config_failed", root=str(selected.path),
            error=str(exc))

    # 2. Move the process: entity ids and cwd-relative helpers follow.
    adopt_project_root(selected)

    # 3. Re-index in the background; ensure_ready makes callers wait or
    #    report progress, never answer from the old graph.
    index = startup.current()
    if index is not None:
        index.restart(".")
        lines.append("Indexing restarted in the background — tool calls "
                     "will wait for it or report progress.")
    else:
        lines.append("No background index handle in this process — call "
                     "`coderadar_reindex` to build the new graph.")

    # 4. The explicit choice outranks the client's workspace forever after.
    retry = lazy.current()
    if retry is not None:
        retry.mark_user_chosen(selected)

    # 5. Remember this launch directory for the next session (P2-3). The
    #    client starts us from a fixed place; the agent has now declared
    #    which project it wants, so the next launch from the same place
    #    resumes here. Best-effort: recording can never fail the switch.
    if _LAUNCH_CWD is not None:
        from coderadar.project_state import record_project
        record_project(_LAUNCH_CWD, selected.path)

    structlog.get_logger(__name__).info(
        "mcp.project.switched", to=str(selected.path),
        confirmed=selected.confirmed)
    return "\n".join(lines)


NO_EXTENSION_MESSAGE = (
    "The CodeRadar native extension is not available in this environment, so "
    "no code intelligence can be served.\n\n"
    "Install it with `uv run maturin develop --release`, then restart the "
    "MCP server. Until then, fall back to reading files directly."
)

#: Entity-ID grammar (Issue 5): appended to every tool description that
#: takes an entity id, so agents stop guessing spellings. The copy/paste
#: source is always a previous tool result; this is the shape to expect.
_ENTITY_ID_GRAMMAR = (
    "Entity ids look like `path/to/file.py::Qualified.name` "
    "(project-root-relative, forward slashes, no `./` prefix; methods as "
    "`File::Class.member`, modules as `File::module`). Absolute paths, "
    "backslashes, and a leading `./` are also accepted, and a dotted "
    "qualified name (`app.helpers.combine`) resolves when its module prefix "
    "matches. `external::name` marks a callee outside the index (not an "
    "entity — it resolves nothing further)."
)


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
        # The index is built on a background thread so the handshake can be
        # answered immediately; this is where a handler that arrived early
        # waits for it. Idempotent, so it is also what starts the index if
        # nothing else has yet.
        from coderadar.mcp.startup import ensure_ready, progress_message
        outcome = ensure_ready()
        if not outcome.ready:
            return progress_message(outcome)

        try:
            from coderadar._core import graph_stats
            if graph_stats().get("modules", 0) == 0:
                return _no_index_message()
        except ImportError:
            return NO_EXTENSION_MESSAGE
        except RuntimeError:
            # No graph loaded — the state this guard exists for.
            return _no_index_message()
        return func(*args, **kwargs)

    return wrapper



def _op_message(e: ops.OpError, failed: str | None = None) -> str:
    """Word an ops error for an agent. `failed` names the operation for
    engine failures ("Dead-code detection failed: …"); analyses also
    prefix argument errors with "Invalid request:"."""
    if isinstance(e, ops.NoExtension):
        return NO_EXTENSION_MESSAGE
    if isinstance(e, ops.NoIndex):
        return _no_index_message()
    if isinstance(e, ops.NotFound):
        return e.detail or render.not_found(e.entity_id, e.candidates)
    if isinstance(e, ops.InvalidRequest):
        return f"Invalid request: {e}" if failed else str(e)
    if isinstance(e, ops.MutationFailed):
        return render.mutation_error(str(e))
    return f"{failed or 'Request failed'}: {e}"


def _stale_prefix(entity: dict) -> str:
    """The "changed on disk" banner for an entity's file, or ""."""
    fp = entity.get("file_path", "")
    if fp:
        stale = ops.stale_files([fp])
        if stale:
            return render.stale_banner(stale, [fp]) + "\n"
    return ""


# ── Read tools ───────────────────────────────────────────────────────────

@requires_index
def _explore(
    graph: Any, query: str, symbols: list[str],
    direction: str, max_files: int,
) -> str:
    """Source plus call paths for the named symbols."""
    try:
        result = ops.explore(query, symbols, direction, max_files)
    except ops.InvalidRequest:
        return render.explore_usage()
    except ops.NotFound:
        return render.explore_miss(ops.parse_names(query, symbols))
    except ops.OpError as e:
        return _op_message(e)
    return render.explore(result)


@requires_index
def _node_detail(graph: Any, entity_id: str, include_neighbors: bool) -> str:
    """Get full entity details."""
    try:
        entity = ops.node(entity_id, include_neighbors)
    except ops.OpError as e:
        return _op_message(e)
    return _stale_prefix(entity) + render.node(entity)


@requires_index
def _search(graph: Any, query: str, kind: str | None, top_k: int) -> str:
    """Keyword search for symbols."""
    try:
        results = ops.search(query, kind, top_k)
    except ops.OpError as e:
        return _op_message(e)
    return render.search(query, kind, results)


@requires_index
def _affected(graph: Any, entity_id: str, max_depth: int) -> str:
    """Transitive impact analysis."""
    try:
        result = ops.affected(entity_id, max_depth)
    except ops.OpError as e:
        return _op_message(e)
    return _stale_prefix(result["entity"]) + render.affected(entity_id, result)


def _query_graph(graph: Any, query: str) -> str:
    """Run a graph query."""
    try:
        rows = ops.query(query)
    except ops.InvalidRequest as e:
        if not query.strip():
            return render.query_usage()
        return f"Query failed: {e}"
    except ops.EngineError as e:
        return f"Query failed: {e}"
    except ops.OpError as e:
        return _op_message(e)
    return render.query(query, rows)


def _compute_embeddings(graph: Any, model_name: str | None = None) -> str:
    """Compute embeddings for every indexed entity."""
    try:
        metrics = ops.compute_embeddings(model_name)
    except ops.OpError as e:
        return _op_message(e)
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return f"Embedding generation failed: {e}\n\nEnsure fastembed is installed: pip install fastembed"
    return render.compute_embeddings(metrics)


@requires_index
def _search_similar(graph: Any, query: str, top_k: int) -> str:
    """Semantic/embedding similarity search."""
    try:
        results = ops.search_similar(query, top_k)
    except ops.MissingDependency:
        return render.FASTEMBED_MISSING
    except ops.NoEmbeddings:
        return render.NO_EMBEDDINGS
    except ops.EngineError as e:
        return str(e)
    except ops.OpError as e:
        return _op_message(e)
    return render.search_similar(query, results)


def _module_children(graph: Any, module_id: str) -> str:
    """List children of a module."""
    try:
        result = ops.module_children(module_id)
    except ops.OpError as e:
        return _op_message(e)
    return render.module_children(result)


def _callers(graph: Any, entity_id: str) -> str:
    """Direct callers of one entity (R2-16 unknown-vs-callerless kept)."""
    try:
        results = ops.callers(entity_id)
    except ops.OpError as e:
        return _op_message(e)
    if not results and ops.find_entity(entity_id) is None \
            and not entity_id.startswith("external::"):
        return f"Unknown entity: {entity_id}"
    return render.callers(entity_id, results)


def _callees(graph: Any, entity_id: str) -> str:
    """Direct callees of one entity (R2-16 unknown-vs-empty kept)."""
    try:
        results = ops.callees(entity_id)
    except ops.OpError as e:
        return _op_message(e)
    if not results and ops.find_entity(entity_id) is None \
            and not entity_id.startswith("external::"):
        return f"Unknown entity: {entity_id}"
    return render.callees(entity_id, results)


def _as_of(graph: Any, timestamp: str, query: str, symbols: list[str]) -> str:
    """Look symbols up at a past timestamp."""
    try:
        result = ops.as_of(timestamp, query, symbols, graph=graph)
    except ops.EngineError as e:
        return f"Temporal query failed: {e}. Ensure Macrame snapshots are enabled."
    except ops.OpError as e:
        return _op_message(e)
    return render.as_of(result)


def _traverse(
    graph: Any, entity_id: str, direction: str,
    edge_kinds: list[str] | None, max_depth: int,
) -> str:
    """Multi-depth BFS edge traversal."""
    try:
        result = ops.traverse(entity_id, direction, edge_kinds, max_depth)
    except ops.EngineError as e:
        return f"Traversal failed: {e}"
    except ops.OpError as e:
        return _op_message(e)
    return render.traverse(result)


def _resolve_ref(graph: Any, name: str, limit: int) -> str:
    """Framework-aware reference resolution: routes (`/users/:id`), service,
    model and view names."""
    try:
        result = ops.resolve(name, limit)
    except ops.OpError as e:
        return _op_message(e)
    return render.resolve(result)


# ── Analyses ─────────────────────────────────────────────────────────────

def _find_clones(
    graph: Any, min_lines: int = 10, min_similarity: float = 0.8, max_groups: int = 100,
) -> str:
    """Token-level clone detection (Types 1-3)."""
    try:
        groups = ops.find_clones(min_lines, min_similarity, max_groups)
    except ops.EngineError as e:
        msg = f"Clone detection failed: {e}"
        if str(e).startswith("engine panic"):
            msg += (". The session is still alive — retry with different "
                    "parameters or skip clone detection for now.")
        return msg
    except ops.OpError as e:
        return _op_message(e)
    return render.find_clones(groups, min_lines, min_similarity)


def _find_scaffolding(include_secrets: bool = False, max_findings: int = 100) -> str:
    """Scaffolding markers, placeholder bodies, temp files and secrets."""
    try:
        findings = ops.find_scaffolding(include_secrets, max_findings)
    except ops.OpError as e:
        return _op_message(e, "Scaffold scan failed")
    return render.find_scaffolding(findings, include_secrets)


def _dead_code(
    graph: Any, min_confidence: float = 0.6, include_test_reachable: bool = False,
    max_findings: int = 100,
) -> str:
    """Functions unreachable from any entry point."""
    try:
        findings = ops.dead_code(min_confidence, include_test_reachable, max_findings)
    except ops.OpError as e:
        return _op_message(e, "Dead-code detection failed")
    return render.dead_code(findings, min_confidence)


def _get_smells(
    graph: Any, entity_id: str | None = None, rule_id: str | None = None,
    strictness: str = "normal",
) -> str:
    """Code-smell findings."""
    try:
        findings = ops.get_smells(entity_id, rule_id, strictness)
    except ops.OpError as e:
        return _op_message(e, "Smell detection failed")
    return render.get_smells(findings, entity_id, rule_id)


# ── Edits ────────────────────────────────────────────────────────────────

def _edit_message(fn, *args: Any, **kwargs: Any) -> str:
    try:
        return render.edit(fn(*args, **kwargs))
    except ops.OpError as e:
        return _op_message(e)
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return render.mutation_error(str(e))


@requires_index
def _replace_body(
    graph: Any, entity_id: str, new_body: str,
    expected_hash: str | None, dry_run: bool,
) -> str:
    """Replace a function body."""
    return _edit_message(ops.replace_body, entity_id, new_body, expected_hash, dry_run,
                         graph=graph)


@requires_index
def _update_signature(
    graph: Any, entity_id: str, new_signature: str,
    inject_defaults: bool, dry_run: bool,
) -> str:
    """Change a function signature."""
    return _edit_message(ops.update_signature, entity_id, new_signature, inject_defaults,
                         dry_run, graph=graph)


@requires_index
def _rename(graph: Any, entity_id: str, new_name: str, dry_run: bool) -> str:
    """Rename an entity."""
    return _edit_message(ops.rename, entity_id, new_name, dry_run, graph=graph)


@requires_index
def _create_entity(
    graph: Any, file_path: str, language: str, kind: str,
    name: str, body: str, decorators: list[str] | None,
    anchor: str, signature: str | None, dry_run: bool,
) -> str:
    """Create a new entity."""
    return _edit_message(ops.create_entity, file_path, language, kind, name, body,
                         decorators, anchor, signature, dry_run, graph=graph)


# ── Index lifecycle ──────────────────────────────────────────────────────

def _reindex(graph: Any, with_embeddings: bool = False, full: bool = False) -> str:
    """Reindex the project: cheap by default, whole-tree walk on `full`.

    The server chdir'd onto the resolved project root before serving, so
    '.' is the project root — and the same spelling startup indexed, which
    keeps entity ids stable across the two.
    """
    try:
        result = ops.reindex(with_embeddings, full=full)
    except ops.NoExtension:
        return "CodeRadar extension not available."
    except ops.OpError as e:
        return f"Reindex failed: {e}"
    return render.reindex(result)


def _update_file(graph: Any, file_path: str, content: str | None) -> str:
    """Incremental single-file sync; a deleted file is dropped."""
    try:
        report = ops.update_file(file_path, content, graph=graph)
    except ops.EngineError as e:
        return f"Update failed: {e}"
    except ops.OpError as e:
        return _op_message(e)
    return render.update_file(file_path, report, content is not None)


def _status() -> str:
    """What this server serves, and how fresh the index is."""
    from coderadar.mcp.startup import current

    try:
        result = ops.status(_served_root())
    except ops.OpError as e:
        return _op_message(e)
    text = render.status(result)
    index = current()
    if index is not None:
        from coderadar.mcp.startup import progress_message
        outcome = index.wait(timeout=0)
        if not outcome.ready:
            text += "\n\n" + progress_message(outcome)
    return text
