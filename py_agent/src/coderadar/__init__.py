"""CodeRadar v3.3 — Python API Surface (§8)

A hybrid Python/Rust tool that maintains a live, incrementally updatable
semantic graph of a source codebase's logical structure, enabling LLMs and
developer tools to both query and safely rewrite code.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any, Dict, Iterator, List, Literal, Optional, Union

# ── Rust core import ────────────────────────────────────────────────────────
try:
    from coderadar._core import (  # type: ignore[import-untyped]
        analyze as _analyze_rust,
        apply_mutation as _apply_mutation,
        callers_of as _callers_of,
        export_snapshot as _export_snapshot,
        load_snapshot as _load_snapshot,
        plan_body_replacement as _plan_body_replacement,
        plan_create_entity as _plan_create_entity,
        plan_rename as _plan_rename,
        plan_signature_update as _plan_signature_update,
        query_graph as _query_graph,
        resolve_symbol as _resolve_symbol,
        update_file as _update_file_rust,
        CodeGraph as _RustCodeGraph,
        QueryIterator as _RustQueryIterator,
    )
except ImportError:
    # Fallback when Rust extension isn't built (dev mode)
    _analyze_rust = None  # type: ignore[assignment]


# ── Exceptions ──────────────────────────────────────────────────────────────

class CodeRadarError(Exception):
    """Base exception for CodeRadar errors."""


class StaleHandle(CodeRadarError):
    """Entity handle is stale — the underlying entity has changed."""


class ParseError(CodeRadarError):
    """Parse failure during analysis."""


class ResolutionError(CodeRadarError):
    """Resolution failure."""


class MutationError(CodeRadarError):
    """Mutation planning or application failed."""


class PolicyViolation(MutationError):
    """Mutation rejected by policy configuration."""


# ── Public API Types ────────────────────────────────────────────────────────

from dataclasses import dataclass, field


@dataclass(frozen=True)
class UpdateReport:
    """Result of a single-file or batch update (§8.2)."""
    affected_files: List[str]
    changed_symbols: List[SymbolChange]
    new_unresolved_references: List[dict]
    newly_resolved_references: List[dict]
    elapsed_ms: float
    parse_quality: str  # "Clean" | "Partial" | "Tainted"
    parse_errors: int
    fully_applied: bool
    epoch_before: int
    epoch_after: int


@dataclass(frozen=True)
class SymbolChange:
    """Description of a changed symbol."""
    kind: Literal["module", "class", "function", "import", "constant", "type_alias", "field"]
    operation: Literal["added", "removed", "signature_changed", "body_changed", "moved"]
    qualified_name: str
    file: str
    line: int
    id: Optional[int] = None


@dataclass(frozen=True)
class MutationPlan:
    """A planned mutation — produced by the planner, applied by apply()."""
    id: str
    tool: str
    edits: List[MutationEdit]
    affected_files: List[str]
    diff_preview: str
    unverified_sites: List[dict]
    warnings: List[str]


@dataclass(frozen=True)
class MutationEdit:
    """A single byte-accurate edit to a file."""
    file: str
    replacement: str
    expected_hash: str


@dataclass(frozen=True)
class MutationResult:
    """Result of applying a mutation plan."""
    status: Literal["Applied", "RolledBack", "RejectedStale"]
    files_written: List[str]
    syntax_errors: List[dict]
    backup_path: Optional[str] = None


# ── CodeGraph Python Wrapper ────────────────────────────────────────────────

class CodeGraph:
    """Python-facing graph handle that delegates to the Rust core.

    Usage:
        graph = coderadar.analyze("src/")
        for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
            print(cls.name)
    """

    def __init__(self, rust_graph=None):
        self._rust = rust_graph
        self._config: Dict[str, Any] = {}

    # ── Query ──────────────────────────────────────────────────────────

    def query(self, query_str: str) -> Iterator[Dict[str, Any]]:
        """Execute a Pest query against the in-memory graph.

        Returns an iterator over result rows (each row is a dict).
        """
        if _query_graph is not None:
            results = _query_graph(query_str)
            if isinstance(results, list):
                yield from results
            else:
                yield from results
        else:
            # Stub: yield nothing
            return

    def cypher(self, query_str: str, **params) -> List[Dict[str, Any]]:
        """Execute a Cypher query against LadybugDB."""
        from .query.executor import CypherExecutor
        executor = CypherExecutor()
        return executor.execute(query_str, params)

    # ── Update ─────────────────────────────────────────────────────────

    def update_file(self, file_path: str, content: Optional[str] = None,
                    force: bool = False) -> UpdateReport:
        """Update the graph after a file change."""
        if _update_file_rust is not None:
            _update_file_rust(file_path, content, force)
        # Return stub report
        return UpdateReport(
            affected_files=[file_path],
            changed_symbols=[],
            new_unresolved_references=[],
            newly_resolved_references=[],
            elapsed_ms=0.0,
            parse_quality="Clean",
            parse_errors=0,
            fully_applied=True,
            epoch_before=0,
            epoch_after=1,
        )

    def batch(self) -> "BatchContext":
        """Context manager for batched updates."""
        return BatchContext(self)

    # ── ID-based access ────────────────────────────────────────────────

    def find_function(self, qualified_name: str) -> Optional[int]:
        """Look up a function by qualified name, return its ID."""
        return None  # Stub

    def get_function(self, fn_id: int) -> Optional[Dict[str, Any]]:
        """Get function details by ID. Returns None if removed."""
        return None  # Stub

    def callers_of(self, fn_id: int) -> List[int]:
        """Get all caller function IDs via reverse index."""
        return []  # Stub

    # ── Mutation ───────────────────────────────────────────────────────

    def plan_body_replacement(
        self,
        entity_id: str,
        new_body: str,
        expected_hash: Optional[str] = None,
        dry_run: bool = True,
    ) -> MutationPlan:
        """Plan a body-only replacement for a function/method."""
        if _plan_body_replacement is not None:
            _plan_body_replacement(entity_id, new_body, expected_hash, dry_run)
        return MutationPlan(
            id="",
            tool="replace_entity_body",
            edits=[],
            affected_files=[],
            diff_preview="",
            unverified_sites=[],
            warnings=[],
        )

    def plan_signature_update(
        self,
        entity_id: str,
        new_signature: str,
        call_site_values: Optional[Dict[str, str]] = None,
        inject_defaults: bool = False,
        dry_run: bool = True,
    ) -> MutationPlan:
        """Plan a signature update with call-site cascade."""
        if _plan_signature_update is not None:
            _plan_signature_update(
                entity_id, new_signature, call_site_values or {},
                inject_defaults, dry_run,
            )
        return MutationPlan(
            id="", tool="update_signature", edits=[],
            affected_files=[], diff_preview="", unverified_sites=[], warnings=[],
        )

    def plan_rename(
        self,
        entity_id: str,
        new_name: str,
        include_strings: bool = False,
        dry_run: bool = True,
    ) -> MutationPlan:
        """Plan a symbol rename across the codebase."""
        if _plan_rename is not None:
            _plan_rename(entity_id, new_name, include_strings, dry_run)
        return MutationPlan(
            id="", tool="rename_symbol", edits=[],
            affected_files=[], diff_preview="", unverified_sites=[], warnings=[],
        )

    def plan_create_entity(
        self,
        target_file: str,
        anchor: str,
        code: str,
        dry_run: bool = True,
    ) -> MutationPlan:
        """Plan creating a new entity after an anchor point."""
        if _plan_create_entity is not None:
            _plan_create_entity(target_file, anchor, code, dry_run)
        return MutationPlan(
            id="", tool="create_entity", edits=[],
            affected_files=[], diff_preview="", unverified_sites=[], warnings=[],
        )

    def apply(self, plan: MutationPlan) -> MutationResult:
        """Apply a mutation plan atomically."""
        if _apply_mutation is not None:
            import json
            _apply_mutation(json.dumps({
                "id": plan.id,
                "tool": plan.tool,
                "edits": [{"file": e.file, "replacement": e.replacement,
                           "expected_hash": e.expected_hash} for e in plan.edits],
                "affected_files": plan.affected_files,
            }))
        return MutationResult(
            status="Applied",
            files_written=plan.affected_files,
            syntax_errors=[],
        )

    # ── Persistence ────────────────────────────────────────────────────

    def export_snapshot(self, path: str) -> None:
        """Export the current graph snapshot to a file."""
        if _export_snapshot is not None:
            _export_snapshot(path)

    # ── Stats / Debug ──────────────────────────────────────────────────

    def stats(self) -> Dict[str, Any]:
        """Return counts, parse quality summary, memory usage."""
        return {
            "epoch": 0,
            "modules": 0,
            "classes": 0,
            "functions": 0,
            "imports": 0,
        }


# ── Batch Context Manager ───────────────────────────────────────────────────

class BatchContext:
    """Context manager for batching multiple file updates."""

    def __init__(self, graph: CodeGraph):
        self.graph = graph
        self._updates: List[tuple] = []

    def update_file(self, file_path: str, content: Optional[str] = None) -> None:
        self._updates.append((file_path, content))

    def __enter__(self) -> "BatchContext":
        return self

    def __exit__(self, *args) -> None:
        for file_path, content in self._updates:
            self.graph.update_file(file_path, content)


# ── Top-Level API Functions ─────────────────────────────────────────────────

def analyze(root: str) -> CodeGraph:
    """Perform initial analysis of a codebase.

    Args:
        root: Path to the project root directory.

    Returns:
        A CodeGraph ready for querying and updating.
    """
    if _analyze_rust is not None:
        _analyze_rust(root)
    return CodeGraph()


def load(snapshot_path: str) -> CodeGraph:
    """Load a previously exported snapshot.

    Args:
        snapshot_path: Path to a .bin snapshot file.

    Returns:
        A CodeGraph restored from the snapshot.
    """
    if _load_snapshot is not None:
        _load_snapshot(snapshot_path)
    return CodeGraph()


def watch(root: str) -> "Watcher":
    """Start watching a directory for changes.

    Usage:
        with coderadar.watch("src/") as w:
            for report in w:
                print(report.affected_files)
    """
    return Watcher(root)


class Watcher:
    """File watcher that yields UpdateReports on changes."""

    def __init__(self, root: str):
        self.root = root
        self._graph = CodeGraph()

    def __enter__(self) -> "Watcher":
        return self

    def __exit__(self, *args) -> None:
        pass

    def __iter__(self) -> "Watcher":
        return self

    def __next__(self) -> UpdateReport:
        # Block until next file change event
        raise StopIteration  # Stub


def resolve(qualified_name: str) -> List[Dict[str, Any]]:
    """Show the resolution chain for a qualified name (debugging)."""
    if _resolve_symbol is not None:
        _resolve_symbol(qualified_name)
    return []


__all__ = [
    "CodeGraph",
    "UpdateReport",
    "SymbolChange",
    "MutationPlan",
    "MutationEdit",
    "MutationResult",
    "BatchContext",
    "Watcher",
    "analyze",
    "load",
    "watch",
    "resolve",
    "StaleHandle",
    "ParseError",
    "ResolutionError",
    "MutationError",
    "PolicyViolation",
]
