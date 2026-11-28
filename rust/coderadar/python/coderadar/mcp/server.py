"""An MCP server whose tools call the Rust engine.

The tool bodies are one-line delegations; schema and routing come from the
`mcp` SDK's decorator, exactly like the Python reference server.
"""

from __future__ import annotations

import json
from typing import Any

from .. import (
    affected,
    annotations_on,
    as_of,
    callers,
    callees,
    changes_between,
    comments_on,
    create_entity,
    dead_code,
    diagnose,
    explore,
    find_clones,
    find_scaffolding,
    get_smells,
    history_of,
    module_children,
    node,
    open_items,
    query,
    read_source,
    rename,
    replace_body,
    resolve,
    search,
    search_hybrid,
    search_similar,
    status,
    traverse,
    update_file,
    update_signature,
)

SERVER_INSTRUCTIONS = "CodeRadar: query the live code graph before editing."


def requires_index(graph: Any = None) -> None:
    """Raise `NoIndex` when nothing is loaded."""
    if not status().get("loaded"):
        from .. import NoIndex

        raise NoIndex("No graph loaded — run coderadar init first")


def _json(value: Any) -> str:
    return json.dumps(value, indent=1, default=str)


def create_server(graph: Any = None):
    """Build an `mcp` server whose tools delegate to the Rust engine."""
    from mcp.server.fastmcp import FastMCP

    server = FastMCP("CodeRadar", instructions=SERVER_INSTRUCTIONS)

    @server.tool()
    def coderadar_explore(query_text: str = "", symbols: list[str] | None = None) -> str:
        """Source plus call paths for the named symbols."""
        return _json(explore(query_text, symbols or [], "both", 8))

    @server.tool()
    def coderadar_search(query_text: str, top_k: int = 10) -> str:
        """Keyword search over names, signatures and docstrings."""
        return _json(search(query_text, None, top_k))

    @server.tool()
    def coderadar_node(entity_id: str, include_neighbors: bool = False) -> str:
        """One entity's details."""
        return _json(node(entity_id, include_neighbors))

    @server.tool()
    def coderadar_callers(entity_id: str) -> str:
        """Direct callers."""
        return _json(callers(entity_id))

    @server.tool()
    def coderadar_callees(entity_id: str) -> str:
        """Direct callees."""
        return _json(callees(entity_id))

    @server.tool()
    def coderadar_affected(entity_id: str, max_depth: int = 5) -> str:
        """Blast radius."""
        return _json(affected(entity_id, max_depth))

    @server.tool()
    def coderadar_query(text: str) -> str:
        """Structured query."""
        return _json(query(text))

    @server.tool()
    def coderadar_traverse(entity_id: str, direction: str = "both", max_depth: int = 3) -> str:
        """Any edge kind, either direction."""
        return _json(traverse(entity_id, direction, None, max_depth))

    @server.tool()
    def coderadar_dead_code() -> str:
        """Ranked dead-code evidence."""
        return _json(dead_code(0.6, False, 100))

    @server.tool()
    def coderadar_get_smells() -> str:
        """Code smell findings."""
        return _json(get_smells(None, None, "normal"))

    @server.tool()
    def coderadar_find_clones() -> str:
        """Duplicate-code groups."""
        return _json(find_clones(10, 0.8, 100))

    @server.tool()
    def coderadar_find_scaffolding() -> str:
        """Scaffolding debt."""
        return _json(find_scaffolding(False, 100))

    @server.tool()
    def coderadar_diagnose() -> str:
        """Graph self-health."""
        return _json(diagnose())

    @server.tool()
    def coderadar_open_items(scope: str | None = None) -> str:
        """Unchecked checklist items."""
        return _json(open_items(scope))

    @server.tool()
    def coderadar_annotations_on(target: str) -> str:
        """Annotations anchored to a concept."""
        return _json(annotations_on(target))

    @server.tool()
    def coderadar_comments_on(target: str) -> str:
        """Comments anchored to a concept."""
        return _json(comments_on(target))

    @server.tool()
    def coderadar_module_children(module_id: str) -> str:
        """A module's contents."""
        return _json(module_children(module_id))

    @server.tool()
    def coderadar_resolve(name: str, limit: int = 5) -> str:
        """Framework reference resolution."""
        return _json(resolve(name, limit))

    @server.tool()
    def coderadar_search_similar(query_text: str, top_k: int = 10) -> str:
        """Semantic search."""
        return _json(search_similar(query_text, top_k))

    @server.tool()
    def coderadar_search_hybrid(query_text: str, top_k: int = 10) -> str:
        """Keyword + vector search."""
        return _json(search_hybrid(query_text, top_k, query_text))

    @server.tool()
    def coderadar_read_source(entity_id: str) -> str:
        """Verbatim source of one entity."""
        return read_source(entity_id) or ""

    @server.tool()
    def coderadar_as_of(timestamp: str, query_text: str = "") -> str:
        """The graph at a past instant."""
        return _json(as_of(timestamp, query_text, None, "main", False, None, False))

    @server.tool()
    def coderadar_history_of(file: str) -> str:
        """Every recorded version of a file."""
        return _json(history_of(file))

    @server.tool()
    def coderadar_changes_between(t1: str, t2: str) -> str:
        """Entity diff between two instants."""
        return _json(changes_between(t1, t2, None, "main", False))

    @server.tool()
    def coderadar_replace_body(entity_id: str, new_body: str, dry_run: bool = True) -> str:
        """Replace a body (dry run by default)."""
        return _json(replace_body(entity_id, new_body, None, dry_run))

    @server.tool()
    def coderadar_update_signature(entity_id: str, new_signature: str, dry_run: bool = True) -> str:
        """Update a signature (dry run by default)."""
        return _json(update_signature(entity_id, new_signature, False, dry_run))

    @server.tool()
    def coderadar_rename(entity_id: str, new_name: str, dry_run: bool = True) -> str:
        """Rename an entity (dry run by default)."""
        return _json(rename(entity_id, new_name, dry_run))

    @server.tool()
    def coderadar_create_entity(
        file_path: str, language: str, kind: str, name: str, body: str, dry_run: bool = True
    ) -> str:
        """Insert an entity (dry run by default)."""
        return _json(create_entity(file_path, language, kind, name, body, None, "end", None, dry_run))

    @server.tool()
    def coderadar_update_file(file_path: str, content: str | None = None) -> str:
        """Incrementally sync one file."""
        return _json(update_file(file_path, content))

    @server.tool()
    def coderadar_status() -> str:
        """What is served and how fresh it is."""
        return _json(status())

    return server


__all__ = ["SERVER_INSTRUCTIONS", "create_server", "requires_index"]
