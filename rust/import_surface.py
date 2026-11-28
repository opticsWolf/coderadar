#!/usr/bin/env python
"""Measure the Python import surface against the Rust-backed bridge.

For every module under ``py_agent/src/coderadar`` this records the public
names the module declares and whether the bridge (``rust/coderadar/python``)
resolves the module and each name. The output is the D3 baseline:

    python rust/import_surface.py            # rewrite the golden file
    python rust/import_surface.py --json     # print it, write nothing
    python rust/import_surface.py --check    # exit 1 when a module regresses

A name listed in ``missing_names`` or a module listed in ``missing_modules``
is the recorded "not ported yet" set; the test in
``tests/test_import_surface.py`` asserts the surface never gets worse than
this file.
"""

from __future__ import annotations

import argparse
import ast
import importlib
import importlib.util
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REFERENCE = ROOT / "py_agent" / "src" / "coderadar"
BRIDGE = ROOT / "rust" / "coderadar" / "python" / "coderadar"
GOLDEN = ROOT / "rust" / "golden" / "import_surface_pending.json"
ALIAS = "coderadar_rs"


def public_names(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    return {
        node.name
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
        and not node.name.startswith("_")
    }


def module_names() -> dict[str, set[str]]:
    found: dict[str, set[str]] = {}
    for path in sorted(REFERENCE.rglob("*.py")):
        module = ".".join(path.relative_to(REFERENCE).with_suffix("").parts)
        if module.endswith(".__init__"):
            module = module[: -len(".__init__")]
        names = public_names(path)
        if names:
            found[module] = names
    return found


def load_bridge() -> object:
    """Load the bridge under a private alias so `coderadar` is untouched."""
    loaded = sys.modules.get(ALIAS)
    if loaded is not None:
        return loaded
    spec = importlib.util.spec_from_file_location(
        ALIAS,
        BRIDGE / "__init__.py",
        submodule_search_locations=[str(BRIDGE)],
    )
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load the bridge from {BRIDGE}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[ALIAS] = module
    spec.loader.exec_module(module)
    return module


def measure() -> dict:
    load_bridge()
    importlib.invalidate_caches()
    errors: dict[str, str] = {}
    missing_modules: list[str] = []
    missing_names: dict[str, list[str]] = {}
    resolved = 0
    declared = 0
    for module, names in module_names().items():
        declared += len(names)
        try:
            target = importlib.import_module(f"{ALIAS}.{module}")
        except Exception as error:  # noqa: BLE001 - the failure is the measurement
            errors[module] = str(error)
            missing_modules.append(module)
            missing_names[module] = sorted(names)
            continue
        absent = sorted(names - set(dir(target)))
        if absent:
            missing_names[module] = absent
        resolved += len(names) - len(absent)
    return {
        "declared_names": declared,
        "resolved_names": resolved,
        "coverage": round(resolved / declared, 4) if declared else 0.0,
        "import_errors": errors,
        "missing_modules": missing_modules,
        "missing_names": missing_names,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--check", action="store_true")
    options = parser.parse_args(argv)

    report = measure()
    if options.json:
        print(json.dumps(report, indent=1, sort_keys=True))
        return 0
    if not options.check:
        GOLDEN.write_text(json.dumps(report, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(
        f"import surface: {report['resolved_names']}/{report['declared_names']} names "
        f"({report['coverage'] * 100:.1f}%), {len(report['missing_modules'])} modules absent"
    )
    if report["missing_modules"]:
        print(f"  absent modules: {report['missing_modules']}")
    if options.check:
        recorded = json.loads(GOLDEN.read_text(encoding="utf-8"))
        if report["resolved_names"] < recorded.get("resolved_names", 0):
            print(
                f"  REGRESSED: was {recorded['resolved_names']}, "
                f"now {report['resolved_names']}"
            )
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
