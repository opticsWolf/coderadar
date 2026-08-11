"""CodeRadar v0.5 — Spring Boot Framework Resolver (F.10 Phase 1)

Handles Spring Boot @RestController route annotations for Java.
Produces synthetic route nodes and handler edges.

Spring Boot patterns:
  @GetMapping("/users")                 → GET /users
  @GetMapping                           → inherits class base path
  @PostMapping(value = "/users")        → POST /users
  @RequestMapping(path="/api", method=GET) → GET /api
  @RequestMapping("/api") on class (no method=) → class base prefix
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Regex patterns ──────────────────────────────────────────────────────────

# HTTP mapping annotation with optional path in parens:
#   @GetMapping
#   @GetMapping()
#   @GetMapping("/path")
#   @GetMapping(value="/path")
# Capture groups: (1) annotation name, (2) path (empty string if bare/empty parens)
_HTTP_ANNOTATION_RE = re.compile(
    r'@(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|HeadMapping|OptionsMapping)'
    r'(?:\(\s*(?:value\s*=\s*)?["\']([^"\']*)["\']\s*\)'
    r'|\(\s*\))'
    r'|@(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|HeadMapping|OptionsMapping)'
    r'(?!\s*\()',
    re.IGNORECASE,
)

# @RequestMapping on a method WITH method= parameter.
_REQUEST_MAPPING_RE = re.compile(
    r"@RequestMapping\s*\(\s*"
    r"(?:path\s*=\s*)?[\"']([^\"']*)[\"']"
    r"[^)]*\bmethod\s*=\s*(?:RequestMethod\.)?(\w+)"
    r"[^)]*\)",
    re.IGNORECASE | re.DOTALL,
)

# Class declaration: public class FooController {
_CLASS_DECL_RE = re.compile(
    r'@RestController\s*\n'
    r'(?:\s*@RequestMapping\s*\(\s*(?:value\s*=\s*)?["\']([^"\']+)["\']\s*\)\s*\n)?'
    r'(?:public|private|protected)\s+class\s+(\w+)',
    re.MULTILINE,
)

_METHOD_DECL_RE = re.compile(
    r'(?:public|private|protected)\s+(?:\w+(?:<[^>]*>)?\s+)?(\w+)\s*\(',
    re.MULTILINE,
)

_METHOD_MAP: dict[str, str] = {
    'GetMapping': 'GET', 'PostMapping': 'POST', 'PutMapping': 'PUT',
    'DeleteMapping': 'DELETE', 'PatchMapping': 'PATCH',
    'HeadMapping': 'HEAD', 'OptionsMapping': 'OPTIONS',
    'GET': 'GET', 'POST': 'POST', 'PUT': 'PUT', 'DELETE': 'DELETE',
    'PATCH': 'PATCH', 'HEAD': 'HEAD', 'OPTIONS': 'OPTIONS',
}

_HANDLER_DIRS = ['controller', 'controllers', 'api', 'handler', 'resource', 'endpoint']
_SERVICE_DIRS = ['service', 'services', 'repository', 'component', 'manager', 'facade']


class SpringBootResolver(FrameworkResolver):
    """Spring Boot @RestController / @RequestMapping resolver for Java."""

    @property
    def name(self) -> str:
        return "springboot"

    def detect(self, project_root: Path) -> bool:
        for marker in ('pom.xml', 'build.gradle', 'build.gradle.kts'):
            p = project_root / marker
            if p.exists():
                try:
                    if 'spring' in p.read_text(encoding="utf-8").lower():
                        return True
                except (OSError, UnicodeDecodeError):
                    pass
        for path in list(project_root.rglob('*.java'))[:200]:
            try:
                content = path.read_text(encoding="utf-8")
                if '@SpringBootApplication' in content or '@RestController' in content:
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
            or parts.endswith("Component")
            or parts.endswith("Handler")
            or parts.endswith("Resource")
            or parts.endswith("Facade")
            or "ServiceImpl" in parts
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        if not file_path.endswith('.java'):
            return FrameworkExtraction(file_path=file_path)

        # ── Find all per-class regions and their base paths ──
        # Build a list of (class_name, base_path, start_pos, end_pos)
        class_regions = list(_find_class_regions(source))

        # ── Pattern 1: @GetMapping-style annotations ──
        for match in _HTTP_ANNOTATION_RE.finditer(source):
            ann_name = match.group(1) or match.group(3)
            if not ann_name:
                continue
            path_fragment = match.group(2) or ""
            method = _METHOD_MAP.get(ann_name, 'GET')

            match_pos = match.start()
            class_name, base_path = _class_context(match_pos, class_regions)
            full_path = self._join_paths(base_path, path_fragment) if path_fragment else base_path

            handler_name = _find_handler_method(source, match.end())
            line_no = source[:match_pos].count('\n') + 1
            route_id = f"spring:route:{file_path}:{line_no}:{method}:{full_path}"

            nodes.append(SyntheticNode(
                id=route_id, name=f"{method} {full_path}", kind="route",
                file_path=file_path,
                metadata={
                    "language": "java", "framework": "spring-boot",
                    "method": method, "path": full_path,
                    "line": line_no, "annotation": ann_name, "class": class_name,
                },
            ))
            if handler_name:
                qualified = f"{class_name}.{handler_name}" if class_name else handler_name
                edges.append(SyntheticEdge(
                    source_id=route_id, target_id=qualified, kind="handles",
                    metadata={"synthesizedBy": "springboot-resolver", "line": line_no, "method": method},
                ))

        # ── Pattern 2: @RequestMapping with explicit method= ──
        for match in _REQUEST_MAPPING_RE.finditer(source):
            path_fragment = match.group(1)
            method_str = match.group(2)
            method = _METHOD_MAP.get(method_str.upper(), 'ANY') if method_str else 'ANY'

            match_pos = match.start()
            class_name, base_path = _class_context(match_pos, class_regions)
            full_path = self._join_paths(base_path, path_fragment)

            handler_name = _find_handler_method(source, match.end())
            line_no = source[:match_pos].count('\n') + 1
            route_id = f"spring:route:{file_path}:{line_no}:{method}:{full_path}"

            nodes.append(SyntheticNode(
                id=route_id, name=f"{method} {full_path}", kind="route",
                file_path=file_path,
                metadata={
                    "language": "java", "framework": "spring-boot",
                    "method": method, "path": full_path,
                    "line": line_no, "annotation": "RequestMapping", "class": class_name,
                },
            ))
            if handler_name:
                qualified = f"{class_name}.{handler_name}" if class_name else handler_name
                edges.append(SyntheticEdge(
                    source_id=route_id, target_id=qualified, kind="handles",
                    metadata={"synthesizedBy": "springboot-resolver", "line": line_no, "method": method},
                ))

        return FrameworkExtraction(file_path=file_path, nodes=nodes, edges=edges)

    def resolve(self, ref_name: str, candidates: List[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
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


# ── Helpers ─────────────────────────────────────────────────────────────────


def _find_class_regions(source: str) -> list[tuple[Optional[str], str, int, int]]:
    """Find all @RestController classes and their @RequestMapping base paths.

    Returns: list of (class_name, base_path, start_pos, end_pos)
    """
    regions: list[tuple[Optional[str], str, int, int]] = []
    prev_end = 0

    for m in _CLASS_DECL_RE.finditer(source):
        base_path = m.group(1) or ""
        class_name = m.group(2)
        start = m.start()
        # Find end of this class (next class declaration or end of file)
        end = len(source)

        # Mark region end where previous region ended
        if regions:
            prev = regions[-1]
            regions[-1] = (prev[0], prev[1], prev[2], start)

        regions.append((class_name, base_path, start, end))

    return regions


def _class_context(pos: int, regions: list[tuple[Optional[str], str, int, int]]) -> tuple[Optional[str], str]:
    """Find the class context for a given position in source."""
    for name, base, start, end in reversed(regions):
        if start <= pos < end:
            return name, base
    return None, ""


def _find_handler_method(source: str, after_pos: int) -> Optional[str]:
    window = source[after_pos:after_pos + 500]
    m = _METHOD_DECL_RE.search(window)
    return m.group(1) if m else None
