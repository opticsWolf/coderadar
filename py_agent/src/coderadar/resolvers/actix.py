"""CodeRadar v0.5 — Rust/Actix Framework Resolver (F.8 Phase 1)

Port of CodeGraph's framework resolver pattern. Handles Actix-web
route registrations: attribute macros (#[get], #[post], etc.) and
programmatic .route() calls. Produces synthetic route nodes and
handler edges that tree-sitter can't see.

Based on CodeGraph's go.ts resolver pattern.
Copyright (c) 2024 Colby McHenry — MIT License
<https://github.com/colbymchenry/codegraph>
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode

# ── Route regex ────────────────────────────────────────────────────────────

# Attribute macro: #[get("/path")], #[post("/path")], #[put(...)], etc.
# Followed by an async fn (possibly with attributes/visibility in between)
_ATTR_ROUTE_RE = re.compile(
    r'#\[(get|post|put|patch|delete|head|options|route)\s*\(\s*"([^"]+)"[^)]*\)\]'
    r'\s*(?:#\[[^\]]*\][\s\n]*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)',
    re.MULTILINE,
)

# Programmatic: .route("/path", web::get().to(handler))
# Also: web::resource("/path").route(web::get().to(handler))
_ROUTE_CALL_RE = re.compile(
    r'\.route\s*\(\s*"([^"]+)"\s*,\s*web::(\w+)\(\)\.to\((\w+)\)\s*\)'
)

_RESOURCE_ROUTE_RE = re.compile(
    r'web::resource\s*\(\s*"([^"]+)"\s*\)'
    r'\s*\.route\s*\(\s*web::(\w+)\(\)\.to\((\w+)\)\s*\)'
)

# ── Resolver ────────────────────────────────────────────────────────────────


class RustActixResolver(FrameworkResolver):
    """Rust Actix-web framework resolver."""

    @property
    def name(self) -> str:
        return "actix"

    def detect(self, project_root: Path) -> bool:
        """Detect Rust/Actix project by Cargo.toml + actix-web dependency."""
        cargo_toml = project_root / "Cargo.toml"
        if cargo_toml.exists():
            try:
                content = cargo_toml.read_text()
                return "actix-web" in content or "actix_web" in content
            except OSError:
                pass
        return False

    def claims_reference(self, name: str) -> bool:
        """Claim Rust/Actix handler and service patterns."""
        return (
            name.endswith("_handler")
            or name.endswith("_service")
            or name.startswith("handle_")
            or name.startswith("get_")
            or name.startswith("post_")
            or name.startswith("put_")
            or name.startswith("delete_")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        """Extract route nodes and handler edges from Rust source."""
        nodes: List[SyntheticNode] = []
        edges: List[SyntheticEdge] = []

        if not file_path.endswith('.rs'):
            return FrameworkExtraction(file_path=file_path)

        lines = source.split('\n')

        # Pattern 1: Attribute macros
        for match in _ATTR_ROUTE_RE.finditer(source):
            method = match.group(1)
            path = match.group(2)
            handler = match.group(3)

            if method == "route":
                # #[route("/path", method="GET")] — extract method from args
                method = "ANY"

            line_no = source[:match.start()].count('\n') + 1
            route_id = f"actix:route:{file_path}:{line_no}:{method.upper()}:{path}"

            nodes.append(SyntheticNode(
                id=route_id,
                name=f"{method.upper()} {path}",
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "rust",
                    "framework": "actix-web",
                    "method": method.upper(),
                    "path": path,
                    "line": line_no,
                },
            ))

            edges.append(SyntheticEdge(
                source_id=route_id,
                target_id=handler,
                kind="handles",
                metadata={
                    "synthesizedBy": "actix-resolver",
                    "line": line_no,
                },
            ))

        # Pattern 2: Programmatic .route() calls
        for match in _ROUTE_CALL_RE.finditer(source):
            path = match.group(1)
            method = match.group(2)
            handler = match.group(3)

            line_no = source[:match.start()].count('\n') + 1
            route_id = f"actix:route:{file_path}:{line_no}:{method.upper()}:{path}"

            nodes.append(SyntheticNode(
                id=route_id,
                name=f"{method.upper()} {path}",
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "rust",
                    "framework": "actix-web",
                    "method": method.upper(),
                    "path": path,
                    "line": line_no,
                },
            ))

            edges.append(SyntheticEdge(
                source_id=route_id,
                target_id=handler,
                kind="handles",
                metadata={
                    "synthesizedBy": "actix-resolver",
                    "line": line_no,
                },
            ))

        # Pattern 3: web::resource() chains
        for match in _RESOURCE_ROUTE_RE.finditer(source):
            path = match.group(1)
            method = match.group(2)
            handler = match.group(3)

            line_no = source[:match.start()].count('\n') + 1
            route_id = f"actix:route:{file_path}:{line_no}:{method.upper()}:{path}"

            nodes.append(SyntheticNode(
                id=route_id,
                name=f"{method.upper()} {path}",
                kind="route",
                file_path=file_path,
                metadata={
                    "language": "rust",
                    "framework": "actix-web",
                    "method": method.upper(),
                    "path": path,
                    "line": line_no,
                },
            ))

            edges.append(SyntheticEdge(
                source_id=route_id,
                target_id=handler,
                kind="handles",
                metadata={
                    "synthesizedBy": "actix-resolver",
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
        """Query-time resolution: prefer Actix handler patterns."""
        if not candidates:
            return None

        # Prefer matches in handler/service directories
        pref_dirs = ['handler', 'handlers', 'routes', 'services', 'service']
        for result in candidates:
            fp = result.get("file_path", "")
            for d in pref_dirs:
                if f"/{d}/" in fp or f"\\{d}\\" in fp:
                    result["confidence"] = 0.85
                    return result

        result = candidates[0]
        result["confidence"] = 0.65
        return result
