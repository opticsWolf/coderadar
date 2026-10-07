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

from collections.abc import Callable
from typing import Any

# ── Public API ──────────────────────────────────────────────────────────────


def resolve_reference(
    name: str,
    searcher: Callable[[str, int], list[dict[str, Any]]],
    resolvers: list,
    *,
    limit: int = 5,
) -> list[dict[str, Any]]:
    """Resolve a reference name through framework resolvers.

    Args:
        name: The unresolved reference (e.g., "UserService", "/users").
        searcher: Callable(name, limit) → list of entity dicts from the graph.
        resolvers: List of FrameworkResolver classes to try.
        limit: Maximum search results per resolver.

    Returns:
        List of resolved entities with ``resolved_by`` and ``confidence``.
    """
    results: list[dict[str, Any]] = []

    for resolver_cls in resolvers:
        resolver = resolver_cls()

        if not resolver.claims_reference(name):
            continue

        try:
            candidates = searcher(name, limit)
        except Exception:  # noqa: BLE001, S112 - one bad searcher must not kill resolution
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
    deduped: list[dict[str, Any]] = []
    for r in sorted(results, key=lambda x: x.get("confidence", 0), reverse=True):
        eid = r.get("id", "")
        if eid and eid not in seen:
            seen.add(eid)
            deduped.append(r)

    return deduped[:limit]


def canonical_route_pattern(path: str) -> tuple[str, ...]:
    """Canonicalize a route pattern into comparable segments.

    Frameworks spell parameters differently (`/users/<id>` Flask/Django,
    `/users/:id` Express, `/users/{id}` Rails/Spring) but route the same
    URL. A segment is a parameter when it starts with `<`, `:`, `{` (or
    is `*`); parameters compare equal to every concrete spelling, so the
    done-clause query `/users/:id` finds a real Flask `/users/<id>`.
    """
    segments = []
    for seg in path.strip("/").split("/"):
        if seg[:1] in ("<", ":", "{") or seg == "*":
            segments.append("*")
        else:
            segments.append(seg)
    return tuple(segments)


def route_patterns_match(query: str, candidate: str) -> bool:
    """True when two route spellings route the same URL shape."""
    q, c = canonical_route_pattern(query), canonical_route_pattern(candidate)
    return len(q) == len(c) and all(a == b or "*" in (a, b)
                                     for a, b in zip(q, c))


def resolve_route(
    path: str,
    searcher: Callable[[str, int], list[dict[str, Any]]],
    callees: Callable[[str], list[dict[str, Any]]] | None = None,
    *,
    limit: int = 5,
) -> list[dict[str, Any]]:
    """Resolve a URL path to its handler(s).

    Finds persisted `route`-kind entities (§1.3) whose pattern matches the
    query across framework spellings, then follows the route→handler edge
    (not text search — the handler never contains the route id) to the
    implementing function. Without `callees` (or without persisted routes)
    the answer is honestly empty: route facts live in the graph, not in
    string proximity.
    """
    results: list[dict[str, Any]] = []
    if callees is None:
        return results

    # The entity scorer matches whitespace-separated tokens against names;
    # a raw path ("/users/:id") is one token no pattern contains, so
    # search the static segments ("users") and segment-match below.
    static = [s for s in path.strip("/").split("/")
              if s[:1] not in ("<", ":", "{") and s != "*"]
    try:
        route_candidates = searcher(" ".join(static) or path, limit * 2)
    except Exception:  # noqa: BLE001, S112 - a bad searcher resolves nothing
        return results
    route_nodes = [
        r for r in route_candidates
        if r.get("kind") == "route"
        and route_patterns_match(path, str(r.get("name", "")))
    ]

    for route in route_nodes:
        route_id = route.get("id", "")
        try:
            neighbours = callees(route_id)
        except Exception:  # noqa: BLE001, S112 - one bad edge read resolves nothing
            continue
        for h in neighbours:
            if h.get("kind") in ("function", "method", "struct"):
                h.setdefault("resolved_by", "route-resolution")
                h.setdefault("confidence", 0.9)
                h.setdefault("route", route)
                results.append(h)

    return results[:limit]


# ── Shared Helpers ──────────────────────────────────────────────────────────


def prefer_in_dir(
    candidates: list, name: str, dir_hint: str, *, confidence: float = 0.85,
) -> dict | None:
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
