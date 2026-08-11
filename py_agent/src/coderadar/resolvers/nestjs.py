"""CodeRadar v0.5.7 — NestJS Framework Resolver (F.14)

Handles NestJS decorator-based module/controller/service declarations
for TypeScript. Produces synthetic route nodes and dependency edges.

Patterns:
  @Module({ controllers: [UserController], providers: [UserService] })
  @Controller('users')
  @Get(':id'), @Post(), @Put(':id'), @Delete(':id'), @Patch(':id')
  @Injectable()
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# @Module({ controllers: [A, B], providers: [X, Y], imports: [Z] })
_MODULE_RE = re.compile(
    r'@Module\s*\(\s*\{\s*'
    r'(?:(?:controllers|providers|imports|exports)\s*:\s*\[([^\]]*)\]\s*,?\s*)+'
    r'\s*\}\s*\)',
    re.IGNORECASE,
)

# Extract individual class references from a bracket list: [UserController, AdminController]
_CLASS_REF_LIST_RE = re.compile(r'\[([^\]]*)\]')

# @Controller('users'), @Controller()
_CONTROLLER_RE = re.compile(
    r'@Controller\s*(?:\(\s*[\"\']([^\"\']*)[\"\']\s*\))?',
    re.IGNORECASE,
)

# @Get(':id'), @Post(), @Put(':id'), @Delete(':id'), @Patch(':id')
# Also: @Get(), @HttpCode(200), @Header('X')
_HTTP_METHOD_RE = re.compile(
    r'@(Get|Post|Put|Delete|Patch|Head|Options|All)'
    r'\s*\(\s*(?:[\"\']([^\"\']*)[\"\']\s*)?\)',
    re.IGNORECASE,
)

# @Injectable()
_INJECTABLE_RE = re.compile(r'@Injectable\s*\(\s*\)', re.IGNORECASE)

# Class name: export class UserService { or export class UserController {
_CLASS_RE = re.compile(
    r'(?:export\s+)?class\s+(\w+)',
    re.MULTILINE,
)

_METHOD_MAP: dict[str, str] = {
    'Get': 'GET', 'Post': 'POST', 'Put': 'PUT', 'Delete': 'DELETE',
    'Patch': 'PATCH', 'Head': 'HEAD', 'Options': 'OPTIONS', 'All': 'ALL',
}

_HANDLER_DIRS = ['controllers', 'services', 'providers', 'modules', 'guards', 'interceptors']


class NestJSResolver(FrameworkResolver):
    """NestJS @Module / @Controller / @Injectable resolver for TypeScript."""

    @property
    def name(self) -> str:
        return "nestjs"

    def detect(self, project_root: Path) -> bool:
        pkg = project_root / "package.json"
        if pkg.exists():
            try:
                import json
                data = json.loads(pkg.read_text(encoding="utf-8"))
                deps = {**data.get("dependencies", {}), **data.get("devDependencies", {})}
                if "@nestjs/core" in deps:
                    return True
            except (OSError, json.JSONDecodeError, UnicodeDecodeError):
                pass
        for path in list(project_root.rglob('*.ts'))[:200]:
            try:
                if '@Module' in path.read_text(encoding="utf-8"):
                    return True
            except (OSError, UnicodeDecodeError):
                pass
        return False

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]
        return (
            parts.endswith("Controller")
            or parts.endswith("Service")
            or parts.endswith("Module")
            or parts.endswith("Guard")
            or parts.endswith("Interceptor")
            or parts.endswith("Provider")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        if not file_path.endswith('.ts'):
            return FrameworkExtraction(file_path=file_path)

        # ── Find class name ──
        class_match = _CLASS_RE.search(source)
        class_name = class_match.group(1) if class_match else None

        # ── Controller route prefix ──
        controller_match = _CONTROLLER_RE.search(source)
        route_prefix = controller_match.group(1) if controller_match else ""

        # ── HTTP method routes ──
        for match in _HTTP_METHOD_RE.finditer(source):
            method_name = match.group(1)
            path_fragment = match.group(2) or ""
            method = _METHOD_MAP.get(method_name, 'GET')

            full_path = _join_paths(route_prefix, path_fragment.strip())
            effective_path = full_path or "/"

            line_no = source[:match.start()].count('\n') + 1

            nodes.append(SyntheticNode(
                id=f"nestjs:route:{file_path}:{line_no}:{method}:{effective_path}",
                name=f"{method} {effective_path}",
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "typescript",
                    "framework": "nestjs",
                    "method": method,
                    "path": effective_path,
                    "line": line_no,
                    "annotation": f"@{method_name}",
                    "class": class_name,
                },
            ))

            # Find the method name right after the decorator
            handler_match = re.search(
                r'(?:async\s+)?(\w+)\s*\(', source[match.end():match.end() + 200]
            )
            if handler_match and class_name:
                handler_name = handler_match.group(1)
                edges.append(SyntheticEdge(
                    source_id=f"nestjs:route:{file_path}:{line_no}:{method}:{effective_path}",
                    target_id=f"{class_name}.{handler_name}",
                    kind="handles",
                    metadata={
                        "synthesizedBy": "nestjs-resolver",
                        "line": line_no,
                        "method": method,
                    },
                ))

        # ── @Module declarations: extract controller/provider/import edges ──
        for module_match in _MODULE_RE.finditer(source):
            module_text = module_match.group(0)
            line_no = source[:module_match.start()].count('\n') + 1

            # Extract class references from each array
            for array_match in re.finditer(r'(controllers|providers|imports|exports)\s*:\s*\[([^\]]*)\]', module_text):
                rel_kind = array_match.group(1)
                body = array_match.group(2)
                for ref in re.finditer(r'(\w+)', body):
                    ref_name = ref.group(1)
                    edges.append(SyntheticEdge(
                        source_id=f"{file_path}::{class_name or 'module'}",
                        target_id=ref_name,
                        kind=rel_kind.rstrip('s'),  # "controllers" → "controller"
                        metadata={
                            "synthesizedBy": "nestjs-resolver",
                            "line": line_no,
                            "reference": ref_name,
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


def _join_paths(base: str, fragment: str) -> str:
    if not base:
        return fragment
    if not fragment:
        return base
    if fragment.startswith('/') and base.endswith('/'):
        return base + fragment[1:]
    if not fragment.startswith('/') and not base.endswith('/'):
        return base + '/' + fragment
    return base + fragment
