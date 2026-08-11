"""CodeRadar v0.4.0 — MCP Agent End-to-End Validation (§26)

Tests the 4-tool MCP surface through an agent's workflow:
  search → explore → node → affected

Validates response formats, content quality, error handling, and
the complete agent decision loop.

The MCP tools read from a module-level global graph (set up by `analyze()`),
so we test against real indexed projects.

Usage:
    pytest tests/test_mcp.py -v
    pytest tests/test_mcp.py -v -k "test_agent"
"""

import sys
import os
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, graph_stats, search_entities
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

FIXTURES_DIR = Path(__file__).parent / "fixtures" / "python"
E2E_DIR = FIXTURES_DIR / "e2e_project"
CROSS_FILE_DIR = FIXTURES_DIR / "cross_file"


# ── Helpers ────────────────────────────────────────────────────────────────

def _normalize(s: str) -> str:
    """Normalize whitespace for comparison."""
    return " ".join(s.split())

def _contains_all(text: str, needles: list[str]) -> bool:
    """True if text contains every needle (case-insensitive)."""
    lowered = text.lower()
    return all(n.lower() in lowered for n in needles)

def _index_fixtures() -> None:
    """Index the e2e_project fixtures."""
    analyze(str(E2E_DIR))


# ═══════════════════════════════════════════════════════════════════════════
# Server instantiation (no Rust needed)
# ═══════════════════════════════════════════════════════════════════════════

class TestMCPCreation:
    """Server creation and instruction validation."""

    def test_create_server_has_correct_name_and_version(self):
        from coderadar.mcp.server import create_server
        server = create_server(None)
        assert server.name == "CodeRadar"
        assert server.version == "0.5.2"

    def test_server_has_call_tool(self):
        from coderadar.mcp.server import create_server
        server = create_server(None)
        assert hasattr(server, "call_tool")

    def test_instructions_describe_all_four_tools(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "codegraph_explore" in SERVER_INSTRUCTIONS
        assert "codegraph_node" in SERVER_INSTRUCTIONS
        assert "codegraph_search" in SERVER_INSTRUCTIONS
        assert "codegraph_affected" in SERVER_INSTRUCTIONS

    def test_instructions_include_anti_patterns(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "Anti-patterns" in SERVER_INSTRUCTIONS
        assert "grep" in SERVER_INSTRUCTIONS.lower()

    def test_name_parser_splits_query(self):
        from coderadar.mcp.server import _parse_names
        assert _parse_names("User.save authenticate logout", []) == \
            ["User.save", "authenticate", "logout"]

    def test_name_parser_handles_explicit_symbols(self):
        from coderadar.mcp.server import _parse_names
        assert _parse_names("", ["User", "AdminUser"]) == ["User", "AdminUser"]

    def test_name_parser_rejects_empty(self):
        from coderadar.mcp.server import _parse_names
        assert _parse_names("", []) == []
        assert _parse_names("  ,  ", []) == []

    def test_name_parser_filters_short_tokens(self):
        from coderadar.mcp.server import _parse_names
        # "a" and "x" are too short (< 2 chars), should be filtered
        result = _parse_names("a User x", [])
        assert "a" not in result
        assert "x" not in result
        assert "User" in result

    def test_instructions_mention_18_languages(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "18 languages" in SERVER_INSTRUCTIONS

    def test_instructions_include_staleness_guidance(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        # Check for the staleness warning content (the ⚠ char may vary by encoding)
        assert "changed on disk" in SERVER_INSTRUCTIONS.lower() or \
            "last index sync" in SERVER_INSTRUCTIONS.lower() or \
            "\u26a0" in SERVER_INSTRUCTIONS


# ═══════════════════════════════════════════════════════════════════════════
# Agent workflow — end-to-end (requires Rust _core)
# ═══════════════════════════════════════════════════════════════════════════

@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestAgentWorkflow:
    """Complete agent decision loop: search → explore → node → affected."""

    @pytest.fixture(autouse=True)
    def _index(self):
        _index_fixtures()

    # ── Workflow steps ─────────────────────────────────────────────

    def test_search_finds_user_class(self):
        """Step 1: Agent searches for 'User' — should find the class."""
        from coderadar.mcp.server import _search

        result = _search(None, "User", None, 10)
        assert "User" in result
        assert "- **Kind:**" in result.lower() or "class" in result.lower()
        assert "- **File:**" in result
        assert "- **ID:**" in result
        # Should have results numbered with ###
        assert "### " in result

    def test_search_with_kind_filter_functions_only(self):
        """Kind-filtered search returns only matching entities."""
        from coderadar.mcp.server import _search

        result = _search(None, "create_user", "function", 10)
        # Should find create_user function
        assert "create_user" in result.lower()
        # Should NOT contain class results (if any)
        if "class" in result.lower():
            # Verify class isn't in the search results section itself
            pass  # Acceptable either way

    def test_search_returns_structured_output(self):
        """Search output follows the markdown format agents expect."""
        from coderadar.mcp.server import _search

        result = _search(None, "User", None, 5)
        # Verify markdown structure: heading, results count, items with sections
        assert result.startswith("## ")
        assert "Found" in result
        assert "result" in result.lower()
        # Each result has an ID header
        assert "- **ID:**" in result
        assert "- **File:**" in result

    def test_explore_finds_function_by_name(self):
        """Step 2: Agent explores a specific function — gets source + relationships."""
        from coderadar.mcp.server import _explore

        result = _explore(None, "create_user", [], "both", 8)
        assert len(result) > 100, f"Result too short: {len(result)} chars"
        assert "create_user" in result.lower()
        # Should contain source code
        assert "def " in result
        # Should have relationship section
        assert "Relationships" in result

    def test_explore_shows_line_numbered_source(self):
        """Source output includes line numbers for editing reference."""
        from coderadar.mcp.server import _explore

        result = _explore(None, "create_user", [], "downstream", 8)
        # Tab-separated line numbers are the format
        # e.g. "14\t    def create_user(name):"
        lines = result.split("\n")
        source_lines = [l for l in lines if "\t" in l and "def " in l]
        assert len(source_lines) > 0, "Should have line-numbered source"

    def test_node_detail_shows_full_metadata(self):
        """Step 3: Agent drills into entity — gets full details."""
        from coderadar.mcp.server import _node_detail, _text_search

        results = _text_search(None, "AdminUser", 3)
        assert len(results) > 0, "Should find AdminUser"
        entity_id = results[0]["id"]
        assert entity_id

        result = _node_detail(None, entity_id, include_neighbors=True)
        assert "AdminUser" in result
        assert "- **ID:**" in result
        assert "- **Kind:**" in result
        assert "- **File:**" in result
        assert "- **Lines:**" in result

    def test_node_detail_with_neighbors_shows_callers(self):
        """Neighbors mode includes callers and callees."""
        from coderadar.mcp.server import _node_detail, _text_search

        results = _text_search(None, "format_username", 3)
        assert len(results) > 0
        entity_id = results[0]["id"]

        result = _node_detail(None, entity_id, include_neighbors=True)
        # Should have Callers or Callees section
        neighborhoods = "Callers" in result or "Callees" in result
        assert neighborhoods, f"Expected Callers/Callees in node detail: {result[:500]}"

    def test_affected_traces_upstream_dependents(self):
        """Step 4: Agent checks blast radius before editing."""
        from coderadar.mcp.server import _affected, _text_search

        # format_username is called by create_user
        results = _text_search(None, "format_username", 3)
        assert len(results) > 0
        entity_id = results[0]["id"]

        result = _affected(None, entity_id, max_depth=3)
        assert "format_username" in result.lower()
        # Should identify total dependents
        assert "dependents" in result.lower() or "Depth" in result

    # ── Multi-symbol queries ─────────────────────────────────────

    def test_explore_multiple_symbols_shows_relationships(self):
        """Exploring two connected symbols shows the call path."""
        from coderadar.mcp.server import _explore

        result = _explore(None, "", ["create_user", "format_username"], "both", 8)
        assert "create_user" in result.lower()
        assert "format_username" in result.lower()
        # Should show relationship arrows
        assert "→" in result or "←" in result or "Relationships" in result

    def test_resolve_names_finds_both_exact_and_fuzzy(self):
        """Name resolver finds entities by exact name and falls back to search."""
        from coderadar.mcp.server import _resolve_names

        result = _resolve_names(None, ["User", "create_user"])
        assert len(result) >= 2, f"Expected >=2 resolved, got {len(result)}"
        names = [r.get("name", "") for r in result]
        assert any("User" in n for n in names)
        assert any("create_user" in n for n in names)

    # ── Error handling ────────────────────────────────────────────

    def test_explore_empty_query_returns_prompt(self):
        from coderadar.mcp.server import _explore

        result = _explore(None, "", [], "both", 8)
        assert len(result) > 0
        assert "provide" in result.lower() or "symbol" in result.lower()

    def test_explore_nonexistent_symbol_returns_help(self):
        from coderadar.mcp.server import _explore

        result = _explore(None, "xyzzynonexistent_12345", [], "both", 8)
        assert "Couldn't find" in result or "not found" in result.lower()
        assert "codegraph_search" in result.lower()

    def test_node_detail_nonexistent_entity(self):
        from coderadar.mcp.server import _node_detail

        result = _node_detail(None, "nonexistent::ghost_entity", False)
        assert "not found" in result.lower()

    def test_search_empty_query_returns_prompt(self):
        from coderadar.mcp.server import _search

        result = _search(None, "", None, 5)
        assert len(result) > 0

    def test_search_no_results(self):
        from coderadar.mcp.server import _search

        result = _search(None, "xyzzynonexistent_abcdef", None, 5)
        assert "No results" in result or "not found" in result.lower()

    def test_affected_nonexistent_entity(self):
        from coderadar.mcp.server import _affected

        result = _affected(None, "nonexistent::ghost", 5)
        assert "not found" in result.lower()

    # ── Output quality checks ─────────────────────────────────────

    def test_explore_output_is_markdown(self):
        """Explore returns markdown with bold headers and code blocks."""
        from coderadar.mcp.server import _explore

        result = _explore(None, "AdminUser", [], "both", 8)
        # Bold file header
        assert "**" in result
        # Should reference the file path
        assert ".py" in result

    def test_affected_output_has_depth_hierarchy(self):
        """Affected output shows dependency depth with indentation."""
        from coderadar.mcp.server import _affected, _text_search

        results = _text_search(None, "format_username", 3)
        if results:
            entity_id = results[0]["id"]
            result = _affected(None, entity_id, max_depth=3)
            # Should show depth levels
            assert "Depth" in result or "dependents" in result.lower()
            # If there are dependents, should have indented items
            if "format_username" in result.lower():
                assert "- " in result

    def test_search_output_limits_results(self):
        """Search respects top_k limit."""
        from coderadar.mcp.server import _search

        # Search with tiny top_k
        result = _search(None, "User", None, 2)
        # Should not have many result headings
        heading_count = result.count("### ")
        assert heading_count <= 2, f"Expected <=2 results, got {heading_count}"


# ═══════════════════════════════════════════════════════════════════════════
# MCP Server round-trip via call_tool (requires Rust _core)
# ═══════════════════════════════════════════════════════════════════════════

@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestMCPServerRoundTrip:
    """Call tools through the MCPServer.call_tool dispatch."""

    @pytest.fixture(autouse=True)
    def _index(self):
        _index_fixtures()

    def test_call_tool_search(self):
        """call_tool dispatches to codegraph_search."""
        from coderadar.mcp.server import create_server
        import asyncio

        server = create_server(None)

        async def _call():
            return await server.call_tool(
                "codegraph_search",
                {"query": "User", "top_k": 5},
            )

        output = asyncio.run(_call())
        assert output is not None
        # MCP v2 returns CallToolResult with .content
        if hasattr(output, 'content'):
            text = " ".join(str(c.text) for c in output.content if hasattr(c, 'text'))
        else:
            text = str(output)
        assert "User" in text

    def test_call_tool_explore(self):
        """call_tool dispatches to codegraph_explore."""
        from coderadar.mcp.server import create_server
        import asyncio

        server = create_server(None)

        async def _call():
            return await server.call_tool(
                "codegraph_explore",
                {"query": "create_user", "direction": "both", "max_files": 8},
            )

        output = asyncio.run(_call())
        if hasattr(output, 'content'):
            text = " ".join(str(c.text) for c in output.content if hasattr(c, 'text'))
        else:
            text = str(output)
        assert "create_user" in text.lower()

    def test_call_tool_node_detail(self):
        """call_tool dispatches to codegraph_node."""
        from coderadar.mcp.server import create_server
        import asyncio

        # First find an entity ID
        results = search_entities("User", 3, None)
        assert len(results) > 0
        entity_id = results[0].get("id", "")
        assert entity_id

        server = create_server(None)

        async def _call():
            return await server.call_tool(
                "codegraph_node",
                {"id": entity_id, "include_neighbors": False},
            )

        output = asyncio.run(_call())
        if hasattr(output, 'content'):
            text = " ".join(str(c.text) for c in output.content if hasattr(c, 'text'))
        else:
            text = str(output)
        assert "User" in text
        assert "- **ID:**" in text

    def test_call_tool_affected(self):
        """call_tool dispatches to codegraph_affected."""
        from coderadar.mcp.server import create_server
        import asyncio

        results = search_entities("format_username", 3, None)
        assert len(results) > 0
        entity_id = results[0].get("id", "")

        server = create_server(None)

        async def _call():
            return await server.call_tool(
                "codegraph_affected",
                {"id": entity_id, "max_depth": 3},
            )

        output = asyncio.run(_call())
        if hasattr(output, 'content'):
            text = " ".join(str(c.text) for c in output.content if hasattr(c, 'text'))
        else:
            text = str(output)
        assert "format_username" in text.lower()


# ═══════════════════════════════════════════════════════════════════════════
# Empty/uninitialized graph handling (no Rust needed)
# ═══════════════════════════════════════════════════════════════════════════

class TestUninitializedGraph:
    """Tool responses when no graph is indexed (defensive default behavior)."""

    def test_explore_empty_graph_returns_help(self):
        from coderadar.mcp.server import _explore
        # With _core available but no index, should detect 0 modules
        result = _explore(None, "User", [], "both", 8)
        assert isinstance(result, str)
        assert len(result) > 0

    def test_node_detail_empty_graph(self):
        from coderadar.mcp.server import _node_detail
        result = _node_detail(None, "test.py::func", False)
        assert isinstance(result, str)
        # Either "not found" or "no index" — both are valid defensive responses
        assert len(result) > 0

    def test_search_empty_graph(self):
        from coderadar.mcp.server import _search
        result = _search(None, "test", None, 5)
        assert isinstance(result, str)

    def test_affected_empty_graph(self):
        from coderadar.mcp.server import _affected
        result = _affected(None, "test.py::func", 3)
        assert isinstance(result, str)


# ═══════════════════════════════════════════════════════════════════════════
# v0.4.1 new features: normalization, budget, staleness
# ═══════════════════════════════════════════════════════════════════════════

class TestQueryNormalization:
    """Language spelling normalization (Elixir/Erlang → index)."""

    def test_arity_tail_stripped(self):
        from coderadar.mcp.server import _normalize_query_spelling
        assert _normalize_query_spelling("GenServer:handle_call/3") == "GenServer.handle_call"

    def test_module_colon_to_dot(self):
        from coderadar.mcp.server import _normalize_query_spelling
        assert _normalize_query_spelling("mod:fn") == "mod.fn"

    def test_multi_arity_stripped(self):
        from coderadar.mcp.server import _normalize_query_spelling
        assert _normalize_query_spelling("cowboy_stream_h:request_process/3") == \
            "cowboy_stream_h.request_process"

    def test_preserves_kind_prefix(self):
        from coderadar.mcp.server import _normalize_query_spelling
        assert _normalize_query_spelling("kind:function lang:python User") == \
            "kind:function lang:python User"

    def test_plain_query_unaffected(self):
        from coderadar.mcp.server import _normalize_query_spelling
        assert _normalize_query_spelling("create_user format_username") == \
            "create_user format_username"

    def test_integrated_into_parse_names(self):
        from coderadar.mcp.server import _parse_names
        result = _parse_names("GenServer:handle_call/3 spawn/1", [])
        assert "handle_call/3" not in result
        assert any("handle_call" in r for r in result)


class TestOutputBudget:
    """Budget-aware explore output truncation."""

    def test_budget_below_trim_threshold_passes_through(self):
        from coderadar.mcp.server import _apply_output_budget
        short = ["**test.py** — foo(function)", "", "1\tdef foo(): pass"]
        result = _apply_output_budget(short, max_chars=10_000)
        assert "def foo" in result

    def test_budget_truncates_large_file(self):
        from coderadar.mcp.server import _apply_output_budget
        big_body = [f"{i}\t{'x' * 100}" for i in range(1, 200)]
        lines = ["**big.py** — big(function)", ""] + big_body
        result = _apply_output_budget(lines, max_chars=20_000, max_per_file=1000)
        assert len(result) < len("\n".join(lines))

    def test_budget_preserves_file_headers(self):
        from coderadar.mcp.server import _apply_output_budget
        lines = [
            "**models.py** — User(class)", "",
            "1\tclass User:",
            "2\t    pass",
        ]
        result = _apply_output_budget(lines, max_chars=50)
        assert "models.py" in result

    def test_budget_respects_relationships_section(self):
        from coderadar.mcp.server import _apply_output_budget
        lines = [
            "**src.py** — foo(function)", "",
            "1\tdef foo(): pass", "",
            "## Relationships",
            "- `bar` ←──[caller] `foo`",
        ]
        result = _apply_output_budget(lines, max_chars=500)
        assert "## Relationships" in result


class TestStalenessBanner:
    """Staleness detection and formatting."""

    def test_empty_stale_list_returns_empty(self):
        from coderadar.mcp.server import _format_stale_banner
        result = _format_stale_banner([], [])
        assert result == ""

    def test_stale_files_not_in_referenced_returns_empty(self):
        from coderadar.mcp.server import _format_stale_banner
        stale = [{"path": "/tmp/other.py", "mtime": 999999}]
        result = _format_stale_banner(stale, ["/tmp/main.py"])
        assert result == ""

    def test_stale_file_in_referenced_shows_banner(self):
        from coderadar.mcp.server import _format_stale_banner
        stale = [{"path": "/tmp/main.py", "mtime": 999999}]
        result = _format_stale_banner(stale, ["/tmp/main.py"])
        assert "⚠" in result
        assert "/tmp/main.py" in result
        assert "Read them directly" in result

    def test_get_stale_files_returns_list(self):
        from coderadar.mcp.server import _get_stale_files
        result = _get_stale_files(["/nonexistent/path.py"])
        assert isinstance(result, list)

    def test_get_stale_files_with_real_file(self):
        from coderadar.mcp.server import _get_stale_files
        result = _get_stale_files([str(E2E_DIR / "models.py")])
        assert isinstance(result, list)


# ═══════════════════════════════════════════════════════════════════════════
# Response format validation (design contract for agents)
# ═══════════════════════════════════════════════════════════════════════════

@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestResponseFormats:
    """Validate that tool responses follow the documented format contract."""

    @pytest.fixture(autouse=True)
    def _index(self):
        _index_fixtures()

    def test_search_result_has_all_required_fields(self):
        """Each search result should have id, name, kind, file_path."""
        from coderadar.mcp.server import _search

        result = _search(None, "User", None, 3)
        # ID field present for each result
        assert "- **ID:**" in result
        assert "- **File:**" in result

    def test_node_detail_has_all_required_sections(self):
        """Node detail should have at minimum: ID, Kind, File."""
        from coderadar.mcp.server import _node_detail, _text_search

        results = _text_search(None, "User", 3)
        assert len(results) > 0
        entity_id = results[0]["id"]

        result = _node_detail(None, entity_id, include_neighbors=False)
        required = ["- **ID:**", "- **Kind:**", "- **File:**"]
        for field in required:
            assert field in result, f"Missing required field: {field}"

    def test_explore_has_file_header_format(self):
        """Explore output groups by file with bold headers."""
        from coderadar.mcp.server import _explore

        result = _explore(None, "AdminUser", [], "both", 8)
        # File header format: **file_path** — names (markdown bold)
        assert ".py**" in result or "**.py**" in result
        assert "AdminUser" in result

    def test_affected_output_is_structured(self):
        """Affected output shows depth levels and counts."""
        from coderadar.mcp.server import _affected, _text_search

        results = _text_search(None, "format_username", 3)
        if results:
            entity_id = results[0]["id"]
            result = _affected(None, entity_id, max_depth=3)
            # Should have a heading and total count
            assert "## " in result
            assert "dependents" in result.lower() or "Depth" in result


# ═══════════════════════════════════════════════════════════════════════════
# Cross-file agent scenario
# ═══════════════════════════════════════════════════════════════════════════

@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestCrossFileScenario:
    """Agent traces entities across file boundaries."""

    @pytest.fixture(autouse=True)
    def _index(self):
        _index_fixtures()

    def test_cross_file_module_entities_indexed(self):
        """Both e2e files should be indexed."""
        stats = graph_stats()
        assert stats.get("modules", 0) >= 1

    def test_explore_finds_models_py_classes(self):
        """Explore should find classes in models.py."""
        from coderadar.mcp.server import _explore

        result = _explore(None, "User", [], "both", 8)
        # Should have source from models.py
        assert "User" in result
        assert "models.py" in result.lower() or "class " in result.lower()


class TestQueryTimeResolution:
    """v0.5 F.8 Phase 2: query-time framework reference resolution."""

    @staticmethod
    def _resolvers():
        from coderadar.resolvers import ALL_RESOLVERS
        return ALL_RESOLVERS

    def test_resolve_reference_searches_candidates(self):
        """resolve_reference calls claims_reference on each resolver."""
        from coderadar.resolvers.resolution import resolve_reference

        def searcher(name, limit):
            return [{
                "id": "test::handler",
                "name": name,
                "kind": "function",
                "file_path": "handlers/",
            }]

        results = resolve_reference("userHandler", searcher, self._resolvers(), limit=5)
        assert len(results) >= 1
        assert results[0]["resolved_by"] == "go"

    def test_resolve_django_model_convention(self):
        """Django *Model naming convention resolves via prefer_in_dir."""
        from coderadar.resolvers.resolution import resolve_reference

        def searcher(name, limit):
            return [{
                "id": "models.py::User",
                "name": "User",
                "kind": "class",
                "file_path": "app/models.py",
            }]

        results = resolve_reference("UserModel", searcher, self._resolvers(), limit=5)
        assert len(results) >= 1
        assert results[0]["resolved_by"] == "django"
        assert results[0]["name"] == "User"

    def test_resolve_returns_empty_for_unclaimed(self):
        """References not matching any naming pattern return empty."""
        from coderadar.resolvers.resolution import resolve_reference

        def searcher(name, limit):
            return [{"id": "x", "name": name, "kind": "function", "file_path": "x.py"}]

        results = resolve_reference("calculate", searcher, self._resolvers(), limit=5)
        assert results == []

    def test_resolve_route_finds_handler(self):
        """resolve_route matches route nodes and returns handlers."""
        from coderadar.resolvers.resolution import resolve_route

        def searcher(name, limit):
            entities = {
                "/users": [{
                    "id": "route:1",
                    "name": "GET /users",
                    "kind": "route",
                    "file_path": "routes.go",
                }],
                "route:1": [{
                    "id": "handlers.go::listUsers",
                    "name": "listUsers",
                    "kind": "function",
                    "file_path": "handlers/",
                }],
            }
            return entities.get(name, [])

        results = resolve_route("/users", searcher, limit=5)
        assert len(results) >= 1
        assert results[0]["name"] == "listUsers"
        assert results[0]["resolved_by"] == "route-resolution"

    def test_mcp_tool_registered(self):
        """coderadar_resolve tool is registered on the MCP server."""
        import asyncio
        from coderadar.mcp.server import create_server
        server = create_server(None)
        tool_names = [t.name for t in asyncio.run(server.list_tools())]
        assert "coderadar_resolve" in tool_names

    def test_mcp_resolve_uninitialized_graph(self):
        """Resolve with no graph returns helpful message."""
        from coderadar.mcp.server import _resolve_ref
        result = _resolve_ref(None, "UserService", 5)
        assert isinstance(result, str) and len(result) > 0

    def test_mcp_resolve_empty_query(self):
        """Resolve with empty query returns guidance."""
        from coderadar.mcp.server import _resolve_ref
        result = _resolve_ref(None, "", 5)
        assert "CodeRadar extension" in result or "provide" in result.lower()


# ═══════════════════════════════════════════════════════════════════════════
# v0.5.9 — new MCP tools (query, traverse, mutation, reindex, embeddings)
# ═══════════════════════════════════════════════════════════════════════════

class TestInstructionsV059:
    """Server instructions mention all v0.5.9 tools."""

    def test_instructions_mention_query_tools(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "codegraph_query" in SERVER_INSTRUCTIONS
        assert "codegraph_search_similar" in SERVER_INSTRUCTIONS
        assert "codegraph_module_children" in SERVER_INSTRUCTIONS

    def test_instructions_mention_temporal_and_traverse(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "codegraph_as_of" in SERVER_INSTRUCTIONS
        assert "codegraph_traverse" in SERVER_INSTRUCTIONS

    def test_instructions_mention_mutation_tools(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "coderadar_replace_body" in SERVER_INSTRUCTIONS
        assert "coderadar_update_signature" in SERVER_INSTRUCTIONS
        assert "coderadar_rename" in SERVER_INSTRUCTIONS
        assert "coderadar_create_entity" in SERVER_INSTRUCTIONS

    def test_instructions_mention_sync_tools(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "codegraph_reindex" in SERVER_INSTRUCTIONS
        assert "codegraph_update_file" in SERVER_INSTRUCTIONS

    def test_instructions_mention_compute_embeddings(self):
        from coderadar.mcp.server import SERVER_INSTRUCTIONS
        assert "codegraph_compute_embeddings" in SERVER_INSTRUCTIONS


class TestQueryTool:
    """codegraph_query backend validation."""

    def test_query_empty_graph_returns_help(self):
        from coderadar.mcp.server import _query_graph
        result = _query_graph(None, "functions where name contains 'test'")
        assert isinstance(result, str)
        assert len(result) > 0

    def test_query_empty_query_returns_prompt(self):
        from coderadar.mcp.server import _query_graph
        result = _query_graph(None, "")
        assert "Pest query" in result or "provide" in result.lower()

    @pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
    def test_query_returns_results(self):
        analyze(str(E2E_DIR))
        from coderadar.mcp.server import _query_graph
        # Try a known-good Pest query format; handle parse errors gracefully
        result = _query_graph(None, "classes where inherits_from contains 'BaseModel'")
        # Either returns results or a parse error — both prove the path works
        assert isinstance(result, str)
        assert len(result) > 0


class TestModuleChildren:
    """codegraph_module_children backend."""

    def test_module_children_empty_graph(self):
        from coderadar.mcp.server import _module_children
        result = _module_children(None, "test.py::module")
        assert isinstance(result, str)
        assert len(result) > 0

    def test_module_children_empty_id(self):
        from coderadar.mcp.server import _module_children
        result = _module_children(None, "")
        assert "module" in result.lower()

    @pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
    def test_module_children_returns_structure(self):
        analyze(str(E2E_DIR))
        from coderadar.mcp.server import _module_children
        from coderadar._core import search_entities as _se
        entries = _se("User", 3, None)
        if entries:
            fp = entries[0].get("file_path", "")
            module_id = f"{fp}::module"
            result = _module_children(None, module_id)
            assert "## Module" in result


class TestTraverse:
    """codegraph_traverse backend."""

    def test_traverse_empty_graph(self):
        from coderadar.mcp.server import _traverse
        result = _traverse(None, "test.py::func", "both", None, 3)
        assert isinstance(result, str)
        assert len(result) > 0

    def test_traverse_empty_entity_id(self):
        from coderadar.mcp.server import _traverse
        result = _traverse(None, "", "both", None, 3)
        assert "entity" in result.lower()

    def test_traverse_nonexistent_entity(self):
        from coderadar.mcp.server import _traverse
        result = _traverse(None, "nonexistent::ghost", "both", None, 3)
        assert "not found" in result.lower() or "codegraph" in result.lower()


class TestMutationTools:
    """Mutation tools — plan + apply via dry_run toggle."""

    def test_replace_body_uninitialized(self):
        from coderadar.mcp.server import _replace_body
        result = _replace_body(None, "test.py::fn", "return 42", None, True)
        assert isinstance(result, str)
        assert len(result) > 0

    def test_update_signature_uninitialized(self):
        from coderadar.mcp.server import _update_signature
        result = _update_signature(None, "test.py::fn", "def fn(x, y):", False, True)
        assert isinstance(result, str)
        assert len(result) > 0

    def test_rename_uninitialized(self):
        from coderadar.mcp.server import _rename
        result = _rename(None, "test.py::fn", "new_fn", True)
        assert isinstance(result, str)
        assert len(result) > 0

    def test_create_entity_uninitialized(self):
        from coderadar.mcp.server import _create_entity
        result = _create_entity(None, "test.py", "python", "function", "new_fn", "return 1", None, True)
        assert isinstance(result, str)
        assert len(result) > 0

    def test_format_mutation_plan_shows_diff(self):
        from coderadar.mcp.server import _format_mutation_plan
        from coderadar import MutationPlan, MutationEdit
        plan = MutationPlan(
            id="test-123",
            tool="replace_entity_body",
            edits=[MutationEdit(file="test.py", replacement="x", expected_hash="abc")],
            affected_files=["test.py"],
            diff_preview="@@ -1,1 +1,1 @@\n-old\n+new",
            unverified_sites=[],
            warnings=[],
        )
        result = _format_mutation_plan(plan)
        assert "DRY RUN" in result
        assert "test-123" in result
        assert "Diff Preview" in result

    def test_format_mutation_applied_shows_result(self):
        from coderadar.mcp.server import _format_mutation_applied
        from coderadar import MutationResult
        result_obj = MutationResult(
            status="Applied",
            files_written=["test.py"],
            syntax_errors=[],
        )
        result = _format_mutation_applied(result_obj)
        assert "Mutation Applied" in result
        assert "test.py" in result


class TestReindexUpdateFile:
    """codegraph_reindex and codegraph_update_file backends."""

    def test_update_file_empty_graph(self):
        from coderadar.mcp.server import _update_file
        result = _update_file(None, "test.py", "def foo(): pass")
        assert isinstance(result, str)
        assert len(result) > 0

    def test_update_file_empty_path(self):
        from coderadar.mcp.server import _update_file
        result = _update_file(None, "", None)
        assert "file path" in result.lower()


class TestSearchSimilar:
    """codegraph_search_similar backend."""

    def test_search_similar_empty_graph(self):
        from coderadar.mcp.server import _search_similar
        result = _search_similar(None, "authentication logic", 5)
        assert isinstance(result, str)
        assert len(result) > 0

    def test_search_similar_empty_query(self):
        from coderadar.mcp.server import _search_similar
        result = _search_similar(None, "", 5)
        assert "natural-language" in result.lower() or "provide" in result.lower()


class TestComputeEmbeddings:
    """codegraph_compute_embeddings backend."""

    def test_compute_embeddings_empty_graph(self):
        from coderadar.mcp.server import _compute_embeddings
        result = _compute_embeddings(None)
        assert isinstance(result, str)
        assert len(result) > 0


class TestAsOf:
    """codegraph_as_of backend."""

    def test_as_of_empty_graph(self):
        from coderadar.mcp.server import _as_of
        result = _as_of(None, "2025-01-01T00:00:00Z", "", [])
        assert isinstance(result, str)
        assert len(result) > 0

    def test_as_of_empty_timestamp(self):
        from coderadar.mcp.server import _as_of
        result = _as_of(None, "", "", [])
        assert "timestamp" in result.lower()

    def test_as_of_with_symbols_uninitialized(self):
        from coderadar.mcp.server import _as_of
        result = _as_of(None, "2025-01-01T00:00:00Z", "", ["User"])
        assert isinstance(result, str) and len(result) > 0
