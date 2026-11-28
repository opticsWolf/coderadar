"""Query-time framework resolution, delegated to the Rust engine."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any


def resolve_reference(
    name: str,
    searcher: Callable[[str, int], list[dict[str, Any]]],
    resolvers: list | None = None,
    *,
    limit: int = 5,
) -> list[dict[str, Any]]:
    """Resolve a reference name through framework resolvers."""
    from .. import resolve_reference as _resolve_reference

    return _resolve_reference(name, searcher, resolvers, limit=limit)


def canonical_route_pattern(path: str) -> tuple[str, ...]:
    """Canonicalize a route pattern into comparable segments."""
    from .. import canonical_route_pattern as _canonical

    return tuple(_canonical(path))


def route_patterns_match(query: str, candidate: str) -> bool:
    """Whether two route spellings route the same URL shape."""
    from .. import route_patterns_match as _match

    return bool(_match(query, candidate))


def resolve_route(
    path: str,
    searcher: Callable[[str, int], list[dict[str, Any]]],
    callees: Callable[[str], list[dict[str, Any]]] | None = None,
    *,
    limit: int = 5,
) -> list[dict[str, Any]]:
    """Resolve a URL path to its handler(s)."""
    from .. import resolve_route as _resolve_route

    return _resolve_route(path, searcher, callees, limit=limit)


def prefer_in_dir(
    candidates: list, name: str, dir_hint: str, *, confidence: float = 0.85
) -> dict | None:
    """Select the best candidate, preferring matches in a specific directory."""
    from .. import prefer_in_dir as _prefer

    return _prefer(candidates, name, dir_hint, confidence=confidence)
