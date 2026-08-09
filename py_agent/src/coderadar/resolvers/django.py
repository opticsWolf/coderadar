"""Django Framework Resolver (§28.2)

Detects Django projects (manage.py), extracts URL routing from urls.py
patterns, resolves Model/View/Form naming conventions.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from .base import (
    FrameworkResolver,
    FrameworkExtraction,
    SyntheticNode,
    SyntheticEdge,
)


class DjangoResolver(FrameworkResolver):
    """Resolves Django-specific constructs:

    - URL routing: path(), re_path(), url() → route nodes + handler edges
    - Model references: *Model → models.py classes
    - View references: *View → views.py functions
    - Form references: *Form → forms.py classes
    - Admin registration: admin.site.register()
    """

    ROUTE_CALLS = frozenset({"path", "re_path", "url"})
    MODEL_SUFFIX = "Model"
    VIEW_SUFFIX = "View"
    FORM_SUFFIX = "Form"

    @property
    def name(self) -> str:
        return "django"

    def detect(self, project_root: Path) -> bool:
        return (project_root / "manage.py").exists()

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]  # bare class name
        return (
            parts.endswith(self.MODEL_SUFFIX)
            or parts.endswith(self.VIEW_SUFFIX)
            or parts.endswith(self.FORM_SUFFIX)
            or name in ("objects", "DoesNotExist", "MultipleObjectsReturned")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        result = FrameworkExtraction(file_path=file_path)

        try:
            tree = ast.parse(source)
        except SyntaxError:
            return result

        for node in ast.walk(tree):
            # URL patterns in urlpatterns lists
            if isinstance(node, ast.Assign):
                for target in node.targets:
                    if isinstance(target, ast.Name) and target.id == "urlpatterns":
                        result.nodes.extend(self._extract_routes(node.value))
                        result.edges.extend(self._extract_route_edges(
                            node.value, file_path))

            # admin.site.register(Model)
            if isinstance(node, ast.Call):
                result.edges.extend(self._extract_admin_register(node, file_path))
                # DRF router.register(prefix, ViewSet)
                result.edges.extend(self._extract_drf_router(node, file_path))

        return result

    def _extract_routes(self, value: ast.expr) -> List[SyntheticNode]:
        """Extract SyntheticNode for each path()/re_path() call."""
        nodes: List[SyntheticNode] = []
        if not isinstance(value, (ast.List, ast.Tuple)):
            return nodes

        for elem in value.elts:
            if isinstance(elem, ast.Call):
                call_name = self._get_call_name(elem)
                if call_name in self.ROUTE_CALLS:
                    route = self._parse_route_call(elem)
                    if route:
                        nodes.append(route)

        return nodes

    def _extract_route_edges(
        self, value: ast.expr, file_path: str,
    ) -> List[SyntheticEdge]:
        """Create edges from route nodes to their view handlers."""
        edges: List[SyntheticEdge] = []
        if not isinstance(value, (ast.List, ast.Tuple)):
            return edges

        for idx, elem in enumerate(value.elts):
            if isinstance(elem, ast.Call):
                call_name = self._get_call_name(elem)
                if call_name in self.ROUTE_CALLS:
                    handler = self._get_route_handler(elem)
                    if handler:
                        route_id = f"{file_path}::__url_pattern_{idx}"
                        handler_id = f"{file_path}::{handler}"
                        edges.append(SyntheticEdge(
                            source_id=route_id,
                            target_id=handler_id,
                            kind="handles",
                            metadata={
                                "synthesized_by": self.name,
                                "framework": "django",
                            },
                        ))

        return edges

    def _extract_admin_register(
        self, node: ast.Call, file_path: str,
    ) -> List[SyntheticEdge]:
        """Extract edges from admin.site.register(Model)."""
        edges: List[SyntheticEdge] = []

        # Check for admin.site.register pattern
        if (
            isinstance(node.func, ast.Attribute)
            and node.func.attr == "register"
            and isinstance(node.func.value, ast.Attribute)
            and node.func.value.attr == "site"
            and isinstance(node.func.value.value, ast.Name)
            and node.func.value.value.id == "admin"
        ):
            if node.args:
                model_name = self._get_name(node.args[0])
                if model_name:
                    edges.append(SyntheticEdge(
                        source_id=f"{file_path}::admin",
                        target_id=f"{file_path}::{model_name}",
                        kind="registers",
                        metadata={
                            "synthesized_by": self.name,
                            "framework": "django",
                            "admin_model": model_name,
                        },
                    ))

        return edges

    def _extract_drf_router(
        self, node: ast.Call, file_path: str,
    ) -> List[SyntheticEdge]:
        """Extract edges from DRF router.register(prefix, ViewSet).

        Pattern: router.register(r'users/', UserViewSet, basename='user')
        """
        edges: List[SyntheticEdge] = []
        call_name = self._get_call_name(node)
        if call_name != "register":
            return edges
        # Ensure it's called on a router object (router.register, not just .register)
        if not isinstance(node.func, ast.Attribute):
            return edges
        if len(node.args) < 2:
            return edges
        prefix = self._get_string_value(node.args[0])
        viewset = self._get_name(node.args[1])
        if prefix and viewset:
            edges.append(SyntheticEdge(
                source_id=f"django:route:{prefix}",
                target_id=f"{file_path}::{viewset}",
                kind="handles",
                metadata={
                    "synthesized_by": self.name,
                    "framework": "django",
                    "pattern": prefix,
                    "viewset": viewset,
                },
            ))
        return edges

    def _parse_route_call(self, call: ast.Call) -> Optional[SyntheticNode]:
        """Parse a path()/re_path() call into a SyntheticNode."""
        if not call.args:
            return None

        route_pattern = self._get_string_value(call.args[0])
        if not route_pattern:
            return None

        metadata: Dict[str, Any] = {"pattern": route_pattern}

        for kw in call.keywords:
            if kw.arg == "name":
                metadata["route_name"] = self._get_string_value(kw.value)
            elif kw.arg == "kwargs":
                metadata["default_kwargs"] = "..."

        return SyntheticNode(
            id=f"django:route:{route_pattern}",
            name=route_pattern,
            kind="route",
            file_path="__django_urls__",
            metadata=metadata,
        )

    def _get_route_handler(self, call: ast.Call) -> Optional[str]:
        """Get the view handler name from a path() call.

        Strips .as_view() suffix for Django class-based views.
        """
        if len(call.args) >= 2:
            handler = self._get_name(call.args[1])
            return self._strip_as_view(handler)
        for kw in call.keywords:
            if kw.arg == "view":
                handler = self._get_name(kw.value)
                return self._strip_as_view(handler)
        return None

    @staticmethod
    def _strip_as_view(handler: Optional[str]) -> Optional[str]:
        """Remove .as_view() suffix from Django CBV handlers.

        views.MyView.as_view → views.MyView
        """
        if handler and handler.endswith(".as_view"):
            return handler[:-len(".as_view")]
        return handler

    def resolve(
        self, ref_name: str, graph: Any,
    ) -> Optional[Dict[str, Any]]:
        """Resolve Django naming conventions.

        *Model → search models.py for matching class
        *View → search views.py for matching function/class
        """
        suffix = None
        search_dir = None

        parts = ref_name.rsplit(".", 1)[-1]
        if parts.endswith(self.MODEL_SUFFIX):
            suffix = self.MODEL_SUFFIX
            search_dir = "models"
        elif parts.endswith(self.VIEW_SUFFIX):
            suffix = self.VIEW_SUFFIX
            search_dir = "views"
        elif parts.endswith(self.FORM_SUFFIX):
            suffix = self.FORM_SUFFIX
            search_dir = "forms"

        if not suffix:
            return None

        base_name = parts[:-len(suffix)] if parts != suffix else parts
        return self._search_project(graph, base_name, search_dir)

    def _search_project(
        self, graph: Any, name: str, module_hint: str,
    ) -> Optional[Dict[str, Any]]:
        """Search the graph for a matching entity."""
        # Try exact match
        try:
            from coderadar._core import search_entities
            results = search_entities(name, top_k=10)
            for r in results:
                r_name = r.get("name", "")
                if r_name == name or r_name.endswith(name):
                    if module_hint and module_hint in r.get("file_path", ""):
                        return r
            # Fallback: first result of correct kind
            for r in results:
                if r.get("kind") in ("class", "function"):
                    return r
        except ImportError:
            pass
        return None

    # ── AST Helpers ─────────────────────────────────────────────────

    @staticmethod
    def _get_call_name(call: ast.Call) -> Optional[str]:
        """Get the function name from a call."""
        if isinstance(call.func, ast.Name):
            return call.func.id
        if isinstance(call.func, ast.Attribute):
            return call.func.attr
        return None

    @staticmethod
    def _get_name(node: ast.expr) -> Optional[str]:
        """Get a dotted name from an AST expression."""
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            base = DjangoResolver._get_name(node.value)
            return f"{base}.{node.attr}" if base else node.attr
        if isinstance(node, ast.Call):
            return DjangoResolver._get_name(node.func)
        return None

    @staticmethod
    def _get_string_value(node: ast.expr) -> Optional[str]:
        """Get a string literal value."""
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            return node.value
        return None
