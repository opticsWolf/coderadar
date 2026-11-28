"""Plan unit S3: the Python import surface is measured, not assumed.

`rust/golden/import_surface_pending.json` records every public name that still
lacks a Rust entry point. This test asserts the recorded surface has not
regressed: listed modules may stay missing, every other module must import and
expose every public name.

The bridge is imported under the private alias from `tests/_rust_bridge.py`,
so the test never replaces the `coderadar` package other tests rely on.
"""

from __future__ import annotations

import ast
import importlib
import json
from pathlib import Path

from tests._rust_bridge import rust_bridge

ROOT = Path(__file__).resolve().parent.parent
PENDING = ROOT / "rust" / "golden" / "import_surface_pending.json"
REFERENCE = ROOT / "py_agent" / "src" / "coderadar"
ALIAS = "coderadar_rs"


def public_names(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    return {
        node.name
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
        and not node.name.startswith("_")
    }


def surface() -> dict[str, set[str]]:
    found: dict[str, set[str]] = {}
    for path in sorted(REFERENCE.rglob("*.py")):
        module = ".".join(path.relative_to(REFERENCE).with_suffix("").parts)
        if module.endswith(".__init__"):
            module = module[: -len(".__init__")]
        names = public_names(path)
        if names:
            found[module] = names
    return found


def test_import_surface_matches_the_recorded_baseline() -> None:
    rust_bridge()
    importlib.invalidate_caches()
    recorded = json.loads(PENDING.read_text(encoding="utf-8"))
    missing_modules = set(recorded["missing_modules"])
    missing_names = {
        module: set(names) for module, names in recorded["missing_names"].items()
    }
    problems: list[str] = []
    for module, names in surface().items():
        if module in missing_modules:
            continue
        try:
            target = importlib.import_module(f"{ALIAS}.{module}")
        except Exception as error:  # noqa: BLE001 - the failure is the report
            problems.append(f"{module}: import failed: {error}")
            continue
        absent = sorted(names - set(dir(target)) - missing_names.get(module, set()))
        if absent:
            problems.append(f"{module}: missing {absent}")
    assert not problems, problems
