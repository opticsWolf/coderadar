"""Phase 7 — precision regressions found by dogfooding CodeRadar on itself.

Every case here was a real false positive (or a real dead function hidden by
one) while running the 0.10 work against `py_agent/src`. They are grouped by
the rule that fixes them, so a future edit that re-breaks one fails next to
the reasoning rather than in a corpus test three layers away.
"""

from __future__ import annotations

import textwrap
from pathlib import Path

import coderadar
import pytest
from coderadar import _core

pytestmark = pytest.mark.skipif(
    not hasattr(_core, "find_dead_code"), reason="needs the built _core extension"
)


def _write(root: Path, rel: str, body: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(body), encoding="utf-8")


def _findings(root: Path, **kwargs) -> dict[str, dict]:
    coderadar.analyze(str(root))
    return {
        f["entity_id"]: f
        for f in _core.find_dead_code(kwargs.get("min_confidence", 0.0), True, 2000)
    }


class TestImportResolution:
    """§7.2: an unresolved import is a missing edge, not dead code."""

    def test_relative_import_with_alias_binds_the_in_repo_symbol(self, tmp_path):
        _write(tmp_path, "pkg/__init__.py", "")
        _write(tmp_path, "pkg/a.py", 'def helper():\n    return 1\n')
        _write(
            tmp_path,
            "pkg/b.py",
            """
            from .a import helper as h

            def use_it():
                return h()
            """,
        )
        coderadar.analyze(str(tmp_path))
        sites = _core.call_sites("pkg/b.py::use_it") or []
        call = next(s for s in sites if s["name"] == "h")
        assert call["status"] == "function", call
        assert call["target"] == "pkg/a.py::helper", call
        assert "pkg/b.py::use_it" in {
            c["id"] for c in _core.callers_of("pkg/a.py::helper") or []
        }

    def test_function_local_relative_import_resolves(self, tmp_path):
        # `serve` imports `install` inside its body; before this fix the call
        # resolved to "external install" and the whole lifecycle subtree
        # looked dead.
        _write(tmp_path, "pkg/__init__.py", "")
        _write(tmp_path, "pkg/life.py", "def install():\n    return 1\n")
        _write(
            tmp_path,
            "pkg/serve.py",
            """
            def serve():
                from .life import install
                return install()
            """,
        )
        coderadar.analyze(str(tmp_path))
        call = next(
            s for s in _core.call_sites("pkg/serve.py::serve") if s["name"] == "install"
        )
        assert call["status"] == "function", call
        assert call["target"] == "pkg/life.py::install", call


class TestLibrarySurface:
    """§2.4: the package's public API is a root, not dead code."""

    def test_package_facade_function_and_method_are_not_dead(self, tmp_path):
        _write(
            tmp_path,
            "pkg/__init__.py",
            """
            from .pool import Pool


            def watch(root):
                return root


            class CodeGraph:
                def query(self, text):
                    return text

                def _helper(self):
                    return 1
            """,
        )
        _write(tmp_path, "pkg/pool.py", "class Pool:\n    pass\n")
        findings = _findings(tmp_path)
        assert "pkg/__init__.py::watch" not in findings
        assert "pkg/__init__.py::CodeGraph.query" not in findings
        # Private helpers of a public class are still fair game.
        assert "pkg/__init__.py::CodeGraph._helper" in findings

    def test_reexported_subpackage_class_method_is_not_dead(self, tmp_path):
        # `coderadar.lsp` re-exports `coderadar.lsp.pool`; `import
        # coderadar.lsp` is a documented way in, so `LSPPool.shutdown` is
        # called by users even though nothing in-repo calls it.
        _write(tmp_path, "pkg/__init__.py", "")
        _write(tmp_path, "pkg/lsp/__init__.py", "from .pool import Pool\n")
        _write(
            tmp_path,
            "pkg/lsp/pool.py",
            """
            class Pool:
                def shutdown(self):
                    return 1

                def _internal(self):
                    return 2
            """,
        )
        findings = _findings(tmp_path)
        assert "pkg/lsp/pool.py::Pool.shutdown" not in findings
        assert "pkg/lsp/pool.py::Pool._internal" in findings


class TestEntryPointRules:
    def test_registration_decorator_and_decorator_definition_are_live(self, tmp_path):
        _write(
            tmp_path,
            "pkg/__init__.py",
            "",
        )
        _write(
            tmp_path,
            "pkg/srv.py",
            """
            import functools


            def requires_index(fn):
                @functools.wraps(fn)
                def wrapper(*args, **kwargs):
                    return fn(*args, **kwargs)

                return wrapper


            @requires_index
            def coderadar_node():
                return 1


            @mcp.tool(description="search")
            def coderadar_search():
                return 2


            @functools.lru_cache
            def _memoized():
                return 3
            """,
        )
        findings = _findings(tmp_path)
        assert "pkg/srv.py::requires_index" not in findings
        assert "pkg/srv.py::coderadar_node" not in findings
        assert "pkg/srv.py::coderadar_search" not in findings
        # A plain attribute-reference decorator registers nothing.
        assert "pkg/srv.py::_memoized" in findings

    def test_module_level_initializer_is_live_but_docstring_prose_is_not(self, tmp_path):
        _write(tmp_path, "pkg/__init__.py", "")
        _write(
            tmp_path,
            "pkg/ver.py",
            '''
            """Dead: `_orphan` (no callers) is prose, not a call site."""


            def _resolve_version():
                return "1.0"


            __version__ = _resolve_version()


            def _orphan():
                return 1
            ''',
        )
        findings = _findings(tmp_path)
        assert "pkg/ver.py::_resolve_version" not in findings
        assert "pkg/ver.py::_orphan" in findings


class TestWeakSurface:
    """A value that escapes is not 0.9-dead — it is Low, with the reason."""

    def test_value_reference_stays_low_when_only_dead_code_holds_it(self, tmp_path):
        # Reachability already treats a value reference as a live edge, so the
        # interesting case is a reference held only by *dead* code: it must
        # stay in the report, at Low, instead of claiming 0.9.
        _write(tmp_path, "pkg/__init__.py", "")
        _write(
            tmp_path,
            "pkg/lazy.py",
            """
            def _index_is_empty():
                return True


            def _register(probe=None):
                return probe or _index_is_empty
            """,
        )
        findings = _findings(tmp_path)
        probe = findings["pkg/lazy.py::_index_is_empty"]
        assert probe["tier"] == "Low", probe
        assert probe["score"] <= 0.45, probe
        assert any("value" in e for e in probe["evidence"]), probe

    def test_public_method_of_an_instantiated_class_caps_at_low(self, tmp_path):
        _write(tmp_path, "pkg/__init__.py", "")
        _write(
            tmp_path,
            "pkg/svc.py",
            """
            class Service:
                def run(self):
                    return 1


            def build():
                return Service()
            """,
        )
        findings = _findings(tmp_path)
        run = findings["pkg/svc.py::Service.run"]
        assert run["tier"] == "Low", run
        assert any("instantiated class" in e for e in run["evidence"]), run
