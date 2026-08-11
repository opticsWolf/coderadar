"""CodeRadar v0.5 — Express Framework Resolver (F.9 Phase 1)

Handles Express.js route registrations for JavaScript and TypeScript.
Produces synthetic route nodes and handler edges that tree-sitter can't see.

Express patterns:
  app.get('/path', handler)          — direct method call
  router.post('/path', handler)      — router instance
  app.use('/path', middleware)       — middleware / mounted router
  app.route('/path').get(handler)    — chained route builder
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# HTTP method call on any receiver: app.get('/path', handler) or router.post(...)
# Matches across newlines. Captures: (1) receiver, (2) method, (3) path, (4) handler
_ROUTE_CALL_RE = re.compile(
    r'(\w+)\.'
    r'(get|post|put|delete|patch|all|use|head|options)'
    r'\s*\(\s*'
    r'(?:[\'"`]([^\'"`]*?)[\'"`]\s*,)?'    # optional path (not present for bare .use(fn))
    r'\s*([^)]*?)'                            # handler expression (greedy but minimal)
    r'\s*\)',
    re.IGNORECASE | re.DOTALL,
)

# Chained route builder: app.route('/path').get(h).post(h)...
# Captures: (1) receiver, (2) path, then repeated (3) method + (4) handler
_ROUTE_CHAIN_RE = re.compile(
    r'(\w+)\.route\s*\(\s*[\'"`]([^\'"`]*?)[\'"`]\s*\)'
    r'((?:\s*\.\s*(?:get|post|put|delete|patch|all|use|head|options)\s*\([^)]*\)\s*)+)',
    re.IGNORECASE,
)

# Individual chained method: .get(handler), .post(h1, h2), etc.
_CHAIN_CALL_RE = re.compile(
    r'\.\s*(get|post|put|delete|patch|all|use|head|options)\s*\(\s*([^)]*?)\s*\)',
    re.IGNORECASE,
)

# Extract last identifier from expression: foo.bar.baz → baz
_TAIL_IDENT_RE = re.compile(r'(?:\.|^)([A-Za-z_$][A-Za-z0-9_$]*)\s*$')

# Framework-conventional directory patterns
_HANDLER_DIRS = ['routes', 'controllers', 'handlers', 'api', 'middleware', 'router']
_SERVICE_DIRS = ['services', 'models', 'utils', 'lib', 'helpers']

# HTTP method normalization
_METHOD_MAP: dict[str, str] = {
    'get': 'GET', 'post': 'POST', 'put': 'PUT', 'delete': 'DELETE',
    'patch': 'PATCH', 'head': 'HEAD', 'options': 'OPTIONS',
}
_HTTP_METHODS = frozenset({'get', 'post', 'put', 'delete', 'patch', 'all', 'use', 'head', 'options'})

# ── Resolver ────────────────────────────────────────────────────────────────


class ExpressResolver(FrameworkResolver):
    """Express.js / Connect framework resolver for JavaScript and TypeScript."""

    @property
    def name(self) -> str:
        return "express"

    def detect(self, project_root: Path) -> bool:
        """Detect Express project by package.json or JS/TS files with express imports."""
        pkg = project_root / "package.json"
        if pkg.exists():
            try:
                import json
                data = json.loads(pkg.read_text(encoding="utf-8"))
                deps = {**data.get("dependencies", {}), **data.get("devDependencies", {})}
                if "express" in deps:
                    return True
            except (OSError, json.JSONDecodeError, UnicodeDecodeError):
                pass
        # Fallback: grep for express require/import in JS/TS files
        for ext in ('.js', '.ts', '.mjs', '.cjs', '.mts', '.cts'):
            for path in list(project_root.rglob(f'*{ext}'))[:200]:
                try:
                    content = path.read_text(encoding="utf-8")
                    if "express" in content and ('require(' in content or 'from ' in content):
                        return True
                except (OSError, UnicodeDecodeError):
                    pass
        return False

    def claims_reference(self, name: str) -> bool:
        """Claim Express-idiomatic reference patterns."""
        parts = name.rsplit(".", 1)[-1]
        return (
            parts.endswith("Controller")
            or parts.endswith("Handler")
            or parts.endswith("Middleware")
            or parts.endswith("Service")
            or parts.endswith("Router")
            or parts.startswith("handle")
            or parts.startswith("get")
            or parts.startswith("post")
            or parts.startswith("put")
            or parts.startswith("delete")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        """Extract route nodes and handler edges from JS/TS source."""
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        ext = Path(file_path).suffix
        if ext not in ('.js', '.ts', '.mjs', '.cjs', '.mts', '.cts'):
            return FrameworkExtraction(file_path=file_path)

        # ── Pattern 1: chained route builder ──
        for chain_match in _ROUTE_CHAIN_RE.finditer(source):
            receiver = chain_match.group(1)
            route_path = chain_match.group(2)
            chain_body = chain_match.group(3)

            for call_match in _CHAIN_CALL_RE.finditer(chain_body):
                raw_method = call_match.group(1).lower()
                handler_exprs_str = call_match.group(2)
                method = _METHOD_MAP.get(raw_method, raw_method.upper())

                # Split comma-separated handlers (Express allows multiple)
                handler_exprs = _split_handlers(handler_exprs_str)

                line_no = source[:chain_match.start()].count('\n') + 1
                route_id = f"express:route:{file_path}:{line_no}:{method}:{route_path}"

                nodes.append(SyntheticNode(
                    id=route_id,
                    name=f"{method} {route_path}",
                    kind="route",
                    file_path=file_path,
                    metadata={
                        "language": "javascript",
                        "framework": "express",
                        "method": method,
                        "path": route_path,
                        "line": line_no,
                        "receiver": receiver,
                    },
                ))

                for handler_expr in handler_exprs:
                    handler_name = _extract_tail_ident(handler_expr)
                    if handler_name and handler_name not in _HTTP_METHODS:
                        edges.append(SyntheticEdge(
                            source_id=route_id,
                            target_id=handler_name,
                            kind="handles",
                            metadata={
                                "synthesizedBy": "express-resolver",
                                "line": line_no,
                                "method": method,
                            },
                        ))

        # ── Pattern 2: direct method calls ──
        for match in _ROUTE_CALL_RE.finditer(source):
            receiver = match.group(1)
            raw_method = match.group(2).lower()
            path_str = match.group(3)
            handler_exprs_str = match.group(4)

            # Skip route builder calls (handled by Pattern 1)
            if raw_method == 'route':
                continue

            method = _METHOD_MAP.get(raw_method, raw_method.upper())
            handler_exprs = _split_handlers(handler_exprs_str)

            line_no = source[:match.start()].count('\n') + 1

            # app.use() without path: just middleware — still create a node
            effective_path = path_str or (f"/*" if raw_method == 'use' else "/")
            route_id = f"express:route:{file_path}:{line_no}:{method}:{effective_path}"

            nodes.append(SyntheticNode(
                id=route_id,
                name=f"{method} {effective_path}",
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "javascript",
                    "framework": "express",
                    "method": method,
                    "path": effective_path,
                    "line": line_no,
                    "receiver": receiver,
                },
            ))

            for handler_expr in handler_exprs:
                handler_name = _extract_tail_ident(handler_expr)
                if handler_name and handler_name not in _HTTP_METHODS:
                    edges.append(SyntheticEdge(
                        source_id=route_id,
                        target_id=handler_name,
                        kind="handles",
                        metadata={
                            "synthesizedBy": "express-resolver",
                            "line": line_no,
                            "method": method,
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
        """Query-time resolution: prefer matches in route/controller/handler directories."""
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


def _split_handlers(exprs_str: str) -> List[str]:
    """Split comma-separated handler expressions respecting nesting depth.

    Handles: handler, (req, res) => {...}, userController.list
    """
    if not exprs_str or not exprs_str.strip():
        return []

    parts: List[str] = []
    depth = 0
    current: List[str] = []
    for ch in exprs_str:
        if ch in '({[':
            depth += 1
        elif ch in ')}]':
            depth -= 1
        if ch == ',' and depth == 0:
            parts.append(''.join(current).strip())
            current = []
        else:
            current.append(ch)
    parts.append(''.join(current).strip())
    return [p for p in parts if p]


def _extract_tail_ident(expr: str) -> Optional[str]:
    """Extract last identifier from expression.

    Examples:
        usersController.list → list
        listUsers             → listUsers
        (req, res) => {...}   → None (arrow function)
        require('./auth')     → auth (stripped extension)
    """
    cleaned = expr.strip()

    # Arrow function: (params) => {...} — skip entirely
    if '=>' in cleaned:
        # Check if it's a wrapped arrow: ((req, res) => {...})
        # The handler is inline, not a named reference
        return None

    # Remove wrapping parens for destructured params: (async (req, res) => ...)
    if cleaned.startswith('(') and cleaned.endswith(')') and '=>' in cleaned:
        return None

    # Remove trailing .bind(this) or similar
    cleaned = re.sub(r'\.bind\s*\([^)]*\)\s*$', '', cleaned)

    m = _TAIL_IDENT_RE.search(cleaned)
    return m.group(1) if m else None
