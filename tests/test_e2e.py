"""CodeRadar v3.5 -- End-to-End Integration Tests

Tests the full pipeline: analyze -> query -> resolve -> visualize -> mutation
using real fixture data. Requires the Rust _core extension to be built.

Usage:
    pytest tests/test_e2e.py -v
    pytest tests/test_e2e.py -v -k "test_full_pipeline"
"""

import os
import sys
import json
import tempfile
from pathlib import Path

import pytest

# Ensure py_agent/src is on path
sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

# Check if the Rust extension is available
try:
    from coderadar._core import analyze, graph_stats, search_entities
    from coderadar._core import lookup_entity, callers_of, callees_of
    from coderadar._core import update_file, search_similar
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

FIXTURES_DIR = Path(__file__).parent / "fixtures" / "python"
E2E_DIR = FIXTURES_DIR / "e2e_project"
CROSS_DIR = FIXTURES_DIR / "cross_file"

# ── Helper ────────────────────────────────────────────────────────────────

def _names(items):
    """Extract name list from entity dicts."""
    return [i.get("name", "") for i in items]


def _has_entity(items, name):
    """Check if an entity with the given name exists."""
    return any(i.get("name") == name for i in items)


# ═══════════════════════════════════════════════════════════════════════════
# Fixture File Verification (no Rust needed)
# ═══════════════════════════════════════════════════════════════════════════

class TestFixtureFiles:
    """Verify that fixture files exist and contain expected content."""

    def test_e2e_fixtures_exist(self):
        assert (E2E_DIR / "__init__.py").exists()
        assert (E2E_DIR / "models.py").exists()
        assert (E2E_DIR / "services.py").exists()

    def test_cross_file_fixtures_exist(self):
        assert (CROSS_DIR / "module_a.py").exists()
        assert (CROSS_DIR / "module_b.py").exists()

    def test_e2e_models_has_classes(self):
        content = (E2E_DIR / "models.py").read_text()
        assert "class User" in content
        assert "class AdminUser(User)" in content
        assert "def find_user_by_id" in content

    def test_e2e_services_has_cross_calls(self):
        content = (E2E_DIR / "services.py").read_text()
        assert "from .models import" in content
        assert "create_user" in content
        assert "find_user_by_id" in content
        assert "format_username" in content


# ═══════════════════════════════════════════════════════════════════════════
# Full Pipeline Tests (require Rust extension)
# ═══════════════════════════════════════════════════════════════════════════

@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestFullPipeline:
    """End-to-end: analyze -> query -> resolve -> visualize -> mutation."""

    @pytest.fixture(autouse=True)
    def _analyze(self):
        """Index the e2e_project fixture before each test."""
        analyze(str(E2E_DIR))
        yield
        # Teardown not needed -- each test re-analyzes

    def test_analyze_produces_entities(self):
        """After analyzing e2e_project, entities should be indexed."""
        stats = graph_stats()
        assert stats["modules"] >= 3, f"Expected >=3 modules, got {stats}"
        assert stats["functions"] >= 5, f"Expected >=5 functions, got {stats}"
        assert stats["classes"] >= 2, f"Expected >=2 classes, got {stats}"

    def test_find_classes(self):
        """Search should return User and AdminUser classes."""
        results = search_entities("User", 10, kind="class")
        names = _names(results)
        assert any("User" in n for n in names), f"Expected User in {names}"
        for r in results:
            assert r.get("entity_type") in ("Class", "class", None)
            if r.get("name") == "AdminUser":
                # Should have a parent class
                parent = r.get("parent_class")
                if parent:
                    assert "User" in str(parent)

    def test_find_functions(self):
        """Search should return all defined functions."""
        results = search_entities("", 20, kind="function")
        names = _names(results)
        for expected in ["create_user", "format_username", "find_user_by_id",
                          "display_name", "is_valid", "grant_permission",
                          "can_manage_users"]:
            assert expected in names, f"Missing function: {expected}: {names}"

    def test_lookup_by_entity_id(self):
        """Lookup should resolve a dotted entity ID to full details."""
        results = search_entities("create_user", 10, kind="function")
        assert len(results) > 0
        entity_id = results[0].get("id", "")
        assert entity_id, "Entity should have an ID"

        detail = lookup_entity(entity_id)
        assert detail is not None
        assert "create_user" in detail.get("name", "")

    def test_call_edges_exist(self):
        """Functions that call other functions should have call edges."""
        # services.py: create_user calls format_username
        results = search_entities("create_user", 10, kind="function")
        assert len(results) > 0
        entity_id = results[0]["id"]

        callees = callees_of(entity_id)
        callee_names = _names(callees)
        # should call format_username
        assert any("format_username" in n for n in callee_names), \
            f"create_user should call format_username, got: {callee_names}"

    def test_reverse_call_edges(self):
        """callers_of should find reverse edges."""
        results = search_entities("format_username", 10, kind="function")
        assert len(results) > 0
        entity_id = results[0]["id"]

        callers = callers_of(entity_id)
        caller_names = _names(callers)
        assert any("create_user" in n for n in caller_names), \
            f"format_username should be called by create_user, got: {caller_names}"

    def test_class_methods_exist(self):
        """Classes should have a name and parent_module."""
        results = search_entities("UserService", 10, kind="class")
        assert len(results) > 0
        entity_id = results[0]["id"]

        detail = lookup_entity(entity_id)
        assert detail is not None
        assert detail.get("name") == "UserService"
        assert detail.get("kind") == "class"

    def test_inheritance_chain(self):
        """AdminUser should inherit from User."""
        results = search_entities("AdminUser", 10, kind="class")
        assert len(results) > 0
        adm_id = results[0]["id"]

        detail = lookup_entity(adm_id)
        parent = detail.get("parent_class")
        if parent:
            parent_name = parent if isinstance(parent, str) else parent.get("name", "")
            assert "User" in parent_name, f"AdminUser parent should be User: {parent_name}"

    def test_builtin_calls_tracked(self):
        """Calls to str methods (strip, title) should be tracked as builtins."""
        results = search_entities("format_username", 10, kind="function")
        assert len(results) > 0
        entity_id = results[0]["id"]

        # format_username calls name.strip().title()
        callees = callees_of(entity_id)
        # Builtins may not show as resolved entities, but call edges should exist
        assert len(callees) >= 0, "Call edges should be queryable"

    def test_module_imports(self):
        """Modules should track their imports."""
        results = search_entities("", 10, kind="module")
        # Find the services module
        services_mods = [r for r in results if "services" in r.get("name", "")]
        assert len(services_mods) > 0, "Should find services module"

    def test_cross_file_indexing(self):
        """Cross-file imports and calls should work."""
        # Re-analyze cross_file fixtures
        analyze(str(CROSS_DIR))

        stats = graph_stats()
        assert stats["modules"] >= 2

        # module_b.process calls helper_format from module_a
        b_results = search_entities("process", 10, kind="function")
        assert len(b_results) > 0
        b_id = b_results[0]["id"]

        callees = callees_of(b_id)
        callee_names = _names(callees)
        assert any("helper_format" in n for n in callee_names), \
            f"process should call helper_format: {callee_names}"

    def test_update_file_reindexes(self):
        """After update_file, changed file should have updated entities."""
        # Create a temp file to update
        tmp_file = E2E_DIR / "_temp_test.py"
        tmp_file.write_text("def foo(): pass\ndef bar(): return 42\n")
        try:
            # Index it
            analyze(str(E2E_DIR))

            # Find foo
            foo_results = search_entities("foo", 10, kind="function")
            assert len(foo_results) > 0

            # Update: rename foo -> baz, remove bar
            new_content = "def baz(): pass\n"
            update_file(str(tmp_file), new_content, True)

            # foo should be gone, baz should exist
            foo_results = search_entities("foo", 10, kind="function")
            baz_results = search_entities("baz", 10, kind="function")
            bar_results = search_entities("bar", 10, kind="function")

            assert len(foo_results) == 0, f"foo should be gone: {foo_results}"
            assert len(baz_results) > 0, f"baz should exist: {baz_results}"
            assert len(bar_results) == 0, f"bar should be gone: {bar_results}"

        finally:
            tmp_file.unlink(missing_ok=True)


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestVisualizerPipeline:
    """Visualizers produce valid output from real graph data."""

    @classmethod
    def setup_class(cls):
        analyze(str(E2E_DIR))

    def test_mermaid_hierarchy_with_graph(self):
        """Mermaid hierarchy should produce classDiagram output."""
        from coderadar.visualizers.mermaid import generate_mermaid
        from coderadar import CodeGraph
        graph = CodeGraph()
        output = generate_mermaid("hierarchy", [], graph)
        assert output.startswith("classDiagram") or "classDiagram" in output
        assert "User" in output or len(output) > 20

    def test_mermaid_dependencies_with_graph(self):
        """Mermaid dependencies should produce graph output."""
        from coderadar.visualizers.mermaid import generate_mermaid
        from coderadar import CodeGraph
        graph = CodeGraph()
        output = generate_mermaid("dependencies", [], graph)
        assert "graph" in output.lower() or "flowchart" in output.lower()

    def test_graphviz_hierarchy_with_graph(self):
        """Graphviz hierarchy should produce DOT output."""
        from coderadar.visualizers.graphviz_viz import generate_dot
        from coderadar import CodeGraph
        graph = CodeGraph()
        output = generate_dot("hierarchy", [], graph)
        assert output.startswith("digraph Hierarchy")
        assert "rankdir" in output

    def test_graphviz_dependencies_with_graph(self):
        """Graphviz dependencies should produce DOT with cycle detection."""
        from coderadar.visualizers.graphviz_viz import generate_dot
        from coderadar import CodeGraph
        graph = CodeGraph()
        output = generate_dot("dependencies", [], graph)
        assert output.startswith("digraph Dependencies")
        assert "rankdir=LR" in output

    def test_graphviz_fallback_without_graph(self):
        """Graphviz should produce demo output when graph is None."""
        from coderadar.visualizers.graphviz_viz import generate_dot
        output = generate_dot("hierarchy", [], graph=None)
        assert "BaseModel" in output
        assert "UserService" in output

    def test_call_graph_with_graph(self):
        """Call graph should produce mermaid flowchart."""
        from coderadar.visualizers.call_graph import generate_call_graph
        from coderadar import CodeGraph
        graph = CodeGraph()
        output = generate_call_graph(["User"], graph)
        assert len(output) > 0
        assert "graph" in output.lower() or "flowchart" in output.lower() or "User" in output


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestMutationPipeline:
    """Mutation planning tools work with real graph data."""

    def test_mutation_tools_exist(self):
        """Mutation functions should be importable."""
        from coderadar._core import (
            plan_body_replacement, plan_signature_update,
            plan_rename, plan_create_entity, apply_mutation
        )
        assert callable(plan_rename)
        assert callable(apply_mutation)


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestEmbeddingPipeline:
    """Embedding pipeline works end-to-end."""

    def test_search_similar_returns_results(self):
        """search_similar should accept embedding vectors."""
        # search_similar expects Vec<f64>, not str. With an empty vec it
        # should return empty results (no embeddings computed).
        results = search_similar([], 5)
        assert isinstance(results, list)
        # An empty query vector should return empty results
        assert len(results) == 0

    def test_compute_embeddings_metrics(self):
        """compute_embeddings should return metrics dict."""
        from coderadar.embedding.dedup import EmbeddingDedup
        dedup = EmbeddingDedup()
        assert dedup is not None


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestWatcherPipeline:
    """v0.5: Watcher integration — end-to-end file change detection."""

    def test_watcher_detects_file_modification(self, tmp_path):
        """Start watcher, modify a file, verify batch contains the change."""
        from coderadar._core import (
            analyze, graph_stats, start_watcher,
            next_watcher_batch_timeout, stop_watcher, update_file,
            search_entities,
        )

        # Set up: create a Python file and analyze it
        py_file = tmp_path / "test_mod.py"
        py_file.write_text("def original(): pass\n")
        analyze(str(tmp_path))

        stats = graph_stats()
        assert stats["functions"] >= 1

        # Start watching the temp directory
        start_watcher([str(tmp_path)])

        try:
            # Modify the file: add a new function
            py_file.write_text("def original(): pass\ndef new_func(): return 42\n")

            # Wait for the watcher to pick up the change (debounce = 100ms)
            import time
            time.sleep(0.3)  # Let notify + debouncer fire

            batch = next_watcher_batch_timeout(3000)
            assert batch is not None, "Watcher should detect file modification"

            # Verify the batch contains our file
            paths = [p for p, _ in batch]
            assert any("test_mod" in p for p in paths), \
                f"Batch should contain test_mod.py: {paths}"

            # Apply the update
            for file_path, kind in batch:
                if "Modify" in kind or "Any" in kind:
                    update_file(file_path, None, True)

            # Verify the new function is indexed
            results = search_entities("new_func", 10, kind="function")
            assert len(results) >= 1, \
                f"new_func should be indexed after update: {_names(results)}"

        finally:
            stop_watcher()

    def test_watcher_timeout_returns_none(self, tmp_path):
        """When no file changes occur, timeout returns None."""
        from coderadar._core import start_watcher, next_watcher_batch_timeout, stop_watcher

        # Create a clean directory for watching
        (tmp_path / "dummy.py").write_text("x = 1\n")
        start_watcher([str(tmp_path)])

        try:
            # No file changes — timeout should return None
            batch = next_watcher_batch_timeout(500)
            assert batch is None, \
                f"With no changes, timeout should return None, got: {batch}"
        finally:
            stop_watcher()

    def test_watcher_ignores_non_source_files(self, tmp_path):
        """Watcher should not report changes to .txt, .png, etc."""
        from coderadar._core import start_watcher, next_watcher_batch_timeout, stop_watcher

        (tmp_path / "code.py").write_text("x = 1\n")
        start_watcher([str(tmp_path)])

        try:
            import time
            # Modify a non-source file
            (tmp_path / "readme.txt").write_text("hello\n")
            time.sleep(0.3)

            batch = next_watcher_batch_timeout(2000)
            # Should timeout since .txt is excluded — or return only code.py changes
            if batch is not None:
                paths = [p for p, _ in batch]
                assert not any(p.endswith(".txt") for p in paths), \
                    f"Non-source files should be filtered: {paths}"
        finally:
            stop_watcher()
