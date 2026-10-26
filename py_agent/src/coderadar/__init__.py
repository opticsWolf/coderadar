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

import contextlib
import functools
import os
import threading

#: Single version source (F11 fix): pyproject.toml is authoritative.
#: `__version__` resolves from installed package metadata first (so an
#: installed wheel/sdist reports its own version) and falls back to the
#: release constant below, which MUST be kept in sync with pyproject.toml
#: and Cargo.toml [workspace.package] on every bump.
_FALLBACK_VERSION = "0.11.0"


def _resolve_version() -> str:
    # Source tree is authoritative in a checkout: pyproject.toml is read
    # first (an editable install freezes metadata at install time — stale
    # 0.7.20 proved it — so metadata is only a fallback for installed
    # wheels/sdists, where it agrees with the build). P2 reinstall-proof:
    # the MCP-serving venv resolves repo sources, so it reports the repo.
    try:
        from pathlib import Path

        import tomllib
        _root = Path(__file__).resolve().parent.parent.parent.parent
        _pyproject = _root / "pyproject.toml"
        if _pyproject.is_file():
            with open(_pyproject, "rb") as _f:
                _ver = tomllib.load(_f).get("project", {}).get("version", "")
            if _ver:
                return _ver
    except Exception:  # noqa: BLE001, S110 - any read failure falls through to metadata
        pass
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
from dataclasses import dataclass, field
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

# Step-4 surface: `coderadar.ops` answers attribute access after a bare
# `import coderadar` (the shared seam's errors catch as
# `coderadar.ops.OpError`, ...). `ops` imports stdlib only at module level,
# so this cannot cycle back through the package.
from . import ops
from .excludes import resolve_entity_path as _resolve_entity_path

_MUTATION_OPERATION_LOCKS: dict[str, threading.Lock] = {}
_MUTATION_OPERATION_LOCKS_GUARD = threading.Lock()


def _mutation_operation_lock() -> threading.Lock:
    """Process-wide per-root lock for disk apply + facade synchronization.

    The Rust graph is process-global: two `CodeGraph` handles are not two
    graphs. An instance-local lock therefore cannot serialize concurrent
    `apply()` calls. This lock is held across `apply_mutation` and all
    required `update_file`/derived-data synchronization so a second
    same-process attempt cannot interleave the disk phases with another
    attempt's graph sync. External processes remain explicitly unserialized.
    """
    try:
        from coderadar._core import indexed_root_py as _root_fn
        root = _root_fn() or os.getcwd()
    except Exception:  # noqa: BLE001 - missing extension/root: cwd is the best key
        root = os.getcwd()
    key = os.path.realpath(root)
    with _MUTATION_OPERATION_LOCKS_GUARD:
        lock = _MUTATION_OPERATION_LOCKS.get(key)
        if lock is None:
            lock = threading.Lock()
            _MUTATION_OPERATION_LOCKS[key] = lock
        return lock


# ── Public API Types ────────────────────────────────────────────────────────

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
    #: xxh3_64 of the exact bytes parsed. Mutation synchronization compares
    #: this with the disk attempt's expected final hash.
    source_hash: str = ""
    #: Operational failure inside `update_file` itself. `None` on every
    #: constructed outcome, including recovered parses.
    update_error: str | None = None
    #: Concept/edge/retirement persistence failed after the projection moved.
    persistence_error: str | None = None
    #: Source-blob storage failed. Configured blob skips do not set this.
    blob_error: str | None = None
    #: Framework-route refresh failed. The projection update stands; the
    #: failure is reported in the mutation sync summary rather than hidden.
    route_warning: str | None = None
    #: The file was gone from disk, so its entities were dropped instead.
    removed: bool = False
    entities_removed: int = 0
    #: §1.9 (DR-30 notice): this update's blob outcome. Watcher batches
    #: fold per-file reports by summation.
    blobs_stored: int = 0
    blobs_skipped_oversize: int = 0
    blobs_skipped_excluded: int = 0
    blobs_bytes: int = 0


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
class MutationFailure:
    """One failed operation in a mutation attempt (stable stage + code)."""
    stage: str
    code: str
    message: str


@dataclass(frozen=True)
class MutationFileError:
    """Structured per-file error detail."""
    code: str
    message: str


@dataclass(frozen=True)
class MutationFileResult:
    """Per-file outcome for one disk attempt."""
    file: str
    state: Literal["Untouched", "Installed", "Restored", "ExternallyChanged", "RestoreFailed", "Unknown"]
    pre_hash: str = ""
    installed_hash: str | None = None
    final_hash: str | None = None
    expected_hash: str | None = None
    recovery_path: str | None = None
    error: MutationFileError | None = None


@dataclass(frozen=True)
class MutationSyncSummary:
    """What public synchronization proved after the disk attempt."""
    state: Literal["NotNeeded", "NotAttempted", "Synchronized", "Partial", "Failed", "Unknown"]
    attempted_files: list[str] = field(default_factory=list)
    updated_files: list[str] = field(default_factory=list)
    errors: list[dict[str, Any]] = field(default_factory=list)
    warnings: list[dict[str, Any]] = field(default_factory=list)


@dataclass(frozen=True)
class MutationResult:
    """Result of applying a mutation plan.

    `status` is public only after `CodeGraph.apply` synchronizes the graph:
    disk `Installed` becomes `Applied` solely with synchronized graph and
    persistence state, while unverified synchronization becomes one of the
    explicit incomplete outcomes. `syntax_errors` carries syntax diagnostics
    only; operational failures use `failure`, per-file outcomes, and `sync`.
    """
    status: Literal["Applied", "RolledBack", "RejectedStale", "RejectedPolicy", "ApplyIncomplete", "RollbackIncomplete"]
    files_written: list[str]
    syntax_errors: list[Any]
    attempt_id: str = ""
    primary: MutationFailure | None = None
    failure: MutationFailure | None = None
    files: list[MutationFileResult] = field(default_factory=list)
    sync: MutationSyncSummary = field(default_factory=lambda: MutationSyncSummary(
        state="NotAttempted",
        attempted_files=[],
        updated_files=[],
        errors=[],
        warnings=[],
    ))
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


# ── Mutation outcome parsing ──────────────────────────────────────────────

def _optional_text(value: Any) -> str | None:
    return str(value) if value not in (None, "") else None


def _parse_mutation_failure(value: Any) -> MutationFailure | None:
    if not isinstance(value, dict):
        return None
    try:
        return MutationFailure(
            stage=str(value["stage"]),
            code=str(value["code"]),
            message=str(value.get("message", "")),
        )
    except KeyError as exc:
        raise CodeRadarError(f"apply_mutation returned a malformed failure: {value!r}") from exc


def _parse_mutation_file_error(value: Any) -> MutationFileError | None:
    if not isinstance(value, dict):
        return None
    try:
        return MutationFileError(code=str(value["code"]), message=str(value.get("message", "")))
    except KeyError as exc:
        raise CodeRadarError(f"apply_mutation returned a malformed file error: {value!r}") from exc


_ALLOWED_FILE_STATES = ("Untouched", "Installed", "Restored", "ExternallyChanged", "RestoreFailed", "Unknown")


def _parse_mutation_file(value: Any) -> MutationFileResult:
    if not isinstance(value, dict):
        raise CodeRadarError(f"apply_mutation returned a malformed file result: {value!r}")
    try:
        state = str(value["state"])
        if state not in _ALLOWED_FILE_STATES:
            raise CodeRadarError(f"apply_mutation returned unknown file state: {state!r}")
        return MutationFileResult(
            file=str(value["file"]),
            state=state,  # type: ignore[arg-type]
            pre_hash=str(value.get("pre_hash", "")),
            installed_hash=_optional_text(value.get("installed_hash")),
            final_hash=_optional_text(value.get("final_hash")),
            expected_hash=_optional_text(value.get("expected_hash")),
            recovery_path=_optional_text(value.get("recovery_path")),
            error=_parse_mutation_file_error(value.get("error")),
        )
    except KeyError as exc:
        raise CodeRadarError(f"apply_mutation returned a malformed file result: {value!r}") from exc


def _blank_sync_summary(state: str = "NotAttempted") -> MutationSyncSummary:
    return MutationSyncSummary(
        state=state,  # type: ignore[arg-type]
        attempted_files=[],
        updated_files=[],
        errors=[],
        warnings=[],
    )


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
        :meth:`callers` or ``plan_rename``. An unknown field raises
        ``ValueError`` listing what the entity does have — it is never a
        silent empty result.

        Example::

            for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
                print(cls["id"], cls["name"])

        Full field reference: ``coderadar docs query-language``.
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
    ) -> dict[str, Any]:
        """Source and call paths for the named symbols (MCP ``coderadar_explore``).

        `query` is symbol names or a question; `symbols` names them
        explicitly. Returns ``{"names", "files", "relationships",
        "stale_files"}`` — see :func:`coderadar.ops.explore`.
        """
        from . import ops
        return ops.explore(query, symbols, direction, max_files)

    # ── Macrame Operations ────────────────────────────────────────────

    @_bound_to_loaded_project
    def traverse(
        self,
        entity_id: str | None = None,
        direction: str = "both",
        edge_kinds: list[str] | None = None,
        max_depth: int = 3,
    ) -> list[dict[str, Any]]:
        """Breadth-first walk along any edge kinds (MCP ``coderadar_traverse``).

        `direction` is ``downstream`` | ``upstream`` | ``both``;
        `edge_kinds` any of ``calls``, ``imports``,
        ``inherits`` (alias ``extends``), ``implements``, ``decorates``, ``instantiates``, ``references``, ``overrides`` —
        None means all.
        Returns the reached entity rows, each with ``depth`` and
        ``edge_type``; an unknown `entity_id` raises ``ops.NotFound``.
        """
        from . import ops
        return ops.traverse(entity_id or "", direction, edge_kinds, max_depth)["results"]

    def as_of(self, timestamp: str) -> Snapshot:
        """Return a point-in-time snapshot: entity reads fold the ledger at
        recorded-time T (Macrame canonical UTC; anything else is
        `ops.InvalidRequest`). Signature decision (plan §0.1a): `as_of` stays
        — MCP/CLI/Python names are pinned by the parity test, and DR-17
        rules out another rename round. Bytes-at-T rides source blobs (§3.0:
        `Snapshot.read_bytes` / `read_source`)."""
        from . import ops
        try:
            from coderadar._core import normalize_timestamp as _normalize_ts
            normalized = _normalize_ts(timestamp or "")
        except ImportError as e:
            raise ops.EngineError(f"native extension unavailable: {e}") from e
        except ValueError as e:
            raise ops.InvalidRequest(str(e)) from None
        return Snapshot(self, normalized)

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

    @_bound_to_loaded_project
    def call_sites(self, entity_id: str) -> list[dict[str, Any]] | None:
        """Every call site extracted from a function, with the resolver's verdict.

        Rows are ``{name, path, line, col, status, target, reason}`` in source
        order; ``status`` is one of function / method / constructor / builtin /
        external / unresolved / pending. ``None`` when ``entity_id`` is not a
        function. Unlike ``callees`` this shows the calls that did *not*
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
    @_bound_to_loaded_project
    def diagnose(self, unresolved: bool = True,
                 low_confidence: bool = True) -> dict[str, Any]:
        """Graph self-health: unresolved targets + ambiguous bases (MCP
        ``coderadar_diagnose``, CLI ``diagnose``)."""
        from . import ops
        return ops.diagnose(unresolved, low_confidence)

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

    @_bound_to_loaded_project
    def search_hybrid(
        self, query: str | list[float], top_k: int = 10,
    ) -> list[dict[str, Any]]:
        """Hybrid keyword+vector search, RRF-fused (MCP ``coderadar_search_hybrid``).

        Same query shape as ``search_similar`` (natural language or a
        ready vector); each row carries both ``vector_rank`` and
        ``keyword_rank`` per-arm evidence alongside the fused ``score``.
        """
        from . import ops
        return ops.search_hybrid(query, top_k)

    def compute_embeddings(self, model_name: str | None = None,
                           batch_size: int = 32,
                           recompute: bool = False) -> dict[str, int]:
        """Compute and store embeddings for all indexable entities (MCP
        ``coderadar_compute_embeddings``). Returns ``{generated, cached,
        total, errors}``; unchanged entities are skipped by keyed content
        hash. A model switch needs ``recompute=True`` (clear + regenerate).
        """
        from . import ops
        return ops.compute_embeddings(model_name, batch_size, recompute)

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

    def archive(self, cutoff: str | None = None) -> dict[str, Any]:
        """One archive session at `cutoff` (§3.4, DR-25).

        Moves blobs last-put before the cutoff and named by no hot log
        entry to the `<store-stem>_archive.db` sibling, alongside archivable
        ledger rows. Omitted `cutoff` reads `[retention] archive_after_days`
        from `./.coderadar.toml`. Reads never break (cold fallback); backup
        is the hot+cold pair — see `docs/store-and-retention.md`."""
        from . import ops
        return ops.archive(cutoff)

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
        dropped: dict | None = None
        route_warning: str | None = None
        if content is None and not _resolve_entity_path(file_path).exists():
            dropped = self._remove_file_full(file_path)
            # §1.3 (DR-10): the file is gone — its routes retire via the
            # file-scoped diff (missing file extracts nothing).
            route_warning = self._refresh_framework_routes(file_path, None)
        if dropped and dropped["entities_removed"]:
            # a file never indexed falls through and fails below
            return UpdateReport(
                affected_files=[file_path], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=0.0, parse_quality="Removed", parse_errors=0,
                fully_applied=True,
                epoch_before=dropped["epoch_before"],
                epoch_after=dropped["epoch_after"],
                removed=True, entities_removed=dropped["entities_removed"],
                route_warning=route_warning,
            )
        try:
            from coderadar._core import update_file as _update_file_rust
            result = _update_file_rust(file_path, content, force)
            # §1.3 (DR-10): re-extract this file's routes (scope = file;
            # ghosts retire via the diff). Best-effort: extraction must
            # never fail the update, but its warning belongs in the report.
            route_warning = self._refresh_framework_routes(file_path, content)
            if isinstance(result, dict):
                persistence_error = result.get("persistence_error")
                blob_error = result.get("blob_error")
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
                    source_hash=str(result.get("source_hash", "")),
                    update_error=None,
                    persistence_error=str(persistence_error) if persistence_error else None,
                    blob_error=str(blob_error) if blob_error else None,
                    route_warning=route_warning,
                    blobs_stored=int(result.get("blobs_stored", 0)),
                    blobs_skipped_oversize=int(result.get("blobs_skipped_oversize", 0)),
                    blobs_skipped_excluded=int(result.get("blobs_skipped_excluded", 0)),
                    blobs_bytes=int(result.get("blobs_bytes", 0)),
                )
            return UpdateReport(
                affected_files=[file_path], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=0.0,
                parse_quality="Error: the coderadar._core extension is not built",
                parse_errors=1,
                fully_applied=False, epoch_before=0, epoch_after=0,
                update_error="the coderadar._core extension is not built",
            )
        except RuntimeError as e:
            return UpdateReport(
                affected_files=[file_path], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=0.0, parse_quality=f"Error: {e}", parse_errors=1,
                fully_applied=False, epoch_before=0, epoch_after=0,
                update_error=str(e),
            )

        # Fallback (unreachable in practice)
        return UpdateReport(
            affected_files=[file_path], changed_symbols=[],
            new_unresolved_references=[], newly_resolved_references=[],
            elapsed_ms=0.0, parse_quality="Clean", parse_errors=0,
            fully_applied=False, epoch_before=0, epoch_after=0,
            update_error="update_file returned no usable report",
        )

    @staticmethod
    def _refresh_framework_routes(file_path: str, content: str | None) -> str | None:
        """Re-extract one file's framework routes (§1.3, DR-10).

        Returns `None` on success or skip, or a warning string the caller
        must surface. Route refresh never fails the projection update.
        """
        try:
            from coderadar import framework as _framework
            from coderadar.excludes import resolve_entity_path as _resolve
            try:
                from coderadar._core import indexed_root_py as _rust_root
                root = _rust_root() or os.getcwd()
            except Exception:  # noqa: BLE001 - fall back to the cwd
                root = os.getcwd()
            abs_path = _resolve(file_path)
            try:
                rel = os.path.relpath(abs_path, root).replace(os.sep, "/")
            except ValueError:
                return None
            if rel.startswith(".."):
                return None
            _framework.refresh_framework_file(root, rel, content)
            return None
        except Exception as exc:  # noqa: BLE001 - best-effort refresh reports, never raises
            return f"framework-route refresh failed for {file_path}: {exc}"

    def _remove_file_full(self, file_path: str) -> dict:
        """Drop a deleted file; full report (epochs included).

        `remove_file` keeps the historic int return; this is the seam
        `update_file`'s delete-drop branch reports through.
        """
        try:
            from coderadar._core import remove_file as _remove_file_rust
        except ImportError as exc:
            raise CodeRadarError(
                "The coderadar._core extension is not built; "
                "nothing can be removed from the graph."
            ) from exc
        result = _remove_file_rust(file_path)
        return {
            "entities_removed": int(result.get("entities_removed", 0)),
            "epoch_before": int(result.get("epoch_before", 0)),
            "epoch_after": int(result.get("epoch_after", 0)),
        }

    @_bound_to_loaded_project
    def remove_file(self, file_path: str) -> int:
        """Drop a deleted file's entities from the graph.

        `update_file` does this too when it finds the file gone from disk.
        Returns the number of entities removed.
        """
        removed = self._remove_file_full(file_path)["entities_removed"]
        # §1.3 (DR-10): the file's routes retire with it.
        try:
            self._refresh_framework_routes(file_path, None)
        except Exception:  # noqa: BLE001, S110 - removal stands regardless
            pass
        return removed

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
        """Apply a mutation plan with guarded compensation and verified sync.

        Disk replacement is atomic per file; a multi-file plan is not a
        filesystem transaction. `Applied` additionally requires the graph,
        persistence, and derived-data synchronization summarized in `sync`.
        Valid source stays in place on graph-sync failure (`ApplyIncomplete`);
        compensation is for write/verification failure, not a pretend
        distributed transaction across disk and SQLite.
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

        with _mutation_operation_lock():
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

            disk_status = str(result.get("disk_status", ""))
            if disk_status not in ("Installed", "Restored", "NotInstalled",
                                   "RejectedStale", "RejectedPolicy", "DiskIncomplete"):
                raise CodeRadarError(f"apply_mutation returned unknown disk_status: {disk_status!r}")
            primary = _parse_mutation_failure(result.get("primary"))
            failure = _parse_mutation_failure(result.get("failure"))
            files = [_parse_mutation_file(entry) for entry in result.get("files", []) or []]
            files_written = list(result.get("files_written", []) or [])
            syntax_errors = list(result.get("errors", []) or [])
            backup_path = result.get("backup_path") or None
            attempt_id = str(result.get("attempt_id", "") or "")

            if disk_status in ("RejectedPolicy", "RejectedStale"):
                return MutationResult(
                    status=disk_status,  # type: ignore[arg-type]
                    files_written=files_written,
                    syntax_errors=syntax_errors,
                    attempt_id=attempt_id,
                    primary=primary,
                    failure=failure or primary,
                    files=files,
                    sync=_blank_sync_summary("NotNeeded"),
                    backup_path=backup_path,
                )

            expected_hashes = {entry.file: entry.expected_hash for entry in files
                               if entry.expected_hash}
            if disk_status in ("Installed", "Restored"):
                sync = self._synchronize_mutation_files(
                    plan.affected_files, expected_hashes, clear_embeddings_for_file)
            else:
                # NotInstalled performed no install; DiskIncomplete has no
                # proven final bytes. Neither gets blind "success" sync.
                sync = _blank_sync_summary()

            if disk_status == "Installed":
                if sync.state == "Synchronized":
                    return MutationResult(
                        status="Applied",
                        files_written=files_written,
                        syntax_errors=syntax_errors,
                        attempt_id=attempt_id,
                        primary=primary,
                        failure=None,
                        files=files,
                        sync=sync,
                        backup_path=backup_path,
                    )
                status = "ApplyIncomplete"
                public_failure = self._sync_failure(sync) or failure or primary
            elif disk_status == "Restored":
                primary_code = primary.code if primary else ""
                if sync.state in ("Synchronized", "NotNeeded"):
                    if primary_code == "stale_content":
                        status = "RejectedStale"
                    else:
                        status = "RolledBack"
                    public_failure = failure or primary
                else:
                    status = "RollbackIncomplete"
                    public_failure = self._sync_failure(sync) or failure or primary
            elif disk_status == "NotInstalled":
                status = "RolledBack"
                public_failure = failure or primary
            else:
                status = "RollbackIncomplete"
                public_failure = failure or primary

            return MutationResult(
                status=status,  # type: ignore[arg-type]
                files_written=files_written,
                syntax_errors=syntax_errors,
                attempt_id=attempt_id,
                primary=primary,
                failure=public_failure,
                files=files,
                sync=sync,
                backup_path=backup_path,
            )

    @staticmethod
    def _sync_failure(sync: MutationSyncSummary) -> MutationFailure | None:
        if not sync.errors:
            return None
        error = sync.errors[0]
        return MutationFailure(
            stage=str(error.get("stage", "graph_sync")),
            code=str(error.get("code", "sync_failed")),
            message=str(error.get("message", "mutation synchronization failed")),
        )

    def _synchronize_mutation_files(
        self,
        affected_files: list[str],
        expected_hashes: dict[str, str],
        clear_embeddings_for_file: Any,
    ) -> MutationSyncSummary:
        """Synchronize every touched file and prove its bytes.

        The sync set is the canonical union of affected files and files the
        disk attempt installed or restored; each installed/restored entry
        carries the exact hash the facade must observe. Multi-file plans
        keep the existing R2-17 second pass so cross-file resolution
        converges.
        """
        attempted_files = sorted(set(affected_files) | set(expected_hashes))
        if not attempted_files:
            return MutationSyncSummary(
                state="NotNeeded",
                attempted_files=[],
                updated_files=[],
                errors=[],
                warnings=[],
            )
        errors: list[dict[str, Any]] = []
        warnings: list[dict[str, Any]] = []
        reports: dict[str, UpdateReport] = {}

        def synchronize_once(label: str) -> None:
            for target in attempted_files:
                try:
                    report = self.update_file(target)
                except Exception as exc:  # noqa: BLE001 - sync must report, not raise
                    errors.append({
                        "stage": "graph_sync",
                        "code": "update_failed",
                        "file": target,
                        "message": f"{label} update failed for {target}: {exc}",
                    })
                    continue
                expected = expected_hashes.get(target)
                if report.update_error:
                    errors.append({
                        "stage": "graph_sync",
                        "code": "update_failed",
                        "file": target,
                        "message": f"{label} update failed for {target}: {report.update_error}",
                    })
                elif expected is not None and report.source_hash != expected:
                    errors.append({
                        "stage": "graph_sync",
                        "code": "concurrent_modification",
                        "file": target,
                        "message": (
                            f"{label} update for {target} parsed bytes {report.source_hash!r}, "
                            f"expected {expected!r}; current disk is another writer's work"
                        ),
                    })
                elif report.persistence_error or report.blob_error:
                    detail = report.persistence_error or report.blob_error or ""
                    errors.append({
                        "stage": "persistence",
                        "code": "persistence_failed",
                        "file": target,
                        "message": f"{label} persistence failed for {target}: {detail}",
                    })
                else:
                    if report.parse_errors:
                        errors.append({
                            "stage": "graph_sync",
                            "code": "recovered_parse",
                            "file": target,
                            "message": (
                                f"{label} update for {target} recovered from "
                                f"{report.parse_errors} parse error(s); synchronization is partial"
                            ),
                        })
                    if report.route_warning:
                        warnings.append({
                            "stage": "graph_sync",
                            "code": "framework_routes",
                            "file": target,
                            "message": report.route_warning,
                        })
                reports[target] = report

        synchronize_once("first")
        # R2-17 convergence: pass 1 settles entities/imports, pass 2 resolves
        # calls against settled siblings. The final reports decide the outcome.
        if len(attempted_files) > 1:
            synchronize_once("second")

        for target in attempted_files:
            report = reports.get(target)
            if report is None or report.update_error:
                continue
            expected = expected_hashes.get(target)
            if expected is not None and report.source_hash != expected:
                continue
            if report.persistence_error or report.blob_error:
                continue
            try:
                cleared = clear_embeddings_for_file(target)
            except Exception as exc:  # noqa: BLE001 - sync must report, not raise
                errors.append({
                    "stage": "derived_data",
                    "code": "derived_data_failed",
                    "file": target,
                    "message": f"derived-data invalidation failed for {target}: {exc}",
                })
                continue
            cleared_count = 0
            if isinstance(cleared, dict):
                try:
                    cleared_count = int(cleared.get("cleared", 0) or 0)
                except (TypeError, ValueError):
                    cleared_count = 0
            if cleared_count:
                # Derived vectors are only cleared in memory here. Persisted
                # per-model rows are overwritten by recompute, not retracted,
                # so name the pending recompute instead of claiming removal.
                warnings.append({
                    "stage": "derived_data",
                    "code": "embeddings_pending_recompute",
                    "file": target,
                    "message": (
                        f"cleared {cleared_count} in-memory embedding vector(s) for {target}; "
                        "persisted per-model vectors remain until compute_embeddings regenerates them"
                    ),
                })

        updated_files = sorted(
            target for target, report in reports.items()
            if report.update_error is None
            and (
                expected_hashes.get(target) is None
                or report.source_hash == expected_hashes.get(target)
            )
            and not report.persistence_error
            and not report.blob_error
            and not report.parse_errors
        )
        hard_errors = [error for error in errors if error.get("code") != "recovered_parse"]
        if hard_errors:
            state = "Failed"
        elif errors:
            state = "Partial"
        else:
            state = "Synchronized"
        return MutationSyncSummary(
            state=state,  # type: ignore[arg-type]
            attempted_files=attempted_files,
            updated_files=updated_files,
            errors=errors,
            warnings=warnings,
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

    Honest by construction (§0.1a, DR-9): the timestamp is normalized to
    Macrame canonical UTC at :meth:`CodeGraph.as_of` time; :meth:`find`
    folds the ledger at recorded-time T (``None`` = "not in the graph at
    T"); downstream :meth:`traverse` walks the at-T edges with at-T node
    bodies; every method WITHOUT a temporal path (`query`, `callers`,
    upstream `traverse`, `explore`) raises `ops.TemporalUnsupported`
    instead of answering from the present. `predates_recorded_history`
    reports a T before the first recorded write distinctly (empty answer,
    not an error). Bytes-at-T rides source blobs (§3.0): `read_bytes`
    serves exact bytes-at-T, `read_source` the display form.

    Usage:
        snapshot = graph.as_of("2025-06-15T10:00:00.000000Z")
        entity = snapshot.find("src/models.py::User.save")  # None if absent at T
    """

    def __init__(self, graph: CodeGraph, timestamp: str):
        self._graph = graph
        self._timestamp = timestamp
        self._predates: bool | None = None

    @property
    def timestamp(self) -> str:
        return self._timestamp

    @property
    def predates_recorded_history(self) -> bool:
        """True when T sits before the first recorded write.

        Computed lazily (one ledger fold, no entity extraction) and cached.
        "True" with every `find` returning `None` is the honest pre-history
        answer — not an error, not present-tense data.
        """
        if self._predates is None:
            from . import ops
            try:
                from coderadar._core import lookup_entities_at as _lookup_at
                batch = _lookup_at([], self._timestamp)
            except ValueError as e:  # normalized at construction; defensive
                raise ops.InvalidRequest(str(e)) from None
            except RuntimeError as e:
                raise ops.EngineError(str(e)) from None
            self._predates = bool(batch.get("predates_recorded_history", False))
        return self._predates

    def find(self, entity_id: str) -> dict[str, Any] | None:
        """One entity as it was at T (live-`lookup_entity` dict shape).

        `None` means "not in the graph at T" — never asserted, not yet
        asserted, or already retired then. Updates the cached
        :attr:`predates_recorded_history`.
        """
        from . import ops
        try:
            from coderadar._core import lookup_entity_at as _lookup_at
            result = _lookup_at(entity_id, self._timestamp)
        except ValueError as e:  # normalized at construction; defensive
            raise ops.InvalidRequest(str(e)) from None
        except RuntimeError as e:
            raise ops.EngineError(str(e)) from None
        self._predates = bool(result.get("predates_recorded_history", False))
        return result.get("entity")

    def query(self, query_str: str) -> Iterator[dict[str, Any]]:
        """Refuses: query execution has no temporal path (raises)."""
        from . import ops
        raise ops.TemporalUnsupported(
            "Snapshot.query has no temporal path: the query engine reads the "
            "present-tense index. Use CodeGraph.query for present-tense search, "
            "or Snapshot.find / Snapshot.traverse(direction='out') for as-of-T reads."
        )

    def callers(self, entity_id: str) -> list[dict[str, Any]]:
        """Refuses: upstream walks have no temporal path (raises)."""
        from . import ops
        raise ops.TemporalUnsupported(
            "Snapshot.callers has no temporal path: Macrame's temporal leg follows "
            "outgoing edges only. Use CodeGraph.callers for present-tense callers, "
            "or Snapshot.callees / Snapshot.traverse(direction='out') for as-of-T reads."
        )

    def callees(self, entity_id: str) -> list[dict[str, Any]]:
        """Direct callees as of T: full entity dicts (live-`lookup` shape).

        A depth-1 downstream walk at T, with reached ids re-read from the
        at-T projection — never present-tense bodies. A start id unknown
        at T walks nowhere and yields `[]`.
        """
        nodes = self.traverse(entity_id, direction="out",
                              edge_kinds=["calls"], max_depth=1)
        ids = [n["id"] for n in nodes
               if isinstance(n, dict) and n.get("id") and n["id"] != entity_id
               and n.get("depth", 1) == 1]
        if not ids:
            return []
        from . import ops
        try:
            from coderadar._core import lookup_entities_at as _lookup_at
            batch = _lookup_at(ids, self._timestamp)
        except ValueError as e:
            raise ops.InvalidRequest(str(e)) from None
        except RuntimeError as e:
            raise ops.EngineError(str(e)) from None
        self._predates = bool(batch.get("predates_recorded_history", False))
        found = batch.get("entities", {})
        return [found[i] for i in ids if found.get(i)]

    def traverse(self, entity_id: str, direction: str = "both",
                 edge_kinds: list[str] | None = None,
                 max_depth: int = 3) -> list[dict[str, Any]]:
        """Downstream walk as of T (raw traversal node dicts: entity fields
        read from the at-T projection, plus `depth` + `edge_type`).

        `direction='out'`/`'downstream'` only — anything upstream raises
        `ops.TemporalUnsupported` (Macrame's temporal leg follows outgoing
        edges). Garbage directions raise `ops.InvalidRequest` via the engine.
        """
        from . import ops
        if direction.strip().lower() in ("in", "upstream", "both"):
            raise ops.TemporalUnsupported(
                f"Snapshot.traverse(direction={direction!r}) has no temporal path: "
                "Macrame's temporal leg follows outgoing edges only. Pass "
                "direction='out', or use CodeGraph.traverse for present-tense walks."
            )
        try:
            from coderadar._core import traverse as _traverse
            return _traverse(entity_id, max_depth, edge_kinds or [],
                             direction, self._timestamp)
        except ValueError as e:
            raise ops.InvalidRequest(str(e)) from None
        except RuntimeError as e:
            raise ops.EngineError(str(e)) from None

    def read_bytes(self, entity_id: str, start: int | None = None,
                   end: int | None = None) -> bytes | None:
        """Exact source bytes for one entity as of T (§3.0 raw-byte path).

        `None` = not in the graph at T (same answer as :meth:`find`, never
        an error). Otherwise the file bytes recorded at T, sliced to the
        entity's at-T byte span — CRLF, non-ASCII, everything preserved
        bit-for-bit. Module ids (`{file}::module`) read the whole file
        (file-bytes-at-T); entity spans are tree-sitter node extents, so a
        trailing line break belongs to no entity. `start`/`end` narrow to
        a sub-range of the returned bytes (outside = `InvalidRequest`).

        Raises `ops.ContentUnavailable` (never a silent present-tense
        answer) when the entity is in the graph at T but its bytes cannot
        be served: no blob recorded (pre-blob generation, excluded,
        oversize, disabled — the message says how to check), recorded
        digest unresolvable (bytes gone), or a synthetic without a source
        span (routes name their handler instead).
        """
        entity, raw, _ref = self._read_raw(entity_id)
        if entity is None or raw is None:
            return None
        if start is None and end is None:
            return raw
        lo = 0 if start is None else start
        hi = len(raw) if end is None else end
        if not (0 <= lo <= hi <= len(raw)):
            from . import ops
            raise ops.InvalidRequest(
                f"range [{lo}, {hi}] is outside {entity['id']}'s "
                f"{len(raw)}-byte span at {self._timestamp}"
            )
        return raw[lo:hi]

    def read_source(self, entity_id: str) -> str | None:
        """Line-numbered source for one entity as of T (§3.0 display path).

        `None` = not in the graph at T; `ContentUnavailable` propagates
        when the bytes are missing. Decodes the raw at-T bytes as UTF-8
        (`errors="replace"` — display decodes lossy, :meth:`read_bytes`
        is exact), slices the entity's at-T line range, and prefixes
        line numbers: the same presentation as `ops.read_source`, but
        from recorded bytes, never the disk.
        """
        entity, raw, ref = self._read_raw(entity_id)
        if entity is None or raw is None:
            return None
        if ref.get("full_file"):
            ref_start, ref_end = 1, len(raw.decode("utf-8", errors="replace").splitlines())
        else:
            ref_start = int(ref.get("start_line", 1))
            ref_end = int(ref.get("end_line", ref_start))
        text = raw.decode("utf-8", errors="replace")
        lines = text.splitlines()
        si = max(0, ref_start - 1)
        ei = min(len(lines), ref_end)
        return "".join(f"{i + 1}\t{lines[i]}\n" for i in range(si, ei))

    def _read_raw(
        self, entity_id: str
    ) -> tuple[dict[str, Any] | None, bytes | None, dict[str, Any] | None]:
        """`(entity-at-T, exact bytes, source ref)` shared by the readers.

        `(None, None, None)` = not in the graph at T. Raises
        `ContentUnavailable` for in-graph-but-byteless.
        """
        from . import ops
        entity = self.find(entity_id)
        if entity is None:
            return None, None, None
        try:
            from coderadar._core import blob_get as _blob_get
            from coderadar._core import entity_source_ref_at as _ref_at
            ref = _ref_at(entity["id"], self._timestamp)
        except ValueError as e:  # normalized at construction; defensive
            raise ops.InvalidRequest(str(e)) from None
        except RuntimeError as e:
            raise ops.EngineError(str(e)) from None
        if ref is None:  # vanished between the two folds; answer as absent
            return None, None, None
        digest = ref.get("digest")
        if not digest:
            raise ops.ContentUnavailable(
                f"no source blob recorded for {ref.get('file_path')} "
                f"at {self._timestamp} (predates blob storage, or the file "
                f"was excluded, oversize, or blobs disabled — "
                f"coderadar.blob_stats() shows this session's puts/skips)"
            )
        if not ref.get("has_span") and not ref.get("full_file"):
            handler = ref.get("handler_id")
            hint = f"; read its handler {handler} instead" if handler else ""
            raise ops.ContentUnavailable(
                f"{ref.get('id')} is a synthetic route with no source span{hint}"
            )
        try:
            blob = _blob_get(digest)
        except ValueError as e:  # recorded digest malformed: ledger corrupt
            raise ops.ContentUnavailable(
                f"recorded digest {digest!r} for {ref.get('file_path')} "
                f"at {self._timestamp} is malformed: {e}"
            ) from None
        except RuntimeError as e:
            raise ops.EngineError(str(e)) from None
        if blob is None:
            raise ops.ContentUnavailable(
                f"source blob {digest} for {ref.get('file_path')} "
                f"at {self._timestamp} is recorded but unresolvable "
                f"(bytes gone — restore the hot+cold backup pair)"
            )
        raw = bytes(blob)
        if ref.get("full_file"):
            # Modules are the file: whole blob, no slicing (file-bytes-at-T).
            return entity, raw, ref
        lo, hi = int(ref["span_start"]), int(ref["span_end"])
        if not (0 <= lo <= hi <= len(raw)):
            raise ops.ContentUnavailable(
                f"recorded span [{lo}, {hi}] for {ref.get('id')} exceeds "
                f"blob bytes ({len(raw)} at digest {digest}): ledger and "
                f"blob disagree"
            )
        return entity, raw[lo:hi], ref


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

    # DR-31: record the honored config so the cheap path can detect a
    # later `.coderadar.toml` change (BUGS_QUIRKS #3). Best-effort: never
    # fail the index when the sidecar cannot be written.
    with contextlib.suppress(OSError):
        from coderadar.coldstart import _record_indexed_config as _record_cfg

        _record_cfg(root)

    # §1.3 (DR-10): framework routes persist as concepts + synthetic edges
    # on every full analyze (not just `init`), so a fresh session from the
    # store resolves routes. Best-effort like the star-export pass above:
    # extraction must never fail the index (counts ride `graph_stats`).
    try:
        from coderadar import framework as _framework

        _framework.run_framework_extraction(root)
    except Exception:  # noqa: BLE001, S110 - extraction must never fail analyze
        pass

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


def blob_stats() -> dict:
    """Cumulative blob activity backing the loaded generation (§1.9).

    Reset on `analyze`/`load_snapshot`, incremented by every write-through
    put — the DR-30 notice (counts ARE the notice). Keys: `stored`,
    `skipped_oversize`, `skipped_excluded`, `bytes`. Empty (all zeros)
    means no blob activity yet, or the kill-switch `[database]
    store_source_blobs=false` is on.
    """
    try:
        from coderadar._core import blob_stats as _blob_stats_rust
        return dict(_blob_stats_rust())
    except ImportError:
        return {"stored": 0, "skipped_oversize": 0,
                "skipped_excluded": 0, "bytes": 0}


__all__ = [
    "BatchContext",
    "CodeGraph",
    "CodeRadarError",
    "MutationEdit",
    "MutationError",
    "MutationFailure",
    "MutationFileError",
    "MutationFileResult",
    "MutationPlan",
    "MutationResult",
    "MutationSyncSummary",
    "ParseError",
    "PolicyViolation",
    "ResolutionError",
    "Snapshot",
    "StaleHandle",
    "SymbolChange",
    "UpdateReport",
    "Watcher",
    "analyze",
    "blob_stats",
    "load",
    "ops",
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
        blobs_stored = 0
        blobs_skipped_oversize = 0
        blobs_skipped_excluded = 0
        blobs_bytes = 0
        update_error: str | None = None
        persistence_errors: list[str] = []
        blob_errors: list[str] = []
        route_warning: str | None = None

        for file_path, change_kind in batch:
            # The watcher stats the path, so "Delete" now actually arrives;
            # before, a deleted file's entities lived on in the graph until
            # the next full analyze.
            if change_kind == "Delete":
                try:
                    # `remove_file` also retires the file's routes (§1.3).
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
            if report.update_error and update_error is None:
                update_error = f"{file_path}: {report.update_error}"
            if report.persistence_error:
                persistence_errors.append(f"{file_path}: {report.persistence_error}")
            if report.blob_error:
                blob_errors.append(f"{file_path}: {report.blob_error}")
            if report.route_warning and route_warning is None:
                route_warning = f"{file_path}: {report.route_warning}"
            blobs_stored += report.blobs_stored
            blobs_skipped_oversize += report.blobs_skipped_oversize
            blobs_skipped_excluded += report.blobs_skipped_excluded
            blobs_bytes += report.blobs_bytes
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
            blobs_stored=blobs_stored,
            blobs_skipped_oversize=blobs_skipped_oversize,
            blobs_skipped_excluded=blobs_skipped_excluded,
            blobs_bytes=blobs_bytes,
            source_hash="",
            update_error=update_error,
            persistence_error="; ".join(persistence_errors) if persistence_errors else None,
            blob_error="; ".join(blob_errors) if blob_errors else None,
            route_warning=route_warning,
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
