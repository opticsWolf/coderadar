"""v0.8 P2-5 (class docstrings): in-body class docstrings are backfilled.

P2-1 backfilled *function* docstrings (a Python body string fires *after*
the function node is emitted, so the field was always None). The class path
was the same gap left open: ``current_class_idx`` was tracked but the
``Tag::Docstring`` arm only backfilled ``Function.docstring``. This locks in
the class-side fix and — just as importantly — the guard that keeps a
method's docstring from leaking onto its enclosing class.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

try:
    from coderadar._core import analyze as _analyze
    from coderadar._core import search_entities
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


def _project(tmp_path, source: str) -> Path:
    (tmp_path / "mod.py").write_text(source, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    return previous


def _classes(project_root: Path):
    return search_entities("", 50, "class")


def _by_name(rows, name):
    for r in rows:
        if r.get("name") == name:
            return r
    return None


class TestClassDocstringBackfill:
    def test_class_docstring_is_backfilled_and_searchable(self, tmp_path):
        source = '''\
class Config:
    """Holds the process-wide configuration."""

    version = 1
'''
        prev = _project(tmp_path, source)
        try:
            _analyze(".")
            cls = _by_name(_classes(tmp_path), "Config")
            assert cls is not None, "Config not indexed"
            assert cls.get("docstring") == "Holds the process-wide configuration.", cls
            # A token that lives only in the class docstring is now found.
            names = [r.get("name") for r in search_entities("process-wide", 10)]
            assert "Config" in names, names
        finally:
            os.chdir(prev)

    def test_method_docstring_does_not_leak_onto_class(self, tmp_path):
        # The class has *no* docstring of its own; the only docstring is the
        # method's. It must land on the method, never on the class.
        source = '''\
class Service:
    def run(self):
        """Execute the pipeline once."""
        return True
'''
        prev = _project(tmp_path, source)
        try:
            _analyze(".")
            cls = _by_name(_classes(tmp_path), "Service")
            assert cls is not None
            assert cls.get("docstring") is None, (
                f"method docstring leaked onto the class: {cls.get('docstring')!r}"
            )
        finally:
            os.chdir(prev)

    def test_class_without_docstring_stays_none(self, tmp_path):
        source = '''\
class Plain:
    x = 1

    def method(self):
        """A method docstring."""
        return self.x
'''
        prev = _project(tmp_path, source)
        try:
            _analyze(".")
            cls = _by_name(_classes(tmp_path), "Plain")
            assert cls is not None
            assert cls.get("docstring") is None, cls.get("docstring")
        finally:
            os.chdir(prev)

    def test_nested_class_docstrings_are_independent(self, tmp_path):
        source = '''\
class Outer:
    """Outer docstring."""

    class Inner:
        """Inner docstring."""
'''
        prev = _project(tmp_path, source)
        try:
            _analyze(".")
            outer = _by_name(_classes(tmp_path), "Outer")
            inner = _by_name(_classes(tmp_path), "Inner")
            assert outer is not None and inner is not None
            assert outer.get("docstring") == "Outer docstring.", outer.get("docstring")
            assert inner.get("docstring") == "Inner docstring.", inner.get("docstring")
        finally:
            os.chdir(prev)
