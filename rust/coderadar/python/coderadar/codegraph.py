"""Thin ``CodeGraph`` facade over the Rust-native ops.

The Rust binary is the product. This module keeps the familiar Python
``coderadar.analyze(...)`` handle working by delegating each method to the
linked Rust extension. It deliberately does not reimplement graph logic in
Python; methods whose Python-only shape has no direct Rust equivalent are
left on the original Python package rather than being faked.
"""

from __future__ import annotations

import threading
import time
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

    def batch(self) -> "BatchContext":
        """Return a context manager that folds updates into one report."""
        return BatchContext(self)

    def watch(
        self,
        paths: list[str] | None = None,
        debounce_ms: int | None = None,
        max_file_size_bytes: int | None = None,
    ) -> "Watcher":
        """Return a Rust-backed file watcher bound to this graph."""
        return Watcher(self, paths or ["src/", "tests/"], debounce_ms, max_file_size_bytes)

    def watch_git(self, repo: str | None = None, *, poll_ms: int = 1000) -> "GitWatcher":
        """Return a started git-ref watcher bound to this graph."""
        watcher = GitWatcher(repo or self._root(), graph=self, poll_ms=poll_ms)
        watcher.start()
        return watcher

    def _root(self) -> str:
        """Project root implied by the store path this handle carries."""
        store = Path(self._db_path).resolve()
        return str(store.parent.parent)


def analyze(
    root: str, create_store: bool = False, exclude: list[str] | None = None
) -> CodeGraph:
    """Index ``root`` with Rust and return a handle to the loaded graph."""
    result = _native.analyze(str(root), create_store, list(exclude or []))
    graph = CodeGraph(str(Path(root) / ".coderadar" / "store" / "coderadar.db"))
    graph.stats_result = result if isinstance(result, dict) else {}
    return graph


def load(snapshot: str, root: str | None = None) -> CodeGraph:
    """Cold-start the in-memory graph from a Macrame ledger file."""
    result = _native.load_snapshot_op(snapshot, root)
    graph = CodeGraph(snapshot)
    graph.stats_result = result if isinstance(result, dict) else {}
    return graph


@dataclass
class UpdateReport:
    """One folded batch of file updates, shaped like the Python report."""

    affected_files: list[str] = field(default_factory=list)
    changed_symbols: list[dict] = field(default_factory=list)
    new_unresolved_references: list[dict] = field(default_factory=list)
    newly_resolved_references: list[dict] = field(default_factory=list)
    elapsed_ms: float = 0.0
    parse_quality: str = "Clean"
    parse_errors: int = 0
    fully_applied: bool = True
    epoch_before: int = 0
    epoch_after: int = 0
    blobs_stored: int = 0
    blobs_skipped_oversize: int = 0
    blobs_skipped_excluded: int = 0
    blobs_bytes: int = 0
    source_hash: str = ""
    update_error: str | None = None
    persistence_error: str | None = None
    blob_error: str | None = None
    route_warning: str | None = None


def _field(value: Any, name: str, default: Any) -> Any:
    if isinstance(value, dict):
        return value.get(name, default)
    return getattr(value, name, default)


class BatchContext:
    """Collect ``update_file`` calls and fold them on context exit."""

    def __init__(self, graph: CodeGraph):
        self._graph = graph
        self._files: list[str] = []
        self._contents: list[str | None] = []
        self.result: Any = None

    def __enter__(self) -> "BatchContext":
        return self

    def update_file(self, file_path: str, content: str | None = None) -> None:
        self._files.append(file_path)
        self._contents.append(content)

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> bool:
        if exc_type is not None:
            return False
        if self._files:
            self.result = _native.batch_update_op(self._files, self._contents)
        return False


class Watcher:
    """Rust-polled file watcher that applies real graph updates."""

    def __init__(
        self,
        graph: CodeGraph,
        paths: list[str],
        debounce_ms: int | None = None,
        max_file_size_bytes: int | None = None,
    ):
        try:
            config = _native.config_op()
        except Exception:
            config = {}
        watch = config.get("watch", {}) if isinstance(config, dict) else {}
        self._graph = graph
        self._paths = list(paths)
        self._debounce_ms = (
            watch.get("debounce_ms", 100) if debounce_ms is None else debounce_ms
        )
        self._max_file_size_bytes = (
            watch.get("max_file_size_bytes", 1_048_576)
            if max_file_size_bytes is None
            else max_file_size_bytes
        )
        self._stamps: dict[str, Any] = {}
        self._running = False

    def start(self) -> "Watcher":
        if self._running:
            return self
        result = _native.watch_changes_op(
            self._paths, self._stamps, self._max_file_size_bytes
        )
        self._stamps = result.get("stamps", {}) if isinstance(result, dict) else {}
        self._running = True
        return self

    def stop(self) -> None:
        self._running = False

    def _relative(self, path: str) -> str:
        candidate = Path(path)
        root_attr = getattr(self._graph, "_root", None)
        if root_attr is None:
            return path
        try:
            root = Path(root_attr()).resolve()
            return str(candidate.resolve().relative_to(root)).replace("\\", "/")
        except (ValueError, OSError):
            return path

    def poll_once(self) -> UpdateReport | None:
        if not self._running:
            self.start()
        result = _native.watch_changes_op(
            self._paths, self._stamps, self._max_file_size_bytes
        )
        if not isinstance(result, dict):
            return None
        changed = result.get("changed", []) or []
        self._stamps = result.get("stamps", {}) or {}
        batch: list[tuple[str, str]] = []
        for path in changed:
            kind = "Delete" if not Path(path).exists() else "Modify"
            batch.append((path, kind))
        if not batch:
            return None
        return self._apply(batch)

    def _apply(self, batch: list[tuple[str, str]], echo: bool = False) -> UpdateReport:
        started = time.perf_counter()
        affected: list[str] = []
        symbols: list[dict] = []
        unresolved: list[dict] = []
        resolved: list[dict] = []
        rank = {"Clean": 0, "Partial": 1, "Tainted": 2, "Deferred": 3}
        quality = "Clean"
        errors = 0
        fully = True
        before: Any = None
        after: Any = None
        update_error: str | None = None
        persistence_error: str | None = None
        blob_error: str | None = None
        route_warning: str | None = None
        blobs = [0, 0, 0, 0]
        for file_path, kind in batch:
            if kind == "Delete":
                try:
                    self._graph.remove_file(file_path)
                    affected.append(file_path)
                except Exception as exc:
                    fully = False
                    update_error = str(exc)
                continue
            if kind not in ("Modify", "Any", "AnyContinuous", "Create"):
                continue
            try:
                report = self._graph.update_file(self._relative(file_path))
            except Exception as exc:
                fully = False
                update_error = str(exc)
                continue
            affected.extend(list(_field(report, "affected_files", []) or [file_path]))
            symbols.extend(list(_field(report, "changed_symbols", []) or []))
            unresolved.extend(list(_field(report, "new_unresolved_references", []) or []))
            resolved.extend(list(_field(report, "newly_resolved_references", []) or []))
            errors += int(_field(report, "parse_errors", 0) or 0)
            fully = fully and bool(_field(report, "fully_applied", True))
            candidate = str(_field(report, "parse_quality", "Clean"))
            if rank.get(candidate, 0) > rank.get(quality, 0):
                quality = candidate
            if before is None:
                before = _field(report, "epoch_before", None)
            after = _field(report, "epoch_after", None)
            for name, slot in (
                ("update_error", "update"),
                ("persistence_error", "persistence"),
                ("blob_error", "blob"),
                ("route_warning", "route"),
            ):
                text = _field(report, name, None)
                if text:
                    if slot == "update":
                        update_error = text
                    elif slot == "persistence":
                        persistence_error = text
                    elif slot == "blob":
                        blob_error = text
                    else:
                        route_warning = text
            keys = (
                "blobs_stored",
                "blobs_skipped_oversize",
                "blobs_skipped_excluded",
                "blobs_bytes",
            )
            for index, key in enumerate(keys):
                blobs[index] += int(_field(report, key, 0) or 0)
            if echo:
                print(f"  {file_path} ({candidate})")
        return UpdateReport(
            affected_files=affected,
            changed_symbols=symbols,
            new_unresolved_references=unresolved,
            newly_resolved_references=resolved,
            elapsed_ms=(time.perf_counter() - started) * 1000.0,
            parse_quality=quality,
            parse_errors=errors,
            fully_applied=fully,
            epoch_before=before if before is not None else 0,
            epoch_after=after if after is not None else 0,
            blobs_stored=blobs[0],
            blobs_skipped_oversize=blobs[1],
            blobs_skipped_excluded=blobs[2],
            blobs_bytes=blobs[3],
            update_error=update_error,
            persistence_error=persistence_error,
            blob_error=blob_error,
            route_warning=route_warning,
        )

    def run_forever(self) -> None:
        self.start()
        try:
            while self._running:
                time.sleep(max(50, self._debounce_ms) / 1000.0)
                report = self.poll_once()
                if report is not None:
                    print(f"updated {len(report.affected_files)} file(s)")
        except KeyboardInterrupt:
            self.stop()

    def __enter__(self) -> "Watcher":
        return self.start()

    def __exit__(self, *args: Any) -> None:
        self.stop()

    def __iter__(self) -> "Watcher":
        return self.start()

    def __next__(self) -> UpdateReport:
        while self._running:
            report = self.poll_once()
            if report is not None and report.affected_files:
                return report
        raise StopIteration


class GitWatcher:
    """Poll git HEAD and delegate each move to the Rust sync op."""

    def __init__(
        self,
        repo: str = ".",
        *,
        graph: CodeGraph | None = None,
        store_path: str | None = None,
        poll_ms: int = 1000,
        catch_up: bool = True,
    ):
        self._repo = repo
        self._graph = graph
        self._store_path = store_path
        self._poll_ms = max(50, int(poll_ms))
        self._catch_up = catch_up
        self._head: Any = None
        self.reports: list[Any] = []
        self.last_error: str | None = None
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def poll_once(self) -> Any:
        try:
            head = _native.git_head(self._repo)
            if head == self._head:
                return None
            self._head = head
            if not self._catch_up:
                return None
            report = _native.sync_git_op(
                self._repo,
                self._store_path,
                "main",
                True,
                "watch",
            )
            self.reports.append(report)
            self.last_error = None
            return report
        except Exception as exc:
            self.last_error = str(exc)
            return None

    def start(self) -> "GitWatcher":
        if self._thread is not None:
            return self
        self._head = _native.git_head(self._repo)

        def loop() -> None:
            while not self._stop.wait(self._poll_ms / 1000.0):
                self.poll_once()

        self._thread = threading.Thread(target=loop, daemon=True)
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=self._poll_ms / 1000.0 + 1)
            self._thread = None

    def __enter__(self) -> "GitWatcher":
        return self.start()

    def __exit__(self, *args: Any) -> None:
        self.stop()


def watch(root: str) -> Watcher:
    """Index ``root``, then return a Rust-polled watcher over it."""
    graph = analyze(root)
    return graph.watch([root])


def watch_git(
    repo: str = ".", *, poll_ms: int = 1000, store_path: str | None = None
) -> GitWatcher:
    """Load ``repo``'s store, then return a git-ref watcher."""
    store = store_path or str(Path(repo) / ".coderadar" / "store" / "coderadar.db")
    graph = load(store, repo)
    watcher = GitWatcher(repo, graph=graph, poll_ms=poll_ms)
    watcher.start()
    return watcher


__all__ = [
    "BatchContext",
    "CodeGraph",
    "GitWatcher",
    "UpdateReport",
    "Watcher",
    "analyze",
    "load",
    "watch",
    "watch_git",
]
