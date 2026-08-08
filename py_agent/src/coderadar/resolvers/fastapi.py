"""FastAPI Framework Resolver (§28.2)

Detects FastAPI projects by scanning for APIRouter imports, FastAPI app
instantiation, and route decorator patterns. Extracts:
- @app.get()/post()/put()/delete() → route nodes + handler edges
- @router.get()/post()/... → router route nodes
- Dependency injection chains: Depends() parameter annotations
- app.include_router(router) → router registration edges
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


class FastAPIResolver(FrameworkResolver):
    """Resolves FastAPI-specific constructs:

    - Route registration: @app.get('/path'), @router.post('/path') → handler edges
    - Router include: app.include_router(router) → module edges
    - Dependency injection: Depends(SomeDep) → dependency edges
    - WebSocket endpoints: @app.websocket('/ws')
    - Middleware: app.add_middleware()
    """

    ROUTE_METHODS = frozenset({
        "get", "post", "put", "delete", "patch", "options", "head",
        "websocket", "api_route", "on_event",
    })

    @property
    def name(self) -> str:
        return "fastapi"

    def detect(self, project_root: Path) -> bool:
        for py_file in project_root.rglob("*.py"):
            if py_file.name.startswith("__"):
                continue
            try:
                content = py_file.read_text(encoding="utf-8")
                if "fastapi" in content.lower():
                    return True
            except (OSError, UnicodeDecodeError):
                pass
        return False

    def claims_reference(self, name: str) -> bool:
        parts = name.rsplit(".", 1)[-1]
        return any(
            kw in parts.lower()
            for kw in ("fastapi", "apirouter", "depends", "query",
                       "path_param", "body_param", "header_param")
        )

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        result = FrameworkExtraction(file_path=file_path)

        try:
            tree = ast.parse(source)
        except SyntaxError:
            return result

        app_name = self._detect_app_name(tree)

        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                # Route decorators
                route_nodes = self._extract_routes(node, app_name, file_path)
                result.nodes.extend(route_nodes)
                for rn in route_nodes:
                    result.edges.append(SyntheticEdge(
                        source_id=rn.id,
                        target_id=f"{file_path}::{node.name}",
                        kind="handles",
                        metadata={
                            "synthesized_by": self.name,
                            "framework": "fastapi",
                            "methods": rn.metadata.get("methods", ["GET"]),
                        },
                    ))

                # Dependency injection from function parameters
                dep_edges = self._extract_dependencies(
                    node, file_path)
                result.edges.extend(dep_edges)

            if isinstance(node, ast.Call):
                # app.include_router()
                router_edges = self._extract_router_include(node, file_path)
                result.edges.extend(router_edges)

        return result

    def _detect_app_name(self, tree: ast.Module) -> Optional[str]:
        """Find the FastAPI app variable name."""
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                if (
                    isinstance(node.value, ast.Call)
                    and self._get_call_name(node.value) == "FastAPI"
                ):
                    for target in node.targets:
                        if isinstance(target, ast.Name):
                            return target.id
        return None

    def _extract_routes(
        self,
        func_node: ast.FunctionDef,
        app_name: Optional[str],
        file_path: str,
    ) -> List[SyntheticNode]:
        """Extract route nodes from function decorators."""
        nodes: List[SyntheticNode] = []

        for decorator in func_node.decorator_list:
            if not isinstance(decorator, ast.Call):
                continue

            decorator_name = self._get_decorator_attr(decorator)
            if decorator_name not in self.ROUTE_METHODS:
                continue

            if not decorator.args:
                continue

            path = self._get_string_value(decorator.args[0])
            if not path:
                continue

            metadata: Dict[str, Any] = {
                "pattern": path,
                "handler": func_node.name,
            }

            if decorator_name == "api_route":
                for kw in decorator.keywords:
                    if kw.arg == "methods":
                        methods = self._get_list_value(kw.value)
                        if methods:
                            metadata["methods"] = methods
                    elif kw.arg == "response_model":
                        model = self._get_name(kw.value)
                        if model:
                            metadata["response_model"] = model
            else:
                metadata["methods"] = [decorator_name.upper()]

            # Extract response_model, status_code, etc.
            for kw in decorator.keywords:
                if kw.arg == "response_model":
                    model = self._get_name(kw.value)
                    if model:
                        metadata["response_model"] = model
                elif kw.arg == "status_code":
                    if isinstance(kw.value, ast.Constant):
                        metadata["status_code"] = kw.value.value

            nodes.append(SyntheticNode(
                id=f"fastapi:route:{file_path}:{path}",
                name=path,
                kind="route",
                file_path=file_path,
                metadata=metadata,
            ))

        return nodes

    def _extract_dependencies(
        self, func_node: ast.FunctionDef | ast.AsyncFunctionDef, file_path: str,
    ) -> List[SyntheticEdge]:
        """Extract dependency injection edges from function parameter defaults.

        Handles: Depends(some_callable) in parameter defaults.
        """
        edges: List[SyntheticEdge] = []

        # args.defaults holds the last N defaults
        defaults = func_node.args.defaults
        num_defaults = len(defaults)
        if num_defaults == 0:
            return edges

        args_with_defaults = func_node.args.args[-num_defaults:]

        for arg, default in zip(args_with_defaults, defaults):
            dep_name = self._find_depends_call(default)
            if dep_name:
                edges.append(SyntheticEdge(
                    source_id=f"{file_path}::{func_node.name}",
                    target_id=f"{file_path}::{dep_name}",
                    kind="depends_on",
                    metadata={
                        "synthesized_by": self.name,
                        "framework": "fastapi",
                        "parameter": arg.arg,
                    },
                ))

        return edges

    def _find_depends_call(self, annotation: ast.expr) -> Optional[str]:
        """Find a Depends() call within an annotation AST node.

        Handles:
          Depends(get_db)
          Annotated[DB, Depends(get_db)]
        """
        for node in ast.walk(annotation):
            if isinstance(node, ast.Call):
                call_name = self._get_call_name(node)
                if call_name == "Depends" and node.args:
                    return self._get_name(node.args[0])
        return None

    def _extract_router_include(
        self, node: ast.Call, file_path: str,
    ) -> List[SyntheticEdge]:
        """Extract edges from app.include_router(router)."""
        edges: List[SyntheticEdge] = []

        if self._get_call_name(node) != "include_router":
            return edges

        if node.args:
            router_name = self._get_name(node.args[0])
            if router_name:
                edges.append(SyntheticEdge(
                    source_id=f"{file_path}::app",
                    target_id=f"router:{router_name}",
                    kind="registers",
                    metadata={
                        "synthesized_by": self.name,
                        "framework": "fastapi",
                    },
                ))

        # Extract prefix/tags kwargs
        for kw in node.keywords:
            if kw.arg == "prefix" and isinstance(kw.value, ast.Constant):
                pass  # metadata

        return edges

    def resolve(
        self, ref_name: str, graph: Any,
    ) -> Optional[Dict[str, Any]]:
        """Resolve FastAPI naming conventions."""
        # FastAPI typically uses explicit imports rather than naming conventions,
        # but we can resolve Depends() targets
        return None

    # ── AST Helpers ─────────────────────────────────────────────────

    @staticmethod
    def _get_call_name(call: ast.Call) -> Optional[str]:
        if isinstance(call.func, ast.Name):
            return call.func.id
        if isinstance(call.func, ast.Attribute):
            return call.func.attr
        return None

    @staticmethod
    def _get_decorator_attr(decorator: ast.Call) -> Optional[str]:
        """Get the attribute name from @app.get or @router.post."""
        if isinstance(decorator.func, ast.Attribute):
            return decorator.func.attr
        if isinstance(decorator.func, ast.Name):
            return decorator.func.id
        return None

    @staticmethod
    def _get_name(node: ast.expr) -> Optional[str]:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            base = FastAPIResolver._get_name(node.value)
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
