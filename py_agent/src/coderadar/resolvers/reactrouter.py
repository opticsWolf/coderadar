"""CodeRadar v0.5.7 — React Router Framework Resolver (F.16)

Handles React Router JSX route declarations and v6 data router configs.
Produces synthetic route nodes and component handler edges.

Patterns:
  <Route path="/users" element={<UserList />} />
  <Route path="/users/:id" component={UserDetail} />
  createBrowserRouter([{ path: '/', element: <Root /> }])
  <Link to="/users">Users</Link>
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# JSX: <Route path="/users" element={<UserList />} />
#      <Route path="/users/:id" component={UserDetail} />
_ROUTE_JSX_RE = re.compile(
    r'<Route\s+'
    r'(?:index\s+)?'
    r'(?:path\s*=\s*["\']([^"\']*?)["\']\s*)?'
    r'(?:element\s*=\s*\{<(\w+)\s*/\s*>|component\s*=\s*\{(\w+)\})',
    re.IGNORECASE,
)

# Data router: createBrowserRouter([{ path: '/', element: <Root /> }])
_DATA_ROUTER_RE = re.compile(
    r"path\s*:\s*[\"']([^\"']*?)[\"']\s*,"
    r"\s*(?:element|component)\s*:\s*(?:<(\w+)\s*/\s*>|\{\s*(\w+)\s*\})",
    re.IGNORECASE,
)

# <Link to="/users"> or <Link to={`/users/${id}`}>
_LINK_RE = re.compile(
    r'<Link\s+.*?to\s*=\s*["\'`]([^"\'`]*?)["\'`]',
    re.IGNORECASE,
)

# <NavLink to="/users" ...>
_NAVLINK_RE = re.compile(
    r'<NavLink\s+.*?to\s*=\s*["\'`]([^"\'`]*?)["\'`]',
    re.IGNORECASE,
)

# <Navigate to="/login" />
_NAVIGATE_RE = re.compile(
    r'<Navigate\s+.*?to\s*=\s*["\']([^"\']*?)["\']',
    re.IGNORECASE,
)

# outlet / layout
_OUTLET_RE = re.compile(r'<Outlet\s*/\s*>', re.IGNORECASE)

_HANDLER_DIRS = ['routes', 'pages', 'components', 'layouts', 'views', 'app']


class ReactRouterResolver(FrameworkResolver):
    """React Router resolver for JavaScript, TypeScript, and JSX/TSX."""

    @property
    def name(self) -> str:
        return "react-router"

    def detect(self, project_root: Path) -> bool:
        pkg = project_root / "package.json"
        if pkg.exists():
            try:
                import json
                data = json.loads(pkg.read_text(encoding="utf-8"))
                deps = {**data.get("dependencies", {}), **data.get("devDependencies", {})}
                if "react-router-dom" in deps or "react-router" in deps:
                    return True
            except (OSError, json.JSONDecodeError, UnicodeDecodeError):
                pass
        for ext in ('.jsx', '.tsx', '.js', '.ts'):
            for path in list(project_root.rglob(f'*{ext}'))[:200]:
                try:
                    content = path.read_text(encoding="utf-8")
                    if '<Route ' in content or 'createBrowserRouter' in content:
                        return True
                except (OSError, UnicodeDecodeError):
                    pass
        return False

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]
        return (
            parts.endswith("Page")
            or parts.endswith("View")
            or parts.endswith("Route")
            or parts.endswith("Layout")
            or parts.endswith("Screen")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        ext = Path(file_path).suffix
        if ext not in ('.jsx', '.tsx', '.js', '.ts'):
            return FrameworkExtraction(file_path=file_path)

        # ── JSX <Route> declarations ──
        for match in _ROUTE_JSX_RE.finditer(source):
            route_path = match.group(1) or "/"  # index route defaults to parent path
            component = match.group(2) or match.group(3)

            line_no = source[:match.start()].count('\n') + 1

            nodes.append(SyntheticNode(
                id=f"react:route:{file_path}:{line_no}:{route_path}",
                name=route_path,
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "javascript",
                    "framework": "react-router",
                    "path": route_path,
                    "component": component,
                    "line": line_no,
                },
            ))

            if component:
                edges.append(SyntheticEdge(
                    source_id=f"react:route:{file_path}:{line_no}:{route_path}",
                    target_id=component,
                    kind="handles",
                    metadata={
                        "synthesizedBy": "react-router-resolver",
                        "line": line_no,
                        "path": route_path,
                    },
                ))

        # ── Data router objects ──
        for match in _DATA_ROUTER_RE.finditer(source):
            route_path = match.group(1)
            component = match.group(2) or match.group(3)

            line_no = source[:match.start()].count('\n') + 1

            nodes.append(SyntheticNode(
                id=f"react:route:{file_path}:{line_no}:{route_path}",
                name=route_path,
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "javascript",
                    "framework": "react-router-v6",
                    "path": route_path,
                    "component": component,
                    "line": line_no,
                },
            ))

            if component:
                edges.append(SyntheticEdge(
                    source_id=f"react:route:{file_path}:{line_no}:{route_path}",
                    target_id=component,
                    kind="handles",
                    metadata={
                        "synthesizedBy": "react-router-resolver",
                        "line": line_no,
                        "path": route_path,
                    },
                ))

        # ── Navigation links (cross-references) ──
        for match in _LINK_RE.finditer(source):
            target = match.group(1)
            line_no = source[:match.start()].count('\n') + 1
            nodes.append(SyntheticNode(
                id=f"react:link:{file_path}:{line_no}:{target}",
                name=target,
                kind="navigation",
                file_path=file_path,
                metadata={
                    "language": "javascript",
                    "framework": "react-router",
                    "target": target,
                    "line": line_no,
                    "element": "Link",
                },
            ))

        for match in _NAVLINK_RE.finditer(source):
            target = match.group(1)
            line_no = source[:match.start()].count('\n') + 1
            nodes.append(SyntheticNode(
                id=f"react:navlink:{file_path}:{line_no}:{target}",
                name=target,
                kind="navigation",
                file_path=file_path,
                metadata={
                    "language": "javascript",
                    "framework": "react-router",
                    "target": target,
                    "line": line_no,
                    "element": "NavLink",
                },
            ))

        return FrameworkExtraction(file_path=file_path, nodes=nodes, edges=edges)

    def resolve(
        self, ref_name: str, candidates: List[Dict[str, Any]],
    ) -> Optional[Dict[str, Any]]:
        if not candidates:
            return None
        for result in candidates:
            fp = result.get("file_path", "")
            for d in _HANDLER_DIRS:
                if f"/{d}/" in fp or f"\\{d}\\" in fp:
                    result["confidence"] = 0.85
                    return result
        result = candidates[0]
        result["confidence"] = 0.65
        return result
