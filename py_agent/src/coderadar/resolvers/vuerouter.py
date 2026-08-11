"""CodeRadar v0.5.7 — Vue Router Framework Resolver (F.15)

Handles Vue Router route declarations. Produces synthetic route nodes
and component handler edges.

Patterns:
  createRouter({ routes: [{ path: '/users', component: UserList }] })
  { path: '/users/:id', name: 'user', component: () => import('./User.vue') }
  router.addRoute('admin', { path: '/settings', component: AdminSettings })
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# Route config object: { path: '/users', component: UserList }
# Named imports: component: () => import('./User.vue')
_ROUTE_OBJECT_RE = re.compile(
    r"\{\s*"
    r"(?:name\s*:\s*[\"']\w+[\"']\s*,\s*)?"
    r"path\s*:\s*[\"']([^\"']*?)[\"']"
    r"[^}]*"
    r"component\s*:\s*"
    r"("
    r"\w+"                          # direct component reference
    r"|"
    r"\(\s*\)\s*=>\s*import\s*\([^)]*\)"  # lazy import
    r")",
    re.IGNORECASE,
)

# router.addRoute('parent', { path: '/foo', component: Foo })
_ADD_ROUTE_RE = re.compile(
    r"addRoute\s*\(\s*[\"'](\w*?)[\"']\s*,\s*\{"
    r"\s*path\s*:\s*[\"']([^\"']*?)[\"']"
    r"[^}]*"
    r"component\s*:\s*(\w+)",
    re.IGNORECASE,
)

# createRouter({ routes: [...] })
_CREATE_ROUTER_RE = re.compile(r'createRouter\s*\(', re.IGNORECASE)

# Extract component name from lazy import: () => import('./User.vue')
_LAZY_IMPORT_RE = re.compile(r"['\"/]*([\w-]+)\.vue", re.IGNORECASE)

_HANDLER_DIRS = ['router', 'routes', 'views', 'pages', 'components', 'layouts']


class VueRouterResolver(FrameworkResolver):
    """Vue Router resolver for JavaScript and TypeScript."""

    @property
    def name(self) -> str:
        return "vue-router"

    def detect(self, project_root: Path) -> bool:
        pkg = project_root / "package.json"
        if pkg.exists():
            try:
                import json
                data = json.loads(pkg.read_text(encoding="utf-8"))
                deps = {**data.get("dependencies", {}), **data.get("devDependencies", {})}
                if "vue-router" in deps or "vue" in deps:
                    return True
            except (OSError, json.JSONDecodeError, UnicodeDecodeError):
                pass
        # Fallback: grep for createRouter or routes array
        for ext in ('.js', '.ts', '.vue'):
            for path in list(project_root.rglob(f'*{ext}'))[:200]:
                try:
                    content = path.read_text(encoding="utf-8")
                    if 'createRouter' in content or 'routes:' in content:
                        return True
                except (OSError, UnicodeDecodeError):
                    pass
        return False

    def claims_reference(self, name: str) -> bool:
        return "View" in name or "Page" in name or "Layout" in name

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        ext = Path(file_path).suffix
        if ext not in ('.js', '.ts', '.vue', '.mjs', '.mts'):
            return FrameworkExtraction(file_path=file_path)

        # Only extract from router files
        is_router_file = (
            'createRouter' in source
            or 'router' in file_path.lower()
            or 'routes.' in file_path.lower()
        )

        # ── Route config objects ──
        for match in _ROUTE_OBJECT_RE.finditer(source):
            route_path = match.group(1)
            comp_ref = match.group(2).strip()

            line_no = source[:match.start()].count('\n') + 1

            # Resolve component name
            component_name = _extract_component_name(comp_ref)

            nodes.append(SyntheticNode(
                id=f"vue:route:{file_path}:{line_no}:{route_path}",
                name=route_path,
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "javascript",
                    "framework": "vue-router",
                    "path": route_path,
                    "component": component_name,
                    "line": line_no,
                },
            ))

            if component_name:
                edges.append(SyntheticEdge(
                    source_id=f"vue:route:{file_path}:{line_no}:{route_path}",
                    target_id=component_name,
                    kind="handles",
                    metadata={
                        "synthesizedBy": "vue-router-resolver",
                        "line": line_no,
                        "path": route_path,
                    },
                ))

        # ── router.addRoute calls ──
        for match in _ADD_ROUTE_RE.finditer(source):
            parent = match.group(1)
            route_path = match.group(2)
            component_ref = match.group(3)

            line_no = source[:match.start()].count('\n') + 1
            effective_path = f"{parent}/{route_path}" if parent else route_path

            nodes.append(SyntheticNode(
                id=f"vue:route:{file_path}:{line_no}:{effective_path}",
                name=effective_path,
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "javascript",
                    "framework": "vue-router",
                    "path": effective_path,
                    "component": component_ref,
                    "line": line_no,
                    "parent": parent,
                },
            ))

            if component_ref and component_ref != 'undefined':
                edges.append(SyntheticEdge(
                    source_id=f"vue:route:{file_path}:{line_no}:{effective_path}",
                    target_id=component_ref,
                    kind="handles",
                    metadata={
                        "synthesizedBy": "vue-router-resolver",
                        "line": line_no,
                        "path": effective_path,
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


def _extract_component_name(ref: str) -> Optional[str]:
    """Extract component name from a reference.

    Direct: UserList → UserList
    Lazy: () => import('./User.vue') → User
    """
    ref = ref.strip()
    if ref.startswith('() =>') or ref.startswith('=>'):
        m = _LAZY_IMPORT_RE.search(ref)
        return m.group(1) if m else None
    if ref.startswith('import('):
        m = _LAZY_IMPORT_RE.search(ref)
        return m.group(1) if m else None
    m = re.match(r'(\w+)$', ref)
    return m.group(1) if m else None
