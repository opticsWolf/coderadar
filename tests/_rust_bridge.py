"""Rust-backed facade for the port tests.

``pyproject.toml`` puts ``py_agent/src`` on ``sys.path``, so a plain
``import coderadar`` in a test reaches the legacy Python package, not the
Rust port. Load the bridge under a private alias instead, and cross-check
its framework extraction against the Python reference when that package is
importable.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from typing import Any

_ROOT = Path(__file__).resolve().parents[1]
_BRIDGE = _ROOT / "rust" / "coderadar" / "python" / "coderadar"
_ALIAS = "coderadar_rs"
_COUNT_KEYS = ("routes", "handlers", "frameworks")


def rust_bridge() -> Any:
    """Return the ``coderadar`` package that calls the Rust engine."""
    loaded = sys.modules.get(_ALIAS)
    if loaded is not None:
        return loaded
    # A previous test may already have imported the bridge as `coderadar`.
    # PyO3 extension modules can only initialise once per process, so reuse
    # that module under the alias instead of loading the .pyd twice.
    existing = sys.modules.get("coderadar")
    if existing is not None:
        origin = getattr(existing, "__file__", None)
        if origin and Path(origin).resolve().parent == _BRIDGE:
            sys.modules[_ALIAS] = existing
            return existing
    spec = importlib.util.spec_from_file_location(
        _ALIAS,
        _BRIDGE / "__init__.py",
        submodule_search_locations=[str(_BRIDGE)],
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load the Rust bridge from {_BRIDGE}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[_ALIAS] = module
    spec.loader.exec_module(module)
    return module


def _python_reference() -> Any | None:
    """The legacy ``coderadar.framework`` module, when it is not the bridge."""
    try:
        import coderadar.framework as framework
    except Exception:
        return None
    if Path(framework.__file__).resolve().parent == _BRIDGE:
        return None
    return framework


def refresh_framework_file(
    root: str, rel_posix: str, content: str | None = None
) -> dict:
    """Refresh one file's framework routes in Rust and cross-check Python.

    # Errors
    Raises ``RuntimeError`` when the Rust bridge cannot be loaded, and
    ``AssertionError`` when extraction counts differ from the reference.
    """
    report = rust_bridge().refresh_framework_file(root, rel_posix, content)
    reference = _python_reference()
    if reference is None:
        return report
    expected = reference.refresh_framework_file(root, rel_posix, content)
    assert {key: report.get(key) for key in _COUNT_KEYS} == {
        key: expected.get(key) for key in _COUNT_KEYS
    }, f"Rust/Python framework extraction differs for {rel_posix}"
    return report
