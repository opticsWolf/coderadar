"""Any tool name a message suggests has to be callable.

`codegraph_as_of` told the agent to use "`codegraph_query` with timestamp"
and "`search_entities`". `codegraph_query` takes no timestamp parameter and
`search_entities` is not a tool at all — it is a function on the Rust core.
An agent following that guidance failed twice and had no way to tell that
the advice, rather than its own call, was wrong.

Guidance strings are the part of an MCP server an agent trusts most, and
nothing checked them.
"""

from __future__ import annotations

import re

import pytest

import coderadar
from coderadar.mcp.server import create_server


TOOL_RE = re.compile(r"`(code(?:graph|radar)_[a-z_]+)`")


@pytest.fixture(scope="module")
def tool_names():
    server = create_server(coderadar.CodeGraph())
    return {t.name for t in _list_tools(server)}


def _list_tools(server):
    import asyncio

    return asyncio.run(server.list_tools())


def _guidance_strings():
    """Every message a backend can return that mentions a tool."""
    import inspect

    from coderadar.mcp import server as s

    out = []
    source = inspect.getsource(s)
    for match in TOOL_RE.finditer(source):
        out.append(match.group(1))
    return out


class TestEveryToolNameMentionedExists:
    def test_guidance_does_not_invent_tools(self, tool_names):
        mentioned = set(_guidance_strings())
        unknown = mentioned - tool_names
        assert not unknown, (
            f"guidance names tools that are not registered: {sorted(unknown)}"
        )

    def test_the_check_can_actually_fail(self, tool_names):
        # Guard against the regex silently matching nothing, which would
        # make the assertion above vacuous — the failure mode this whole
        # file exists to catch.
        assert len(_guidance_strings()) > 5
        assert "codegraph_search" in tool_names


class TestAsOfGuidance:
    @pytest.fixture
    def indexed(self, tmp_path, monkeypatch):
        """`as_of` is behind the index guard, so give it one."""
        from coderadar._core import analyze

        (tmp_path / "app.py").write_text(
            "def f():\n    return 1\n", encoding="utf-8")
        monkeypatch.chdir(tmp_path)
        analyze(".")
        return tmp_path

    def test_it_no_longer_points_at_a_timestamp_parameter(self, indexed):
        from coderadar.mcp.server import _as_of

        message = _as_of(coderadar.CodeGraph(), "2026-08-01T00:00:00Z", "", [])

        assert "search_entities" not in message
        assert "with timestamp" not in message
        # It must still tell the agent what to do next.
        assert "symbols" in message
