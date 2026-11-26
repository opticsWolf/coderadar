"""Thin ``CodeGraph`` facade over the Rust-native ops.

The Rust binary is the product. This module keeps the familiar Python
``coderadar.analyze(...)`` handle working by delegating each method to the
linked Rust extension. It deliberately does not reimplement graph logic in
Python; methods whose Python-only shape has no direct Rust equivalent are
left on the original Python package rather than being faked.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import _native


@dataclass(frozen=True)
class _MutationEdit:
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
class _MutationPlan:
    id: str
    tool: str
    edits: list[_MutationEdit] = field(default_factory=list)
    affected_files: list[str] = field(default_factory=list)
    diff_preview: str = ""
    unverified_sites: list[dict] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class _MutationResult:
    status: str
    files_written: list[str] = field(default_factory=list)
    syntax_errors: list[Any] = field(default_factory=list)
    attempt_id: str = ""
    raw: dict[str, Any] = field(default_factory=dict)


def _plan(name: str, payload: Any) -> _MutationPlan:
    outer = payload if isinstance(payload, dict) else {}
    data = outer.get("plan", outer) if isinstance(outer.get("plan"), dict) else outer
    return _MutationPlan(
        id=str(data.get("id", "")),
        tool=name,
        edits=[_MutationEdit(**entry) for entry in data.get("edits", []) or []],
        affected_files=list(data.get("affected_files", []) or []),
        diff_preview=str(data.get("diff_preview", "") or ""),
        unverified_sites=list(data.get("unverified_sites", []) or []),
        warnings=list(data.get("warnings", []) or []),
    )


class CodeGraph:
    """A handle to the graph loaded by the Rust engine."""

    def __init__(self, db_path: str | None = None):
        self._db_path = db_path or str(
            Path(".coderadar") / "store" / "coderadar.db"
        )
        self.stats_result: dict[str, Any] = {}

    # Query / traversal
    def query(self, query_str: str) -> Any:
        return _native.query(query_str)

    def explore(
        self,
        query: str = "",
        symbols: list[str] | None = None,
        direction: str = "both",
        max_files: int = 8,
    ) -> Any:
        return _native.explore(query, symbols, direction, max_files)

    def traverse(
        self,
        entity_id: str,
        direction: str = "both",
        edge_kinds: list[str] | None = None,
        max_depth: int = 3,
    ) -> Any:
        return _native.traverse(entity_id, direction, edge_kinds, max_depth)

    def callers(self, entity_id: str) -> Any:
        return _native.callers(entity_id)

    def callees(self, entity_id: str) -> Any:
        return _native.callees(entity_id)

    def node(self, entity_id: str, include_neighbors: bool = False) -> Any:
        return _native.node(entity_id, include_neighbors)

    def search(self, query: str, kind: str | None = None, top_k: int = 10) -> Any:
        return _native.search(query, kind, top_k)

    def affected(self, entity_id: str, max_depth: int = 5) -> Any:
        return _native.affected(entity_id, max_depth)

    def module_children(self, module_id: str) -> Any:
        return _native.module_children(module_id)

    def resolve(self, name: str, limit: int = 5) -> Any:
        return _native.resolve(name, limit)

    def find_entity(self, *args: Any, **kwargs: Any) -> Any:
        return _native.find_entity(*args, **kwargs)

    # Quality / storage reads
    def dead_code(
        self,
        min_confidence: float = 0.6,
        include_test_reachable: bool = False,
        max_findings: int = 100,
    ) -> Any:
        return _native.dead_code(min_confidence, include_test_reachable, max_findings)

    def get_smells(
        self,
        entity_id: str | None = None,
        rule_id: str | None = None,
        strictness: str = "normal",
    ) -> Any:
        return _native.get_smells(entity_id, rule_id, strictness)

    def find_clones(
        self,
        min_lines: int = 10,
        min_similarity: float = 0.8,
        max_groups: int = 100,
    ) -> Any:
        return _native.find_clones(min_lines, min_similarity, max_groups)

    def find_scaffolding(
        self, include_secrets: bool = False, max_findings: int = 100
    ) -> Any:
        return _native.find_scaffolding(include_secrets, max_findings)

    def read_source(self, entity: str) -> Any:
        return _native.read_source(entity)

    def stale_files(self, file_paths: list[str]) -> Any:
        return _native.stale_files(file_paths)

    def blob_get(self, digest: str) -> Any:
        return _native.blob_get(digest)

    def archive(self, cutoff: str | None = None) -> Any:
        return _native.archive(cutoff)

    # Bitemporal reads
    def as_of(
        self,
        timestamp: str,
        query: str = "",
        symbols: list[str] | None = None,
        branch: str = "main",
        strict: bool = False,
        at: str | None = None,
        valid: bool = False,
    ) -> Any:
        return _native.as_of(
            timestamp, query, symbols, branch, strict=strict, at=at, valid=valid
        )

    def verify_history(
        self,
        from_ts: str | None = None,
        to_ts: str | None = None,
        generations: int = 25,
        all: bool = False,
        branch: str = "main",
        offenders: int = 10,
        max_generations: int | None = None,
    ) -> Any:
        return _native.verify_history(
            from_ts,
            to_ts,
            generations,
            all,
            branch,
            offenders,
            max_generations,
        )

    def history_of(self, file: str) -> Any:
        return _native.history_of(file)

    def changes_between(
        self,
        t1: str,
        t2: str,
        scope: str | None = None,
        branch: str = "main",
        valid: bool = False,
    ) -> Any:
        return _native.changes_between(t1, t2, scope, branch, valid=valid)

    # Annotations / comments
    def annotate(self, about: str, body: str) -> Any:
        return _native.annotate(about, body)

    def comment(self, about: str, body: str) -> Any:
        return _native.comment(about, body)

    def revise_annotation(self, id: str, body: str) -> Any:
        return _native.revise_annotation(id, body)

    def retire_annotation(self, id: str) -> Any:
        return _native.retire_annotation(id)

    def annotations_on(self, target: str) -> Any:
        return _native.annotations_on(target)

    def comments_on(self, target: str) -> Any:
        return _native.comments_on(target)

    def annotation_history(self, id: str) -> Any:
        return _native.annotation_history(id)

    def comment_history(self, id: str) -> Any:
        return _native.comment_history(id)

    # Mutations (dry-run by default, as in the Python API)
    def _refresh_framework_routes(
        self, file_path: str, content: str | None
    ) -> str | None:
        root = str(Path(self._db_path).parents[2])
        absolute = Path(file_path).resolve()
        try:
            rel = absolute.relative_to(Path(root).resolve()).as_posix()
        except ValueError:
            return None
        try:
            _native.refresh_framework_file(root, rel, content)
        except Exception as exc:  # noqa: BLE001 - refresh must never fail the update
            return f"framework-route refresh failed for {file_path}: {exc}"
        return None

    def update_file(self, file_path: str, content: str | None = None, **_: Any) -> Any:
        report = _native.update_file(file_path, content)
        warning = self._refresh_framework_routes(file_path, content)
        if isinstance(report, dict):
            report["route_warning"] = warning
        return report

    def record_checkout(self, branch: str | None) -> None:
        _native.record_checkout_op(branch)

    def replace_body(
        self,
        entity_id: str,
        new_body: str,
        expected_hash: str | None = None,
        dry_run: bool = True,
        **_: Any,
    ) -> Any:
        return _native.replace_body(entity_id, new_body, expected_hash, dry_run)

    def update_signature(
        self,
        entity_id: str,
        new_signature: str,
        inject_defaults: bool = False,
        dry_run: bool = True,
        **_: Any,
    ) -> Any:
        return _native.update_signature(
            entity_id, new_signature, inject_defaults, dry_run
        )

    def rename(
        self, entity_id: str, new_name: str, dry_run: bool = True, **_: Any
    ) -> Any:
        return _native.rename(entity_id, new_name, dry_run)

    def create_entity(
        self,
        file_path: str,
        language: str,
        kind: str,
        name: str,
        body: str,
        decorators: list[str] | None = None,
        anchor: str = "end",
        signature: str | None = None,
        dry_run: bool = True,
        **_: Any,
    ) -> Any:
        return _native.create_entity(
            file_path,
            language,
            kind,
            name,
            body,
            decorators,
            anchor,
            signature,
            dry_run,
        )

    # Git sync / migration
    def sync_git(self, repo: str, **kwargs: Any) -> Any:
        return _native.sync_git(
            repo,
            kwargs.get("store") or kwargs.get("store_path"),
            kwargs.get("parent_ref", "main"),
            kwargs.get("catch_up", True),
            kwargs.get("source", "watch"),
        )

    def reingest_merged_commits(
        self,
        repo: str,
        branch: str = "main",
        dry_run: bool = True,
        store_path: str | None = None,
    ) -> Any:
        return _native.reingest_merged_commits(repo, branch, dry_run, store_path)

    def backfill_commit_messages(
        self, repo: str, store_path: str | None = None
    ) -> Any:
        return _native.backfill_commit_messages(repo, store_path)

    # Facade state and renderer-shaped APIs with op-shaped Rust wrappers
    def stats(self) -> Any:
        return _native.stats_op()

    def status(self) -> Any:
        return _native.status_op()

    def reindex(
        self,
        with_embeddings: bool = False,
        full: bool = False,
        exclude: list[str] | None = None,
    ) -> Any:
        return _native.reindex_op(str(Path(self._db_path).parents[2]))

    def remove_file(self, file_path: str, lineage: str | None = None) -> Any:
        return _native.remove_file_op(file_path, lineage)

    def plan_replace_body(
        self,
        entity_id: str,
        new_body: str,
        expected_hash: str | None = None,
        dry_run: bool = True,
    ) -> _MutationPlan:
        return _plan(
            "replace_entity_body",
            _native.plan_replace_body_op(entity_id, new_body, expected_hash, dry_run),
        )

    def plan_rename(
        self, entity_id: str, new_name: str, dry_run: bool = True, **_: Any
    ) -> _MutationPlan:
        return _plan("rename_entity", _native.plan_rename_op(entity_id, new_name, dry_run))

    def plan_create_entity(
        self, target_file: str, anchor: str, code: str, dry_run: bool = True
    ) -> _MutationPlan:
        return _plan(
            "create_entity",
            _native.plan_create_entity_op(target_file, anchor, code, dry_run),
        )

    def plan_update_signature(
        self,
        entity_id: str,
        new_signature: str,
        inject_defaults: bool = False,
        dry_run: bool = True,
    ) -> _MutationPlan:
        return _plan(
            "update_signature",
            _native.plan_update_signature_op(
                entity_id, new_signature, inject_defaults, dry_run
            ),
        )

    def apply(self, plan: _MutationPlan) -> _MutationResult:
        """Apply a plan, then sync each affected file into the live graph.

        The disk engine does not own graph synchronization, so this facade
        performs the same follow-up sync the Python handle did. ``status`` is
        only ``Applied`` when those syncs succeed.
        """
        raw = _native.apply_plan_op(plan.id)
        data = raw if isinstance(raw, dict) else {}
        disk = str(data.get("disk_status", ""))
        files_written = list(data.get("files_written", []) or [])
        if disk in {"RejectedStale", "RejectedPolicy"}:
            status = disk
        elif disk == "Installed":
            for file in plan.affected_files:
                _native.update_file(file, None)
            status = "Applied"
        elif disk == "Restored":
            status = "RolledBack"
        else:
            status = "ApplyIncomplete"
        return _MutationResult(
            status=status,
            files_written=files_written,
            syntax_errors=list(data.get("errors", []) or []),
            attempt_id=str(data.get("attempt_id", "") or ""),
            raw=data,
        )

    def call_sites(self, entity_id: str) -> Any:
        return _native.call_sites_op(entity_id)

    def export_annotations(self, path: str | Path | None = None) -> Any:
        return _native.export_annotations_op(str(path) if path is not None else None)

    def import_annotations(
        self, source: Any, on_conflict: str = "error"
    ) -> Any:
        if isinstance(source, Path):
            source = str(source)
        return _native.import_annotations_op(source, on_conflict)

    def open_items(self, scope: str | None = None) -> Any:
        return _native.open_items_op(scope)

    def ingest_status(self) -> Any:
        return _native.ingest_status_op()

    def diagnose(
        self, unresolved: bool = False, low_confidence: bool = False
    ) -> Any:
        return _native.diagnose_op(unresolved, low_confidence)

    def compute_embeddings(
        self,
        model_name: str | None = None,
        batch_size: int = 32,
        recompute: bool = False,
    ) -> Any:
        return _native.compute_embeddings_op(model_name, batch_size, recompute)

    def search_similar(self, query: str, top_k: int = 10) -> Any:
        return _native.search_similar_op(query, top_k)

    def search_hybrid(self, query: str, top_k: int = 10) -> Any:
        return _native.search_hybrid_op(query, top_k, query)


def analyze(
    root: str, create_store: bool = False, exclude: list[str] | None = None
) -> CodeGraph:
    """Index ``root`` with Rust and return a handle to the loaded graph."""
    result = _native.analyze(str(root), create_store, list(exclude or []))
    graph = CodeGraph(str(Path(root) / ".coderadar" / "store" / "coderadar.db"))
    graph.stats_result = result if isinstance(result, dict) else {}
    return graph


__all__ = ["CodeGraph", "analyze"]
