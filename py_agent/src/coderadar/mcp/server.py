"""CodeRadar MCP Server — §26 Agent Interface Design

Four-tool MCP surface wrapping the Macrame + ProjectedGraph backend.
Implements the explore-budget scaling (§26.3) and success-shaped error
guidance (§26.1 item 3) patterns from CodeGraph's production experience.

Tools:
  codegraph_explore  — primary, 80%+ of agent calls
  codegraph_node     — depth drill-down after explore
  codegraph_search   — discovery (hybrid keyword + vector)
  codegraph_affected — impact analysis
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional

from mcp.server import Server, ServerRequestContext
from mcp.server.stdio import stdio_server
from mcp import types as mcp_types

from .budget import ExploreBudget, get_explore_budget
from .explore import explore
from .node import node_detail
from .search import hybrid_search
from .affected import affected

import structlog

logger = structlog.get_logger(__name__)

# ── Instructions (§26.1 item 2: meet the agent where it goes) ────────────

SERVER_INSTRUCTIONS = """# CodeRadar — live semantic graph of your codebase

CodeRadar is a pre-computed knowledge graph of every symbol, edge, and file
in the workspace — cached intelligence for thousands of parse/trace decisions
you would otherwise re-derive by reading files. Indexes Python + TypeScript;
reads are sub-millisecond.

## One primary tool: codegraph_explore

`codegraph_explore` takes symbol names and returns the **verbatim, line-numbered
source** of the relevant symbols grouped by file — the same shape `Read` gives
you, safe to `Edit` from — PLUS the call paths among them and a blast-radius
summary of what depends on them.

Whether you're answering "how does X work" or implementing a change, call
`codegraph_explore` before you Read. ONE call usually answers the whole question.

## How to query

- **"How does X work" / architecture / bug / "what is X"** → `codegraph_explore` with the symbol name(s). Returns verbatim source + callers/callees.
- **"The flow from X to Y"** → `codegraph_explore` with both endpoints. Surfacing the call path between them.
- **Reading/editing a specific symbol** → name it in `codegraph_explore`. Returns line-numbered source you can `Edit` from directly.
- **Need more?** Call `codegraph_explore` again with more specific names.

## Other tools

- `codegraph_node` — full details for a specific entity identified via explore.
- `codegraph_search` — find symbols by keyword or description.
- `codegraph_affected` — transitive impact: "what calls this, all the way up?"

## Anti-patterns

- **Trust the results — don't re-verify with grep.** Results come from full AST parse.
- **Don't grep or Read first** — ONE `codegraph_explore` returns source together.
- **Don't reconstruct flows by hand** — name the endpoints and explore does it.
- **If a project isn't indexed**, stop calling codegraph tools and use built-in tools.
"""


# ── Server Factory ────────────────────────────────────────────────────────

def create_server(graph: Any) -> Server:
    """Create an MCP server wrapping the given CodeGraph instance.

    Args:
        graph: A CodeGraph instance (or None for uninitialized — tools
               return success-shaped guidance per §26.1 item 3).

    Returns:
        A configured mcp.server.Server ready for stdio transport.
    """
    server = Server(
        "coderadar",
        version="0.2.0",
        description="Live semantic graph — pre-computed symbol, edge, and file intelligence",
        instructions=SERVER_INSTRUCTIONS,
    )

    # Determine if we have a working graph
    project_file_count = _get_file_count(graph)

    # ── tools/list handler ───────────────────────────────────────────

    async def list_tools(
        ctx: ServerRequestContext,
        params: mcp_types.PaginatedRequestParams | None,
    ) -> mcp_types.ListToolsResult:
        budget = get_explore_budget(project_file_count)
        budget_hint = (
            f" Budget: make at most {budget.max_calls} explore call(s) for this project "
            f"({project_file_count} files indexed)."
        ) if budget.include_budget_note else ""

        tools = [
            mcp_types.Tool(
                name="codegraph_explore",
                description=(
                    "Explore the code graph: given symbol or file names, returns "
                    "verbatim line-numbered source of the relevant symbols grouped "
                    "by file, PLUS the call paths between them and a blast-radius "
                    "summary. Use this instead of grep + Read for any structural or "
                    "flow question. For multiple symbols, pass them together in one "
                    "call to get the relationships between them."
                    + budget_hint
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language question or symbol/file names to explore."
                        },
                        "symbols": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Specific symbol names to look up (alternative to query)."
                        },
                        "direction": {
                            "type": "string",
                            "enum": ["downstream", "upstream", "both"],
                            "description": "Traversal direction (default: both)."
                        },
                        "max_files": {
                            "type": "integer",
                            "description": f"Max files to return source for (default: {budget.default_max_files})."
                        },
                    },
                },
            ),
            mcp_types.Tool(
                name="codegraph_node",
                description=(
                    "Get full details for a specific entity (function, class, module, etc.) "
                    "identified via codegraph_explore. Returns complete metadata, source "
                    "location, and optionally immediate neighbors."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Entity ID (e.g. 'src/models.py::User.save')."
                        },
                        "include_neighbors": {
                            "type": "boolean",
                            "description": "Also return immediate callers and callees (default: false)."
                        },
                    },
                    "required": ["id"],
                },
            ),
            mcp_types.Tool(
                name="codegraph_search",
                description=(
                    "Search for symbols by keyword or natural-language description. "
                    "Returns ranked results with snippets. Use when you don't know the "
                    "exact symbol name."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query — keyword or description."
                        },
                        "kind": {
                            "type": "string",
                            "description": "Optional entity kind filter (function, class, module, etc.)."
                        },
                        "top_k": {
                            "type": "integer",
                            "description": "Max results to return (default: 10, max: 20)."
                        },
                    },
                    "required": ["query"],
                },
            ),
            mcp_types.Tool(
                name="codegraph_affected",
                description=(
                    "Find all entities transitively affected by a given entity — "
                    "the blast radius. Returns a tree of dependent callers."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Entity ID to analyze impact for."
                        },
                        "max_depth": {
                            "type": "integer",
                            "description": "Maximum depth to traverse upstream (default: 5)."
                        },
                    },
                    "required": ["id"],
                },
            ),
        ]
        return mcp_types.ListToolsResult(tools=tools)

    # ── tools/call handler ───────────────────────────────────────────

    async def call_tool(
        ctx: ServerRequestContext,
        params: mcp_types.CallToolRequestParams,
    ) -> mcp_types.CallToolResult:
        """Route tool calls to implementations."""
        name = params.name
        args = params.arguments or {}

        try:
            if name == "codegraph_explore":
                content = await explore(graph, args, project_file_count)
            elif name == "codegraph_node":
                content = await node_detail(graph, args)
            elif name == "codegraph_search":
                content = await hybrid_search(graph, args)
            elif name == "codegraph_affected":
                content = await affected(graph, args)
            else:
                content = f"Unknown tool: {name}"
                return mcp_types.CallToolResult(
                    content=[mcp_types.TextContent(type="text", text=content)],
                    is_error=True,
                )

            return mcp_types.CallToolResult(
                content=[mcp_types.TextContent(type="text", text=content)],
            )
        except Exception as exc:
            logger.exception("tool_call_failed", tool=name)
            return mcp_types.CallToolResult(
                content=[mcp_types.TextContent(
                    type="text",
                    text=(
                        f"CodeRadar couldn't process this request: {exc}\n\n"
                        "The graph may need re-indexing — try running `coderadar sync` "
                        "and retry the call."
                    ),
                )],
                is_error=True,
            )

    server.on_list_tools = list_tools  # type: ignore[assignment]
    server.on_call_tool = call_tool  # type: ignore[assignment]

    return server


# ── Entry Point ──────────────────────────────────────────────────────────

async def serve(graph: Any) -> None:
    """Run the MCP server over stdio."""
    server = create_server(graph)
    async with stdio_server() as (reader, writer):
        await server.run(reader, writer)


def _get_file_count(graph: Any) -> int:
    """Get indexed file count from the graph, or 0 if uninitialized."""
    if graph is None:
        return 0
    try:
        from coderadar._core import graph_stats
        stats = graph_stats()
        return stats.get("file_count", 0)
    except (ImportError, AttributeError):
        return 0
