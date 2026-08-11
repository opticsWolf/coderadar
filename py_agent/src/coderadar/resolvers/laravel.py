"""CodeRadar v0.5 — Laravel Framework Resolver (F.11 Phase 1)

Handles Laravel route registrations for PHP.
Produces synthetic route nodes and handler edges.

Laravel patterns:
  Route::get('/users', [UserController::class, 'index'])
  Route::post('/users', [UserController::class, 'store'])
  Route::resource('users', UserController::class)
  Route::group(['prefix' => 'admin'], function () { ... })
  Route::match(['GET', 'POST'], '/users', handler)
  Route::any('/health', handler)
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# Route::method('path', handlerExpr)
_ROUTE_RE = re.compile(
    r"Route::(get|post|put|patch|delete|options|any|match)"
    r"\s*\(\s*"
    r"[\"']([^\"']*?)[\"']\s*,\s*"
    r"("
    r"\[[^\]]*\]"
    r"|'[^']*'"
    r"|\S+\([^)]*\)"
    r"|\$?\w+\s*::\s*class"
    r"|\S+"
    r")",
    re.IGNORECASE,
)

# Route::resource('users', UserController::class)
_RESOURCE_RE = re.compile(
    r"Route::resource\s*\(\s*"
    r"[\"']([^\"']*?)[\"']\s*,\s*"
    r"(\w+(?:\s*::\s*class)?)"
    r"\s*[^)]*\)",
    re.IGNORECASE,
)

# Route::group(['prefix' => 'admin'], function() { ... })
# Explicitly matches 'prefix' or "prefix" with => arrow.
_GROUP_RE = re.compile(
    r"Route::group\s*\(\s*\["
    r".*?[\"']prefix[\"']\s*=>\s*[\"']([^\"']+?)[\"']"
    r"[^)]*\)",
    re.IGNORECASE | re.DOTALL,
)

# [Controller::class, 'method'] array destructuring
_CONTROLLER_ARRAY_RE = re.compile(
    r"(\w+)\s*::\s*class\s*,\s*[\"'](\w+)[\"']",
    re.IGNORECASE,
)

# 'Controller@method' string syntax
_CONTROLLER_STRING_RE = re.compile(
    r"[\"'](\w+)@(\w+)[\"']",
    re.IGNORECASE,
)

# ['uses' => 'Controller@method'] array syntax
_USES_ARRAY_RE = re.compile(
    r"\buses\b[^'\"]*[\"']([^\"']+)[\"']",
    re.IGNORECASE,
)

# ::class reference: UserController::class
_CLASS_REF_RE = re.compile(
    r"(\w+)\s*::\s*class",
    re.IGNORECASE,
)

# Bare handler identifier: getUserById
_BARE_HANDLER_RE = re.compile(r"^\$?(\w+)$")

_METHOD_MAP: dict[str, str] = {
    'get': 'GET', 'post': 'POST', 'put': 'PUT', 'patch': 'PATCH',
    'delete': 'DELETE', 'options': 'OPTIONS', 'any': 'ANY', 'match': 'MATCH',
}

_HANDLER_DIRS = ['Http/Controllers', 'Controllers', 'routes', 'handlers', 'api']
_SERVICE_DIRS = ['Services', 'Repositories', 'Models', 'Providers', 'Middleware']


class LaravelResolver(FrameworkResolver):
    """Laravel Route:: facade resolver for PHP."""

    @property
    def name(self) -> str:
        return "laravel"

    def detect(self, project_root: Path) -> bool:
        p = project_root / "composer.json"
        if p.exists():
            try:
                import json
                data = json.loads(p.read_text(encoding="utf-8"))
                deps = {**data.get("require", {}), **data.get("require-dev", {})}
                if any("laravel" in k.lower() for k in deps):
                    return True
            except (OSError, json.JSONDecodeError, UnicodeDecodeError):
                pass
        for path in list(project_root.rglob('*.php'))[:200]:
            try:
                if 'Route::' in path.read_text(encoding="utf-8"):
                    return True
            except (OSError, UnicodeDecodeError):
                pass
        return False

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]
        return (
            parts.endswith("Controller")
            or parts.endswith("Service")
            or parts.endswith("Repository")
            or parts.endswith("Provider")
            or parts.endswith("Middleware")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        if not file_path.endswith('.php'):
            return FrameworkExtraction(file_path=file_path)

        # ── Extract group prefixes ──
        group_prefixes: dict[int, str] = {}
        for match in _GROUP_RE.finditer(source):
            prefix = match.group(1)
            group_prefixes[match.end()] = prefix

        def _group_prefix_for(pos: int) -> str:
            best = ""
            for end, prefix in group_prefixes.items():
                if pos > end:
                    best = prefix
            return best

        # ── Pattern 1: Route::method ──
        for match in _ROUTE_RE.finditer(source):
            raw_method = match.group(1).lower()
            route_path = match.group(2)
            handler_expr = match.group(3).strip()

            method = _METHOD_MAP.get(raw_method, 'GET')
            group_prefix = _group_prefix_for(match.start())
            full_path = group_prefix + route_path if group_prefix else route_path

            line_no = source[:match.start()].count('\n') + 1
            route_id = f"laravel:route:{file_path}:{line_no}:{method}:{full_path}"

            nodes.append(SyntheticNode(
                id=route_id,
                name=f"{method} {full_path}",
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "php", "framework": "laravel",
                    "method": method, "path": full_path,
                    "line": line_no, "raw_method": raw_method,
                },
            ))

            handler_info = _parse_handler(handler_expr)
            if handler_info:
                edges.append(SyntheticEdge(
                    source_id=route_id, target_id=handler_info,
                    kind="handles",
                    metadata={
                        "synthesizedBy": "laravel-resolver",
                        "line": line_no, "method": method,
                    },
                ))

        # ── Pattern 2: Route::resource ──
        for match in _RESOURCE_RE.finditer(source):
            resource_name = match.group(1)
            controller_ref = match.group(2).strip()
            controller = _parse_class_ref(controller_ref) or controller_ref

            line_no = source[:match.start()].count('\n') + 1
            group_prefix = _group_prefix_for(match.start())
            base = group_prefix + '/' + resource_name if group_prefix else '/' + resource_name

            resource_routes = [
                ('GET', base, 'index'),
                ('GET', f"{base}/{{id}}", 'show'),
                ('POST', base, 'store'),
                ('PUT', f"{base}/{{id}}", 'update'),
                ('DELETE', f"{base}/{{id}}", 'destroy'),
            ]

            for method, path, action in resource_routes:
                rid = f"laravel:route:{file_path}:{line_no}:{method}:{path}"
                nodes.append(SyntheticNode(
                    id=rid, name=f"{method} {path}", kind="route",
                    file_path=file_path,
                    metadata={
                        "language": "php", "framework": "laravel",
                        "method": method, "path": path,
                        "line": line_no, "resource": resource_name,
                    },
                ))
                edges.append(SyntheticEdge(
                    source_id=rid,
                    target_id=f"{controller}.{action}" if controller else action,
                    kind="handles",
                    metadata={
                        "synthesizedBy": "laravel-resolver",
                        "line": line_no, "method": method,
                        "resource": resource_name,
                    },
                ))

        return FrameworkExtraction(file_path=file_path, nodes=nodes, edges=edges)

    def resolve(
        self, ref_name: str, candidates: List[Dict[str, Any]],
    ) -> Optional[Dict[str, Any]]:
        if not candidates:
            return None
        pref_dirs = _HANDLER_DIRS + _SERVICE_DIRS
        for result in candidates:
            fp = result.get("file_path", "")
            for d in pref_dirs:
                if f"/{d}/" in fp or f"\\{d}\\" in fp:
                    result["confidence"] = 0.85
                    return result
        result = candidates[0]
        result["confidence"] = 0.65
        return result


# ── Helpers ─────────────────────────────────────────────────────────────────


def _parse_handler(expr: str) -> Optional[str]:
    expr = expr.rstrip(',)] ')

    m = _CONTROLLER_ARRAY_RE.search(expr)
    if m:
        return f"{m.group(1)}.{m.group(2)}"

    m = _CONTROLLER_STRING_RE.search(expr)
    if m:
        return f"{m.group(1)}.{m.group(2)}"

    if 'uses' in expr:
        m = _USES_ARRAY_RE.search(expr)
        if m:
            ref = m.group(1)
            m2 = _CONTROLLER_STRING_RE.search(f"'{ref}'")
            if m2:
                return f"{m2.group(1)}.{m2.group(2)}"
            return ref

    m = _CLASS_REF_RE.search(expr)
    if m:
        return m.group(1)

    m = _BARE_HANDLER_RE.search(expr)
    if m:
        name = m.group(1)
        if name.lower() in ('array', 'function', 'null', 'true', 'false', 'this'):
            return None
        return name

    return None


def _parse_class_ref(expr: str) -> Optional[str]:
    m = _CLASS_REF_RE.search(expr)
    return m.group(1) if m else None
