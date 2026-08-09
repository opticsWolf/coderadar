"""CodeRadar v0.5 — Go Framework Resolver (F.8 Phase 1)

Port of CodeGraph's go.ts resolver. Handles Gin, Echo, Fiber, Chi,
and net/http route registrations. Produces synthetic route nodes and
handler edges that tree-sitter can't see.

Based on CodeGraph's go.ts resolver.
Copyright (c) 2024 Colby McHenry — MIT License
<https://github.com/colbymchenry/codegraph>
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import TYPE_CHECKING, Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

if TYPE_CHECKING:
    pass

# ── Route regex ────────────────────────────────────────────────────────────

# <anyVar>.METHOD("/path", handler) — Gin (GET/POST/...), Chi (Get/Post/...),
# net/http (HandleFunc/Handle).  Matches ANY receiver identifier (not just
# router|r|mux|app|e) since real apps route on group variables.
_ROUTE_RE = re.compile(
    r'\b\w+\.(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|'
    r'Get|Post|Put|Patch|Delete|Handle|HandleFunc)'
    r'\s*\(\s*"([^"]+)"\s*,\s*([^)]+)\)'
)

# Go 1.22 net/http mux patterns: mux.HandleFunc("GET /users/{id}", h)
_GO122_METHOD_RE = re.compile(
    r'^(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|CONNECT|TRACE)\s+\S'
)

# Extract last identifier from expr like `pkg.Sub.handler` or `handler`
_TAIL_IDENT_RE = re.compile(r'(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$')

# Framework-conventional directory patterns
_HANDLER_DIRS = ['handler', 'handlers', 'api', 'routes', 'controller', 'controllers']
_SERVICE_DIRS = ['service', 'services', 'repository', 'store', 'pkg']
_MIDDLEWARE_DIRS = ['middleware', 'middlewares']
_MODEL_DIRS = ['model', 'models', 'entity', 'entities', 'domain', 'pkg']

# ── Resolver ────────────────────────────────────────────────────────────────


class GoResolver(FrameworkResolver):
    """Go framework resolver — Gin, Echo, Fiber, Chi, net/http."""

    @property
    def name(self) -> str:
        return "go"

    def detect(self, project_root: Path) -> bool:
        """Detect Go project by go.mod or .go files."""
        if (project_root / "go.mod").exists():
            return True
        return any(project_root.rglob("*.go"))

    def claims_reference(self, name: str) -> bool:
        """Claim Go-idiomatic reference patterns."""
        return (
            name.endswith("Handler")
            or name.startswith("Handle")
            or name.endswith("Service")
            or name.endswith("Repository")
            or name.endswith("Store")
            or name.endswith("Middleware")
            or name.startswith("Auth")
            or name.startswith("Log")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        """Extract route nodes and handler edges from Go source."""
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        if not file_path.endswith('.go'):
            return FrameworkExtraction(file_path=file_path)

        lines = source.split('\n')

        for match in _ROUTE_RE.finditer(source):
            raw_method = match.group(1)
            route_path = match.group(2)
            handler_expr = match.group(3)

            # Gate: path must be URL-shaped or Go-1.22 method-prefixed
            go122_method = _match_go122_method(route_path, raw_method)
            if not route_path.startswith('/') and not go122_method:
                continue

            line_no = source[:match.start()].count('\n') + 1

            # Resolve method and path
            if go122_method:
                method = go122_method
                path = route_path[len(go122_method):].strip()
            elif raw_method in ('Handle', 'HandleFunc'):
                method = 'ANY'
                path = route_path
            else:
                method = raw_method.upper()
                path = route_path

            route_id = f"go:route:{file_path}:{line_no}:{method}:{path}"

            nodes.append(SyntheticNode(
                id=route_id,
                name=f"{method} {path}",
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "go",
                    "framework": "gin/chi/nethttp",
                    "method": method,
                    "path": path,
                    "line": line_no,
                },
            ))

            handler_name = _extract_tail_ident(handler_expr)
            if handler_name:
                edges.append(SyntheticEdge(
                    source_id=route_id,
                    target_id=handler_name,
                    kind="handles",
                    metadata={
                        "synthesizedBy": "go-resolver",
                        "line": line_no,
                    },
                ))

        return FrameworkExtraction(
            file_path=file_path,
            nodes=nodes,
            edges=edges,
        )

    def resolve(
        self, ref_name: str, candidates: List[Dict[str, Any]],
    ) -> Optional[Dict[str, Any]]:
        """Query-time resolution: prefer matches in framework-conventional dirs."""
        if not candidates:
            return None

        # Prefer matches in handler/service/middleware/model directories
        pref_dirs = _HANDLER_DIRS + _SERVICE_DIRS + _MIDDLEWARE_DIRS + _MODEL_DIRS
        for result in candidates:
            fp = result.get("file_path", "")
            for d in pref_dirs:
                if f"/{d}/" in fp:
                    result["confidence"] = 0.85
                    return result

        # Fallback: first match with lower confidence
        result = candidates[0]
        result["confidence"] = 0.65
        return result


# ── Helpers ─────────────────────────────────────────────────────────────────


def _match_go122_method(route_path: str, raw_method: str) -> Optional[str]:
    """Detect Go 1.22 mux pattern: HandleFunc("GET /path", h)."""
    if raw_method not in ('Handle', 'HandleFunc'):
        return None
    m = _GO122_METHOD_RE.match(route_path)
    return m.group(1) if m else None


def _extract_tail_ident(expr: str) -> Optional[str]:
    """Extract last identifier from expression like pkg.Sub.handler or handler."""
    cleaned = re.sub(r'\s+', '', expr.strip()).rstrip('()')
    m = _TAIL_IDENT_RE.search(cleaned)
    return m.group(1) if m else None
