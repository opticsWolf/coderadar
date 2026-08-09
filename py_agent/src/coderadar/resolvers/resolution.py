"""CodeRadar v0.5 — Query-Time Reference Resolution (F.8 Phase 2)

Orchestrates framework resolvers at query time. When an agent asks
"what handles /users?" or "where is UserService?", this module:

1. Dispatches to each resolver's `claims_reference()` to find candidates
2. Calls `resolve()` with search results from the indexed graph
3. Merges results with confidence scores and resolver provenance

Based on CodeGraph's resolution/index.ts orchestration pattern.
Copyright (c) 2024 Colby McHenry — MIT License
<https://github.com/colbymchenry/codegraph>
"""

from __future__ import annotations

from typing import Any, Callable, Dict, List, Optional


# ── Public API ──────────────────────────────────────────────────────────────


def resolve_reference(
    name: str,
    searcher: Callable[[str, int], List[Dict[str, Any]]],
    resolvers: list,
    *,
    limit: int = 5,
) -> List[Dict[str, Any]]:
    """Resolve a reference name through framework resolvers.

    Args:
        name: The unresolved reference (e.g., "UserService", "/users").
        searcher: Callable(name, limit) → list of entity dicts from the graph.
        resolvers: List of FrameworkResolver classes to try.
        limit: Maximum search results per resolver.

    Returns:
        List of resolved entities with ``resolved_by`` and ``confidence``.
    """
    results: List[Dict[str, Any]] = []

    for resolver_cls in resolvers:
        resolver = resolver_cls()

        if not resolver.claims_reference(name):
            continue

        try:
            candidates = searcher(name, limit)
        except Exception:
            continue

        if not candidates:
            continue

        resolved = resolver.resolve(name, candidates)
        if resolved is not None:
            if isinstance(resolved, list):
                for r in resolved:
                    r.setdefault("resolved_by", resolver.name)
                    r.setdefault("confidence", 0.7)
                    results.append(r)
            else:
                resolved.setdefault("resolved_by", resolver.name)
                resolved.setdefault("confidence", 0.7)
                results.append(resolved)

    # Sort by confidence descending, deduplicate by ID
    seen: set[str] = set()
    deduped: List[Dict[str, Any]] = []
    for r in sorted(results, key=lambda x: x.get("confidence", 0), reverse=True):
        eid = r.get("id", "")
        if eid and eid not in seen:
            seen.add(eid)
            deduped.append(r)

    return deduped[:limit]


def resolve_route(
    path: str,
    searcher: Callable[[str, int], List[Dict[str, Any]]],
    *,
    limit: int = 5,
) -> List[Dict[str, Any]]:
    """Resolve a URL path to its handler(s).

    Searches for route nodes matching the path pattern, then follows
    handler edges to find the implementing function.
    """
    results: List[Dict[str, Any]] = []

    route_candidates = searcher(path, limit * 2)
    route_nodes = [
        r for r in route_candidates
        if r.get("kind") == "route" and path in r.get("name", "")
    ]

    for route in route_nodes:
        route_id = route.get("id", "")
        handler_candidates = searcher(route_id, 3)
        for h in handler_candidates:
            if h.get("kind") in ("function", "method", "struct"):
                h.setdefault("resolved_by", "route-resolution")
                h.setdefault("confidence", 0.9)
                h.setdefault("route", route)
                results.append(h)

    return results[:limit]


# ── Shared Helpers ──────────────────────────────────────────────────────────


def prefer_in_dir(
    candidates: list, name: str, dir_hint: str, *, confidence: float = 0.85,
) -> Optional[dict]:
    """Select the best candidate, preferring matches in a specific directory."""
    if not candidates:
        return None
    for c in candidates:
        if c.get("name") == name and dir_hint in c.get("file_path", ""):
            c["confidence"] = confidence
            return c
    for c in candidates:
        if c.get("name") == name:
            c["confidence"] = confidence - 0.15
            return c
    return None
