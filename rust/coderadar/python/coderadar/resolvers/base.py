"""Shape-only base types for the Rust-backed framework resolvers."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


@dataclass
class SyntheticNode:
    """A framework-synthesized entity that does not exist literally in source."""

    id: str
    name: str
    kind: str
    file_path: str
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass
class SyntheticEdge:
    """A framework-synthesized edge between graph entities."""

    source_id: str
    target_id: str
    kind: str
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass
class FrameworkExtraction:
    """Result of one resolver's per-file extraction."""

    file_path: str
    nodes: list[SyntheticNode] = field(default_factory=list)
    edges: list[SyntheticEdge] = field(default_factory=list)


class FrameworkResolver(ABC):
    """Interface shared with the original Python resolver classes."""

    @property
    @abstractmethod
    def name(self) -> str:
        """Resolver name used in edge metadata."""

    @abstractmethod
    def detect(self, project_root: Path) -> bool:
        """Whether this resolver's framework is present in ``project_root``."""

    @abstractmethod
    def claims_reference(self, name: str) -> bool:
        """Whether this resolver should try to resolve ``name``."""

    @abstractmethod
    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        """Extract synthetic nodes and edges from one file."""

    @abstractmethod
    def resolve(
        self, ref_name: str, candidates: list[dict[str, Any]]
    ) -> dict[str, Any] | None:
        """Resolve a reference against search candidates."""
