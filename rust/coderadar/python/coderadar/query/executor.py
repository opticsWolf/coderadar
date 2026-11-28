"""Macrame query operations over the Rust engine.

Every method here is a delegation: traversal, lookup, listing by kind, the
call indexes and stats all run in `core_indexer`. Three methods are honest
placeholders in the reference too (`timeline`, `search_similar`,
`search_text` return `[]`), and they stay that way rather than inventing
results.
"""

from __future__ import annotations

from typing import Any, Literal

from .. import (
    callers as _callers,
    callees as _callees,
    find_entity as _find_entity,
    query as _query,
    stats as _stats,
    traverse as _traverse,
)


class MacrameQuery:
    """Direct engine-backed query operations."""

    def __init__(self, graph: Any = None):
        self._graph = graph

    def traverse(
        self,
        start_id: str,
        max_depth: int = 3,
        edge_types: list[str] | None = None,
        direction: Literal["out", "in", "both"] = "both",
    ) -> list[dict[str, Any]]:
        """Traverse the graph from `start_id` along typed edges."""
        kinds = [kind.lower() for kind in (edge_types or [])]
        report = _traverse(start_id, direction, kinds, max_depth)
        results = report.get("results") if isinstance(report, dict) else None
        return list(results or [])

    def as_of(self, timestamp: str) -> "MacrameSnapshot":
        """Reconstruct the graph at a point in time."""
        return MacrameSnapshot(self._graph, timestamp)

    def timeline(self, entity_id: str) -> list[dict[str, Any]]:
        """Entity version history (not wired in the reference either)."""
        return []

    def find(self, entity_id: str) -> dict[str, Any] | None:
        """Look up a single entity by ID."""
        return _find_entity(entity_id)

    def list_by_kind(self, kind: str, limit: int = 100) -> list[dict[str, Any]]:
        """List all entities of a given kind."""
        return list(_query(f"{kind}s"))[:limit]

    def callers_of(self, entity_id: str) -> list[dict[str, Any]]:
        """Find all callers of an entity (reverse call index)."""
        return list(_callers(entity_id))

    def callees_of(self, entity_id: str) -> list[dict[str, Any]]:
        """Find all callees called by an entity (forward call index)."""
        return list(_callees(entity_id))

    def search_similar(
        self,
        query_embedding: list[float],
        top_k: int = 10,
        kind_filter: str | None = None,
    ) -> list[dict[str, Any]]:
        """Vector similarity search (not wired in the reference either)."""
        return []

    def search_text(
        self,
        query: str,
        top_k: int = 10,
        kind_filter: str | None = None,
    ) -> list[dict[str, Any]]:
        """Full-text search (not wired in the reference either)."""
        return []

    def stats(self) -> dict[str, Any]:
        """Entity counts and graph statistics."""
        return _stats()


class MacrameSnapshot:
    """A point-in-time view (the timestamp rides along; reads are live)."""

    def __init__(self, graph: Any, timestamp: str):
        self._graph = graph
        self.timestamp = timestamp

    def find(self, entity_id: str) -> dict[str, Any] | None:
        """Look up an entity as it existed at snapshot time."""
        return MacrameQuery(self._graph).find(entity_id)

    def traverse(
        self,
        start_id: str,
        max_depth: int = 3,
        edge_types: list[str] | None = None,
    ) -> list[dict[str, Any]]:
        """Traverse at snapshot time."""
        return MacrameQuery(self._graph).traverse(start_id, max_depth, edge_types)

    def callers_of(self, entity_id: str) -> list[dict[str, Any]]:
        """Callers at snapshot time."""
        return MacrameQuery(self._graph).callers_of(entity_id)

    def to_dict(self) -> dict[str, Any]:
        """Snapshot metadata."""
        return {"timestamp": self.timestamp}


__all__ = ["MacrameQuery", "MacrameSnapshot"]
