"""Framework route extraction (§1.3, DR-10).

Runs the framework resolvers (Flask, Django, FastAPI, Express, ...) after
analysis and persists what they find: route *nodes* become canonical
`route`-kind concepts and route→handler *edges* become `synthetic:*` ledger
rows, so a cold start restores them and `resolve \"/users/:id\"` reaches the
handler in a fresh session from the store.

Two entry points, matching the two build paths:

- :func:`run_framework_extraction` — whole-tree, scope `\"full\"`; called
  from `coderadar.analyze` after the Rust walk (so `init`, `reindex --full`
  and every full analyze persist current routes).
- :func:`refresh_framework_file` — one file, scope = its rel path; called
  from `CodeGraph.update_file` (and the drop path, where the missing file
  simply extracts nothing and its ghost routes retire).

Id prefixes: resolvers emit bare ids (`flask:route:/users/<id>`); the hook
prefixes the defining file (`app.py::flask:route:/users/<id>`) so
`file_path_of`, canonical lookup and per-file retirement work by
construction, and ids are stable across root spellings (files are threaded
root-relative posix throughout).

Detection cost: project-level `detect()` walks the tree, so the incremental
path cannot afford it per file. `refresh_framework_file` extracts with the
cached-active resolvers plus untried ones as trials; a trial that emits is
confirmed by one `detect()` (then cached active for the root) or rejected
(then cached inactive for that file). Steady state is one AST parse per
resolver per updated file and zero walks; framework adoption via a new file
is picked up on its first update, not just on full analyzes.
"""

from __future__ import annotations

from pathlib import Path

# Resolver classes confirmed for a resolved root by a full detect or a
# confirmed trial. Invalidated wholesale by `run_framework_extraction`
# (every full analyze re-detects); the incremental path only adds.
_ACTIVE: dict[str, list] = {}
# (root, rel-file) pairs whose trial emission a `detect()` rejected — a
# coincidental decorator, not a framework. Per-file (not per-root) so a
# later file adopting the framework still triggers confirmation.
_INACTIVE_FILES: set[tuple[str, str]] = set()


def _ledger_synthetic_edge_kind(resolver_name: str, edge_kind: str) -> str:
    """Return a stable, application-namespaced Macrame edge kind."""
    return f"synthetic:{resolver_name.strip().lower()}:{edge_kind.strip().lower()}"


def _active_classes(root: str) -> list:
    return list(_ACTIVE.get(root, []))


def _detecting_classes(root_path: Path) -> list:
    from coderadar.resolvers import ALL_RESOLVERS

    active = []
    for cls in ALL_RESOLVERS:
        try:
            if cls().detect(root_path):
                active.append(cls)
        except Exception:  # noqa: BLE001, S112 - one broken detector must not sink extraction
            continue
    return active


def _extract_file(resolver, rel_posix: str, source: str):
    """Extract one file with one resolver; never raises."""
    try:
        return resolver.extract(rel_posix, source)
    except Exception:  # noqa: BLE001 - one bad file must not sink extraction
        return None


def _register(nodes, edges, scope: str) -> dict:
    """Persist nodes+edges via the Rust hook; memory-only without a graph."""
    try:
        from coderadar._core import register_synthetic_routes as _register_rs
    except ImportError:
        return {"routes_upserted": 0, "routes_retired": 0, "pairs_retired": 0}
    try:
        report = _register_rs(nodes, edges, scope)
        return {
            "routes_upserted": int(report.get("routes_upserted", 0)),
            "routes_retired": int(report.get("routes_retired", 0)),
            "pairs_retired": int(report.get("pairs_retired", 0)),
        }
    except RuntimeError:
        # No graph loaded (storeless library use without analyze) — the
        # extraction counts still report; nothing persists.
        return {"routes_upserted": 0, "routes_retired": 0, "pairs_retired": 0}


def _collect(extractions: list, rel_posix: str) -> tuple[list, list, int, int]:
    """Remap one file's extractions to file-prefixed ids.

    Returns `(nodes, edges, route_count, handler_count)` where nodes are
    `(id, pattern, file, handler, methods, framework)` and edges are
    `(source, target, kind)`.
    """
    nodes: list[tuple] = []
    edges: list[tuple] = []
    routes = handlers = 0
    for resolver_name, extraction in extractions:
        if extraction is None:
            continue
        remap = {}
        for node in extraction.nodes:
            new_id = f"{rel_posix}::{node.id}"
            remap[node.id] = new_id
            nodes.append((
                new_id,
                node.name,
                rel_posix,
                "",  # handler filled from the edge below
                list(node.metadata.get("methods", [])),
                resolver_name,
            ))
            routes += 1
        for edge in extraction.edges:
            source = remap.get(edge.source_id, edge.source_id)
            target = edge.target_id
            edges.append((
                source, target,
                _ledger_synthetic_edge_kind(resolver_name, edge.kind),
            ))
            handlers += 1
    # Attach each route's handler from its edge (the edge is the fact;
    # the node carries it for `route_to_dict` without a second lookup).
    handler_of = {}
    for source, target, _kind in edges:
        handler_of.setdefault(source, target)
    nodes = [
        (i, p, f, handler_of.get(i, h), m, fw) for (i, p, f, h, m, fw) in nodes
    ]
    return nodes, edges, routes, handlers


def run_framework_extraction(root: str | Path) -> dict:
    """Whole-tree framework extraction (the `analyze` path, scope `\"full\"`)."""
    from coderadar.excludes import iter_project_files

    root_path = Path(root)
    root_key = str(root_path.resolve())
    results: dict = {
        "routes": 0, "handlers": 0, "frameworks": [],
        "routes_upserted": 0, "routes_retired": 0, "pairs_retired": 0,
    }
    active_classes = _detecting_classes(root_path)
    _ACTIVE[root_key] = active_classes
    if not active_classes:
        # No framework — but ghosts from a removed framework still retire.
        report = _register([], [], "full")
        results.update(report)
        return results
    resolvers = [cls() for cls in active_classes]
    results["frameworks"] = sorted(r.name for r in resolvers)
    all_nodes: list[tuple] = []
    all_edges: list[tuple] = []
    for py_file in iter_project_files(root_path, suffixes=(".py",)):
        if py_file.name.startswith("__"):
            continue
        try:
            source = py_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        try:
            rel = py_file.relative_to(root_path).as_posix()
        except ValueError:
            continue
        extractions = [
            (r.name, _extract_file(r, rel, source)) for r in resolvers
        ]
        nodes, edges, routes, handlers = _collect(extractions, rel)
        results["routes"] += routes
        results["handlers"] += handlers
        all_nodes.extend(nodes)
        all_edges.extend(edges)
    report = _register(all_nodes, all_edges, "full")
    results.update(report)
    return results


def refresh_framework_file(root: str | Path, rel_posix: str,
                            content: str | None = None) -> dict:
    """One-file framework extraction (the `update_file` path).

    A missing file (the drop path) extracts nothing — its ghost routes
    retire via the file-scoped diff.
    """
    from coderadar.resolvers import ALL_RESOLVERS

    root_path = Path(root)
    root_key = str(root_path.resolve())
    results: dict = {
        "routes": 0, "handlers": 0, "frameworks": [],
        "routes_upserted": 0, "routes_retired": 0, "pairs_retired": 0,
    }
    source: str | None = content
    if source is None:
        try:
            source = (root_path / rel_posix).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            source = None
    active = _active_classes(root_key)
    trial = [c for c in ALL_RESOLVERS
             if c not in _ACTIVE.get(root_key, [])
             and (root_key, rel_posix) not in _INACTIVE_FILES]
    resolvers = [c() for c in active] + [c() for c in trial]
    if source is None or not resolvers:
        if source is None:
            # Dropped file: nothing to extract, ghosts retire below.
            report = _register([], [], rel_posix)
            results.update(report)
        return results
    extractions = [(r.name, _extract_file(r, rel_posix, source))
                   for r in resolvers]
    # Confirm or reject trial emissions with one project detect each.
    confirmed: list = []
    for cls, (_name, extraction) in zip([c for c in active] + trial,
                                       extractions):
        if cls in active or extraction is None:
            continue
        if not extraction.nodes and not extraction.edges:
            continue
        try:
            if cls().detect(root_path):
                confirmed.append(cls)
            else:
                _INACTIVE_FILES.add((root_key, rel_posix))
        except Exception:  # noqa: BLE001 - detection must not sink the update
            _INACTIVE_FILES.add((root_key, rel_posix))
    if confirmed:
        _ACTIVE[root_key] = active + confirmed
    # Drop rejected trials' extractions (coincidental decorators).
    confirmed_names = {c().name for c in active} | {c().name for c in confirmed}
    kept = [(name, ext) for (name, ext) in extractions
            if name in confirmed_names]
    nodes, edges, routes, handlers = _collect(kept, rel_posix)
    results["routes"] = routes
    results["handlers"] = handlers
    results["frameworks"] = sorted({c().name for c in active + confirmed})
    report = _register(nodes, edges, rel_posix)
    results.update(report)
    return results
