"""Thin ``CodeGraph`` facade over the Rust-native ops.

The Rust binary is the product. This module keeps the familiar Python
``coderadar.analyze(...)`` handle working by delegating each method to the
linked Rust extension. It deliberately does not reimplement graph logic in
Python; methods whose Python-only shape has no direct Rust equivalent are
left on the original Python package rather than being faked.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from . import _native


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
    def update_file(self, file_path: str, content: str | None = None, **_: Any) -> Any:
        return _native.update_file(file_path, content)

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


def analyze(
    root: str, create_store: bool = False, exclude: list[str] | None = None
) -> CodeGraph:
    """Index ``root`` with Rust and return a handle to the loaded graph."""
    result = _native.analyze(str(root), create_store, list(exclude or []))
    graph = CodeGraph(str(Path(root) / ".coderadar" / "store" / "coderadar.db"))
    graph.stats_result = result if isinstance(result, dict) else {}
    return graph


__all__ = ["CodeGraph", "analyze"]
