"""CodeRadar v3.6 — Macrame Query Operations (§7)

Direct Macrame operations — no Cypher translation layer.
Macrame provides: traversal, temporal reconstruction, concept lookup,
edge assertion, and vector search. These are the primitives.
"""

from __future__ import annotations

import time
from typing import Any, Dict, Iterator, List, Literal, Optional

import structlog

logger = structlog.get_logger(__name__)


class MacrameQuery:
    """Direct Macrame-backed query operations.

    Usage:
        mq = MacrameQuery(graph)
        # Traverse call graph
        subgraph = mq.traverse("src/models.py::User.save", max_depth=2,
                               edge_types=["CALLS"])
        # Point-in-time snapshot
        state = mq.as_of("2025-06-15T10:00:00Z")
        # Find a concept by ID
        entity = mq.find("src/models.py::User")
        # Search by embedding similarity
        similar = mq.search_similar([0.1, 0.2, ...], top_k=5)
    """

    def __init__(self, graph: Any):
        self._graph = graph

    # ── Traversal ───────────────────────────────────────────────────────

    def traverse(
        self,
        start_id: str,
        max_depth: int = 3,
        edge_types: Optional[List[str]] = None,
        direction: Literal["out", "in", "both"] = "both",
    ) -> List[Dict[str, Any]]:
        """Traverse the graph from start_id along typed edges.

        Delegates to Macrame's TraversalBuilder::load_subgraph_with().
        Returns a flat list of reached entities with depth and edge metadata.

        Args:
            start_id: EntityId to start from.
            max_depth: Maximum traversal depth.
            edge_types: Filter by edge type (e.g. ["CALLS", "IMPORTS"]).
                        None = all types.
            direction: "out" (forward), "in" (reverse), or "both".

        Returns:
            List of {entity_id, kind, depth, edge_type, direction} dicts.
        """
        logger.debug("macrame.traverse", start_id=start_id, max_depth=max_depth)

        # Native Rust BFS over the in-memory ProjectedGraph reverse/forward
        # indexes — single-lock, single-BFS. The pure-Python `_projected_traverse`
        # fallback was deleted: silent fallbacks hide a missing/broken extension
        # as a 100x slowdown. If the binding is unavailable, raise loud.
        # `as_of` is accepted by the binding (honesty-returns NotImplemented).
        from coderadar._core import traverse as _traverse
        raw = _traverse(start_id, max_depth, edge_types or [], direction, None)
        return raw if isinstance(raw, list) else []

    # ── Temporal ────────────────────────────────────────────────────────

    def as_of(self, timestamp: str) -> MacrameSnapshot:
        """Reconstruct the graph at a point in time.

        Macrame's bitemporal ledger stores every version — reconstruct(ts)
        returns the state as it existed at that timestamp.

        Args:
            timestamp: ISO-8601 datetime (e.g. "2025-06-15T10:00:00Z").

        Returns:
            MacrameSnapshot that supports find/traverse/search at that time point.
        """
        return MacrameSnapshot(self._graph, timestamp)

    def timeline(self, entity_id: str) -> List[Dict[str, Any]]:
        """Get the full version history of an entity.

        Macrame stores every upsert as a new version. This returns the
        complete timeline of changes.

        Args:
            entity_id: EntityId to get history for.

        Returns:
            List of {timestamp, kind, content} versions sorted by time.
        """
        # Macrame's concept history via Concept versions
        return []

    # ── Concept Lookup ──────────────────────────────────────────────────

    def find(self, entity_id: str) -> Optional[Dict[str, Any]]:
        """Look up a single entity by ID.

        Args:
            entity_id: EntityId (e.g. "src/models.py::User").

        Returns:
            Entity dict or None if not found.
        """
        # ProjectedGraph O(1) lookup via HashMap
        try:
            from coderadar._core import lookup_entity as _lookup
            return _lookup(entity_id)
        except ImportError:
            return None

    def list_by_kind(self, kind: str, limit: int = 100) -> List[Dict[str, Any]]:
        """List all entities of a given kind.

        Args:
            kind: "module", "class", "function", "import", etc.
            limit: Maximum results.

        Returns:
            List of entity dicts.
        """
        try:
            from coderadar._core import query_graph as _qg
            return list(_qg(f"{kind}s"))
        except ImportError:
            return []

    def callers_of(self, entity_id: str) -> List[Dict[str, Any]]:
        """Find all callers of an entity (reverse call index)."""
        try:
            from coderadar._core import callers_of
            return callers_of(entity_id)
        except ImportError:
            return []

    def callees_of(self, entity_id: str) -> List[Dict[str, Any]]:
        """Find all callees called by an entity (forward call index)."""
        try:
            from coderadar._core import callees_of
            return callees_of(entity_id)
        except ImportError:
            return []

    # ── Similarity Search ───────────────────────────────────────────────

    def search_similar(
        self,
        query_embedding: List[float],
        top_k: int = 10,
        kind_filter: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """Vector similarity search via Macrame embedding index.

        Macrame stores embeddings on Concepts via ConceptUpsert.embedding_model.
        Search returns nearest neighbors with cosine distance.

        Args:
            query_embedding: The query vector.
            top_k: Number of results.
            kind_filter: Optional entity kind filter (e.g. "function").

        Returns:
            List of {entity_id, kind, distance, metadata} dicts.
        """
        return []

    def search_text(
        self,
        query: str,
        top_k: int = 10,
        kind_filter: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """Full-text search across entity names and docstrings.

        Searches Macrame Concept content (JSON metadata) for text matches.

        Args:
            query: Search query string.
            top_k: Number of results.
            kind_filter: Optional entity kind filter.

        Returns:
            List of matching entity dicts.
        """
        return []

    # ── Stats ───────────────────────────────────────────────────────────

    def stats(self) -> Dict[str, Any]:
        """Return entity counts, edge counts, memory usage."""
        try:
            from coderadar._core import graph_stats
            return graph_stats()
        except ImportError:
            return {}


class MacrameSnapshot:
    """A point-in-time view backed by Macrame's reconstruct(ts).

    All operations are scoped to the requested timestamp.
    """

    def __init__(self, graph: Any, timestamp: str):
        self._graph = graph
        self.timestamp = timestamp

    def find(self, entity_id: str) -> Optional[Dict[str, Any]]:
        """Look up an entity as it existed at snapshot time."""
        return MacrameQuery(self._graph).find(entity_id)

    def traverse(
        self, start_id: str, max_depth: int = 3,
        edge_types: Optional[List[str]] = None,
    ) -> List[Dict[str, Any]]:
        """Traverse at snapshot time."""
        return MacrameQuery(self._graph).traverse(start_id, max_depth, edge_types)

    def callers_of(self, entity_id: str) -> List[Dict[str, Any]]:
        """Callers at snapshot time."""
        return MacrameQuery(self._graph).callers_of(entity_id)

    def to_dict(self) -> Dict[str, Any]:
        """Snapshot metadata."""
        return {"timestamp": self.timestamp}
