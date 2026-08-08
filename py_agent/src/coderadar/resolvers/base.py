"""CodeRadar Framework Resolvers — §28 Framework Resolver Interface

FrameworkResolver trait and Phase 1 implementations for Django, Flask, and
FastAPI. These resolvers detect framework usage patterns in Python projects
and synthesize additional nodes/edges that tree-sitter can't see: URL routing,
middleware chains, dependency injection, ORM model relationships.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Set


@dataclass
class SyntheticNode:
    """A framework-synthesized entity that doesn't exist in source literally.

    Examples: a Django URL pattern, a Flask route registration, a FastAPI
    dependency injection chain. These become nodes in the code graph with
    `provenance: "heuristic"` edges.
    """
    id: str                      # EntityId in CodeRadar's format
    name: str
    kind: str                    # "route", "middleware", "dependency"
    file_path: str
    metadata: Dict[str, Any] = field(default_factory=dict)


@dataclass
class SyntheticEdge:
    """A framework-synthesized edge connecting nodes.

    Edges carry `provenance: "heuristic"` and the resolver name in metadata
    so agents can distinguish them from tree-sitter-extracted edges.
    """
    source_id: str
    target_id: str
    kind: str                    # "handles", "uses", "injects", "registers"
    metadata: Dict[str, Any] = field(default_factory=dict)


@dataclass
class FrameworkExtraction:
    """Result of a per-file framework extraction."""
    file_path: str
    nodes: List[SyntheticNode] = field(default_factory=list)
    edges: List[SyntheticEdge] = field(default_factory=list)


class FrameworkResolver(ABC):
    """Base trait for framework resolvers.

    Each resolver handles one web framework and synthesizes graph nodes
    and edges for framework-specific constructs that tree-sitter can't
    see directly (URL routing, middleware, DI, ORM relationships).
    """

    @property
    @abstractmethod
    def name(self) -> str:
        """Resolver name — used in edge metadata.synthesizedBy."""
        ...

    @abstractmethod
    def detect(self, project_root: Path) -> bool:
        """Can this resolver handle this project?

        Args:
            project_root: Root directory of the project.

        Returns:
            True if the framework's telltale files/signatures are present.
        """
        ...

    @abstractmethod
    def claims_reference(self, name: str) -> bool:
        """Does this resolver claim to resolve this reference?

        E.g. Django resolver claims `*Model`, `*View`, `*Form` patterns.

        Args:
            name: Reference name (bare or qualified).

        Returns:
            True if this resolver should attempt to resolve the reference.
        """
        ...

    @abstractmethod
    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        """Extract synthetic nodes and edges from a single file.

        Called during incremental re-indexing for each file in the project.

        Args:
            file_path: Path to the source file.
            source: File content as a string.

        Returns:
            Synthetic nodes and edges to merge into the graph.
        """
        ...

    @abstractmethod
    def resolve(
        self, ref_name: str, graph: Any,
    ) -> Optional[Dict[str, Any]]:
        """Resolve a single reference.

        Args:
            ref_name: The unresolved reference string.
            graph: The CodeGraph instance for entity lookups.

        Returns:
            Resolved target dict or None if this resolver can't resolve it.
        """
        ...
