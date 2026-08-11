"""CodeRadar v0.5 — ASP.NET Framework Resolver (F.12 Phase 1)

Handles ASP.NET Core attribute-based route registrations for C#.
Produces synthetic route nodes and handler edges.

ASP.NET patterns:
  [HttpGet("/users")]                  → GET /users
  [HttpPost("/users")]                 → POST /users
  [HttpPut("/users/{id}")]             → PUT /users/{id}
  [HttpDelete("/users/{id}")]          → DELETE /users/{id}
  [HttpPatch("/users/{id}")]           → PATCH /users/{id}
  [Route("/api/[controller]")] on class → base path prefix
  [ApiController]                      → marks controller class
  Minimal API: app.MapGet("/users", handler) → .NET 6+
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# Class declaration with optional [Route] base path:
# [ApiController]
# [Route("api/[controller]")]
# public class UsersController : ControllerBase {
_CLASS_DECL_RE = re.compile(
    r'\[ApiController\]\s*\n'
    r'(?:\s*\[Route\s*\(\s*"(.*?)"\s*\)\]\s*\n)?'
    r'(?:public|private|protected|internal)\s+(?:partial\s+)?class\s+(\w+)',
    re.MULTILINE,
)

# HTTP method attribute: [HttpGet("/path")], [HttpPost("/path")], etc.
# Also handles bare [HttpGet] (inherits class base path)
_HTTP_ATTRIBUTE_RE = re.compile(
    r'\[(HttpGet|HttpPost|HttpPut|HttpDelete|HttpPatch|HttpHead|HttpOptions)'
    r'\s*\(\s*"([^"]*)"\s*\)'
    r'|\[(HttpGet|HttpPost|HttpPut|HttpDelete|HttpPatch|HttpHead|HttpOptions)\s*\]',
    re.IGNORECASE,
)

# Minimal API: app.MapGet("/users", handler), app.MapPost(...)
_MINIMAL_API_RE = re.compile(
    r'(?:app|group)\.'
    r'(MapGet|MapPost|MapPut|MapDelete|MapPatch)'
    r'\s*\(\s*"([^"]*)"\s*,\s*'
    r'([^)]+)',
    re.IGNORECASE,
)

# Method declaration: public IActionResult MethodName(
_METHOD_DECL_RE = re.compile(
    r'(?:public|private|protected|internal)\s+(?:async\s+)?'
    r'(?:\w+(?:<[^>]*>)?\s*)?(\w+)\s*\(',
    re.MULTILINE,
)

# Route token replacement: [controller] → controller class name without "Controller"
_ROUTE_TOKEN_RE = re.compile(r'\[controller\]', re.IGNORECASE)

_METHOD_MAP: dict[str, str] = {
    'HttpGet': 'GET', 'HttpPost': 'POST', 'HttpPut': 'PUT',
    'HttpDelete': 'DELETE', 'HttpPatch': 'PATCH',
    'HttpHead': 'HEAD', 'HttpOptions': 'OPTIONS',
    'MapGet': 'GET', 'MapPost': 'POST', 'MapPut': 'PUT',
    'MapDelete': 'DELETE', 'MapPatch': 'PATCH',
}

_HANDLER_DIRS = ['Controllers', 'Controllers/Api', 'Endpoints', 'Handlers']
_SERVICE_DIRS = ['Services', 'Repositories', 'Data', 'Models', 'Middleware']


class AspNetResolver(FrameworkResolver):
    """ASP.NET Core attribute-based and Minimal API route resolver for C#."""

    @property
    def name(self) -> str:
        return "aspnet"

    def detect(self, project_root: Path) -> bool:
        for marker in ('*.csproj', '*.sln'):
            for path in project_root.rglob(marker):
                break
            else:
                continue
            break
        else:
            # Fallback: grep for ASP.NET attributes in .cs files
            for path in list(project_root.rglob('*.cs'))[:200]:
                try:
                    content = path.read_text(encoding="utf-8")
                    if '[ApiController]' in content or '[HttpGet' in content:
                        return True
                except (OSError, UnicodeDecodeError):
                    pass
            return False

        for marker in ('*.csproj', '*.sln'):
            for path in project_root.rglob(marker):
                try:
                    content = path.read_text(encoding="utf-8")
                    if 'Microsoft.NET.Sdk.Web' in content or 'Microsoft.AspNetCore' in content:
                        return True
                except (OSError, UnicodeDecodeError):
                    pass
            break
        return False

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]
        return (
            parts.endswith("Controller")
            or parts.endswith("Service")
            or parts.endswith("Repository")
            or parts.endswith("Handler")
            or parts.endswith("Endpoint")
            or parts.endswith("Middleware")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        if not file_path.endswith('.cs'):
            return FrameworkExtraction(file_path=file_path)

        # ── Find per-class regions ──
        class_regions = list(_find_class_regions(source))

        # ── Pattern 1: [HttpGet] style attributes ──
        for match in _HTTP_ATTRIBUTE_RE.finditer(source):
            # Groups 1+2: with path; Groups 3+4: bare attribute
            ann_name = match.group(1) or match.group(3)
            if not ann_name:
                continue
            path_fragment = match.group(2) or ""
            method = _METHOD_MAP.get(ann_name, 'GET')

            match_pos = match.start()
            class_name, base_path = _class_context(match_pos, class_regions)
            full_path = self._join_paths(base_path, path_fragment, class_name)

            handler_name = _find_handler_method(source, match.end())
            line_no = source[:match_pos].count('\n') + 1
            route_id = f"aspnet:route:{file_path}:{line_no}:{method}:{full_path}"

            nodes.append(SyntheticNode(
                id=route_id, name=f"{method} {full_path}", kind="route",
                file_path=file_path,
                metadata={
                    "language": "csharp", "framework": "aspnet",
                    "method": method, "path": full_path,
                    "line": line_no, "annotation": ann_name, "class": class_name,
                },
            ))
            if handler_name:
                qualified = f"{class_name}.{handler_name}" if class_name else handler_name
                edges.append(SyntheticEdge(
                    source_id=route_id, target_id=qualified, kind="handles",
                    metadata={
                        "synthesizedBy": "aspnet-resolver",
                        "line": line_no, "method": method,
                    },
                ))

        # ── Pattern 2: Minimal API (app.MapGet, etc.) ──
        for match in _MINIMAL_API_RE.finditer(source):
            map_name = match.group(1)
            route_path = match.group(2)
            handler_expr = match.group(3).strip()
            method = _METHOD_MAP.get(map_name, 'GET')

            line_no = source[:match.start()].count('\n') + 1
            route_id = f"aspnet:route:{file_path}:{line_no}:{method}:{route_path}"

            nodes.append(SyntheticNode(
                id=route_id, name=f"{method} {route_path}", kind="route",
                file_path=file_path,
                metadata={
                    "language": "csharp", "framework": "aspnet-minimal",
                    "method": method, "path": route_path,
                    "line": line_no,
                },
            ))

            handler_name = _parse_minimal_handler(handler_expr)
            if handler_name:
                edges.append(SyntheticEdge(
                    source_id=route_id, target_id=handler_name, kind="handles",
                    metadata={
                        "synthesizedBy": "aspnet-resolver",
                        "line": line_no, "method": method,
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

    @staticmethod
    def _join_paths(base: str, fragment: str, class_name: Optional[str] = None) -> str:
        resolved_base = _resolve_route_tokens(base, class_name)
        if not resolved_base:
            return _resolve_route_tokens(fragment, class_name)
        if not fragment:
            return resolved_base
        resolved_frag = _resolve_route_tokens(fragment, class_name)
        if resolved_frag.startswith('/') and resolved_base.endswith('/'):
            return resolved_base + resolved_frag[1:]
        if not resolved_frag.startswith('/') and not resolved_base.endswith('/'):
            return resolved_base + '/' + resolved_frag
        return resolved_base + resolved_frag


# ── Helpers ─────────────────────────────────────────────────────────────────


def _resolve_route_tokens(path: str, class_name: Optional[str]) -> str:
    """Replace [controller] token with class name minus 'Controller' suffix."""
    if not path or not class_name:
        return path
    if '[controller]' in path:
        short_name = class_name
        if short_name.endswith('Controller'):
            short_name = short_name[:-10]
        path = _ROUTE_TOKEN_RE.sub(short_name.lower(), path)
    return path


def _find_class_regions(source: str) -> list[tuple[Optional[str], str, int, int]]:
    """Find all [ApiController] classes and their [Route] base paths."""
    regions: list[tuple[Optional[str], str, int, int]] = []
    for m in _CLASS_DECL_RE.finditer(source):
        base_path = m.group(1) or ""
        class_name = m.group(2)
        start = m.start()
        end = len(source)
        if regions:
            prev = regions[-1]
            regions[-1] = (prev[0], prev[1], prev[2], start)
        regions.append((class_name, base_path, start, end))
    return regions


def _class_context(pos: int, regions: list[tuple[Optional[str], str, int, int]]) -> tuple[Optional[str], str]:
    for name, base, start, end in reversed(regions):
        if start <= pos < end:
            return name, base
    return None, ""


def _find_handler_method(source: str, after_pos: int) -> Optional[str]:
    window = source[after_pos:after_pos + 500]
    m = _METHOD_DECL_RE.search(window)
    return m.group(1) if m else None


def _parse_minimal_handler(expr: str) -> Optional[str]:
    """Parse a minimal API handler expression.

    Patterns:
      GetUsers           → GetUsers
      new { ... }        → None (anonymous)
      async (ctx) => ... → None (lambda)
    """
    expr = expr.strip().rstrip(',); ')
    if expr.startswith('new '):
        return None
    if '=>' in expr:
        return None
    m = re.search(r'(\w+)$', expr)
    return m.group(1) if m else None
