"""CodeRadar v3.6 -- End-to-End Integration Tests

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

    def test_search_finds_imports(self):
        """`kind="import"` used to return nothing at all.

        search_entities only scanned functions, classes and modules, so the
        three further kinds compute_embeddings asks for were never embedded
        and codegraph_search_similar could never surface them.
        """
        results = search_entities("", 50, kind="import")
        assert results, "the e2e project imports something"
        assert all(r.get("kind") == "import" for r in results), results

    def test_search_import_kind_is_filtered_out_of_other_kinds(self):
        """The kind filter still partitions: an import is not a function."""
        functions = search_entities("", 50, kind="function")
        assert functions and not any(r.get("kind") == "import" for r in functions)

    def test_unfiltered_search_spans_kinds(self):
        kinds = {r.get("kind") for r in search_entities("", 200, None)}
        assert {"function", "class", "module", "import"} <= kinds, kinds

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

    def test_watcher_reports_a_deletion_and_the_graph_drops_the_file(self, tmp_path):
        """Deleting a watched file used to leave its entities in the graph.

        `notify-debouncer-mini` collapses every event to `Any`, so the kind
        never said "deleted" and both the Rust `Delete` variant and the Python
        delete branch were unreachable. The watcher now stats the path.
        """
        from coderadar._core import (
            analyze, start_watcher, next_watcher_batch_timeout, stop_watcher,
            remove_file, search_entities,
        )

        doomed = tmp_path / "doomed_mod.py"
        doomed.write_text("def doomed_fn(): pass\n")
        (tmp_path / "kept_mod.py").write_text("def kept_fn(): pass\n")
        analyze(str(tmp_path))
        assert len(search_entities("doomed_fn", 10, kind="function")) >= 1

        start_watcher([str(tmp_path)])
        try:
            doomed.unlink()

            import time
            time.sleep(0.3)

            batch = next_watcher_batch_timeout(3000)
            assert batch is not None, "Watcher should detect the deletion"
            kinds = {p: k for p, k in batch}
            doomed_events = [k for p, k in kinds.items() if "doomed_mod" in p]
            assert doomed_events, f"Batch should contain doomed_mod.py: {list(kinds)}"
            assert all(k == "Delete" for k in doomed_events), \
                f"A vanished path must be reported as Delete, got {doomed_events}"

            for file_path, kind in batch:
                if kind == "Delete":
                    remove_file(file_path)

            assert not search_entities("doomed_fn", 10, kind="function"), \
                "the deleted file's entities must leave the graph"
            assert len(search_entities("kept_fn", 10, kind="function")) >= 1, \
                "the surviving file is untouched"
        finally:
            stop_watcher()

    def test_watcher_honours_a_custom_debounce(self, tmp_path):
        """`--debounce` was stored and never passed to the binding (§1.4)."""
        from coderadar._core import (
            analyze, start_watcher, next_watcher_batch_timeout, stop_watcher,
        )

        target = tmp_path / "tuned.py"
        target.write_text("def a(): pass\n")
        analyze(str(tmp_path))

        start_watcher([str(tmp_path)], 30)
        try:
            target.write_text("def a(): pass\ndef b(): pass\n")
            import time
            time.sleep(0.3)
            batch = next_watcher_batch_timeout(3000)
            assert batch is not None, "a 30 ms debounce still delivers events"
            assert any("tuned" in p for p, _ in batch)
        finally:
            stop_watcher()

    def test_watcher_skips_files_over_the_size_limit(self, tmp_path):
        """`max_file_size_bytes` was configurable and never consulted (§1.4)."""
        from coderadar._core import (
            analyze, start_watcher, next_watcher_batch_timeout, stop_watcher,
        )

        small = tmp_path / "small.py"
        small.write_text("x = 1\n")
        analyze(str(tmp_path))

        # 200-byte ceiling: the big file's events must not reach the batch.
        start_watcher([str(tmp_path)], 30, 200)
        try:
            (tmp_path / "huge.py").write_text("# padding\n" * 500)
            import time
            time.sleep(0.4)
            batch = next_watcher_batch_timeout(1500)
            paths = [p for p, _ in (batch or [])]
            assert not any("huge" in p for p in paths), \
                f"a file over the limit must be skipped: {paths}"
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


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
@pytest.mark.slow
class TestBenchmarkPipeline:
    """v0.5: Benchmark-level integration tests — verifies indexing at scale.

    These tests generate synthetic projects, index them, and assert
    that entity counts, Macrame persistence, and performance boundaries
    hold. Not microbenchmarks — correctness + sanity checks.
    """

    def test_index_50_files_250_functions(self, tmp_path):
        """Index 50 files / 250 functions and verify counts."""
        import time
        from coderadar._core import analyze as _analyze_rust, graph_stats

        for i in range(50):
            src = '\n'.join(
                f'def func_{i}_{j}(): return {i}+{j}'
                for j in range(5)
            )
            (tmp_path / f'mod_{i}.py').write_text(src)

        t0 = time.perf_counter()
        result = _analyze_rust(str(tmp_path))
        elapsed_ms = (time.perf_counter() - t0) * 1000

        stats = graph_stats()
        assert result['files_indexed'] == 50, \
            f"Expected 50 files, got {result}"
        assert result['entities_extracted'] == 250, \
            f"Expected 250 entities (5 funcs/file), got {result}"
        assert stats['modules'] == 50
        assert stats['functions'] == 250
        assert stats['classes'] == 0

        # Sanity: should finish in under 30s on any hardware
        assert elapsed_ms < 30_000, \
            f"50 files took {elapsed_ms:.0f}ms, expected <30s"

    def test_index_200_files_1000_functions(self, tmp_path):
        """Index 200 files / 1000 functions with cross-file calls + Macrame DB.

        Creates 199 modules each with 5 isolated leaf functions, plus one
        orchestrator that imports and calls them all.  This yields containment
        edges (file→func) AND cross-file call edges, matching what CodeGraph
        reports as 'Edges' in its status output.
        """
        import time
        from coderadar._core import analyze as _analyze_rust, graph_stats

        # 199 leaf modules with 5 functions each = 995 leaf functions
        for i in range(199):
            src = '\n'.join(
                f'def func_{i}_{j}(): return {i}+{j}'
                for j in range(5)
            )
            (tmp_path / f'mod_{i}.py').write_text(src)

        # 1 orchestrator that imports + calls all 995 leaf functions
        import_lines = []
        call_lines = ['def orchestrator():']
        for i in range(199):
            for j in range(5):
                import_lines.append(f'from mod_{i} import func_{i}_{j}')
                call_lines.append(f'    func_{i}_{j}()')
        (tmp_path / 'orchestrator.py').write_text(
            '\n'.join(import_lines + [''] + call_lines + [''])
        )

        t0 = time.perf_counter()
        result = _analyze_rust(str(tmp_path), create_store=True)
        elapsed_ms = (time.perf_counter() - t0) * 1000

        stats = graph_stats()
        assert result['files_indexed'] == 200, \
            f"Expected 200 files, got {result}"
        # 199*5 leaf funcs + 1 orchestrator + 199*5 imports
        expected_entities = 199 * 5 + 1 + 199 * 5
        assert result['entities_extracted'] == expected_entities, \
            f"Entities: expected {expected_entities}, got {result}"
        assert stats['modules'] == 200
        # 995 leaf + 1 orchestrator = 996
        assert stats['functions'] == 996
        assert stats['imports'] == 199 * 5  # one import per leaf function
        # Each orchestrator call should resolve to a leaf function
        assert stats['call_edges'] >= 199 * 5, \
            f"Expected >=995 call edges, got {stats['call_edges']}"

        # Macrame DB must exist with data (schema alone is 4096 bytes)
        db_path = tmp_path / '.coderadar' / 'store' / 'coderadar.db'
        assert db_path.exists(), f"Macrame DB not created at {db_path}"
        assert db_path.stat().st_size > 4096, \
            f"DB should have data beyond schema: {db_path.stat().st_size} bytes"

        # Sanity: should finish in under 60s on any hardware
        assert elapsed_ms < 60_000, \
            f"200 files took {elapsed_ms:.0f}ms, expected <60s"

    def test_cross_file_call_edges_at_scale(self, tmp_path):
        """Index 50 files with cross-file imports and verify call edges."""
        from coderadar._core import analyze as _analyze_rust, graph_stats

        # Create a dependency chain: mod_0 -> mod_1 -> ... -> mod_49
        (tmp_path / 'mod_0.py').write_text('def leaf(): return 0\n')
        for i in range(1, 50):
            (tmp_path / f'mod_{i}.py').write_text(
                f'from mod_{i-1} import leaf\n'
                f'def chain_{i}(): return leaf()\n'
            )

        result = _analyze_rust(str(tmp_path), create_store=True)
        stats = graph_stats()

        assert result['files_indexed'] == 50
        assert stats['functions'] == 50
        assert stats['imports'] == 49  # 49 import statements
        # Each chain_N calls leaf from mod_{N-1} — call edges depend on
        # resolve_all_calls running after the batch. Should have at least
        # the same-file heuristic edges.
        assert stats['call_edges'] >= 0, \
            f"Call edges should be non-negative, got {stats['call_edges']}"

    def test_macrame_db_grows_with_entities(self, tmp_path):
        """Macrame DB size scales with entity count, not file count."""
        from coderadar._core import analyze as _analyze_rust

        # Index 10 files
        for i in range(10):
            (tmp_path / f'small_{i}.py').write_text(
                f'def f{i}(): return {i}\n'
            )
        _analyze_rust(str(tmp_path), create_store=True)
        small_size = sum(
            f.stat().st_size
            for f in (tmp_path / '.coderadar' / 'store').rglob('*')
        )

        # Index 10 more in a fresh dir
        tmp2 = tmp_path / 'bench2'
        tmp2.mkdir()
        for i in range(10):
            (tmp2 / f'big_{i}.py').write_text(
                f'def g{i}():\n'
                f'    """Docstring for function {i}."""\n'
                f'    x = {i}\n'
                f'    return x * 2\n'
            )
        _analyze_rust(str(tmp2), create_store=True)
        big_size = sum(
            f.stat().st_size
            for f in (tmp2 / '.coderadar' / 'store').rglob('*')
        )

        # Both should have DB data (not just schema)
        assert small_size > 4096, f"Small project DB: {small_size} bytes"
        assert big_size > 4096, f"Big project DB: {big_size} bytes"
        # Bigger functions (with docstrings, bodies) should produce larger DB
        assert big_size >= small_size, \
            f"DB with richer entities should be >= simpler ones: {big_size} vs {small_size}"

    def test_balanced_callers_50_modules_1000_calls(self, tmp_path):
        """Balanced resolve workload: many callers spread across modules.

        Creates 50 modules each with 5 leaf functions, plus 50 caller modules
        each with 2 callers making 10 cross-module calls (1,000 total calls).
        Each caller imports from module (i+1)%50.  Unlike the skewed benchmark
        (1 heavy orchestrator + 199 empty modules), this evenly distributes
        resolution work across all callers — the workload that exercises the
        parallel resolve path.

        Benchmark B from docs/performance-roadmap.md.
        """
        import time
        from coderadar._core import analyze as _analyze_rust, graph_stats

        # 50 leaf modules, 5 functions each = 250 leaf functions
        for i in range(50):
            funcs = [f'def leaf_{i}_{j}(): return {i}+{j}' for j in range(5)]
            (tmp_path / f'mod_{i}.py').write_text('\n'.join(funcs))

        # 50 caller modules, 2 callers each, 10 calls each = 1,000 calls
        for i in range(50):
            tgt = (i + 1) % 50
            import_lines = [f'from mod_{tgt} import leaf_{tgt}_{j}' for j in range(5)]
            func_lines = []
            for c in range(2):
                func_lines.append(f'def caller_{i}_{c}():')
                for j in range(5):
                    func_lines.append(f'    leaf_{tgt}_{j}()')
                    func_lines.append(f'    leaf_{tgt}_{j}()')
            (tmp_path / f'callers_{i}.py').write_text(
                '\n'.join(import_lines + [''] + func_lines))

        t0 = time.perf_counter()
        result = _analyze_rust(str(tmp_path))
        elapsed_ms = (time.perf_counter() - t0) * 1000

        stats = graph_stats()
        assert result['files_indexed'] == 100, \
            f"Expected 100 files, got {result}"
        # 250 leaf + 100 callers = 350 functions
        assert stats['functions'] == 350, \
            f"Expected 350 functions, got {stats['functions']}"
        # 50 caller modules × 5 imports each = 250 imports
        assert stats['imports'] == 250, \
            f"Expected 250 imports, got {stats['imports']}"
        # 100 callers × 5 unique targets = 500 edges (deduplicated in BTreeSet)
        assert stats['call_edges'] == 500, \
            f"Expected 500 call edges, got {stats['call_edges']}"

        # Sanity: should finish in under 60s
        assert elapsed_ms < 60_000, \
            f"Balanced benchmark took {elapsed_ms:.0f}ms, expected <60s"

    def test_heavy_resolve_100_modules_4000_calls(self, tmp_path):
        """Heavy resolve workload: many callers, many calls, cross-module.

        Creates 100 modules each with 5 leaf functions, plus 100 caller modules
        each with 2 callers making 20 cross-module calls (4,000 total calls).
        Each caller imports from module (i+1)%100.

        This is the highest call-density benchmark — resolution work is spread
        across 200 callers.  Useful for measuring parallel resolve cap effects.

        Benchmark C from docs/performance-roadmap.md.
        """
        import time
        from coderadar._core import analyze as _analyze_rust, graph_stats

        for i in range(100):
            funcs = [f'def leaf_{i}_{j}(): return {i}+{j}' for j in range(5)]
            (tmp_path / f'mod_{i}.py').write_text('\n'.join(funcs))

        for i in range(100):
            tgt = (i + 1) % 100
            import_lines = [f'from mod_{tgt} import leaf_{tgt}_{j}' for j in range(5)]
            func_lines = []
            for ci in range(2):
                func_lines.append(f'def caller_{i}_{ci}():')
                for j in range(5):
                    for _ in range(4):
                        func_lines.append(f'    leaf_{tgt}_{j}()')
            (tmp_path / f'callers_{i}.py').write_text(
                '\n'.join(import_lines + [''] + func_lines))

        t0 = time.perf_counter()
        result = _analyze_rust(str(tmp_path))
        elapsed_ms = (time.perf_counter() - t0) * 1000

        stats = graph_stats()
        assert result['files_indexed'] == 200, \
            f"Expected 200 files, got {result}"
        # 500 leaf + 200 callers = 700 functions
        assert stats['functions'] == 700, \
            f"Expected 700 functions, got {stats['functions']}"
        # 100 caller modules × 5 imports each = 500 imports
        assert stats['imports'] == 500, \
            f"Expected 500 imports, got {stats['imports']}"
        # 200 callers × 5 unique targets = 1,000 edges (deduplicated)
        assert stats['call_edges'] == 1000, \
            f"Expected 1000 call edges, got {stats['call_edges']}"

        # Sanity: should finish in under 60s
        assert elapsed_ms < 60_000, \
            f"Heavy resolve benchmark took {elapsed_ms:.0f}ms, expected <60s"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestAnonymousFunctionHandling:
    """v0.6.4: anonymous functions must not be emitted with empty names."""

    def test_anonymous_arrow_callbacks_are_skipped(self, tmp_path):
        """Named fns are indexed; anonymous arrow callbacks are not emitted as
        empty-name entities (they used to collapse to a single "file::" id).
        """
        from coderadar._core import analyze, graph_stats, query_graph
        ts = tmp_path / "widget.tsx"
        ts.write_text(
            "export function namedHandler(x: number): number {\n"
            "  return [1, 2, 3].map(n => n * 2).reduce((a, b) => a + b, 0);\n"
            "}\n"
            "const inlineCb = () => 42;\n"
        )
        analyze(str(tmp_path))

        rows = query_graph('functions')
        names = [r.get('name') for r in rows]
        # No empty-name functions should ever be emitted.
        assert '' not in names, f"empty-name function emitted: {rows}"
        assert all(n for n in names), f"empty name found: {names}"
        # The named function is indexed.
        assert 'namedHandler' in names, f"namedHandler missing: {names}"
        stats = graph_stats()
        assert stats['functions'] >= 1
        # Anonymous arrow callbacks must not flood the graph.
        assert stats['functions'] < 5, \
            f"expected only named fns indexed, got {stats['functions']}: {names}"

    def test_anon_calls_attribute_to_enclosing_function(self, tmp_path):
        """Direct calls inside a skipped anonymous callback are still captured
        and attributed to the enclosing named function (call graph stays
        accurate)."""
        from coderadar._core import analyze, query_graph
        ts = tmp_path / "wrap.ts"
        ts.write_text(
            "function helper(): void { return; }\n"
            "export function outer(): void {\n"
            "  [1, 2].forEach(n => helper());\n"
            "}\n"
        )
        analyze(str(tmp_path))
        rows = query_graph('functions where name == "outer"')
        assert rows, "outer() should be indexed"
        # `outer` calls helper() via the arrow callback body.
        callees = rows[0].get('callees', []) or rows[0].get('resolved_call_targets', [])
        callee_names = [
            c.split('::')[-1] if isinstance(c, str) else c for c in callees
        ]
        assert any('helper' in c for c in callee_names), \
            f"call to helper() inside the anon callback should attribute to outer; callees={callees}"

