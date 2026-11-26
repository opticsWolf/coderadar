"""Bench-only shim for the Python bridge name collision.

`rust-anything bench` imports the Rust facade as `coderadar` before it loads
the original `coderadar.ops` source. Python then tries to find `ops` inside
the facade package and fails with `ModuleNotFoundError`. This shim drops the
cached facade just before the original package is imported; the `new` module
object the benchmark holds stays valid because Python keeps its reference.
"""

from __future__ import annotations

import importlib
import os
import sys

_real_import_module = importlib.import_module

_FACADE_MARKER = os.path.join("rust", "coderadar", "python", "coderadar")


def _is_facade(module: object) -> bool:
    file = getattr(module, "__file__", None)
    return isinstance(file, str) and _FACADE_MARKER in file.replace("/", os.sep)


def import_module(name: str, package: str | None = None):
    if name.startswith("coderadar."):
        cached = sys.modules.get("coderadar")
        if cached is not None and _is_facade(cached):
            for key in [key for key in sys.modules if key == "coderadar" or key.startswith("coderadar.")]:
                del sys.modules[key]
    return _real_import_module(name, package)


importlib.import_module = import_module
