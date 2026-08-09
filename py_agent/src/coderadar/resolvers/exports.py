"""CodeRadar v3.6 — Module Export Resolution (§6.3a, F.4)

Handles `__all__` detection for Python modules. Supports:
- `__all__ = ["foo", "bar"]` (literal list)
- `__all__ += [...]` (augmented assignment)
- `__all__.extend([...])` (method call)

Based on CodeGraph's export resolution pattern (python.rs).
Copyright (c) 2024 Colby McHenry — MIT License
<https://github.com/colbymchenry/codegraph>
"""

from __future__ import annotations

import ast
from typing import List, Optional


def extract_all_exports(source: str) -> Optional[List[str]]:
    """Extract __all__ exports from Python source.

    Returns None if __all__ cannot be statically determined.
    """
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return None

    names: List[str] = []
    all_found = False

    for node in ast.walk(tree):
        # __all__ = ["foo", "bar"]
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == "__all__":
                    all_found = True
                    names.extend(_extract_list_literals(node.value))

        # __all__ += ["baz"]
        if isinstance(node, ast.AugAssign):
            if isinstance(node.target, ast.Name) and node.target.id == "__all__":
                all_found = True
                names.extend(_extract_list_literals(node.value))

        # __all__.extend(["qux"])
        if isinstance(node, ast.Call):
            if (
                isinstance(node.func, ast.Attribute)
                and node.func.attr == "extend"
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id == "__all__"
            ):
                all_found = True
                if node.args:
                    names.extend(_extract_list_literals(node.args[0]))

        # __all__.append("single")
        if isinstance(node, ast.Call):
            if (
                isinstance(node.func, ast.Attribute)
                and node.func.attr == "append"
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id == "__all__"
                and node.args
            ):
                all_found = True
                if isinstance(node.args[0], ast.Constant) and isinstance(
                    node.args[0].value, str
                ):
                    names.append(node.args[0].value)

    if not all_found:
        return None

    return list(dict.fromkeys(names))  # deduplicate, preserve order


def _extract_list_literals(node: ast.expr) -> List[str]:
    """Extract string literals from a list/tuple node."""
    names: List[str] = []
    if isinstance(node, (ast.List, ast.Tuple)):
        for elem in node.elts:
            if isinstance(elem, ast.Constant) and isinstance(elem.value, str):
                names.append(elem.value)
    return names
