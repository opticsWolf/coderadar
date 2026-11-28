"""Public result types and exceptions, field-for-field with the reference.

These are data shapes, not behavior: every one of them is produced by the Rust
engine and merely *shaped* here so existing Python callers keep working. The
reference package defines them in Python too, so this is parity, not new
policy. The engine's gate decoder keeps using the private aliases in
`codegraph.py`; the public names live here.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal


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
class UpdateReport:
    """Result of a single-file or batch update (§8.2)."""

    affected_files: list[str]
    changed_symbols: list[SymbolChange]
    new_unresolved_references: list[dict]
    newly_resolved_references: list[dict]
    elapsed_ms: float
    parse_quality: str
    parse_errors: int
    fully_applied: bool
    epoch_before: int
    epoch_after: int
    source_hash: str = ""
    update_error: str | None = None
    persistence_error: str | None = None
    blob_error: str | None = None
    route_warning: str | None = None
    removed: bool = False
    entities_removed: int = 0
    observed_at: str = ""
    source: str = ""
    lineage: str = ""
    blobs_stored: int = 0
    blobs_skipped_oversize: int = 0
    blobs_skipped_excluded: int = 0
    blobs_bytes: int = 0


@dataclass(frozen=True)
class MutationEdit:
    """A single byte-accurate edit to a file."""

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
class MutationPlan:
    """A planned mutation — produced by the planner, applied by `apply`."""

    id: str
    tool: str
    edits: list[MutationEdit]
    affected_files: list[str] = field(default_factory=list)
    diff_preview: str = ""
    unverified_sites: list[dict] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)


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
    state: str
    pre_hash: str = ""
    installed_hash: str | None = None
    final_hash: str | None = None
    expected_hash: str | None = None
    recovery_path: str | None = None
    error: MutationFileError | None = None


@dataclass(frozen=True)
class MutationSyncSummary:
    """What public synchronization proved after the disk attempt."""

    state: str
    attempted_files: list[str] = field(default_factory=list)
    updated_files: list[str] = field(default_factory=list)
    errors: list[dict[str, Any]] = field(default_factory=list)
    warnings: list[dict[str, Any]] = field(default_factory=list)
    observations: list[dict[str, Any]] = field(default_factory=list)


@dataclass(frozen=True)
class MutationResult:
    """Result of applying a mutation plan."""

    status: str
    files_written: list[str]
    syntax_errors: list[Any]
    attempt_id: str = ""
    primary: MutationFailure | None = None
    failure: MutationFailure | None = None
    files: list[MutationFileResult] = field(default_factory=list)
    sync: MutationSyncSummary = field(
        default_factory=lambda: MutationSyncSummary(state="NotAttempted")
    )
    backup_path: str | None = None


# ── Exceptions ────────────────────────────────────────────────────────────


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


__all__ = [
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
    "StaleHandle",
    "SymbolChange",
    "UpdateReport",
]
