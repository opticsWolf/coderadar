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

import functools

#: Single version source (F11 fix): pyproject.toml is authoritative.
#: `__version__` resolves from installed package metadata first (so an
#: installed wheel/sdist reports its own version) and falls back to the
#: release constant below, which MUST be kept in sync with pyproject.toml
#: and Cargo.toml [workspace.package] on every bump.
_FALLBACK_VERSION = "0.11.0"


def _resolve_version() -> str:
    # Source-tree constant is authoritative in a checkout: an editable
    # install freezes metadata at install time (stale 0.7.20 proved this),
    # so when metadata and source disagree the NEWER wins. In a proper
    # release flow both agree after reinstall.
    def _tup(v: str) -> tuple:
        try:
            return tuple(int(p) for p in v.split("."))
        except ValueError:
            return ()
    best = _FALLBACK_VERSION
    try:
        from importlib.metadata import version as _pkg_version
        for _dist in ("coderadar-rs", "coderadar"):
            try:
                _meta = _pkg_version(_dist)
                if _tup(_meta) > _tup(best):
                    best = _meta
            except ImportError:
                continue
    except ImportError:
        pass
    return best


__version__ = _resolve_version()

import json
from collections.abc import Iterator
from pathlib import Path
from typing import Any, Literal

# R2-13: structlog with no configuration logs to stdout at DEBUG, so every
# logger.debug/info call in the package (macrame.traverse, lsp lines, ...)
# polluted piped output. Route logs to stderr, default WARNING level;
# CODERADAR_DEBUG=1 restores DEBUG. Configured here so every entry path
# (CLI, MCP server, library, background) inherits it.
try:
    import logging as _logging
    import os as _os
    import sys as _sys

    import structlog as _structlog
    _structlog.configure(
        wrapper_class=_structlog.make_filtering_bound_logger(
            _logging.DEBUG
            if _os.environ.get("CODERADAR_DEBUG")
            else _logging.WARNING
        ),
        processors=[
            _structlog.processors.add_log_level,
            _structlog.processors.TimeStamper(fmt="iso", utc=True),
            _structlog.dev.ConsoleRenderer(colors=False),
        ],
        logger_factory=_structlog.PrintLoggerFactory(file=_sys.stderr),
    )
except ImportError:
    pass

# The caller/callee indexes the facade walks carry exactly one kind of
# edge. Named so the legacy call-graph walk can answer honestly when asked
# for another.
EDGE_KIND_CALLS = "calls"


def _deprecated(old: str, new: str) -> None:
    """Warn once per call site that `old` is now spelled `new`."""
    import warnings
    warnings.warn(f"CodeGraph.{old} is deprecated; use {new}", DeprecationWarning,
                  stacklevel=3)

# ── Public API Types ────────────────────────────────────────────────────────

from dataclasses import dataclass


@dataclass(frozen=True)
class UpdateReport:
    """Result of a single-file or batch update (§8.2)."""
    affected_files: list[str]
    changed_symbols: list[SymbolChange]
    new_unresolved_references: list[dict]
    newly_resolved_references: list[dict]
    elapsed_ms: float
    parse_quality: str  # "Clean" | "Partial" | "Tainted"
    parse_errors: int
    fully_applied: bool
    epoch_before: int
    epoch_after: int
    #: The file was gone from disk, so its entities were dropped instead.
    removed: bool = False
    entities_removed: int = 0


@dataclass(frozen=True)
class SymbolChange:
    """Description of a changed symbol."""
    kind: Literal["module", "class", "function", "import", "constant", "type_alias", "field"]
    operation: Literal["added", "removed", "signature_changed", "body_changed", "moved"]
    qualified_name: str
    file: str
    line: int
    id: int | None = None


@dataclass(frozen=True)
class MutationPlan:
    """A planned mutation — produced by the planner, applied by apply()."""
    id: str
    tool: str
    edits: list[MutationEdit]
    affected_files: list[str]
    diff_preview: str
    unverified_sites: list[dict]
    warnings: list[str]


@dataclass(frozen=True)
class MutationEdit:
    """A single byte-accurate edit to a file.

    ``line``/``col`` (1-indexed line, 0-indexed byte column) and their
    ``end_*`` companions describe the same span as ``span_start``/``span_end``
    in the form a reviewer reads (plan §5.4).
    """
    file: str
    replacement: str
    expected_hash: str = ""
    span_start: int | None = None
    span_end: int | None = None
    line: int | None = None
    col: int | None = None
    end_line: int | None = None
    end_col: int | None = None


@dataclass(frozen=True)
class MutationResult:
    """Result of applying a mutation plan."""
    status: Literal["Applied", "RolledBack", "RejectedStale", "RejectedPolicy"]
    files_written: list[str]
    syntax_errors: list[dict]
    backup_path: str | None = None


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

def _parse_plan_dict(result: dict, tool: str) -> MutationPlan:
    """Convert a Rust plan_to_dict result into a MutationPlan."""
    edits = []
    for e in result.get("edits", []) or []:
        edits.append(MutationEdit(
            file=e.get("file", ""),
            replacement=e.get("replacement", ""),
            expected_hash=e.get("expected_hash", ""),
            span_start=e.get("span_start"),
            span_end=e.get("span_end"),
            line=e.get("line"),
            col=e.get("col"),
            end_line=e.get("end_line"),
            end_col=e.get("end_col"),
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


def _bound_to_loaded_project(fn):
    """Refuse to answer about a project this handle was not created for.

    The core keeps one global graph and `analyze` replaces it wholesale, so a
    handle that outlives a re-index used to answer about the new tree without
    a word (plan §5.5). The first call binds the handle; a later call against
    a different root raises `StaleHandle`.
    """

    @functools.wraps(fn)
    def wrapper(self, *args, **kwargs):
        self._check_root()
        return fn(self, *args, **kwargs)

    return wrapper


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
        for caller in graph.callers("src/models.py::User.save"):
            print(caller["name"])
    """

    def __init__(self, db_path: str | None = None):
        self._db_path = db_path or ".coderadar/store/coderadar.db"
        self._config: dict[str, Any] = {}
        self._macrame = None  # Macrame Database handle (lazy)
        self._root: str | None = None  # bound on first use (plan §5.5)

    def _check_root(self) -> None:
        """Bind to the loaded project on first use; refuse a different one after."""
        try:
            from coderadar._core import indexed_root_py as _root
            current = _root()
        except (ImportError, RuntimeError):  # no extension / no graph: nothing to bind
            return
        if not current:
            return
        if self._root is None:
            self._root = current
        elif current != self._root:
            raise StaleHandle(
                f"This CodeGraph handle belongs to {self._root!r}, but the loaded "
                f"graph is now {current!r} — call analyze() for the new project and "
                f"use a fresh CodeGraph handle."
            )

    # ── Query ──────────────────────────────────────────────────────────

    @_bound_to_loaded_project
    def query(self, query_str: str) -> Iterator[dict[str, Any]]:
        """Execute a query against the in-memory graph; rows are dicts.

        Shape: ``<entity> [select ...] [where ...] [group by ...]
        [order by ...] [limit N]``, with entity one of ``modules``,
        ``classes``, ``functions``, ``methods``, ``constants``, ``entities``,
        ``imports``, ``calls``, ``fields``.

        Operators: ``==``, ``!=``, ``<``, ``<=``, ``>``, ``>=``,
        ``contains``, ``matches`` (regex), ``starts_with``, ``ends_with``,
        ``in``; combine predicates with ``and``, ``or``, ``not``.

        Every row carries ``id``, ``file_path``, ``kind`` and ``parent_id``
        whatever ``select`` narrows to, so a row can be passed straight to
        :meth:`callers_of` or ``plan_rename``. An unknown field raises
        ``ValueError`` listing what the entity does have — it is never a
        silent empty result.

        Example::

            for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
                print(cls["id"], cls["name"])

        Full field reference: ``docs/query-language.md``.
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

    @_bound_to_loaded_project
    def explore(
        self,
        query: str = "",
        symbols: list[str] | None = None,
        direction: Literal["downstream", "upstream", "both"] = "both",
        max_files: int = 8,
        *,
        start_id: str | None = None,
        max_depth: int | None = None,
        edge_kinds: list[str] | None = None,
    ) -> dict[str, Any]:
        """Source and call paths for the named symbols (MCP ``coderadar_explore``).

        `query` is symbol names or a question; `symbols` names them
        explicitly. Returns ``{"names", "files", "relationships",
        "stale_files"}`` — see :func:`coderadar.ops.explore`.

        The old call-graph walk (``explore(start_id, direction, max_depth,
        edge_kinds)`` returning ``{entity_id, edge_kind, direction, depth}``
        rows) still runs when called with ``start_id=``, ``max_depth=`` or
        ``edge_kinds=``, or with direction ``in`` / ``out``, and warns: use
        :meth:`traverse` with ``edge_kinds=["calls"]``.
        """
        if (start_id is not None or max_depth is not None or edge_kinds is not None
                or direction in ("in", "out")):
            _deprecated("explore(start_id, ...)", "CodeGraph.traverse")
            return self._walk(start_id or query, direction,
                              3 if max_depth is None else max_depth, edge_kinds)
        from . import ops
        return ops.explore(query, symbols, direction, max_files)

    def _walk(
        self,
        start_id: str,
        direction: str = "both",
        max_depth: int = 3,
        edge_kinds: list[str] | None = None,
    ) -> list[dict[str, Any]]:
        """The legacy call-graph walk behind ``explore(start_id=...)``.

        Breadth-first, so the depth reported for an entity is the length of
        the shortest path to it, and an entity reached both ways is reported
        once — at whichever depth found it first. Direction is "in"
        (callers), "out" (callees) or "both"; the only edge kind this walk
        follows is "calls". Rows: `entity_id`, `edge_kind`, `direction`,
        `depth`.
        """
        direction = {"upstream": "in", "downstream": "out"}.get(direction, direction)
        if edge_kinds is not None and EDGE_KIND_CALLS not in edge_kinds:
            return []

        directions: list[str] = (
            ["out", "in"] if direction == "both" else [direction]
        )
        results: list[dict[str, Any]] = []
        visited: set = {start_id}
        frontier: list[str] = [start_id]

        for depth in range(1, max(max_depth, 0) + 1):
            next_frontier: list[str] = []
            for current in frontier:
                for way in directions:
                    rows = (
                        self._get_outgoing(current)
                        if way == "out"
                        else self._get_incoming(current)
                    )
                    for row in rows:
                        # These rows are entities, not edge records: the edge
                        # is implied by which index they came out of.
                        neighbour = row.get("id")
                        if not neighbour or neighbour in visited:
                            continue
                        visited.add(neighbour)
                        next_frontier.append(neighbour)
                        results.append({
                            "entity_id": neighbour,
                            "edge_kind": EDGE_KIND_CALLS,
                            "direction": way,
                            "depth": depth,
                        })
            if not next_frontier:
                break
            frontier = next_frontier

        return results

    def _get_incoming(self, entity_id: str) -> list[dict[str, Any]]:
        """Internal: the entities that call entity_id."""
        # Delegates to Rust core via _core module
        try:
            from coderadar._core import callers_of as _callers_of
            return _callers_of(entity_id)
        except ImportError:
            return []

    def _get_outgoing(self, entity_id: str) -> list[dict[str, Any]]:
        """Internal: the entities entity_id calls."""
        try:
            from coderadar._core import callees_of as _callees_of
            return _callees_of(entity_id)
        except ImportError:
            return []

    # ── Macrame Operations ────────────────────────────────────────────

    @_bound_to_loaded_project
    def traverse(
        self,
        entity_id: str | None = None,
        direction: str = "both",
        edge_kinds: list[str] | None = None,
        max_depth: int = 3,
        *,
        start_id: str | None = None,
        edge_types: list[str] | None = None,
    ) -> list[dict[str, Any]]:
        """Breadth-first walk along any edge kinds (MCP ``coderadar_traverse``).

        `direction` is ``downstream`` | ``upstream`` | ``both`` (``out`` /
        ``in`` accepted); `edge_kinds` any of ``calls``, ``imports``,
        ``inherits`` (alias ``extends``), ``overrides`` — None means all.
        Returns the reached entity rows, each with ``depth`` and
        ``edge_type``; an unknown `entity_id` raises ``ops.NotFound``.

        The old argument order ``traverse(start_id, max_depth, edge_types,
        direction)`` and the ``start_id=`` / ``edge_types=`` keywords still
        work, with a DeprecationWarning.
        """
        if isinstance(direction, int) or start_id is not None or edge_types is not None:
            _deprecated("traverse(start_id, max_depth, edge_types, direction)",
                        "traverse(entity_id, direction, edge_kinds, max_depth)")
            if isinstance(direction, int):  # old positional order
                old_depth, old_types = direction, edge_kinds
                old_direction = max_depth if isinstance(max_depth, str) else "both"
                direction, edge_kinds, max_depth = old_direction, old_types, old_depth
            entity_id = start_id or entity_id
            edge_kinds = edge_types if edge_types is not None else edge_kinds
        from . import ops
        return ops.traverse(entity_id or "", direction, edge_kinds, max_depth)["results"]

    def as_of(self, timestamp: str) -> Snapshot:
        """Return a point-in-time snapshot via Macrame's reconstruct(ts)."""
        return Snapshot(self, timestamp)

    @_bound_to_loaded_project
    def find(self, entity_id: str) -> dict[str, Any] | None:
        """Deprecated: :meth:`node` returns the same record and raises
        ``ops.NotFound`` (with candidates) instead of returning None."""
        _deprecated("find", "CodeGraph.node")
        from .query import MacrameQuery
        return MacrameQuery(self).find(entity_id)

    @_bound_to_loaded_project
    def callers(self, entity_id: str) -> list[dict[str, Any]]:
        """Direct callers, from the reverse call index (CLI ``callers``)."""
        from .query import MacrameQuery
        return MacrameQuery(self).callers_of(entity_id)

    @_bound_to_loaded_project
    def callees(self, entity_id: str) -> list[dict[str, Any]]:
        """Direct callees, from the forward call index (CLI ``callees``)."""
        from .query import MacrameQuery
        return MacrameQuery(self).callees_of(entity_id)

    def callers_of(self, entity_id: str) -> list[dict[str, Any]]:
        """Deprecated spelling of :meth:`callers`."""
        _deprecated("callers_of", "CodeGraph.callers")
        return self.callers(entity_id)

    def callees_of(self, entity_id: str) -> list[dict[str, Any]]:
        """Deprecated spelling of :meth:`callees`."""
        _deprecated("callees_of", "CodeGraph.callees")
        return self.callees(entity_id)

    @_bound_to_loaded_project
    def call_sites(self, entity_id: str) -> list[dict[str, Any]] | None:
        """Every call site extracted from a function, with the resolver's verdict.

        Rows are ``{name, path, line, col, status, target, reason}`` in source
        order; ``status`` is one of function / method / constructor / builtin /
        external / unresolved / pending. ``None`` when ``entity_id`` is not a
        function. Unlike ``callees_of`` this shows the calls that did *not*
        become edges.
        """
        from coderadar._core import call_sites
        return call_sites(entity_id)

    # ── Shared operations (coderadar.ops; MCP and CLI expose the same) ──
    # Names are the MCP tool names without their prefix. Errors raise
    # coderadar.ops.OpError subclasses: NoIndex, InvalidRequest, NotFound
    # (with .candidates), EngineError.

    @_bound_to_loaded_project
    def node(self, entity_id: str, include_neighbors: bool = False) -> dict[str, Any]:
        """One entity's full record (MCP ``coderadar_node``); with
        `include_neighbors`, plus ``callers`` and ``callees`` lists."""
        from . import ops
        return ops.node(entity_id, include_neighbors)

    @_bound_to_loaded_project
    def search(self, query: str, kind: str | None = None,
               top_k: int = 10) -> list[dict[str, Any]]:
        """Keyword search over names, signatures and docstrings (MCP
        ``coderadar_search``). `kind`: function | class | type_alias |
        constant | module | import."""
        from . import ops
        return ops.search(query, kind, top_k)

    @_bound_to_loaded_project
    def affected(self, entity_id: str, max_depth: int = 5) -> dict[str, Any]:
        """Transitive callers, centrality-ranked per depth (MCP
        ``coderadar_affected``): ``{entity, max_depth, depths, central_ids}``."""
        from . import ops
        return ops.affected(entity_id, max_depth)

    @_bound_to_loaded_project
    def module_children(self, module_id: str) -> dict[str, Any]:
        """A module's classes, functions, imports and constants (MCP
        ``coderadar_module_children``)."""
        from . import ops
        return ops.module_children(module_id)

    @_bound_to_loaded_project
    def resolve(self, name: str, limit: int = 5) -> dict[str, Any]:
        """Framework-aware resolution of a route (``/users/:id``) or a
        service / model / view name (MCP ``coderadar_resolve``)."""
        from . import ops
        return ops.resolve(name, limit)

    @_bound_to_loaded_project
    def dead_code(self, min_confidence: float = 0.6, include_test_reachable: bool = False,
                  max_findings: int = 100) -> list[dict[str, Any]]:
        """Dead-code findings, most safely deletable first (MCP
        ``coderadar_dead_code``). Ranked evidence, not proof: check
        :meth:`affected` before deleting."""
        from . import ops
        return ops.dead_code(min_confidence, include_test_reachable, max_findings)

    @_bound_to_loaded_project
    def get_smells(self, entity_id: str | None = None, rule_id: str | None = None,
                   strictness: str = "normal") -> list[dict[str, Any]]:
        """Code-smell findings (MCP ``coderadar_get_smells``); `strictness`
        is strict | normal | loose."""
        from . import ops
        return ops.get_smells(entity_id, rule_id, strictness)

    @_bound_to_loaded_project
    def find_clones(self, min_lines: int = 10, min_similarity: float = 0.8,
                    max_groups: int = 100) -> list[dict[str, Any]]:
        """Clone groups, Types 1-3, largest first (MCP ``coderadar_find_clones``)."""
        from . import ops
        return ops.find_clones(min_lines, min_similarity, max_groups)

    @_bound_to_loaded_project
    def find_scaffolding(self, include_secrets: bool = False,
                         max_findings: int = 100) -> list[dict[str, Any]]:
        """Scaffolding debt: markers, placeholder bodies, temp files, opt-in
        redacted secrets (MCP ``coderadar_find_scaffolding``). The last row
        (``kind == "scan-stats"``) is a coverage footer."""
        from . import ops
        return ops.find_scaffolding(include_secrets, max_findings)

    @_bound_to_loaded_project
    def search_similar(
        self, query: str | list[float], top_k: int = 10,
    ) -> list[dict[str, Any]]:
        """Semantic search (MCP ``coderadar_search_similar``).

        `query` is natural language — embedded locally with fastembed, at
        most 20 results, embeddings computed on first use — or a ready
        embedding vector.
        """
        from . import ops
        return ops.search_similar(query, top_k)

    def compute_embeddings(self, model_name: str | None = None,
                           batch_size: int = 32) -> dict[str, int]:
        """Compute and store embeddings for all indexable entities (MCP
        ``coderadar_compute_embeddings``). Returns ``{generated, cached,
        total, errors}``; unchanged entities are skipped by content hash."""
        from . import ops
        return ops.compute_embeddings(model_name, batch_size)

    # ── Edits (dry run by default) ─────────────────────────────────────
    # Each returns {"plan": MutationPlan, "result": MutationResult | None,
    # "note": str}; result is None for a dry run. dry_run=False applies the
    # plan, writing the file and updating the graph together. Failures
    # raise coderadar.ops.MutationFailed. The plan_* methods below are the
    # two-step form (plan, then apply).

    @_bound_to_loaded_project
    def replace_body(self, entity_id: str, new_body: str,
                     expected_hash: str | None = None,
                     dry_run: bool = True) -> dict[str, Any]:
        """Replace a function/method body (MCP ``coderadar_replace_body``)."""
        from . import ops
        return ops.replace_body(entity_id, new_body, expected_hash, dry_run, graph=self)

    @_bound_to_loaded_project
    def update_signature(self, entity_id: str, new_signature: str,
                         inject_defaults: bool = False,
                         dry_run: bool = True) -> dict[str, Any]:
        """Change a signature (MCP ``coderadar_update_signature``); call
        sites come back as ``plan.unverified_sites``."""
        from . import ops
        return ops.update_signature(entity_id, new_signature, inject_defaults, dry_run,
                                    graph=self)

    @_bound_to_loaded_project
    def rename(self, entity_id: str, new_name: str, dry_run: bool = True) -> dict[str, Any]:
        """Rename an entity and every reference (MCP ``coderadar_rename``)."""
        from . import ops
        return ops.rename(entity_id, new_name, dry_run, graph=self)

    @_bound_to_loaded_project
    def create_entity(self, file_path: str, language: str, kind: str, name: str,
                      body: str, decorators: list[str] | None = None,
                      anchor: str = "end", signature: str | None = None,
                      dry_run: bool = True) -> dict[str, Any]:
        """Insert a function, class or constant (MCP ``coderadar_create_entity``)."""
        from . import ops
        return ops.create_entity(file_path, language, kind, name, body, decorators,
                                 anchor, signature, dry_run, graph=self)

    # ── Index lifecycle ────────────────────────────────────────────────

    def reindex(self, with_embeddings: bool = False, full: bool = False,
                exclude: list[str] | None = None) -> dict[str, Any]:
        """Bring the index for the cwd's project up to date (MCP
        ``coderadar_reindex``): the cheap way by default, the whole tree
        with `full` or one-shot `exclude` patterns. Returns ``{"stats",
        "embeddings"?}``."""
        from . import ops
        result = ops.reindex(with_embeddings, full, exclude)
        self._root = None  # the graph was rebuilt; rebind on next use
        return result

    def status(self) -> dict[str, Any]:
        """What is indexed for the cwd's project and how fresh it is (MCP
        ``coderadar_status``)."""
        from . import ops
        return ops.status()

    # ── Update ─────────────────────────────────────────────────────────

    @_bound_to_loaded_project
    def update_file(self, file_path: str, content: str | None = None,
                    force: bool = False) -> UpdateReport:
        """Update the graph after a file change.

        Phase 1: Rust parses, diffs, and stages changes.
        Phase 2: Macrame persists entities and edges with timestamps.
        Phase 3: ProjectedGraph rebuilds reverse indexes.

        A file that no longer exists on disk (and no `content` given) is
        dropped from the graph instead: the report says ``removed`` and
        how many entities went.
        """
        removed = 0
        if content is None and not Path(file_path).exists():
            removed = self.remove_file(file_path)
        if removed:  # a file never indexed falls through and fails below
            return UpdateReport(
                affected_files=[file_path], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=0.0, parse_quality="Removed", parse_errors=0,
                fully_applied=True, epoch_before=0, epoch_after=0,
                removed=True, entities_removed=removed,
            )
        try:
            from coderadar._core import update_file as _update_file_rust
            result = _update_file_rust(file_path, content, force)
            if isinstance(result, dict):
                return UpdateReport(
                    affected_files=result.get("affected_files") or [file_path],
                    changed_symbols=[
                        SymbolChange(
                            kind=s["kind"], operation=s["operation"],
                            qualified_name=s["id"], file=s["id"].split("::")[0],
                            line=int(s["line"]),
                        )
                        for s in result.get("changed_symbols", [])
                    ],
                    new_unresolved_references=list(result.get("new_unresolved", [])),
                    newly_resolved_references=list(result.get("newly_resolved", [])),
                    elapsed_ms=float(result.get("elapsed_ms", 0.0)),
                    parse_quality=str(result.get("parse_quality", "Clean")),
                    parse_errors=int(result.get("parse_errors", 0)),
                    fully_applied=bool(result.get("fully_applied", True)),
                    epoch_before=int(result.get("epoch_before", 0)),
                    epoch_after=int(result.get("epoch_after", 0)),
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
                fully_applied=False, epoch_before=0, epoch_after=0,
            )
        except RuntimeError as e:
            return UpdateReport(
                affected_files=[file_path], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=0.0, parse_quality=f"Error: {e}", parse_errors=1,
                fully_applied=False, epoch_before=0, epoch_after=0,
            )

        # Fallback (unreachable in practice)
        return UpdateReport(
            affected_files=[file_path], changed_symbols=[],
            new_unresolved_references=[], newly_resolved_references=[],
            elapsed_ms=0.0, parse_quality="Clean", parse_errors=0,
            fully_applied=False, epoch_before=0, epoch_after=0,
        )

    @_bound_to_loaded_project
    def remove_file(self, file_path: str) -> int:
        """Drop a deleted file's entities from the graph.

        `update_file` does this too when it finds the file gone from disk.
        Returns the number of entities removed.
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

    def watch(self, paths: list[str] | None = None,
              debounce_ms: int | None = None,
              max_file_size_bytes: int | None = None) -> Watcher:
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

    def batch(self) -> BatchContext:
        """Context manager for batched updates."""
        return BatchContext(self)

    # ── Mutation ───────────────────────────────────────────────────────

    @_bound_to_loaded_project
    def plan_replace_body(
        self,
        entity_id: str,
        new_body: str,
        expected_hash: str | None = None,
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

    @_bound_to_loaded_project
    def plan_update_signature(
        self,
        entity_id: str,
        new_signature: str,
        call_site_values: dict[str, str] | None = None,
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

    def plan_body_replacement(self, *args: Any, **kwargs: Any) -> MutationPlan:
        """Deprecated spelling of :meth:`plan_replace_body`."""
        _deprecated("plan_body_replacement", "CodeGraph.plan_replace_body")
        return self.plan_replace_body(*args, **kwargs)

    def plan_signature_update(self, *args: Any, **kwargs: Any) -> MutationPlan:
        """Deprecated spelling of :meth:`plan_update_signature`."""
        _deprecated("plan_signature_update", "CodeGraph.plan_update_signature")
        return self.plan_update_signature(*args, **kwargs)

    @_bound_to_loaded_project
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

    @_bound_to_loaded_project
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

    @_bound_to_loaded_project
    def apply(self, plan: MutationPlan) -> MutationResult:
        """Apply a mutation plan atomically.

        Phase 1: Write edits to disk (with backup).
        Phase 2: Re-parse changed files → stage → Macrame persist.
        Phase 3: Rebuild ProjectedGraph reverse indexes.
        """
        try:
            from coderadar._core import apply_mutation as _am
            from coderadar._core import clear_embeddings_for_file
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
                except Exception:  # noqa: BLE001, S110 - best-effort graph refresh
                    pass
            # R2-17: multi-file plans resolve order-dependently — an
            # importer re-resolved before its source re-indexed keeps a
            # stale external:: edge (rename across a re-export chain hit
            # exactly this). A second pass over the same files converges:
            # pass 1 leaves every affected file's entities/imports current,
            # so pass 2 resolves calls against settled siblings. Single-file
            # plans cannot strand cross-file edges; skip the extra work.
            if len(plan.affected_files) > 1:
                for f in plan.affected_files:
                    try:
                        self.update_file(f)
                    except Exception:  # noqa: BLE001, S110 - best-effort graph refresh
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

    # ── Stats / Debug ──────────────────────────────────────────────────

    @_bound_to_loaded_project
    def stats(self) -> dict[str, Any]:
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

    def query(self, query_str: str) -> Iterator[dict[str, Any]]:
        """Execute a query against the reconstructed snapshot.

        Same language and row shape as :meth:`CodeGraph.query` — see
        ``docs/query-language.md``.
        """
        # Macrame reconstruct(ts) + ProjectedGraph from that point
        return self._graph.query(query_str)

    def callers(self, entity_id: str) -> list[dict[str, Any]]:
        """Callers at this point in time."""
        return self._graph.callers(entity_id)

    def callees(self, entity_id: str) -> list[dict[str, Any]]:
        """Callees at this point in time."""
        return self._graph.callees(entity_id)

    def traverse(self, entity_id: str, direction: str = "both",
                 edge_kinds: list[str] | None = None,
                 max_depth: int = 3) -> list[dict[str, Any]]:
        """Traverse from entity_id at this point in time."""
        return self._graph.traverse(entity_id, direction, edge_kinds, max_depth)

    def callers_of(self, entity_id: str) -> list[dict[str, Any]]:
        """Deprecated spelling of :meth:`callers`."""
        _deprecated("Snapshot.callers_of", "Snapshot.callers")
        return self.callers(entity_id)

    def callees_of(self, entity_id: str) -> list[dict[str, Any]]:
        """Deprecated spelling of :meth:`callees`."""
        _deprecated("Snapshot.callees_of", "Snapshot.callees")
        return self.callees(entity_id)

    def explore(
        self,
        start_id: str,
        direction: Literal["in", "out", "both"] = "both",
        max_depth: int = 3,
    ) -> list[dict[str, Any]]:
        """Deprecated call-graph walk; use :meth:`traverse`."""
        _deprecated("Snapshot.explore", "Snapshot.traverse")
        return self._graph._walk(start_id, direction, max_depth)


# ── Batch Context Manager ───────────────────────────────────────────────────

class BatchContext:
    """Context manager for batching multiple file updates."""

    def __init__(self, graph: CodeGraph):
        self.graph = graph
        self._updates: list[tuple] = []

    def update_file(self, file_path: str, content: str | None = None) -> None:
        self._updates.append((file_path, content))

    def __enter__(self) -> BatchContext:  # noqa: PYI034 - typing.Self needs 3.11+
        return self

    def __exit__(self, *args) -> None:
        for file_path, content in self._updates:
            self.graph.update_file(file_path, content)


# ── Top-Level API Functions ─────────────────────────────────────────────────

def _project_excludes(root: str) -> list:
    """`[project] exclude` patterns from `<root>/.coderadar.toml`, or [].

    R2-7 narrow step: the walk-level fix only. A first cut pushed the whole
    file via `activate_config`, but that also enforced `[mutation] allow`
    (and roots/embedding keys) on library flows -- bare-library mutations
    outside src/lib/tests/scripts started failing where the CLI/server had
    always gated them, a behavior change far beyond "honor excludes".
    Full activation stays where it was: CLI `_activate`, MCP `_set_project`.
    Best-effort: a missing or broken toml yields [], never an exception.
    """
    try:
        from pathlib import Path

        from .config import load_config
        pats = load_config(Path(root)).project.exclude or []
        return [p for p in pats if p]
    except Exception:  # noqa: BLE001 - best-effort: broken toml yields [], never raises
        return []

def analyze(root: str, create_store: bool = False, exclude: list | None = None) -> CodeGraph:
    """Perform initial analysis of a codebase.

    Args:
        root: Path to the project root directory.
        create_store: Create `.coderadar/store/` if it is missing. Only
            `coderadar init` should pass True — analyze used to create it
            unconditionally, which planted the very marker root discovery
            walks up looking for, making a wrong guess self-confirming.
        exclude: One-shot extra exclusion patterns (gitignore syntax) for
            this run, merged with `[project] exclude` + the built-in
            baseline — ad-hoc narrowing without touching config (item 7).
            The toml's `[project] exclude` is picked up automatically
            (R2-7), so library users get the full exclusion stack with or
            without a config file. (Only excludes -- other file keys still
            need `_activate` / server project selection.)

    Returns:
        A CodeGraph backed by Macrame persistence.

    Phase 1: Walk directory, parse all source files, extract entities + edges.
    Phase 2: Persist to Macrame via content-addressed Concepts.
    Phase 3: Build in-memory ProjectedGraph with reverse indexes.
    """
    # R2-7: honor `<root>/.coderadar.toml` excludes on the library path too.
    # CLI flows activate the whole file via `_activate`; bare `analyze()`
    # never did, so `[project] exclude` was silently ignored outside the CLI
    # (and a bare analyze could even write excluded concepts back into an
    # attached store). Toml patterns go FIRST, mirroring exclusion_gitignore
    # (config -> extra -> baseline) so one-shot `!` negations keep working.
    _merged_excludes = _project_excludes(root) + list(exclude or [])
    try:
        from coderadar._core import analyze as _analyze_rust
        _analyze_rust(root, create_store, _merged_excludes)
    except ImportError:
        pass
    # F14: readers (ops.read_source et al.) resolve canonical relative ids
    # against this, not the CWD.
    try:
        from coderadar.excludes import set_indexed_root as _set_root
        _set_root(root)
    except ImportError:
        pass

    # v0.5: Extract __all__ star exports for wildcard import resolution.
    # Must run after Rust analysis populates modules, before MCP server reads.
    _apply_star_exports(root)

    return CodeGraph()


#: Item 7: the star-export pass walks through the shared exclusion helper
#: (`coderadar.excludes`), backed by the same Rust matcher the index walk
#: uses — no private skip-dir list, user `[project] exclude` honored by
#: construction.
def _apply_star_exports(root: str) -> None:
    """Extract `__all__` star exports from source and apply them to the
    in-memory graph.

    Must run after the graph is populated — by `analyze` or by `load`
    (v0.8 P1: cold start re-runs the same pass, because star exports are
    derived, in-memory state and the ledger does not persist them).
    """
    try:
        import os
        import pathlib

        from coderadar._core import set_module_star_exports_bulk
        from coderadar.resolvers.exports import extract_all_exports
        # Collected, then applied in one call: the per-module variant clones
        # the whole ProjectedGraph each time, i.e. once per file with __all__.
        #
        # The module id is the file path the analyze that wrote the store
        # walked. A load may pass the root in a different form than that
        # analyze did (relative vs absolute - `coderadar init` resolves, an
        # explicit `coderadar analyze` does not), so each module is offered
        # under every id form the path could take. `set_module_star_exports_bulk`
        # skips ids that are not in the graph, so the extra candidates are
        # harmless; they just make the pass root-form-agnostic.
        root_path = pathlib.Path(root)
        star_exports = []
        scanned = 0
        from coderadar.excludes import iter_project_files as _iter_files
        for py_file in _iter_files(root_path, suffixes=(".py",)):
            scanned += 1
            try:
                source = py_file.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            names = extract_all_exports(source)
            if not names:
                continue
            # Canonical ids are root-relative with forward slashes and no
            # dot prefix (plan 5.1). The other spellings are kept as
            # fallbacks for stores written before the migration; the Rust
            # side canonicalizes whatever it gets.
            candidates = {f"{py_file}::module"}
            try:
                rel = py_file.relative_to(root_path)
                candidates.add(f"{rel.as_posix()}::module")
                candidates.add(f".{os.sep}{rel}::module")
                candidates.add(f"./{rel.as_posix()}::module")
            except ValueError:
                pass
            candidates.add(f"{py_file.resolve()}::module")
            star_exports.extend((module_id, names) for module_id in candidates)
        if star_exports:
            try:
                set_module_star_exports_bulk(star_exports)
            except RuntimeError:
                pass
    except ImportError:
        pass


def load(db_path: str, root: str | None = None) -> CodeGraph:
    """Cold-start a CodeGraph from a Macrame ledger (v0.8 P1).

    Rebuilds the in-memory ProjectedGraph from the ledger instead of
    re-parsing source: concept JSON v2 carries post-resolution state
    (including `resolved_calls`), and the cheap resolution cascade is
    re-run in memory. `indexed_at` comes from the store file's mtime.

    Args:
        db_path: Path to a coderadar.db file (the Macrame store).
        root: The project root used with `analyze` - entity ids are keyed
            by file path, so pass the same root. Stored as the indexed
            root for mutation policy and staleness checks.

    Returns:
        A CodeGraph restored from the bitemporal ledger.

    Raises:
        ValueError: if the store holds concept-JSON v1 concepts (missing
            `meta_version: 2`) - re-run `coderadar analyze` to upgrade it.
    """
    try:
        from coderadar._core import load_snapshot as _ls
        _ls(db_path, root)
    except ImportError:
        pass

    if root:
        # R2-7: load() needs no exclude handling -- it walks nothing, and
        # post-load passes only attach to graph entities (excluded paths
        # have none, and set_module_star_exports_bulk skips unknown ids).
        _apply_star_exports(root)
        # F14: readers resolve canonical relative ids against this.
        try:
            from coderadar.excludes import set_indexed_root as _set_root
            _set_root(root)
        except ImportError:
            pass

    return CodeGraph()


def watch(root: str) -> Watcher:
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
    "BatchContext",
    "CodeGraph",
    "MutationEdit",
    "MutationError",
    "MutationPlan",
    "MutationResult",
    "ParseError",
    "PolicyViolation",
    "ResolutionError",
    "Snapshot",
    "StaleHandle",
    "SymbolChange",
    "UpdateReport",
    "Watcher",
    "analyze",
    "load",
    "watch",
]


class Watcher:
    """Live file watcher that auto-updates the CodeGraph on file changes."""

    def __init__(self, graph: CodeGraph, paths: list[str],
                 debounce_ms: int | None = None,
                 max_file_size_bytes: int | None = None):
        """None takes the value from `[watch]` in .coderadar.toml.

        An explicit argument still wins, so a CLI flag overrides the file.
        """
        from .config import WatchConfig, load_config
        try:
            watch = load_config(Path.cwd()).watch
        except Exception:  # noqa: BLE001 - broken watch config falls back to defaults
            watch = WatchConfig()
        self._graph = graph
        self._paths = paths
        self._debounce_ms = (
            watch.debounce_ms if debounce_ms is None else debounce_ms)
        self._max_file_size_bytes = (
            watch.max_file_size_bytes
            if max_file_size_bytes is None else max_file_size_bytes)
        self._running = False

    def start(self) -> Watcher:
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
        affected: list[str] = []
        changed: list[SymbolChange] = []
        new_unresolved: list[dict] = []
        newly_resolved: list[dict] = []
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
                except Exception as e:  # noqa: BLE001 - watch loop must not die on one file
                    fully_applied = False
                    if echo:
                        print(f"  {file_path}: {e}")
                continue

            if change_kind not in ("Modify", "Any", "AnyContinuous", "Create"):
                continue

            try:
                report = self._graph.update_file(file_path)
            except Exception as e:  # noqa: BLE001 - watch loop must not die on one file
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

    def __enter__(self) -> Watcher:  # noqa: PYI034 - typing.Self needs 3.11+
        return self.start()

    def __exit__(self, *args) -> None:
        self.stop()

    def __iter__(self) -> Watcher:
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
