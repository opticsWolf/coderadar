"""Flask Framework Resolver (§28.2)

Detects Flask projects by scanning for @app.route patterns, extracts
route registration → handler edges, Blueprint registration.
"""

from __future__ import annotations

import ast
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import (
    FrameworkResolver,
    FrameworkExtraction,
    SyntheticNode,
    SyntheticEdge,
)


class FlaskResolver(FrameworkResolver):
    """Resolves Flask-specific constructs:

    - Route registration: @app.route('/path') → handler functions
    - Blueprint registration: app.register_blueprint(bp)
    - Method-based routing: @app.get(), @app.post(), etc.
    - Error handlers: @app.errorhandler(code)
    """

    ROUTE_DECORATORS = frozenset({
        "route", "get", "post", "put", "delete", "patch",
        "before_request", "after_request", "errorhandler",
    })

    @property
    def name(self) -> str:
        return "flask"

    def detect(self, project_root: Path) -> bool:
        # Check for flask import in any Python file
        for py_file in project_root.rglob("*.py"):
            if py_file.name.startswith("__"):
                continue
            try:
                content = py_file.read_text(encoding="utf-8")
                if "flask" in content.lower():
                    return True
            except (OSError, UnicodeDecodeError):
                pass
        return False

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]
        return any(
            kw in parts.lower()
            for kw in ("blueprint", "route", "flask", "current_app", "g")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        result = FrameworkExtraction(file_path=file_path)

        try:
            tree = ast.parse(source)
        except SyntaxError:
            return result

        app_name = self._detect_app_name(tree)

        for node in ast.walk(tree):
            if isinstance(node, ast.FunctionDef):
                route_nodes = self._extract_route_decorators(
                    node, app_name, file_path)
                result.nodes.extend(route_nodes)
                if route_nodes:
                    for rn in route_nodes:
                        handler_id = f"{file_path}::{node.name}"
                        result.edges.append(SyntheticEdge(
                            source_id=rn.id,
                            target_id=handler_id,
                            kind="handles",
                            metadata={
                                "synthesized_by": self.name,
                                "framework": "flask",
                                "methods": rn.metadata.get("methods", ["GET"]),
                            },
                        ))

            # Blueprint registration
            if isinstance(node, ast.Call):
                bp_edges = self._extract_blueprint_register(
                    node, file_path)
                result.edges.extend(bp_edges)

        return result

    def _detect_app_name(self, tree: ast.Module) -> Optional[str]:
        """Find the Flask app variable name."""
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                if (
                    isinstance(node.value, ast.Call)
                    and self._get_call_name(node.value) == "Flask"
                ):
                    for target in node.targets:
                        if isinstance(target, ast.Name):
                            return target.id
        return "app"  # default

    def _extract_route_decorators(
        self,
        func_node: ast.FunctionDef,
        app_name: Optional[str],
        file_path: str,
    ) -> List[SyntheticNode]:
        """Extract route SyntheticNodes from @app.route() decorators."""
        nodes: List[SyntheticNode] = []

        for decorator in func_node.decorator_list:
            # Handle @app.route(...) and @app.get(...) etc.
            if isinstance(decorator, ast.Call):
                decorator_name = self._get_decorator_name(decorator)
                if decorator_name in self.ROUTE_DECORATORS:
                    node = self._parse_route_decorator(
                        decorator, decorator_name, func_node.name, file_path)
                    if node:
                        nodes.append(node)

                # Handle @bp.route(...) for Blueprints
                elif isinstance(decorator.func, ast.Attribute):
                    decorator_name = decorator.func.attr
                    if decorator_name in self.ROUTE_DECORATORS:
                        node = self._parse_route_decorator(
                            decorator, decorator_name, func_node.name, file_path)
                        if node:
                            nodes.append(node)

        return nodes

    def _parse_route_decorator(
        self,
        decorator: ast.Call,
        decorator_name: str,
        handler_name: str,
        file_path: str,
    ) -> Optional[SyntheticNode]:
        """Parse a route decorator into a SyntheticNode."""
        if not decorator.args:
            return None

        route_pattern = self._get_string_value(decorator.args[0])
        if not route_pattern:
            return None

        metadata: Dict[str, Any] = {
            "pattern": route_pattern,
            "handler": handler_name,
        }

        if decorator_name == "route" and len(decorator.args) >= 2:
            methods = self._get_list_value(decorator.args[1])
            if methods:
                metadata["methods"] = methods
        elif decorator_name in ("get", "post", "put", "delete", "patch"):
            metadata["methods"] = [decorator_name.upper()]

        # Extract endpoint name if specified
        for kw in decorator.keywords:
            if kw.arg == "endpoint":
                metadata["endpoint"] = self._get_string_value(kw.value)

        return SyntheticNode(
            id=f"flask:route:{route_pattern}",
            name=route_pattern,
            kind="route",
            file_path=file_path,
            metadata=metadata,
        )

    def _extract_blueprint_register(
        self, node: ast.Call, file_path: str,
    ) -> List[SyntheticEdge]:
        """Extract edges from app.register_blueprint(bp)."""
        edges: List[SyntheticEdge] = []

        if self._get_call_name(node) != "register_blueprint":
            return edges

        if node.args:
            blueprint_name = self._get_name(node.args[0])
            if blueprint_name:
                edges.append(SyntheticEdge(
                    source_id=f"{file_path}::app",
                    target_id=f"{file_path}::{blueprint_name}",
                    kind="registers",
                    metadata={
                        "synthesized_by": self.name,
                        "framework": "flask",
                    },
                ))

        return edges

    def resolve(
        self, ref_name: str, graph: Any,
    ) -> Optional[Dict[str, Any]]:
        """Resolve Flask naming conventions."""
        return None  # Flask uses direct imports, not naming conventions

    # ── AST Helpers ─────────────────────────────────────────────────

    @staticmethod
    def _get_call_name(call: ast.Call) -> Optional[str]:
        if isinstance(call.func, ast.Name):
            return call.func.id
        if isinstance(call.func, ast.Attribute):
            return call.func.attr
        return None

    @staticmethod
    def _get_decorator_name(decorator: ast.Call) -> Optional[str]:
        """Get the function name from a decorator call."""
        if isinstance(decorator.func, ast.Name):
            return decorator.func.id
        if isinstance(decorator.func, ast.Attribute):
            return decorator.func.attr
        return None

    @staticmethod
    def _get_name(node: ast.expr) -> Optional[str]:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            base = FlaskResolver._get_name(node.value)
            return f"{base}.{node.attr}" if base else node.attr
        return None

    @staticmethod
    def _get_string_value(node: ast.expr) -> Optional[str]:
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            return node.value
        return None

    @staticmethod
    def _get_list_value(node: ast.expr) -> Optional[List[str]]:
        if isinstance(node, ast.List):
            return [
                e.value
                for e in node.elts
                if isinstance(e, ast.Constant) and isinstance(e.value, str)
            ]
        return None
