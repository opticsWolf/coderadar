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
import re
from pathlib import Path
from typing import Any, Literal

import structlog
from mcp.server import MCPServer

from coderadar import ops, render

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

A pre-built graph of every symbol, call edge and file in the workspace
(41 languages). Use it before and while editing, not only for questions: one
call returns verbatim source plus who calls it and what it affects.

## Start with codegraph_explore

Any structural or flow question — "how does X work?", "the flow from X to
Y", reading a symbol before editing it — is one `codegraph_explore` call
naming the symbol(s). It returns line-numbered source grouped by file (safe
to Edit from), the call paths between the symbols and a blast-radius summary.
Need more? Call it again with more specific names.

## Other read tools

- `codegraph_search` — find symbols by keyword when you don't know the name
- `codegraph_search_similar` — semantic search (embeddings via `codegraph_compute_embeddings`, computed on first use)
- `codegraph_node` — one entity's details and neighbours; `codegraph_module_children` — a module's contents
- `codegraph_affected` — transitive callers (blast radius), centrality-ranked
- `codegraph_traverse` — any edge kind, upstream or downstream
- `codegraph_query` — structured queries, e.g. `functions where caller_count == 0` (docs/query-language.md)
- `codegraph_as_of` — the graph at a past timestamp
- `coderadar_resolve` — framework references: routes (`/users/:id`), `*Model` / `*View` names
- `codegraph_get_smells`, `codegraph_dead_code`, `codegraph_find_clones`, `codegraph_find_scaffolding` — quality findings. Dead code is ranked evidence, not proof: check `codegraph_affected` before deleting.

## Editing

`coderadar_replace_body`, `coderadar_update_signature`, `coderadar_rename`
and `coderadar_create_entity` edit the file and the graph together. They
dry-run by default: review the diff, then apply with dry_run=False.
After editing with Read/Edit instead, sync with `codegraph_update_file` (one
file) or `codegraph_reindex` (many files).

## Anti-patterns

- **Don't re-verify results with grep.** They come from a full parse.
- **Don't grep or Read first** to find indexed code, and don't reconstruct a flow by hand: name the endpoints and explore.
- **Files flagged "⚠ changed on disk after index sync"** are stale: Read those. Every other file is fresh.
- **No index for a project?** Use built-in tools. Indexing is the user's decision: mention `coderadar init`, don't run it.
- **"Indexing in progress" is not an error.** Retry in a few seconds rather than falling back to grep.

## One project at a time

The server serves one project. Every tool takes an optional `project_path`:
a path inside the served project is accepted, another project is refused
with the reason. `codegraph_set_project` switches wholesale (config re-read,
background re-index, earlier event ids invalid).

The same graph is available from the shell: `coderadar status`, `query`,
`traverse`, `callers`, `callees`.
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

    # ── codegraph_explore (§26.2 primary tool) ─────────────────────────

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
    def codegraph_explore(
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

    # ── codegraph_node (§26.2 depth tool) ──────────────────────────────

    @mcp.tool(
        description=(
            "Get full details for a specific entity identified via codegraph_explore. "
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
    def codegraph_node(
        id: str,
        include_neighbors: bool = False,
        project_path: str | None = None,
    ) -> str:
        """Depth drill-down for a single entity."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _node_detail(graph, id, include_neighbors)

    # ── codegraph_search (§26.2 discovery tool) ────────────────────────

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
    def codegraph_search(
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

    # ── codegraph_affected (§26.2 impact tool) ─────────────────────────

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
    def codegraph_affected(
        id: str,
        max_depth: int = 5,
        project_path: str | None = None,
    ) -> str:
        """Transitive impact analysis."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
        project_path: str | None = None,
    ) -> str:
        """Framework-aware reference resolution."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _resolve_ref(graph, name, limit)

    # ── codegraph_query — Pest graph query language ───────────────────

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
            "fed to codegraph_affected / coderadar_rename. Unknown fields are "
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
    def codegraph_query(
        query: str,
        project_path: str | None = None,
    ) -> str:
        """Execute a Pest graph query."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
        project_path: str | None = None,
    ) -> str:
        """Semantic similarity search."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
        project_path: str | None = None,
    ) -> str:
        """Compute embeddings for semantic search."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
        project_path: str | None = None,
    ) -> str:
        """List module children."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
        symbols: list[str] | None = None,
        project_path: str | None = None,
    ) -> str:
        """Temporal graph query."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _as_of(graph, timestamp, query, symbols or [])

    # ── codegraph_traverse — edge traversal ──────────────────────────

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
    def codegraph_traverse(
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

    # ── codegraph_get_smells — code smell detection ─────────────────

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
    def codegraph_get_smells(
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

    # ── codegraph_dead_code — unreachable-code detection ───────────────

    @mcp.tool(
        description=(
            "Find dead code: functions unreachable from any entry point "
            "(the mirror image of codegraph_affected). Entry points: mains, "
            "framework handlers (routes, CLIs, pytest, Qt slots), dunder methods, "
            "overrides of external bases (e.g. Qt paintEvent), Protocol/ABC "
            "members, __all__ and package exports, project scripts, and tests. "
            "Calls, callbacks passed as values and virtual dispatch all extend "
            "liveness. Each finding carries kind (unreachable | "
            "transitively-dead | test-only | rta-dead), tier, confidence, "
            "removable lines and the evidence behind it, ranked "
            "most-safely-deletable first. rta-dead is the weakest kind: the "
            "method lives only through dispatch on a class never constructed "
            "in the indexed root. Verify with codegraph_affected before "
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
    def codegraph_dead_code(
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

    # ── codegraph_find_clones — token-level clone detection ───────────

    @mcp.tool(
        description=(
            "Detect syntactic code clones (Types 1-3): identical bodies, "
            "renamed bodies, and near-duplicates above a similarity floor. "
            "Each group lists its instances (entity_id, file, byte span) with "
            "a confidence tier; groups are ranked largest first. Type-4 "
            "(semantic-only) similarity is covered by codegraph_search_similar. "
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
    def codegraph_find_clones(
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

    # ── codegraph_find_scaffolding — AI-scaffolding & secrets scan ────

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
    def codegraph_find_scaffolding(
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
        project_path: str | None = None,
    ) -> str:
        """Full reindex with optional embeddings."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

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
        content: str | None = None,
        project_path: str | None = None,
    ) -> str:
        """Incrementally sync one file."""
        mismatch = _wrong_project(project_path)
        if mismatch:
            return mismatch

        return _update_file(graph, file_path, content)

    # ── codegraph_set_project — switch the served project ───────────

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
    def codegraph_set_project(project_path: str, confirm: bool = False) -> str:
        """Switch the served project."""
        return _set_project(project_path, confirm)

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
        from coderadar._core import graph_stats
        stats = graph_stats()
        # This read a key named "epoch" that graph_stats never set, so
        # `indexed_at` was always 0 and the guard below never passed — every
        # staleness banner in this server was unreachable. The core now sets
        # `indexed_at` at each commit_projection.
        indexed_at = stats.get("indexed_at", 0.0)
    except (ImportError, RuntimeError):
        # RuntimeError: no graph loaded yet — nothing to be stale against.
        return stale

    if not indexed_at:
        return stale

    for fp in file_paths:
        try:
            mtime = os.path.getmtime(fp)
            if mtime > indexed_at:
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
        (
            "⚠️ Some files referenced below were edited since the last index sync — "
            "their codegraph entries may be stale:"
        ),
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
        f"`codegraph_set_project` with `{selected.path}` — this server "
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
            "Run `codegraph_reindex` to index it now.",
        ]
    else:
        lines += [
            (
                "Nothing on disk confirmed that directory as a project root — no "
                "`.coderadar/` or `.coderadar.toml` was found at or above it."
            ),
            "",
            (
                "If that is the wrong project, call `codegraph_set_project` with "
                "the right directory to switch this server there, restart with "
                "`coderadar mcp serve --path <project root>`, or run "
                "`coderadar init` in the right directory so it can be found "
                "automatically."
            ),
            "",
            "If it is the right project, run `codegraph_reindex` to index it.",
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
            "`codegraph_reindex` to refresh the index."
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
                     "`codegraph_reindex` to build the new graph.")

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
    """Word an ops error for an agent. `failed` names the analysis for
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
    return f"{failed or 'Request failed'}: {e}"


def _stale_prefix(entity: dict) -> str:
    """The "changed on disk" banner for an entity's file, or ""."""
    fp = entity.get("file_path", "")
    if fp:
        stale = _get_stale_files([fp])
        if stale:
            return _format_stale_banner(stale, [fp]) + "\n"
    return ""

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
            f"Couldn't find {name_list} in the index. Each name was matched "
            "exactly, then as search tokens, against names, signatures and "
            "docstrings. Try a single well-known symbol, `codegraph_search` "
            "with one token, or `codegraph_search_similar` for semantic search."
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
    try:
        entity = ops.node(entity_id, include_neighbors)
    except ops.OpError as e:
        return _op_message(e)
    return _stale_prefix(entity) + render.node(entity)


def _search_miss_message(query: str, kind: str | None) -> str:
    """A miss that tells the agent what was actually tried (see render)."""
    return render.search_miss(query, kind)


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


# ── New Tool Implementations (v0.5.9) ─────────────────────────────────

def _query_graph(graph: Any, query: str) -> str:
    """Execute a Pest query against the graph."""
    # An empty query is a usage error regardless of graph state, so prompt
    # for it before touching the (possibly unloaded) in-memory graph.
    if not query.strip():
        return "Please provide a query. Examples:\n" \
               "  - classes where inherits_from contains 'BaseModel'\n" \
               "  - methods where is_async == true\n" \
               "  - functions where name starts_with 'test_'\n" \
               "  - imports where import_kind == 'from'\n" \
               "Entities: modules, classes, functions, methods, constants, entities, " \
               "imports, calls, fields. Full field reference: docs/query-language.md"

    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return _no_index_message()
    except ImportError:
        return NO_EXTENSION_MESSAGE
    except RuntimeError:
        # No graph loaded. Reporting that as a missing extension sent the
        # agent off to rebuild the wheel for something `coderadar init`
        # fixes — these were the last copies of the per-tool guard that
        # `requires_index` replaced everywhere else.
        return _no_index_message()

    try:
        from coderadar._core import query_graph as _qg
        rows = _qg(query)
        if not rows:
            return f"Query `{query}` returned no results."
        lines = [f"## Query: `{query}`", f"Found {len(rows)} result(s)", ""]
        for i, row in enumerate(rows[:30], 1):
            name = row.get("name", row.get("id", "?"))
            kind = row.get("kind", row.get("entity_type", "?"))
            fp = _display_file(row)
            rid = row.get("id", row.get("entity_id", ""))
            sl = row.get("start_line", row.get("line", ""))
            lines.append(f"{i}. `{name}` ({kind}) — `{fp}`")
            if rid:
                lines.append(f"   ID: `{rid}`")
            if sl:
                lines.append(f"   Line: {sl}")
            sig = row.get("signature")
            if sig:
                lines.append(f"   Signature: `{sig}`")
        if len(rows) > 30:
            lines.append(f"... and {len(rows) - 30} more")
        return "\n".join(lines)
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return f"Query failed: {e}"


def _compute_embeddings(graph: Any) -> str:
    """Compute embeddings for all functions."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return _no_index_message()
    except ImportError:
        return NO_EXTENSION_MESSAGE
    except RuntimeError:
        # No graph loaded. Reporting that as a missing extension sent the
        # agent off to rebuild the wheel for something `coderadar init`
        # fixes — these were the last copies of the per-tool guard that
        # `requires_index` replaced everywhere else.
        return _no_index_message()

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
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return f"Embedding generation failed: {e}\n\nEnsure fastembed is installed: pip install fastembed"


def _get_embedding_model():
    """Lazily load and cache the fastembed model (avoid reload per query)."""
    global _EMBED_MODEL
    if _EMBED_MODEL is None:
        from fastembed import TextEmbedding

        from coderadar.embedding import embedding_settings
        model_name, _dimension = embedding_settings()
        _EMBED_MODEL = TextEmbedding(model_name=model_name)
    return _EMBED_MODEL


@requires_index
def _search_similar(graph: Any, query: str, top_k: int) -> str:
    """Semantic/embedding similarity search."""
    if not query.strip():
        return "Please provide a natural-language query for semantic search."

    # Try to embed the query using a cached fastembed model
    try:
        embedding = next(iter(_get_embedding_model().embed([query])))
    except ImportError:
        return (
            "Semantic search requires `fastembed` to be installed. "
            "Run: pip install fastembed\n"
            "Then run compute_embeddings() to index all entities."
        )
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return f"Embedding failed: {e}"

    try:
        from coderadar._core import search_similar as _ss
        results = _ss(list(embedding), min(top_k, 20))
    except RuntimeError:
        # No embeddings in index — try to auto-compute
        try:
            graph.compute_embeddings()
            results = _ss(list(embedding), min(top_k, 20))
        except Exception:  # noqa: BLE001 - auto-compute is best-effort, message covers it
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
        fp = _display_file(r)
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
        result = ops.module_children(module_id)
    except ops.OpError as e:
        return _op_message(e)
    return render.module_children(result)


def _as_of(graph: Any, timestamp: str, query: str, symbols: list[str]) -> str:
    """Query the graph at a past timestamp."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return _no_index_message()
    except ImportError:
        return NO_EXTENSION_MESSAGE
    except RuntimeError:
        # No graph loaded. Reporting that as a missing extension sent the
        # agent off to rebuild the wheel for something `coderadar init`
        # fixes — these were the last copies of the per-tool guard that
        # `requires_index` replaced everywhere else.
        return _no_index_message()

    if not timestamp:
        return "Please provide an ISO 8601 timestamp (e.g. '2025-01-15T10:00:00Z')."

    # R2-10: garbage used to sail through into a snapshot template that
    # echoed it back with no complaint (only "" was validated).
    try:
        from datetime import datetime
        datetime.fromisoformat(timestamp)
    except ValueError:
        return (f"Invalid timestamp {timestamp!r}: expected ISO 8601 "
                f"(e.g. '2025-01-15T10:00:00Z').")

    try:
        snapshot = graph.as_of(timestamp)
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return f"Temporal query failed: {e}. Ensure Macrame snapshots are enabled."

    names = _parse_names(query, symbols)
    if not names:
        # Both suggestions here named things that do not exist:
        # `codegraph_query` takes no timestamp, and there is no
        # `search_entities` tool — an agent following this guidance failed
        # twice. Nothing is loaded at this point either; `as_of` resolves
        # per symbol.
        lines = [
            f"## Snapshot at `{timestamp}`",
            "",
            (
                "Pass `symbols` to look entities up as they were at this "
                "timestamp — for example "
                f'codegraph_as_of(timestamp="{timestamp}", symbols=["User"]).'
            ),
            "",
            (
                "Only symbol lookup is reconstructed from the ledger. "
                "`codegraph_query` and `codegraph_search` always run against the "
                "current index."
            ),
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


def _find_clones(
    graph: Any,
    min_lines: int = 10,
    min_similarity: float = 0.8,
    max_groups: int = 100,
) -> str:
    """Run clone detection and render groups ranked by size."""
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
    """Run the scaffold scanner and render grouped findings."""
    try:
        findings = ops.find_scaffolding(include_secrets, max_findings)
    except ops.OpError as e:
        return _op_message(e, "Scaffold scan failed")
    return render.find_scaffolding(findings, include_secrets)


def _dead_code(
    graph: Any,
    min_confidence: float = 0.6,
    include_test_reachable: bool = False,
    max_findings: int = 100,
) -> str:
    """Run dead-code detection and render ranked, deletability-sorted findings."""
    try:
        findings = ops.dead_code(min_confidence, include_test_reachable, max_findings)
    except ops.OpError as e:
        return _op_message(e, "Dead-code detection failed")
    return render.dead_code(findings, min_confidence)


def _get_smells(
    graph: Any, entity_id: str | None, rule_id: str | None, strictness: str = "normal"
) -> str:
    """Run the native smell engine and render findings as markdown."""
    try:
        findings = ops.get_smells(entity_id, rule_id, strictness)
    except ops.OpError as e:
        return _op_message(e, "Smell detection failed")
    return render.get_smells(findings, entity_id, rule_id)


def _traverse(
    graph: Any, entity_id: str, direction: str,
    edge_kinds: list[str] | None, max_depth: int,
) -> str:
    """Proper multi-depth BFS edge traversal via MacrameQuery."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return _no_index_message()
    except ImportError:
        return NO_EXTENSION_MESSAGE
    except RuntimeError:
        # No graph loaded. Reporting that as a missing extension sent the
        # agent off to rebuild the wheel for something `coderadar init`
        # fixes — these were the last copies of the per-tool guard that
        # `requires_index` replaced everywhere else.
        return _no_index_message()

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
        return _not_found_message(graph, entity_id)
    entity_id = _canonical_entity_id(entity_id)

    depth = min(max_depth, 10)
    # Map MCP direction names to MacrameQuery direction
    macrame_direction = {"downstream": "out", "upstream": "in", "both": "both"}.get(direction, "both")

    try:
        results = graph.traverse(entity_id, depth, edge_kinds, macrame_direction)
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return f"Traversal failed: {e}"

    # 2.3: surface silent truncation — count targets the walk could not follow.
    try:
        from coderadar._core import traverse_unresolved
        unresolved = traverse_unresolved(entity_id, depth, edge_kinds or [], macrame_direction)
    except Exception:  # noqa: BLE001 - truncation count is best-effort, 0 means "unknown"
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
        (
            f"Direction: {direction}, max depth: {depth}, "
            f"edge kinds: {edge_kinds or 'all'}"
        ),
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
            fp = _display_file(item)
            fp_str = f" — `{fp}`" if fp and fp != "?" else ""
            id_str = f" — `{eid}`" if eid and eid != name else ""
            lines.append(f"- `{name}` ({ek}){fp_str}{id_str}")
        if len(items) > 15:
            lines.append(f"  ... and {len(items) - 15} more")
        lines.append("")

    return "\n".join(lines)


def _format_mutation_error(e: BaseException) -> str:
    """Translate raw engine errors into LLM-actionable prose (F10 fix)."""
    raw = str(e)
    if "StaleIndex" in raw or "stale" in raw.lower():
        return (
            "## Mutation Rejected — Stale Index\n\n"
            "The file changed on disk after it was indexed, so the planned "
            "span no longer lines up. Nothing was written.\n\n"
            "**Next step:** run `codegraph_update_file` on the file (or "
            "re-analyze), then retry the mutation.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    if "RejectedPolicy" in raw or "policy" in raw.lower():
        return (
            "## Mutation Rejected — Policy\n\n"
            "The target path is outside the `[mutation] allow` list. "
            "Nothing was written.\n\n"
            "**Next step:** pick a target under an allowed root, or ask the "
            "user to extend the allow list.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    if "SpanOutOfBounds" in raw or "out of bounds" in raw.lower():
        return (
            "## Mutation Failed — Span Mismatch\n\n"
            "The computed edit span fell outside the file — likely a stale "
            "concept or an off-by-one in the planner. Nothing was written "
            "(rollback confirmed).\n\n"
            "**Next step:** update the file in the graph and retry; if it "
            "persists, report it with the entity id.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    if "ParseError" in raw or "syntax" in raw.lower():
        return (
            "## Mutation Failed — Syntax\n\n"
            "The edited file did not re-parse, so the change was rolled "
            "back. Nothing was written.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    return f"Mutation failed: {raw}"


@requires_index
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
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return _format_mutation_error(e)


@requires_index
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
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return _format_mutation_error(e)


@requires_index
def _rename(graph: Any, entity_id: str, new_name: str, dry_run: bool) -> str:
    """Rename an entity."""
    try:
        entity_id = _canonical_entity_id(entity_id)
        plan = graph.plan_rename(entity_id, new_name, dry_run=True)
        if dry_run:
            return _format_mutation_plan(plan) + "\n**To apply:** call again with `dry_run=False`."
        result = graph.apply(plan)
        return _format_mutation_applied(result, plan.unverified_sites)
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return _format_mutation_error(e)


def _render_entity_code(
    language: str, kind: str, name: str, body: str, decorators: list[str] | None,
    signature: str = "",
) -> str:
    """Render a source snippet for a new entity using language-aware syntax.

    `signature`, when given, is the complete function/method header to write
    verbatim (`fn f(a: T) -> U`, `def f(self) -> None`, …). The renderer only
    adds the language's body delimiter (the Python colon, the C-style braces,
    Ruby's `end`) so the agent can express full Rust/typed signatures that the
    name-only rendering never could (field session: `create_entity` could not
    express `fn sync_status_text(store: &Store) -> String`).
    """
    lang = (language or "").lower()
    kind_norm = (kind or "function").lower()
    body = (body or "").rstrip("\n")
    dec = "\n".join(decorators or [])
    dec_block = (dec + "\n") if dec else ""
    sig = (signature or "").strip()

    def indent(text: str, spaces: int = 4) -> str:
        pad = " " * spaces
        return "\n".join((pad + line) if line.strip() else line for line in text.split("\n"))

    if kind_norm in ("function", "method", "fn"):
        if sig:
            if lang in ("python", "py"):
                header = sig if sig.endswith(":") else sig + ":"
                inner = indent(body) or "    pass"
                return f"{dec_block}{header}\n{inner}\n"
            if lang in ("ruby", "rb"):
                return f"{dec_block}{sig}\n{body}\nend\n"
            # C-style block languages: rust, go, js/ts, java, csharp, php, …
            return f"{dec_block}{sig} {{\n{body}\n}}\n"
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
    if file_path.startswith(('./', '.\\')):
        return file_path
    return '.' + os.sep + file_path


@requires_index
def _create_entity(
    graph: Any, file_path: str, language: str, kind: str,
    name: str, body: str, decorators: list[str] | None,
    anchor: str, signature: str | None, dry_run: bool,
) -> str:
    """Create a new entity."""
    try:
        code = _render_entity_code(language, kind, name, body, decorators, signature or "")
        if not code.strip():
            return "Cannot render entity: provide a non-empty body or kind."
        note = ""
        if (signature or "").strip() and kind.lower() not in ("function", "method", "fn"):
            note = (
                f"Note: `signature` is only used for function-like kinds; it was "
                f"ignored for kind '{kind}'.\n\n"
            )
        target = _canonical_file_path(file_path)
        # If the anchor is an entity ID (not 'top'/'end'), canonicalize it too
        anchor_norm = anchor or "end"
        if anchor_norm not in ("top", "end"):
            anchor_norm = _canonical_entity_id(anchor_norm)
        plan = graph.plan_create_entity(
            target, anchor_norm, code, dry_run=True,
        )
        if dry_run:
            return (note + _format_mutation_plan(plan)
                    + "\n**To apply:** call again with `dry_run=False`.")
        result = graph.apply(plan)
        return note + _format_mutation_applied(result, plan.unverified_sites)
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return _format_mutation_error(e)


def _format_mutation_plan(plan: Any) -> str:
    """Format a MutationPlan for MCP output (dry-run)."""
    lines = [f"## Mutation Plan: `{plan.tool}` (DRY RUN)", ""]
    lines.append(f"- **Plan ID:** `{plan.id}`")
    lines.append(f"- **Affected files:** {len(plan.affected_files)}")

    if plan.diff_preview:
        lines.append("")
        lines.append("### Diff Preview")
        lines.append("```diff")
        lines.extend(plan.diff_preview.split("\n")[:60])
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
            # The core now sends these across as dicts; they used never to
            # arrive at all, so this list was always empty.
            if isinstance(site, dict):
                where = f"{site.get('file', '?')}:{site.get('line', 0)}"
                lines.append(
                    f"- `{where}` — {site.get('reason', '')}"
                    + (f" (`{site['snippet']}`)" if site.get("snippet") else "")
                )
            else:
                lines.append(f"- `{site}`")

    if plan.warnings:
        lines.append("")
        lines.append("### Warnings")
        for w in plan.warnings:
            lines.append(f"- ⚠ {w}")

    return "\n".join(lines)


def _format_mutation_applied(result: Any, unverified_sites: list | None = None) -> str:
    """Format a MutationResult — truthfully, whatever the outcome.

    BUGS_QUIRKS #2: this used to print "## Mutation Applied" and "Graph has
    been updated" even for rejections, while a separate status line said
    RejectedPolicy and the file was untouched. The header, the state claims,
    and the status now derive from one source: result.status.
    """
    status = str(getattr(result, "status", ""))
    files_written = list(getattr(result, "files_written", []) or [])
    applied = status == "Applied"

    header = {
        "Applied": "## Mutation Applied",
        "RolledBack": "## Mutation Rolled Back",
        "RejectedStale": "## Mutation Rejected — Stale",
        "RejectedPolicy": "## Mutation Rejected — Policy",
    }.get(status, f"## Mutation Result — {status or 'Unknown'}")

    lines = [header, ""]
    lines.append(f"- **Status:** {status}")
    lines.append(f"- **File written:** {'yes' if files_written else 'no'}")
    lines.append(
        f"- **Graph updated:** {'yes' if applied else 'no — nothing changed'}"
    )
    if files_written:
        lines.append(f"- **Files written:** {len(files_written)}")
        for f in files_written:
            lines.append(f"  - `{f}`")
    if result.syntax_errors:
        lines.append(f"- **Syntax errors:** {len(result.syntax_errors)}")
        for e in result.syntax_errors[:5]:
            if isinstance(e, dict):
                where = f"{e.get('file', '?')}:{e.get('line', 0)}:{e.get('column', 0)}"
                lines.append(f"  - `{where}` — {e.get('message', '')}")
            else:
                lines.append(f"  - {e}")
    if getattr(result, "backup_path", None):
        lines.append(f"- **Backup:** `{result.backup_path}`")
    if not applied:
        lines.append("")
        if status == "RejectedStale":
            lines.append(
                "The file changed since planning — call the plan tool again to "
                "re-read the current content, then apply the fresh plan."
            )
        else:
            lines.append("No changes were kept. Address the reason above and re-plan.")
    if unverified_sites:
        lines.append("")
        lines.append(
            f"⚠️ **WARNING: {len(unverified_sites)} call site(s) could not be "
            f"verified/rewritten. Manual review required.**"
        )
        for site in unverified_sites[:10]:
            lines.append(f"- `{site}`")
    return "\n".join(lines)


def _reindex(graph: Any, with_embeddings: bool = False) -> str:
    """Reindex the project: current, the cheap way (v0.8 P2-4).

    A warm repo loads its ledger and updates only the files that changed;
    a repo without a loadable store gets the full walk.
    """
    try:
        from coderadar import coldstart
        from coderadar._core import graph_stats
        # Use relative root ('.') to keep entity IDs consistent with startup
        # (analyze('.')) — absolute os.getcwd() would change ID prefixes.
        # The server chdir's onto the resolved project root before serving,
        # so '.' is the project root by construction rather than by luck.
        coldstart.build_graph('.')
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
            except Exception as e:  # noqa: BLE001 - diagnose reports, never raises on embeddings
                lines.append(f"- **Embeddings:** failed — {e}")
        return "\n".join(lines)
    except ImportError:
        return "CodeRadar extension not available."
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
        return f"Reindex failed: {e}"


def _update_file(graph: Any, file_path: str, content: str | None) -> str:
    """Incremental single-file update."""
    try:
        from coderadar._core import graph_stats
        if graph_stats().get("modules", 0) == 0:
            return _no_index_message()
    except ImportError:
        return NO_EXTENSION_MESSAGE
    except RuntimeError:
        # No graph loaded. Reporting that as a missing extension sent the
        # agent off to rebuild the wheel for something `coderadar init`
        # fixes — these were the last copies of the per-tool guard that
        # `requires_index` replaced everywhere else.
        return _no_index_message()

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
    except Exception as e:  # noqa: BLE001 - MCP tool boundary returns errors, never raises
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
    # F14: entity paths are canonical root-relative ids — resolve against
    # the indexed root, not the CWD.
    try:
        from coderadar.excludes import resolve_entity_path as _resolve
        file_path = _resolve(file_path)
    except ImportError:
        pass
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


def _friendly_entity_id(entity_id: str) -> str:
    """Shell-friendly entity id (see `coderadar.ops.friendly_entity_id`)."""
    return ops.friendly_entity_id(entity_id)


def _canonical_entity_id(entity_id: str) -> str:
    """Stored entity id for any accepted spelling (see `coderadar.ops`)."""
    return ops.canonical_entity_id(entity_id)


def _display_file(d: dict) -> str:
    """Best-effort file for an entity dict (see `coderadar.ops.display_file`)."""
    return ops.display_file(d)


def _find_entity(graph: Any, entity_id: str) -> dict | None:
    return ops.find_entity(entity_id)


def _not_found_message(graph: Any, entity_id: str) -> str:
    """A miss with candidates instead of a bare `not found` (plan §5.2)."""
    return render.not_found(entity_id, ops.suggest_entities(entity_id))


def _text_search(graph: Any, query: str, top_k: int, kind: str | None = None) -> list[dict]:
    return ops._text_search(query, top_k, kind)


def _get_callers(graph: Any, entity_id: str) -> list[dict]:
    return ops._callers(entity_id)


def _get_callees(graph: Any, entity_id: str) -> list[dict]:
    return ops._callees(entity_id)


# ── Query-Time Resolution (F.8 Phase 2) ────────────────────────────────────


def _resolve_ref(graph: Any, name: str, limit: int) -> str:
    """Framework-aware reference resolution: routes (`/users/:id`), service,
    model and view names."""
    try:
        result = ops.resolve(name, limit)
    except ops.OpError as e:
        return _op_message(e)
    return render.resolve(result)
