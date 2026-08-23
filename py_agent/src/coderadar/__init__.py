"""CodeRadar v3.6 — Python API Surface (§8)

A hybrid Python/Rust tool that maintains a live, incrementally updatable
semantic graph of a source codebase's logical structure, enabling LLMs and
developer tools to both query and safely rewrite code.

v3.6 Architecture:
  - Macrame (bitemporal graph) — source of truth for persistence, temporal
    queries, agent traversals, vector search
  - In-memory ProjectedGraph — sub-ms Pest queries, reverse indexes,
    mutation planning
  - Flat-buffer FFI — one boundary crossing per file (132-byte entity rows)
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, Iterator, List, Literal, Optional, Union

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
    expected_hash: str = ""
    span_start: Optional[int] = None
    span_end: Optional[int] = None


@dataclass(frozen=True)
class MutationResult:
    """Result of applying a mutation plan."""
    status: Literal["Applied", "RolledBack", "RejectedStale", "RejectedPolicy"]
    files_written: List[str]
    syntax_errors: List[dict]
    backup_path: Optional[str] = None


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


# ── CodeGraph Python Wrapper ────────────────────────────────────────────────

def _parse_plan_dict(result: dict, tool: str) -> "MutationPlan":
    """Convert a Rust plan_to_dict result into a MutationPlan."""
    edits = []
    for e in result.get("edits", []) or []:
        edits.append(MutationEdit(
            file=e.get("file", ""),
            replacement=e.get("replacement", ""),
            expected_hash=e.get("expected_hash", ""),
            span_start=e.get("span_start"),
            span_end=e.get("span_end"),
        ))
    return MutationPlan(
        id=result.get("id", ""),
        tool=tool,
        edits=edits,
        affected_files=list(result.get("affected_files", []) or []),
        diff_preview=result.get("diff_preview", ""),
        unverified_sites=list(result.get("unverified_sites", []) or []),
        warnings=list(result.get("warnings", []) or []),
    )


class CodeGraph:
    """Python-facing graph handle backed by Macrame bitemporal ledger.

    The CodeGraph class holds a handle to the in-memory ProjectedGraph
    (built from extracted ASTs) and an optional Macrame persistent store.
    It is NOT a reference to the CodeGraph project — it's the graph
    data structure at the heart of the CodeRadar system.

    Usage:
        graph = coderadar.analyze("src/")
        for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
            print(cls.name)
        snapshot = graph.as_of("2025-06-15T10:00:00Z")
        for caller in graph.callers_of("src/models.py::User.save"):
            print(caller["name"])
    """

    def __init__(self, db_path: Optional[str] = None):
        self._db_path = db_path or ".coderadar/store/coderadar.db"
        self._config: Dict[str, Any] = {}
        self._macrame = None  # Macrame Database handle (lazy)

    # ── Query ──────────────────────────────────────────────────────────

    def query(self, query_str: str) -> Iterator[Dict[str, Any]]:
        """Execute a Pest query against the in-memory ProjectedGraph.

        Returns an iterator over result rows (each row is a dict).
        """
        try:
            from coderadar._core import query_graph as _query_graph
            results = _query_graph(query_str)
            if isinstance(results, list):
                yield from results
            else:
                yield from results
        except ImportError:
            return

    def explore(
        self,
        start_id: str,
        direction: Literal["in", "out", "both"] = "both",
        max_depth: int = 3,
        edge_kinds: Optional[List[str]] = None,
    ) -> List[Dict[str, Any]]:
        """Traverse the graph from start_id via Macrame.

        Uses Macrame's TraversalBuilder for subgraph loading, then
        enriches results with entity metadata from ProjectedGraph.

        Args:
            start_id: EntityId to start traversal from (e.g. "src/models.py::User").
            direction: "in" (callers), "out" (callees), or "both".
            max_depth: Maximum traversal depth.
            edge_kinds: Filter by edge kinds (e.g. ["calls", "imports"]).
                        None means all kinds.

        Returns:
            List of dicts with `entity_id`, `edge_kind`, `direction`, `depth`.
        """
        # Macrame traversal returns subgraph via TraversalBuilder
        # For now, use ProjectedGraph reverse indexes as fallback
        results: List[Dict[str, Any]] = []
        visited: set = {start_id}

        if direction in ("out", "both"):
            edges = self._get_outgoing(start_id, edge_kinds)
            for e in edges:
                if e["target"] not in visited:
                    visited.add(e["target"])
                    results.append({
                        "entity_id": e["target"],
                        "edge_kind": e["kind"],
                        "direction": "out",
                        "depth": 1,
                    })

        if direction in ("in", "both"):
            edges = self._get_incoming(start_id, edge_kinds)
            for e in edges:
                if e["source"] not in visited:
                    visited.add(e["source"])
                    results.append({
                        "entity_id": e["source"],
                        "edge_kind": e["kind"],
                        "direction": "in",
                        "depth": 1,
                    })

        return results

    def _get_incoming(self, entity_id: str, edge_kinds: Optional[List[str]] = None) -> List[Dict[str, Any]]:
        """Internal: get incoming edges for an entity."""
        # Delegates to Rust core via _core module
        try:
            from coderadar._core import callers_of as _callers_of
            raw = _callers_of(entity_id)
            if edge_kinds:
                return [r for r in raw if r.get("kind") in edge_kinds]
            return raw
        except ImportError:
            return []

    def _get_outgoing(self, entity_id: str, edge_kinds: Optional[List[str]] = None) -> List[Dict[str, Any]]:
        """Internal: get outgoing edges for an entity."""
        try:
            from coderadar._core import callees_of as _callees_of
            raw = _callees_of(entity_id)
            if edge_kinds:
                return [r for r in raw if r.get("kind") in edge_kinds]
            return raw
        except ImportError:
            return []

    # ── Macrame Operations ────────────────────────────────────────────

    def traverse(
        self,
        start_id: str,
        max_depth: int = 3,
        edge_types: Optional[List[str]] = None,
        direction: Literal["in", "out", "both"] = "both",
    ) -> List[Dict[str, Any]]:
        """Traverse the graph from start_id via Macrame."""
        from .query import MacrameQuery
        return MacrameQuery(self).traverse(start_id, max_depth, edge_types, direction)

    def as_of(self, timestamp: str) -> "Snapshot":
        """Return a point-in-time snapshot via Macrame's reconstruct(ts)."""
        return Snapshot(self, timestamp)

    def find(self, entity_id: str) -> Optional[Dict[str, Any]]:
        """Look up an entity by ID."""
        from .query import MacrameQuery
        return MacrameQuery(self).find(entity_id)

    def callers_of(self, entity_id: str) -> List[Dict[str, Any]]:
        """Find callers via reverse call index."""
        from .query import MacrameQuery
        return MacrameQuery(self).callers_of(entity_id)

    def callees_of(self, entity_id: str) -> List[Dict[str, Any]]:
        """Find callees via forward call index."""
        from .query import MacrameQuery
        return MacrameQuery(self).callees_of(entity_id)

    def search_similar(
        self, query_embedding: List[float], top_k: int = 10,
    ) -> List[Dict[str, Any]]:
        """Vector similarity search via cosine similarity against stored embeddings.

        Requires embeddings to be pre-computed and stored via the embedding pipeline.
        """
        try:
            from coderadar._core import search_similar as _ss
            return _ss(query_embedding, top_k)
        except ImportError:
            return []

    # ── Embedding Pipeline ────────────────────────────────────────────

    def compute_embeddings(self, model_name: Optional[str] = None,
                           batch_size: int = 32) -> Dict[str, int]:
        """Compute and store embeddings for all indexable entities.

        Uses fastembed for local embedding generation. Embeddings are
        written into the in-memory Function.embedding field, making them
        immediately available for search_similar() queries.

        Args:
            model_name: HuggingFace model name for fastembed. None takes the
                configured model, which is also what the search path loads —
                a mismatch there silently breaks similarity.
            batch_size: Batch size for embedding generation.

        Returns:
            Dict with metrics: {generated, cached, total, errors}.
        """
        from .embedding import (
            EmbeddingDedup, EmbedTarget, compute_content_hash, embedding_settings,
        )

        configured_model, dimension = embedding_settings()
        dedup = EmbeddingDedup(model_name=model_name or configured_model,
                               dimension=dimension, batch_size=batch_size)
        targets: List[EmbedTarget] = []

        try:
            from coderadar._core import search_entities
            # Collect all embeddable entities across all kinds
            for kind in ("function", "class", "module", "import", "constant", "type_alias"):
                entities = search_entities("", 10_000, kind)
                for entity in entities:
                    entity_id = entity.get("id", "")
                    if not entity_id:
                        continue
                    body = entity.get("signature", "") or entity.get("name", "") or ""
                    content_hash = compute_content_hash(body.encode())
                    targets.append(EmbedTarget(
                        entity_id=entity_id,
                        body=body,
                        content_hash=content_hash,
                        kind=kind,
                    ))
        except ImportError:
            return {"generated": 0, "cached": 0, "total": 0, "errors": 1}

        results = dedup.embed_batch(targets, db=None)
        cached = 0
        try:
            from coderadar._core import set_embeddings_bulk
        except ImportError:
            return {"generated": 0, "cached": 0, "total": len(targets), "errors": 1}

        # One call, one projection clone. Looping set_embedding cloned the
        # whole ProjectedGraph per entity — O(N²) on a project of any size.
        entries = []
        for target, vec in zip(targets, results):
            if vec is None:
                cached += 1
                continue
            entries.append((target.id, list(vec), target.content_hash))

        try:
            report = set_embeddings_bulk(entries)
        except RuntimeError:
            return {"generated": 0, "cached": cached,
                    "total": len(targets), "errors": len(entries)}

        generated = int(report.get("applied", 0))
        errors = len(report.get("missing", []))
        return {"generated": generated, "cached": cached,
                "total": len(targets), "errors": errors}

    # ── Update ─────────────────────────────────────────────────────────

    def update_file(self, file_path: str, content: Optional[str] = None,
                    force: bool = False) -> UpdateReport:
        """Update the graph after a file change.

        Phase 1: Rust parses, diffs, and stages changes.
        Phase 2: Macrame persists entities and edges with timestamps.
        Phase 3: ProjectedGraph rebuilds reverse indexes.
        """
        try:
            from coderadar._core import update_file as _update_file_rust
            result = _update_file_rust(file_path, content, force)
            if isinstance(result, dict):
                return UpdateReport(
                    affected_files=result.get("affected_files") or [file_path],
                    changed_symbols=[],
                    new_unresolved_references=[],
                    newly_resolved_references=[],
                    elapsed_ms=float(result.get("elapsed_ms", 0.0)),
                    parse_quality=str(result.get("parse_quality", "Clean")),
                    parse_errors=int(result.get("parse_errors", 0)),
                    fully_applied=bool(result.get("fully_applied", True)),
                    epoch_before=0,
                    epoch_after=1,
                )
        except ImportError:
            # Nothing parsed anything, so "Clean" and fully_applied=True were
            # a report about work that did not happen.
            return UpdateReport(
                affected_files=[file_path], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=0.0,
                parse_quality="Error: the coderadar._core extension is not built",
                parse_errors=1,
                fully_applied=False, epoch_before=0, epoch_after=1,
            )
        except RuntimeError as e:
            return UpdateReport(
                affected_files=[file_path], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=0.0, parse_quality=f"Error: {e}", parse_errors=1,
                fully_applied=False, epoch_before=0, epoch_after=1,
            )

        # Fallback (unreachable in practice)
        return UpdateReport(
            affected_files=[file_path], changed_symbols=[],
            new_unresolved_references=[], newly_resolved_references=[],
            elapsed_ms=0.0, parse_quality="Clean", parse_errors=0,
            fully_applied=False, epoch_before=0, epoch_after=1,
        )

    def remove_file(self, file_path: str) -> int:
        """Drop a deleted file's entities from the graph.

        `update_file` cannot do this — it re-reads the file, and a deleted
        file has no content to diff. Returns the number of entities removed.
        """
        try:
            from coderadar._core import remove_file as _remove_file_rust
        except ImportError as exc:
            raise CodeRadarError(
                "The coderadar._core extension is not built; "
                "nothing can be removed from the graph."
            ) from exc
        result = _remove_file_rust(file_path)
        return int(result.get("entities_removed", 0))

    def watch(self, paths: Optional[List[str]] = None,
              debounce_ms: Optional[int] = None,
              max_file_size_bytes: Optional[int] = None) -> "Watcher":
        """Start watching paths for file changes and auto-update the graph.

        Returns a Watcher handle that runs the event loop.

        Usage:
            watcher = graph.watch(["src/", "tests/"])
            watcher.run_forever()  # blocking loop

        Args:
            paths: Directories to watch (default: ["src/", "tests/"]).
            debounce_ms: Debounce window; None takes it from `[watch]`.
            max_file_size_bytes: Skip larger files; None takes it from `[watch]`.
        """
        return Watcher(self, paths or ["src/", "tests/"], debounce_ms,
                       max_file_size_bytes)

    def batch(self) -> "BatchContext":
        """Context manager for batched updates."""
        return BatchContext(self)

    # ── Mutation ───────────────────────────────────────────────────────

    def plan_body_replacement(
        self,
        entity_id: str,
        new_body: str,
        expected_hash: Optional[str] = None,
        dry_run: bool = True,
    ) -> MutationPlan:
        """Plan a body-only replacement for a function/method.

        Uses ProjectedGraph for span lookups; mutation engine for edit planning.
        """
        try:
            from coderadar._core import plan_body_replacement as _pbr
            result = _pbr(entity_id, new_body, expected_hash, dry_run)
            if isinstance(result, dict):
                return _parse_plan_dict(result, "replace_entity_body")
        except ImportError:
            pass
        return MutationPlan(
            id="", tool="replace_entity_body", edits=[],
            affected_files=[], diff_preview="", unverified_sites=[], warnings=[],
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
        try:
            from coderadar._core import plan_signature_update as _psu
            result = _psu(entity_id, new_signature, call_site_values or {},
                          inject_defaults, dry_run)
            if isinstance(result, dict):
                return _parse_plan_dict(result, "update_signature")
        except ImportError:
            pass
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
        try:
            from coderadar._core import plan_rename as _pr
            result = _pr(entity_id, new_name, include_strings, dry_run)
            if isinstance(result, dict):
                return _parse_plan_dict(result, "rename_symbol")
        except ImportError:
            pass
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
        try:
            from coderadar._core import plan_create_entity as _pce
            result = _pce(target_file, anchor, code, dry_run)
            if isinstance(result, dict):
                return _parse_plan_dict(result, "create_entity")
        except ImportError:
            pass
        return MutationPlan(
            id="", tool="create_entity", edits=[],
            affected_files=[], diff_preview="", unverified_sites=[], warnings=[],
        )

    def apply(self, plan: MutationPlan) -> MutationResult:
        """Apply a mutation plan atomically.

        Phase 1: Write edits to disk (with backup).
        Phase 2: Re-parse changed files → stage → Macrame persist.
        Phase 3: Rebuild ProjectedGraph reverse indexes.
        """
        try:
            from coderadar._core import apply_mutation as _am, clear_embeddings_for_file
        except ImportError as exc:  # pragma: no cover - requires an unbuilt extension
            # This used to fall through to status="Applied" with
            # files_written=plan.affected_files — reporting a write that could
            # not have happened, because the code that writes is missing.
            raise CodeRadarError(
                "the coderadar._core extension is not built, so no mutation "
                "can be applied"
            ) from exc

        result = _am(json.dumps({
            "id": plan.id,
            "tool": plan.tool,
            "edits": [{"file": e.file, "span_start": e.span_start or 0,
                       "span_end": e.span_end or 0, "replacement": e.replacement,
                       "expected_hash": e.expected_hash or ""} for e in plan.edits],
            "affected_files": plan.affected_files,
        }))
        if not isinstance(result, dict):
            raise CodeRadarError(f"apply_mutation returned {type(result).__name__}, "
                                 "expected a result dict")

        applied = bool(result.get("applied", False))
        if applied:
            # Only a plan that reached disk changes what the graph should hold.
            for f in plan.affected_files:
                try:
                    self.update_file(f)
                except Exception:
                    pass
            for f in plan.affected_files:
                try:
                    clear_embeddings_for_file(f)
                except RuntimeError:
                    pass

        raw_status = str(result.get("status", ""))
        if applied:
            status = "Applied"
        elif "RejectedStale" in raw_status:
            status = "RejectedStale"
        elif "RejectedPolicy" in raw_status:
            # A policy refusal is not a failed write; collapsing every
            # non-Applied status to RolledBack lost the reason.
            status = "RejectedPolicy"
        else:
            status = "RolledBack"

        return MutationResult(
            status=status,
            # The files the engine says it wrote, not the ones the plan hoped to.
            files_written=result.get("files_written", []),
            syntax_errors=result.get("errors", []),
            backup_path=result.get("backup_path"),
        )

    # ── Persistence ────────────────────────────────────────────────────

    def export_snapshot(self, path: str) -> None:
        """Export the current ProjectedGraph snapshot + Macrame state to a file."""
        try:
            from coderadar._core import export_snapshot as _es
            _es(path)
        except ImportError:
            pass

    # ── Stats / Debug ──────────────────────────────────────────────────

    def stats(self) -> Dict[str, Any]:
        """Return counts, parse quality summary, memory usage."""
        try:
            from coderadar._core import graph_stats as _gs
            return _gs()
        except (ImportError, RuntimeError):
            return {
                "epoch": 0, "modules": 0, "classes": 0,
                "functions": 0, "imports": 0,
            }


# ── Temporal Snapshot ───────────────────────────────────────────────────────

class Snapshot:
    """A point-in-time view of the graph via Macrame's bitemporal ledger.

    Usage:
        snapshot = graph.as_of("2025-06-15T10:00:00Z")
        for fn in snapshot.query("functions where name contains 'handle'"):
            print(fn)
    """

    def __init__(self, graph: CodeGraph, timestamp: str):
        self._graph = graph
        self._timestamp = timestamp

    @property
    def timestamp(self) -> str:
        return self._timestamp

    def query(self, query_str: str) -> Iterator[Dict[str, Any]]:
        """Execute a Pest query against the reconstructed snapshot."""
        # Macrame reconstruct(ts) + ProjectedGraph from that point
        return self._graph.query(query_str)

    def callers_of(self, entity_id: str) -> List[Dict[str, Any]]:
        """Find callers at this point in time."""
        return self._graph.callers_of(entity_id)

    def callees_of(self, entity_id: str) -> List[Dict[str, Any]]:
        """Find callees at this point in time."""
        return self._graph.callees_of(entity_id)

    def explore(
        self,
        start_id: str,
        direction: Literal["in", "out", "both"] = "both",
        max_depth: int = 3,
    ) -> List[Dict[str, Any]]:
        """Traverse from start_id at this point in time."""
        return self._graph.explore(start_id, direction, max_depth)


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

def analyze(root: str, create_store: bool = False) -> CodeGraph:
    """Perform initial analysis of a codebase.

    Args:
        root: Path to the project root directory.
        create_store: Create `.coderadar/store/` if it is missing. Only
            `coderadar init` should pass True — analyze used to create it
            unconditionally, which planted the very marker root discovery
            walks up looking for, making a wrong guess self-confirming.

    Returns:
        A CodeGraph backed by Macrame persistence.

    Phase 1: Walk directory, parse all source files, extract entities + edges.
    Phase 2: Persist to Macrame via content-addressed Concepts.
    Phase 3: Build in-memory ProjectedGraph with reverse indexes.
    """
    try:
        from coderadar._core import analyze as _analyze_rust
        _analyze_rust(root, create_store)
    except ImportError:
        pass

    # v0.5: Extract __all__ star exports for wildcard import resolution.
    # Must run after Rust analysis populates modules, before MCP server reads.
    try:
        from coderadar.resolvers.exports import extract_all_exports
        from coderadar._core import set_module_star_exports_bulk
        import pathlib
        # Collected, then applied in one call: the per-module variant clones
        # the whole ProjectedGraph each time, i.e. once per file with __all__.
        star_exports = []
        for py_file in pathlib.Path(root).rglob("*.py"):
            try:
                source = py_file.read_text(encoding="utf-8")
                names = extract_all_exports(source)
                if names:
                    star_exports.append((f"{py_file}::module", names))
            except (OSError, UnicodeDecodeError):
                pass
        if star_exports:
            try:
                set_module_star_exports_bulk(star_exports)
            except RuntimeError:
                pass
    except ImportError:
        pass

    return CodeGraph()


def load(db_path: str) -> CodeGraph:
    """Load a CodeRadar database from a Macrame .db file.

    Args:
        db_path: Path to a coderadar.db file.

    Returns:
        A CodeGraph restored from the bitemporal ledger.
    """
    raise NotImplementedError(
        "load(db_path) is not implemented — the in-memory ProjectedGraph is not "
        "yet restorable from the Macrame ledger without re-parsing source. "
        "Call analyze(root) to rebuild the graph. Cold-start persistence is "
        "Phase 3B work (see docs/traversal-matrix.md §3)."
    )


def watch(root: str) -> "Watcher":
    """Index `root`, then return a watcher over it.

    This used to construct a stub `Watcher(root)` defined further down the
    module, which the real `Watcher` then shadowed — so every call raised
    `TypeError: __init__() missing 1 required positional argument`. The
    watcher needs a populated graph to update, hence the `analyze` first.

    Usage:
        with coderadar.watch("src/") as w:
            for report in w:
                print(report.affected_files)
    """
    graph = analyze(root)
    return graph.watch([root])


__all__ = [
    "CodeGraph",
    "Snapshot",
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
    "StaleHandle",
    "ParseError",
    "ResolutionError",
    "MutationError",
    "PolicyViolation",
]


class Watcher:
    """Live file watcher that auto-updates the CodeGraph on file changes."""

    def __init__(self, graph: "CodeGraph", paths: List[str],
                 debounce_ms: Optional[int] = None,
                 max_file_size_bytes: Optional[int] = None):
        """None takes the value from `[watch]` in .coderadar.toml.

        An explicit argument still wins, so a CLI flag overrides the file.
        """
        from .config import WatchConfig, load_config
        try:
            watch = load_config(Path.cwd()).watch
        except Exception:
            watch = WatchConfig()
        self._graph = graph
        self._paths = paths
        self._debounce_ms = (
            watch.debounce_ms if debounce_ms is None else debounce_ms)
        self._max_file_size_bytes = (
            watch.max_file_size_bytes
            if max_file_size_bytes is None else max_file_size_bytes)
        self._running = False

    def start(self) -> "Watcher":
        """Begin watching. Idempotent; `run_forever` and iteration call it."""
        if self._running:
            return self
        from coderadar._core import start_watcher
        # `--debounce` was stored here and never passed on, so every watcher
        # ran at the 100 ms default whatever the user asked for.
        start_watcher(self._paths, self._debounce_ms, self._max_file_size_bytes)
        self._running = True
        return self

    def _apply(self, batch, echo: bool = False) -> UpdateReport:
        """Apply one batch of changes to the graph, merged into one report.

        A batch can touch several files, so the per-file reports are folded
        together: the worst parse quality wins, `fully_applied` is the
        conjunction, and the epochs span the whole batch.
        """
        import time
        started = time.perf_counter()
        affected: List[str] = []
        changed: List[SymbolChange] = []
        new_unresolved: List[dict] = []
        newly_resolved: List[dict] = []
        quality_rank = {"Clean": 0, "Partial": 1, "Tainted": 2}
        quality = "Clean"
        parse_errors = 0
        fully_applied = True
        epoch_before = None
        epoch_after = None

        for file_path, change_kind in batch:
            # The watcher stats the path, so "Delete" now actually arrives;
            # before, a deleted file's entities lived on in the graph until
            # the next full analyze.
            if change_kind == "Delete":
                try:
                    removed = self._graph.remove_file(file_path)
                    affected.append(file_path)
                    if echo:
                        print(f"  - {file_path} ({removed} entities removed)")
                except Exception as e:
                    fully_applied = False
                    if echo:
                        print(f"  {file_path}: {e}")
                continue

            if change_kind not in ("Modify", "Any", "AnyContinuous", "Create"):
                continue

            try:
                report = self._graph.update_file(file_path)
            except Exception as e:
                fully_applied = False
                if echo:
                    print(f"  {file_path}: {e}")
                continue

            affected.extend(report.affected_files or [file_path])
            changed.extend(report.changed_symbols)
            new_unresolved.extend(report.new_unresolved_references)
            newly_resolved.extend(report.newly_resolved_references)
            parse_errors += report.parse_errors
            fully_applied = fully_applied and report.fully_applied
            if quality_rank.get(report.parse_quality, 0) > quality_rank[quality]:
                quality = report.parse_quality
            if epoch_before is None:
                epoch_before = report.epoch_before
            epoch_after = report.epoch_after
            if echo:
                prefix = "+" if change_kind == "Create" else " "
                print(f"  {prefix} {file_path} ({report.parse_quality})")

        return UpdateReport(
            affected_files=affected,
            changed_symbols=changed,
            new_unresolved_references=new_unresolved,
            newly_resolved_references=newly_resolved,
            elapsed_ms=(time.perf_counter() - started) * 1000.0,
            parse_quality=quality,
            parse_errors=parse_errors,
            fully_applied=fully_applied,
            epoch_before=epoch_before if epoch_before is not None else 0,
            epoch_after=epoch_after if epoch_after is not None else 0,
        )

    def __enter__(self) -> "Watcher":
        return self.start()

    def __exit__(self, *args) -> None:
        self.stop()

    def __iter__(self) -> "Watcher":
        self.start()
        return self

    def __next__(self) -> UpdateReport:
        """Block until the next batch, apply it, and return one report."""
        from coderadar._core import next_watcher_batch
        while self._running:
            batch = next_watcher_batch()
            if batch is None:
                break
            report = self._apply(batch)
            # A batch of ignored paths applies to nothing; keep waiting
            # rather than handing the caller an empty report.
            if report.affected_files:
                return report
        raise StopIteration

    def run_forever(self) -> None:
        """Blocking loop: watch files and update graph on changes."""
        try:
            from coderadar._core import next_watcher_batch
            self.start()
        except ImportError:
            print("Watcher not available")
            return

        print(f"CodeRadar watcher: watching {self._paths}")
        try:
            while self._running:
                batch = next_watcher_batch()
                if batch is None:
                    break
                self._apply(batch, echo=True)
        except KeyboardInterrupt:
            print("\nWatcher stopped.")
            self._running = False

    def stop(self) -> None:
        """Stop the watcher."""
        self._running = False
        try:
            from coderadar._core import stop_watcher
            stop_watcher()
        except ImportError:
            pass
