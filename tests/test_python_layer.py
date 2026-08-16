"""CodeRadar — Comprehensive Test Suite: Python Layer

Tests all Python-side logic that can run without the compiled Rust extension:
  - Configuration loading & validation
  - Query planning & intent classification
  - Cypher template validity
  - Query cache behaviour
  - Embedding dedup logic
  - Mutation tool routing
  - LSP pool lifecycle
  - Visualizer output
  - Pest query grammar (via Rust FFI when available)

Usage:
    pytest tests/ -v
    pytest tests/ -v -k "test_config"
"""

import os
import sys
import json
import time
import tempfile
import textwrap
from pathlib import Path
from unittest.mock import MagicMock, patch, PropertyMock

import pytest

# Ensure py_agent/src is on path
sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))


# ═══════════════════════════════════════════════════════════════════════════
# Configuration Tests
# ═══════════════════════════════════════════════════════════════════════════

class TestConfigLoading:
    """Load and validate .coderadar.toml and .harness/config.toml."""

    def test_default_config_is_valid(self):
        """Default CodeRadarConfig should instantiate without errors."""
        from coderadar.config import CodeRadarConfig
        cfg = CodeRadarConfig()
        assert cfg.project.languages == ["python"]
        assert cfg.resolution.min_confidence == 0.3
        assert cfg.embedding.dimension == 896
        assert cfg.mutation.enabled is True
        assert cfg.query.default_top_k == 10

    def test_default_harness_config_is_valid(self):
        """Default HarnessConfig should instantiate without errors."""
        from coderadar.config import HarnessConfig
        cfg = HarnessConfig()
        assert cfg.general.watch_paths == ["src/", "tests/"]
        assert cfg.general.debounce_ms == 500
        assert cfg.general.max_file_size_bytes == 1_048_576

    def test_load_config_from_file(self):
        """Load from a real .coderadar.toml file on disk."""
        from coderadar.config import CodeRadarConfig, load_config
        cfg = load_config(Path(__file__).parent.parent)
        assert isinstance(cfg, CodeRadarConfig)
        assert cfg.project.languages is not None

    def test_custom_config_override(self):
        """Pydantic model should accept field overrides."""
        from coderadar.config import (
            CodeRadarConfig, ProjectConfig, ResolutionConfig, StackGraphConfig
        )
        cfg = CodeRadarConfig(
            project=ProjectConfig(languages=["python", "rust"]),
            resolution=ResolutionConfig(
                stack_graph=StackGraphConfig(max_path_depth=20)
            ),
        )
        assert cfg.project.languages == ["python", "rust"]
        assert cfg.resolution.stack_graph.max_path_depth == 20

    def test_mutation_config_deny_list(self):
        """Deny list should contain sensitive paths."""
        from coderadar.config import MutationConfig
        cfg = MutationConfig()
        assert ".git/" in cfg.deny
        assert ".harness/" in cfg.deny

    def test_lsp_server_commands(self):
        """Default LSP server commands for known languages."""
        from coderadar.config import LSPConfig
        cfg = LSPConfig()
        assert cfg.servers["python"] == "pyright-langserver --stdio"
        assert cfg.servers["typescript"] == "typescript-language-server --stdio"
        assert cfg.servers["rust"] == "rust-analyzer"

    def test_embedding_config_dimensions(self):
        """Embedding config carries both full and truncated dimensions."""
        from coderadar.config import EmbeddingConfig
        cfg = EmbeddingConfig()
        assert cfg.dimension == 896
        assert cfg.truncated_dimension == 64


# ═══════════════════════════════════════════════════════════════════════════
# Query Planner Tests
# ═══════════════════════════════════════════════════════════════════════════

class TestQueryPlanner:
    """Intent classification for natural-language queries."""

    @pytest.fixture
    def planner(self):
        from coderadar.agent.graphrag import QueryPlanner
        return QueryPlanner()

    @pytest.mark.parametrize("query,expected_intent", [
        ("find functions that handle auth", "similarity_search"),
        ("search for user validation logic", "similarity_search"),
        ("similar functions to validate_email", "similarity_search"),
        ("what breaks if I change UserService", "impact_analysis"),
        ("who calls create_user", "impact_analysis"),
        ("impact of changing the User model", "impact_analysis"),
        ("path from main to execute_sql", "call_chain"),
        ("how does login reach the database", "call_chain"),
        ("show call chain from handler to query", "call_chain"),
        ("module dependencies for app.services", "dependency_graph"),
        ("what imports app.models", "dependency_graph"),
        ("what is UserService", "definition_lookup"),
        ("show me the signature of validate_email", "definition_lookup"),
        ("define UserRepository", "definition_lookup"),
    ])
    def test_intent_classification(self, planner, query, expected_intent):
        intent, _params = planner.classify(query)
        assert intent.value == expected_intent, \
            f"Query '{query}' classified as {intent.value}, expected {expected_intent}"

    def test_defaults_to_scope_exploration(self, planner):
        """Unrecognized queries fall back to scope_exploration."""
        intent, _params = planner.classify("hello world")
        assert intent.value == "scope_exploration"


# ═══════════════════════════════════════════════════════════════════════════
# Query Cache Tests
# ═══════════════════════════════════════════════════════════════════════════

class TestQueryCache:
    """LRU cache with TTL, epoch-based invalidation, and eviction."""

    @pytest.fixture
    def cache(self):
        from coderadar.query.cache import QueryCache
        return QueryCache(max_size=10, ttl_seconds=60)

    def test_set_and_get(self, cache):
        cache.set("key1", {"result": 42})
        assert cache.get("key1") == {"result": 42}

    def test_miss_returns_none(self, cache):
        assert cache.get("nonexistent") is None

    def test_invalidate_clears_all(self, cache):
        cache.set("a", 1)
        cache.set("b", 2)
        cache.invalidate()
        assert cache.get("a") is None
        assert cache.get("b") is None

    def test_ttl_expiry(self, cache):
        from coderadar.query.cache import QueryCache
        short_cache = QueryCache(max_size=10, ttl_seconds=0)
        short_cache.set("x", 99)
        assert short_cache.get("x") is None  # immediate expiry

    def test_eviction_when_full(self):
        from coderadar.query.cache import QueryCache
        tiny = QueryCache(max_size=2, ttl_seconds=300)
        tiny.set("a", 1)
        tiny.set("b", 2)
        tiny.set("c", 3)  # Should evict oldest ("a")
        assert tiny.get("a") is None
        assert tiny.get("b") == 2
        assert tiny.get("c") == 3

    def test_prune_expired(self, cache):
        from coderadar.query.cache import QueryCache
        short_cache = QueryCache(max_size=5, ttl_seconds=0)
        short_cache.set("a", 1)
        short_cache.set("b", 2)
        removed = short_cache.prune_expired()
        assert removed == 2
        assert len(short_cache) == 0

    def test_make_key_deterministic(self):
        from coderadar.query.cache import QueryCache
        k1 = QueryCache.make_key("t1", {"a": 1, "b": 2}, 5)
        k2 = QueryCache.make_key("t1", {"b": 2, "a": 1}, 5)  # different order
        assert k1 == k2, "Key should be order-independent"

    def test_make_key_different_for_different_epochs(self):
        from coderadar.query.cache import QueryCache
        k1 = QueryCache.make_key("t1", {"a": 1}, 1)
        k2 = QueryCache.make_key("t1", {"a": 1}, 2)
        assert k1 != k2, "Epoch change must produce different key"


# ═══════════════════════════════════════════════════════════════════════════
# Embedding Dedup Tests
# ═══════════════════════════════════════════════════════════════════════════

class TestEmbeddingDedup:
    """Content-addressed embedding cache with xxHash dedup."""

    def test_dedup_initialization(self):
        from coderadar.embedding.dedup import EmbeddingDedup
        dedup = EmbeddingDedup()
        assert dedup.model_name == "jinaai/jina-code-embeddings-0.5b"
        assert dedup.dimension == 896
        assert dedup.truncated_dimension == 64
        assert dedup.batch_size == 32

    def test_metrics_start_at_zero(self):
        from coderadar.embedding.dedup import EmbeddingDedup
        dedup = EmbeddingDedup()
        assert dedup.metrics["cache_hit"] == 0
        assert dedup.metrics["cache_miss"] == 0

    def test_cache_hit_rate_zero_initially(self):
        from coderadar.embedding.dedup import EmbeddingDedup
        dedup = EmbeddingDedup()
        assert dedup.cache_hit_rate() == 0.0

    def test_cache_hit_rate_computation(self):
        from coderadar.embedding.dedup import EmbeddingDedup
        dedup = EmbeddingDedup()
        dedup.metrics["cache_hit"] = 85
        dedup.metrics["cache_miss"] = 15
        assert dedup.cache_hit_rate() == 0.85

    def test_content_hash_computation(self):
        from coderadar.embedding.dedup import compute_content_hash
        h1 = compute_content_hash(b"hello")
        h2 = compute_content_hash(b"hello")
        h3 = compute_content_hash(b"world")
        assert len(h1) > 0
        assert h1 == h2, "Same content must produce same hash"
        assert h1 != h3, "Different content must produce different hash"

    def test_embed_target_creation(self):
        from coderadar.embedding.dedup import EmbedTarget
        t = EmbedTarget("func_1", "def foo(): pass", "abc123", "function")
        assert t.id == "func_1"
        assert t.body == "def foo(): pass"
        assert t.content_hash == "abc123"
        assert t.kind == "function"

    def test_embed_batch_cache_hit(self):
        """When DB returns a cached embedding, we skip recomputation."""
        from coderadar.embedding.dedup import EmbeddingDedup, EmbedTarget
        dedup = EmbeddingDedup()
        mock_db = MagicMock()

        target = EmbedTarget("fn1", "def foo(): pass", "hash123", "function")

        # Both _get_cached AND _model_embed must be mocked to avoid
        # loading the real fastembed model.
        with patch.object(dedup, "_get_cached", return_value=[0.1] * 896):
            with patch.object(dedup, "_model_embed", return_value=[]):
                results = dedup.embed_batch([target], mock_db)
                # cache hit → embedding not recomputed
                assert results == [None]
                assert dedup.metrics["cache_hit"] == 1

    def test_embed_batch_cache_miss_triggers_embed(self):
        """When DB has no cached embedding, we call the model."""
        from coderadar.embedding.dedup import EmbeddingDedup, EmbedTarget
        dedup = EmbeddingDedup()
        mock_db = MagicMock()

        target = EmbedTarget("fn2", "def bar(): pass", "hash456", "function")

        with patch.object(dedup, "_get_cached", return_value=None):
            with patch.object(dedup, "_model_embed", return_value=[[0.5] * 896]):
                results = dedup.embed_batch([target], mock_db)
                assert len(results) == 1
                assert results[0] is not None
                assert len(results[0]) == 896


# ═══════════════════════════════════════════════════════════════════════════
# Mutation Tool Router Tests
# ═══════════════════════════════════════════════════════════════════════════

class TestMutationToolRouter:
    """LLM tool call routing to the four mutation planners."""

    @pytest.fixture
    def router(self):
        from coderadar.mutation.tool_router import ToolRouter
        return ToolRouter(graph=None, dry_run=True)

    def test_unknown_tool_returns_error(self, router):
        from coderadar.mutation.tool_router import ToolCall
        call = ToolCall(
            tool_name="nonexistent_tool",
            arguments={},
            call_id="call_1",
        )
        result = router.route(call)
        assert result.success is False
        assert "Unknown tool" in result.error

    def test_replace_entity_body_rejects_without_graph(self, router):
        from coderadar.mutation.tool_router import ToolCall
        call = ToolCall(
            tool_name="replace_entity_body",
            arguments={"entity_id": "x", "new_body": "pass"},
            call_id="call_2",
        )
        result = router.route(call)
        assert result.success is False
        assert "No graph available" in result.error

    def test_update_signature_rejects_without_graph(self, router):
        from coderadar.mutation.tool_router import ToolCall
        call = ToolCall(
            tool_name="update_signature",
            arguments={"entity_id": "x", "new_signature": "def foo(x: int):"},
            call_id="call_3",
        )
        result = router.route(call)
        assert not result.success

    def test_rename_symbol_rejects_without_graph(self, router):
        from coderadar.mutation.tool_router import ToolCall
        call = ToolCall(
            tool_name="rename_symbol",
            arguments={"entity_id": "x", "new_name": "bar"},
            call_id="call_4",
        )
        result = router.route(call)
        assert not result.success

    def test_create_entity_rejects_without_graph(self, router):
        from coderadar.mutation.tool_router import ToolCall
        call = ToolCall(
            tool_name="create_entity",
            arguments={"target_file": "a.py", "anchor": "end", "code": "pass"},
            call_id="call_5",
        )
        result = router.route(call)
        assert not result.success

    def test_all_four_tool_names_are_recognized(self, router):
        """No unknown-tool errors for the four spec-defined tools."""
        from coderadar.mutation.tool_router import ToolCall
        # Provide the minimum required args for each tool so they route past validation
        tool_args = {
            "replace_entity_body": {"entity_id": "x", "new_body": "pass"},
            "update_signature": {"entity_id": "x", "new_signature": "def f(): pass"},
            "rename_symbol": {"entity_id": "x", "new_name": "y"},
            "create_entity": {"target_file": "a.py", "anchor": "end", "code": "pass"},
        }
        for tool in ["replace_entity_body", "update_signature",
                      "rename_symbol", "create_entity"]:
            call = ToolCall(tool_name=tool, arguments=tool_args[tool], call_id="c")
            result = router.route(call)
            # Should fail with "No graph" not "Unknown tool"
            assert "Unknown tool" not in (result.error or ""), \
                f"Tool {tool} should be recognized, got: {result.error}"

    def test_router_does_not_claim_to_enforce_policy(self, router):
        """Policy lives in MutationEngine::apply — the FFI is the trust boundary.

        The router used to carry a check_policy stub that returned True and was
        never called, which read as an implemented gate.
        """
        assert not hasattr(router, "check_policy")

    def test_tool_call_dataclass_fields(self):
        from coderadar.mutation.tool_router import ToolCall, ToolResult
        call = ToolCall("replace_entity_body", {"k": "v"}, "id1")
        assert call.tool_name == "replace_entity_body"
        assert call.arguments == {"k": "v"}
        assert call.call_id == "id1"

        result = ToolResult("id1", True, {"status": "ok"})
        assert result.success is True
        assert result.error is None


# ═══════════════════════════════════════════════════════════════════════════
# LSP Pool Tests
# ═══════════════════════════════════════════════════════════════════════════

class TestLSPPool:
    """Persistent LSP warm pool lifecycle."""

    @pytest.fixture
    def pool(self):
        from coderadar.lsp.pool import LSPPool
        return LSPPool(enabled=False)

    def test_disabled_pool_never_enables(self, pool):
        assert pool.is_enabled("python") is False
        assert pool.is_enabled("typescript") is False

    def test_enabled_pool_for_configured_language(self):
        from coderadar.lsp.pool import LSPPool
        enabled = LSPPool(enabled=True)
        assert enabled.is_enabled("python") is True
        assert enabled.is_enabled("rust") is True
        assert enabled.is_enabled("unknown_lang") is False

    def test_ensure_server_returns_none_when_disabled(self, pool):
        result = pool.ensure_server("python", "/workspace")
        assert result is None

    def test_sync_file_noops_when_disabled(self, pool):
        # Should not raise
        pool.sync_file("test.py", "x = 1", "python", "/ws")

    def test_definition_noops_when_disabled(self, pool):
        result = pool.definition("test.py", 1, 1, "hash", "python", "/ws")
        assert result is None

    def test_shutdown_clears_servers_and_cache(self, pool):
        pool.shutdown()  # Should not raise
        assert len(pool._servers) == 0
        assert len(pool._cache) == 0

    def test_cache_invalidation_by_prefix(self, pool):
        pool._cache = {("a.py", 1, 1, "h1"): "result1",
                       ("b.py", 1, 1, "h2"): "result2"}
        pool._invalidate_prefix("a.py")
        assert ("a.py", 1, 1, "h1") not in pool._cache
        assert ("b.py", 1, 1, "h2") in pool._cache  # untouched

    def test_override_threshold_default(self, pool):
        assert pool.override_threshold == 0.90

    def test_lsp_override_dataclass(self):
        from coderadar.lsp.pool import LSPOverride
        override = LSPOverride(
            edge_id="e1", target_file="a.py",
            target_line=10, target_column=5,
        )
        assert override.confidence == 1.0
        assert override.edge_id == "e1"

    def test_managed_server_bump_version(self):
        from coderadar.lsp.pool import ManagedServer
        server = ManagedServer("python", "cmd", "/ws")
        assert server.is_open("f.py") is False
        v1 = server.bump_version("f.py")
        v2 = server.bump_version("f.py")
        assert v2 == v1 + 1


# ═══════════════════════════════════════════════════════════════════════════
# Visualizer Tests
# ═══════════════════════════════════════════════════════════════════════════

class TestVisualizers:
    """Mermaid and Graphviz output generators."""

    def test_mermaid_hierarchy_renders_classdiagram(self):
        from coderadar.visualizers.mermaid import generate_mermaid
        result = generate_mermaid("hierarchy", [])
        assert "classDiagram" in result

    def test_mermaid_call_graph_renders_flowchart(self):
        from coderadar.visualizers.mermaid import generate_mermaid
        result = generate_mermaid("call-graph", [])
        assert "flowchart" in result

    def test_mermaid_dependencies_renders_flowchart(self):
        from coderadar.visualizers.mermaid import generate_mermaid
        result = generate_mermaid("dependencies", [])
        assert "flowchart" in result

    def test_mermaid_unknown_type_shows_message(self):
        from coderadar.visualizers.mermaid import generate_mermaid
        result = generate_mermaid("bogus", [])
        assert "Unknown" in result

    def test_graphviz_dependency_graph_has_cluster(self):
        from coderadar.visualizers.graphviz_viz import generate_dot
        result = generate_dot("dependencies", [])
        assert "digraph Dependencies" in result
        assert "rankdir=LR" in result

    def test_graphviz_hierarchy(self):
        from coderadar.visualizers.graphviz_viz import generate_dot
        result = generate_dot("hierarchy", [])
        assert "digraph Hierarchy" in result

    def test_call_graph_fan_out(self):
        from coderadar.visualizers.call_graph import generate_call_graph
        result = generate_call_graph(["main", "out", "3", "0.7"])
        assert "flowchart TD" in result
        assert "main" in result

    def test_call_graph_fan_in(self):
        from coderadar.visualizers.call_graph import generate_call_graph
        result = generate_call_graph(["auth.login", "in"])
        assert "flowchart TD" in result

    def test_safe_id_replaces_dots(self):
        from coderadar.visualizers.call_graph import _safe_id
        assert _safe_id("app.services.UserService") == "app_services_UserService"
        assert _safe_id("path::toplevel") == "path__toplevel"

    def test_scc_detection_identifies_cycle(self):
        from coderadar.visualizers.graphviz_viz import _find_sccs
        edges = [("a", "b"), ("b", "a")]
        sccs = _find_sccs(edges)
        assert len(sccs) == 1
        assert sccs[0] == {"a", "b"}

    def test_scc_detection_no_cycle(self):
        from coderadar.visualizers.graphviz_viz import _find_sccs
        edges = [("a", "b"), ("b", "c")]
        sccs = _find_sccs(edges)
        assert all(len(s) == 1 for s in sccs)


# ═══════════════════════════════════════════════════════════════════════════
# CodeRadar API Tests (stubs — verify interface contracts)
# ═══════════════════════════════════════════════════════════════════════════

class TestCodeRadarAPI:
    """Verify the Python API matches the spec contract (§8)."""

    def test_import_top_level(self):
        import coderadar
        assert hasattr(coderadar, "analyze")
        assert hasattr(coderadar, "CodeGraph")
        assert hasattr(coderadar, "UpdateReport")
        assert hasattr(coderadar, "MutationPlan")
        assert hasattr(coderadar, "MutationResult")

    def test_update_report_dataclass(self):
        from coderadar import UpdateReport
        report = UpdateReport(
            affected_files=["a.py"],
            changed_symbols=[],
            new_unresolved_references=[],
            newly_resolved_references=[],
            elapsed_ms=15.0,
            parse_quality="Clean",
            parse_errors=0,
            fully_applied=True,
            epoch_before=1,
            epoch_after=2,
        )
        assert report.fully_applied is True
        assert report.affected_files == ["a.py"]

    def test_stale_handle_exception(self):
        from coderadar import StaleHandle
        err = StaleHandle("entity changed")
        assert str(err) == "entity changed"
        assert isinstance(err, Exception)

    def test_mutation_error_hierarchy(self):
        from coderadar import MutationError, PolicyViolation
        assert issubclass(PolicyViolation, MutationError)

    def test_symbol_change_dataclass(self):
        from coderadar import SymbolChange
        sc = SymbolChange(
            kind="function",
            operation="body_changed",
            qualified_name="app.services.foo",
            file="a.py",
            line=42,
        )
        assert sc.kind == "function"
        assert sc.operation == "body_changed"

    def test_codegraph_instantiation(self):
        from coderadar import CodeGraph
        graph = CodeGraph()
        assert graph is not None

    def test_codegraph_stats_returns_dict(self):
        from coderadar import CodeGraph
        graph = CodeGraph()
        stats = graph.stats()
        assert isinstance(stats, dict)
        assert "modules" in stats
        assert "functions" in stats

    def test_codegraph_batch_context_manager(self):
        from coderadar import CodeGraph
        graph = CodeGraph()
        with graph.batch() as b:
            b.update_file("a.py", "def foo(): pass\n")
        # Should not raise

    def test_compute_embeddings_writes_in_one_bulk_call(self, monkeypatch):
        """Looping set_embedding cloned the whole projection per entity."""
        try:
            from coderadar._core import analyze
            import coderadar._core as core
            from coderadar import CodeGraph
        except ImportError:
            pytest.skip("Rust _core extension not built")
        from pathlib import Path
        e2e = Path(__file__).parent / "fixtures" / "python" / "e2e_project"
        analyze(str(e2e))

        calls = []
        real_bulk = core.set_embeddings_bulk

        def counting_bulk(entries):
            calls.append(len(entries))
            return real_bulk(entries)

        monkeypatch.setattr(core, "set_embeddings_bulk", counting_bulk)

        report = CodeGraph().compute_embeddings()
        assert isinstance(report, dict)
        if report["generated"]:
            assert len(calls) == 1, f"expected one bulk write, got {len(calls)}"
            assert calls[0] == report["generated"] + report["errors"]

    def test_update_file_reports_failure(self):
        try:
            from coderadar._core import analyze
            from coderadar import CodeGraph
        except ImportError:
            pytest.skip("Rust _core extension not built")
        from pathlib import Path
        e2e = Path(__file__).parent / "fixtures" / "python" / "e2e_project"
        analyze(str(e2e))
        graph = CodeGraph()
        report = graph.update_file(r".\__definitely_missing__.py")
        assert report.fully_applied is False
        assert report.parse_errors >= 1
        assert "Error" in report.parse_quality

    def test_update_file_reports_a_recovered_parse(self, tmp_path):
        """The report used to be hardcoded clean, so this branch was dead."""
        try:
            from coderadar._core import analyze
            from coderadar import CodeGraph
        except ImportError:
            pytest.skip("Rust _core extension not built")
        target = tmp_path / "recovered.py"
        target.write_text("def f():\n    return 1\n", encoding="utf-8")
        analyze(str(tmp_path))

        graph = CodeGraph()
        report = graph.update_file(str(target), "def f(:\n    return 1\n")
        assert report.fully_applied is False
        assert report.parse_errors >= 1
        assert report.parse_quality == "partial"

    def test_update_file_reports_real_timing_on_a_clean_parse(self, tmp_path):
        try:
            from coderadar._core import analyze
            from coderadar import CodeGraph
        except ImportError:
            pytest.skip("Rust _core extension not built")
        target = tmp_path / "clean.py"
        target.write_text("def f():\n    return 1\n", encoding="utf-8")
        analyze(str(tmp_path))

        graph = CodeGraph()
        report = graph.update_file(str(target), "def f():\n    return 2\n")
        assert report.fully_applied is True
        assert report.parse_quality == "clean"
        assert report.parse_errors == 0
        assert report.elapsed_ms > 0.0, "elapsed_ms was hardcoded to 0.0"

    def test_remove_file_drops_a_deleted_files_entities(self, tmp_path):
        """A deleted file used to live on in the graph until the next analyze."""
        try:
            from coderadar._core import analyze, search_entities
            from coderadar import CodeGraph
        except ImportError:
            pytest.skip("Rust _core extension not built")
        gone = tmp_path / "gone.py"
        gone.write_text("def doomed():\n    return 1\n", encoding="utf-8")
        (tmp_path / "kept.py").write_text(
            "def survivor():\n    return 2\n", encoding="utf-8")
        analyze(str(tmp_path))
        assert any(e["name"] == "doomed" for e in search_entities("doomed", 20, None))

        graph = CodeGraph()
        gone.unlink()
        removed = graph.remove_file(str(gone))

        assert removed >= 1, "the module and its function should both go"
        assert not any(e["name"] == "doomed" for e in search_entities("doomed", 20, None))
        assert any(e["name"] == "survivor" for e in search_entities("survivor", 20, None))

    def test_remove_file_reports_zero_for_a_file_that_was_never_indexed(self, tmp_path):
        try:
            from coderadar._core import analyze
            from coderadar import CodeGraph
        except ImportError:
            pytest.skip("Rust _core extension not built")
        (tmp_path / "only.py").write_text("x = 1\n", encoding="utf-8")
        analyze(str(tmp_path))

        assert CodeGraph().remove_file(str(tmp_path / "never.py")) == 0


# ═══════════════════════════════════════════════════════════════════════════
# Pest Query Grammar Tests (parseable examples from §7.2a)
# ═══════════════════════════════════════════════════════════════════════════

class TestPestQueries:
    """Verify all §7.2a example queries can be parsed (when Rust is built)."""

    EXAMPLE_QUERIES = [
        'classes where inherits_from contains "BaseModel"',
        'functions where line_count > 50',
        'functions where caller_count == 0 and not name matches "^test_.*"',
        'classes where method_count > 20 order by method_count desc limit 25',
        'functions where module.name == "app.services" and is_async == true',
        'classes where has_method("__init__") == true and has_method("__eq__") == false',
        'classes select module.name, count(*) as class_count, avg(method_count) as avg_methods group by module.name order by class_count desc limit 20',
        'calls where unresolved_reason == "TypeInferenceRequired"',
        'functions where decorators contains "deprecated"',
        'imports where kind == "StarImport"',
        'functions where kind == "Property" and has_setter == false',
        'functions where overrides_of("BaseService.handle") == true',
    ]

    @pytest.mark.parametrize("query", EXAMPLE_QUERIES)
    def test_query_parses(self, query):
        """Each example query should parse without error."""
        try:
            from coderadar._core import query_graph
            import coderadar
            # If Rust extension is built, try parsing
            graph = coderadar.CodeGraph()
            results = graph.query(query)
            # Just verifying no exception
        except ImportError:
            pytest.skip("Rust extension not built")
        except Exception as e:
            pytest.fail(f"Query '{query}' failed: {e}")
