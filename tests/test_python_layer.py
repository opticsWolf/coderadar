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
        assert cfg.project.roots == []
        assert cfg.resolution.min_confidence == 0.3
        assert cfg.embedding.dimension == 384
        assert cfg.mutation.enabled is True
        assert cfg.query.default_top_k == 10

    def test_watch_defaults_are_the_watcher_defaults(self):
        """[watch] replaced the harness file's general section.

        `HarnessConfig`/`.harness/config.toml` are gone (the file went in
        105762e), and the watcher's two settings moved into `[watch]`, where
        `Watcher.__init__` actually reads them.
        """
        from coderadar.config import CodeRadarConfig
        cfg = CodeRadarConfig()
        assert cfg.watch.debounce_ms == 100
        assert cfg.watch.max_file_size_bytes == 1_048_576

    def test_load_config_from_file(self):
        """Load from a real .coderadar.toml file on disk."""
        from coderadar.config import CodeRadarConfig, load_config
        cfg = load_config(Path(__file__).parent.parent)
        assert isinstance(cfg, CodeRadarConfig)
        # This repo's own .coderadar.toml leaves `roots` out on purpose (its
        # sources live under two top-level directories) and sets `exclude`.
        assert cfg.project.roots == []
        assert "**/__pycache__/**" in cfg.project.exclude

    def test_custom_config_override(self):
        """Pydantic model should accept field overrides."""
        from coderadar.config import (
            CodeRadarConfig, ImportGraphConfig, ProjectConfig, ResolutionConfig
        )
        cfg = CodeRadarConfig(
            project=ProjectConfig(roots=["src/", "lib/"]),
            resolution=ResolutionConfig(
                import_graph=ImportGraphConfig(max_import_depth=20)
            ),
        )
        assert cfg.project.roots == ["src/", "lib/"]
        assert cfg.resolution.import_graph.max_import_depth == 20

    def test_mutation_config_deny_list(self):
        """Deny list should contain sensitive paths."""
        from coderadar.config import MutationConfig
        cfg = MutationConfig()
        assert ".git/" in cfg.deny
        # `.harness/` was the old name; the store lives under `.coderadar/`.
        assert ".coderadar/" in cfg.deny

    def test_lsp_server_commands(self):
        """Default LSP server commands for known languages."""
        from coderadar.config import LSPConfig
        cfg = LSPConfig()
        assert cfg.servers["python"] == "pyright-langserver --stdio"
        assert cfg.servers["typescript"] == "typescript-language-server --stdio"
        assert cfg.servers["rust"] == "rust-analyzer"

    def test_embedding_config_dimensions(self):
        """Embedding config carries both full and truncated dimensions.

        384 is BAAI/bge-small-en-v1.5, the model both the index path and the
        search path load. The pair moves together or similarity breaks.
        """
        from coderadar.config import EmbeddingConfig
        cfg = EmbeddingConfig()
        assert cfg.dimension == 384
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
        assert dedup.model_name == "BAAI/bge-small-en-v1.5"
        assert dedup.dimension == 384
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

    def test_missing_fastembed_raises_instead_of_returning_zero_vectors(self):
        """A placeholder vector is indistinguishable from a real one downstream.

        Stored with a valid content hash it makes `has_embedding` true, the
        dedup cache treats it as fresh for ever, and semantic search is
        silently and permanently poisoned. It must fail loudly instead.
        """
        import builtins
        from coderadar.embedding.dedup import EmbeddingDedup, EmbeddingUnavailable

        dedup = EmbeddingDedup()
        real_import = builtins.__import__

        def no_fastembed(name, *args, **kwargs):
            if name == "fastembed" or name.startswith("fastembed."):
                raise ImportError("No module named 'fastembed'")
            return real_import(name, *args, **kwargs)

        with patch.object(builtins, "__import__", no_fastembed):
            with pytest.raises(EmbeddingUnavailable):
                dedup._model_embed(["def foo(): pass"])

    def test_embed_batch_propagates_the_unavailable_model(self):
        """The failure must reach the caller, not be swallowed into a batch."""
        from coderadar.embedding.dedup import (
            EmbeddingDedup, EmbedTarget, EmbeddingUnavailable,
        )
        dedup = EmbeddingDedup()
        target = EmbedTarget("fn3", "def baz(): pass", "hash789", "function")

        with patch.object(dedup, "_get_cached", return_value=None):
            with patch.object(dedup, "_model_embed",
                              side_effect=EmbeddingUnavailable("no model")):
                with pytest.raises(EmbeddingUnavailable):
                    dedup.embed_batch([target], MagicMock())


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

    def test_route_reaches_the_real_planner(self, tmp_path):
        """The graph=None tests never touched a planner.

        The router's live branches assume attribute access (`plan.id`,
        `plan.diff_preview`), so this pins them against the facade that
        actually answers — which returns `MutationPlan`, not a dict.
        """
        try:
            from coderadar._core import analyze
        except ImportError:
            pytest.skip("Rust _core extension not built")
        from coderadar import CodeGraph
        from coderadar.mutation.tool_router import ToolCall, ToolRouter

        target = tmp_path / "m.py"
        target.write_text("def f(a):\n    return a\n", encoding="utf-8")
        analyze(str(tmp_path))
        graph = CodeGraph()

        from coderadar._core import search_entities
        hits = search_entities("f", 10, kind="function")
        assert hits, "fixture function was not indexed"
        entity_id = hits[0]["id"]

        result = ToolRouter(graph=graph, dry_run=True).route(ToolCall(
            tool_name="replace_entity_body",
            arguments={"entity_id": entity_id,
                       "new_body": "    return a + 1\n"},
            call_id="call_live",
        ))

        assert result.success is True, result.error
        assert result.result["plan"], "planner returned no plan id"

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


# ═══════════════════════════════════════════════════════════════════════════
# Query executor: lazy rows and limit pushdown (plan §2.4)
# ═══════════════════════════════════════════════════════════════════════════


class TestQueryExecution:
    """The executor used to materialise every row before filtering it.

    These pin the observable contract that the laziness must not change:
    same rows, same fields, same ordering.
    """

    @staticmethod
    @pytest.fixture(scope="class")
    def indexed(tmp_path_factory):
        try:
            from coderadar._core import analyze
        except ImportError:
            pytest.skip("Rust _core extension not built")
        root = tmp_path_factory.mktemp("queryable")
        (root / "a.py").write_text(
            "import os\n"
            "\n"
            "def alpha():\n"
            "    return 1\n"
            "\n"
            "def beta():\n"
            "    return alpha()\n"
            "\n"
            "class Widget:\n"
            "    def one(self): pass\n"
            "    def two(self): pass\n",
            encoding="utf-8",
        )
        (root / "b.py").write_text(
            "def gamma():\n    return 3\n\nclass Gadget:\n    def only(self): pass\n",
            encoding="utf-8",
        )
        analyze(str(root))
        import coderadar
        return coderadar.CodeGraph()

    def test_where_returns_only_matching_rows(self, indexed):
        rows = list(indexed.query('functions where name == "alpha"'))
        assert [r["name"] for r in rows] == ["alpha"]

    def test_a_filtered_row_still_carries_every_field(self, indexed):
        """Survivors are built with the full field set, not the probe set."""
        rows = list(indexed.query('functions where name == "beta"'))
        assert len(rows) == 1, rows
        for key in ("name", "line", "line_count", "is_async", "caller_count",
                    "callee_count", "parameter_count"):
            assert key in rows[0], f"{key} missing from {sorted(rows[0])}"

    def test_select_projects_and_the_predicate_field_may_be_absent(self, indexed):
        rows = list(indexed.query('functions select name where name == "gamma"'))
        assert rows and all(set(r) == {"name"} for r in rows), rows

    def test_limit_stops_the_scan_without_changing_the_shape(self, indexed):
        assert len(list(indexed.query("functions limit 2"))) == 2

    def test_limit_with_order_by_still_takes_the_top_rows(self, indexed):
        """Pushdown must not apply here: ORDER BY needs every row first."""
        rows = list(indexed.query("classes order by method_count desc limit 1"))
        assert len(rows) == 1
        assert rows[0]["method_count"] == 2, rows

    def test_a_derived_call_predicate_still_sees_its_fields(self, indexed):
        """`contains(...)` reads fields the predicate never names, so the
        executor has to fall back to building all of them."""
        rows = list(indexed.query('imports where kind contains "Module"'))
        assert rows, "the os import should match"

    def test_unmatched_predicate_returns_no_rows(self, indexed):
        assert list(indexed.query('functions where name == "nonexistent_zzz"')) == []


# ═══════════════════════════════════════════════════════════════════════════
# Configuration reaching the Rust core (plan §3, step A)
# ═══════════════════════════════════════════════════════════════════════════


class TestConfigReachesTheCore:
    """`.coderadar.toml` used to be read by nobody.

    Every consumer built `GraphConfig::default()` at its own construction
    site, so the whole documented config surface had zero effect. These pin
    that a pushed config actually lands, and that keys with no mapping say so
    instead of appearing to work.
    """

    @pytest.fixture(autouse=True)
    def _reset_config(self):
        """set_config is process-wide; put the defaults back afterwards."""
        try:
            from coderadar._core import set_config
        except ImportError:
            pytest.skip("Rust _core extension not built")
        yield
        set_config({})

    def test_applied_keys_come_back(self):
        from coderadar._core import set_config
        report = set_config({"mutation": {"max_edits_per_plan": 7}})
        assert report["applied"]["mutation.max_edits_per_plan"] == 7
        assert report["ignored"] == []

    def test_the_value_is_readable_afterwards(self):
        from coderadar._core import set_config, get_config
        set_config({"resolution": {"import_graph": {"max_import_depth": 9}}})
        assert get_config()["resolution"]["import_graph"]["max_import_depth"] == 9

    def test_unmapped_keys_are_reported_not_silently_dropped(self):
        """The whole point: a knob that does nothing must say so."""
        from coderadar._core import set_config
        report = set_config({
            "llm": {"provider": "openai", "model": "gpt-4o"},
            "database": {"hnsw_m": 16},
            "mutation": {"enabled": True},
        })
        assert report["ignored"] == [
            "database.hnsw_m", "llm.model", "llm.provider",
        ]
        assert "mutation.enabled" in report["applied"]

    def test_an_empty_config_restores_the_defaults(self):
        """set_config replaces, it does not merge — each call starts from
        GraphConfig::default(), so a removed key reverts."""
        from coderadar._core import set_config, get_config
        set_config({"mutation": {"max_files_per_plan": 3}})
        assert get_config()["mutation"]["max_files_per_plan"] == 3
        set_config({})
        assert get_config()["mutation"]["max_files_per_plan"] == 100

    def test_a_wrong_type_names_the_key(self):
        from coderadar._core import set_config
        with pytest.raises(TypeError) as exc:
            set_config({"mutation": {"max_edits_per_plan": "lots"}})
        assert "mutation.max_edits_per_plan" in str(exc.value)

    def test_a_scalar_where_a_table_belongs_is_an_error(self):
        from coderadar._core import set_config
        with pytest.raises(TypeError) as exc:
            set_config({"mutation": 5})
        assert "mutation" in str(exc.value)

    def test_lists_survive_the_crossing(self):
        from coderadar._core import set_config, get_config
        set_config({"mutation": {"deny": ["vendor/", "/*.lock"]}})
        assert get_config()["mutation"]["deny"] == ["vendor/", "/*.lock"]


class TestConfiguredMutationPolicy:
    """The mutation policy gate is the one consumer that was already live —
    it just never saw a configured value."""

    @pytest.fixture(autouse=True)
    def _reset_config(self):
        try:
            from coderadar._core import set_config
        except ImportError:
            pytest.skip("Rust _core extension not built")
        yield
        set_config({})

    @staticmethod
    def _two_edit_plan(target):
        from coderadar import MutationEdit, MutationPlan
        edit = MutationEdit(file=str(target), replacement="value = 2\n",
                            span_start=0, span_end=10)
        return MutationPlan(
            id="two-edits", tool="create_entity",
            edits=[edit, edit],
            affected_files=[str(target)],
            diff_preview="", unverified_sites=[], warnings=[],
        )

    def test_configured_edit_limit_refuses_the_plan(self, tmp_path):
        from coderadar._core import analyze, set_config
        from coderadar import CodeGraph
        target = tmp_path / "mod.py"
        target.write_text("value = 1\n", encoding="utf-8")
        analyze(str(tmp_path))

        set_config({"mutation": {"max_edits_per_plan": 1}})
        result = CodeGraph().apply(self._two_edit_plan(target))
        assert result.status == "RejectedPolicy"
        assert target.read_text(encoding="utf-8") == "value = 1\n"

    def test_the_same_plan_passes_the_gate_under_the_default_limit(self, tmp_path):
        """Proves the refusal above came from the config, not from the plan."""
        from coderadar._core import analyze, set_config
        from coderadar import CodeGraph
        target = tmp_path / "mod.py"
        target.write_text("value = 1\n", encoding="utf-8")
        analyze(str(tmp_path))

        set_config({})
        result = CodeGraph().apply(self._two_edit_plan(target))
        assert result.status != "RejectedPolicy", result.status

    def test_disabling_mutation_refuses_everything(self, tmp_path):
        from coderadar._core import analyze, set_config
        from coderadar import CodeGraph
        target = tmp_path / "mod.py"
        target.write_text("value = 1\n", encoding="utf-8")
        analyze(str(tmp_path))

        set_config({"mutation": {"enabled": False}})
        result = CodeGraph().apply(self._two_edit_plan(target))
        assert result.status == "RejectedPolicy"


# ═══════════════════════════════════════════════════════════════════════════
# Configuration changing what gets indexed (plan §3, step B)
# ═══════════════════════════════════════════════════════════════════════════


class TestConfiguredWalk:
    """`[project] roots` and `exclude` reach the walk `analyze` runs."""

    @pytest.fixture(autouse=True)
    def _reset_config(self):
        try:
            from coderadar._core import set_config
        except ImportError:
            pytest.skip("Rust _core extension not built")
        yield
        set_config({})

    @staticmethod
    def _project(root):
        (root / "keep.py").write_text("def kept(): pass\n", encoding="utf-8")
        (root / "vendor").mkdir()
        (root / "vendor" / "dep.py").write_text("def vendored(): pass\n",
                                                encoding="utf-8")
        (root / "app").mkdir()
        (root / "app" / "main.py").write_text("def app_fn(): pass\n",
                                              encoding="utf-8")

    @staticmethod
    def _names():
        from coderadar._core import search_entities
        return {e["name"] for e in search_entities("", 200, "function")}

    def test_without_config_everything_is_indexed(self, tmp_path):
        from coderadar._core import analyze, set_config
        self._project(tmp_path)
        set_config({})
        analyze(str(tmp_path))
        assert {"kept", "vendored", "app_fn"} <= self._names()

    def test_exclude_keeps_a_directory_out_of_the_index(self, tmp_path):
        from coderadar._core import analyze, set_config
        self._project(tmp_path)
        set_config({"project": {"exclude": ["vendor/**"]}})
        analyze(str(tmp_path))
        names = self._names()
        assert "vendored" not in names, names
        assert {"kept", "app_fn"} <= names, names

    def test_roots_narrows_the_walk(self, tmp_path):
        from coderadar._core import analyze, set_config
        self._project(tmp_path)
        set_config({"project": {"roots": ["app"]}})
        analyze(str(tmp_path))
        names = self._names()
        assert "app_fn" in names, names
        assert "kept" not in names and "vendored" not in names, names

    def test_empty_roots_means_the_whole_project(self, tmp_path):
        """The default must not narrow anything — a wrong default here is a
        silently truncated index."""
        from coderadar._core import analyze, get_config, set_config
        self._project(tmp_path)
        set_config({})
        assert get_config()["project"]["roots"] == []
        analyze(str(tmp_path))
        assert {"kept", "vendored", "app_fn"} <= self._names()


class TestConfiguredStorePath:
    @pytest.fixture(autouse=True)
    def _reset_config(self):
        try:
            from coderadar._core import set_config
        except ImportError:
            pytest.skip("Rust _core extension not built")
        yield
        set_config({})

    def test_store_lands_where_the_config_says(self, tmp_path):
        from coderadar._core import analyze, set_config
        (tmp_path / "m.py").write_text("def f(): pass\n", encoding="utf-8")
        set_config({"database": {"path": "custom/place/graph.db"}})
        analyze(str(tmp_path))
        assert (tmp_path / "custom" / "place" / "graph.db").exists()

    def test_the_default_store_path_is_unchanged(self, tmp_path):
        from coderadar._core import analyze, set_config
        (tmp_path / "m.py").write_text("def f(): pass\n", encoding="utf-8")
        set_config({})
        analyze(str(tmp_path))
        assert (tmp_path / ".coderadar" / "store" / "coderadar.db").exists()


class TestEmbeddingModelAgreement:
    """Index-time and query-time must name the same model.

    They used to disagree — jina/896 in the config and EmbeddingDedup,
    BAAI/384 in compute_embeddings and the MCP search path — which is a
    search that returns confident nonsense rather than an error.
    """

    def test_one_source_for_model_and_dimension(self):
        from coderadar.embedding import embedding_settings
        model, dimension = embedding_settings()
        assert isinstance(model, str) and model
        assert dimension > 0

    def test_dedup_defaults_match_the_configured_pair(self):
        from coderadar.embedding import EmbeddingDedup, embedding_settings
        model, dimension = embedding_settings()
        dedup = EmbeddingDedup()
        assert (dedup.model_name, dedup.dimension) == (model, dimension)

    def test_compute_embeddings_uses_the_configured_model(self):
        """Index-time takes its model from the same helper the search does."""
        from unittest.mock import MagicMock, patch
        import coderadar
        from coderadar.embedding import embedding_settings
        model, dimension = embedding_settings()

        fake = MagicMock()
        fake.return_value.embed_batch.return_value = []
        with patch("coderadar.embedding.EmbeddingDedup", fake):
            coderadar.CodeGraph().compute_embeddings()

        assert fake.call_args.kwargs["model_name"] == model
        assert fake.call_args.kwargs["dimension"] == dimension


class TestWatcherIsOneClass:
    """`__init__.py` defined `Watcher` twice with incompatible signatures.

    The stub came first and the live class shadowed it, so the documented
    `coderadar.watch(root)` — which called the stub — raised TypeError, and
    the iteration protocol it advertised belonged to the dead half.
    """

    def test_module_watch_builds_the_live_watcher(self):
        from unittest.mock import MagicMock, patch
        import coderadar

        graph = MagicMock()
        with patch("coderadar.analyze", return_value=graph) as analyze:
            watcher = coderadar.watch("src/")

        analyze.assert_called_once_with("src/")
        graph.watch.assert_called_once_with(["src/"])
        assert watcher is graph.watch.return_value

    def test_live_watcher_supports_with_and_for(self):
        import coderadar
        for attr in ("__enter__", "__exit__", "__iter__", "__next__"):
            assert hasattr(coderadar.Watcher, attr), attr

    def test_graph_watch_returns_that_class(self):
        import coderadar
        watcher = coderadar.CodeGraph().watch(["src/"])
        assert isinstance(watcher, coderadar.Watcher)
        assert watcher._paths == ["src/"]

    def test_batch_folds_into_one_report(self):
        """A batch touching three files yields one merged UpdateReport."""
        from unittest.mock import MagicMock
        import coderadar

        def report(quality, errors, applied, before, after):
            return coderadar.UpdateReport(
                affected_files=[], changed_symbols=[],
                new_unresolved_references=[], newly_resolved_references=[],
                elapsed_ms=1.0, parse_quality=quality, parse_errors=errors,
                fully_applied=applied, epoch_before=before, epoch_after=after,
            )

        graph = MagicMock()
        graph.update_file.side_effect = [
            report("Clean", 0, True, 4, 5),
            report("Partial", 2, True, 5, 6),
        ]
        graph.remove_file.return_value = 3

        watcher = coderadar.Watcher(graph, ["src/"])
        merged = watcher._apply([
            ("a.py", "Modify"), ("b.py", "Create"), ("c.py", "Delete"),
        ])

        assert merged.affected_files == ["a.py", "b.py", "c.py"]
        # Worst quality wins; the epochs span the batch.
        assert merged.parse_quality == "Partial"
        assert merged.parse_errors == 2
        assert (merged.epoch_before, merged.epoch_after) == (4, 6)
        assert merged.fully_applied is True

    def test_a_failing_file_clears_fully_applied(self):
        from unittest.mock import MagicMock
        import coderadar

        graph = MagicMock()
        graph.update_file.side_effect = RuntimeError("parse blew up")
        merged = coderadar.Watcher(graph, ["src/"])._apply([("a.py", "Modify")])

        assert merged.fully_applied is False
        assert merged.affected_files == []
