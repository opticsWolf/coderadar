"""v0.8 P2-6: empty-result wording (guide: docs/v0.8-p2-agent-ux-guide.md).

A miss has to say what was actually tried — "Try broader terms" made an
empty result look like the index was missing the thing, and the agent kept
retrying variations instead of switching tools (Süvea session report 1,
item 3).
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from coderadar.mcp import lazy, server as server_mod, startup

try:
    from coderadar._core import analyze as _analyze
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


@pytest.fixture(autouse=True)
def _clean_handles(tmp_path):
    lazy.configure(None)
    startup.configure(None)
    previous = Path(os.getcwd())
    yield
    os.chdir(previous)
    lazy.configure(None)
    startup.configure(None)


def test_multi_token_miss_lists_the_tokens():
    msg = server_mod._search_miss_message("SyncNode exchange_with", None)
    assert "No results found for 'SyncNode exchange_with'" in msg
    assert "`SyncNode`" in msg and "`exchange_with`" in msg
    # The miss explains the OR semantics and points at the escape hatches.
    assert "OR" in msg
    assert "codegraph_search_similar" in msg
    assert "codegraph_explore" in msg


def test_single_token_miss_names_the_match_scope():
    msg = server_mod._search_miss_message("fool", "function")
    assert "(kind: function)" in msg
    assert "name" in msg and "signature" in msg and "docstring" in msg
    assert "codegraph_search_similar" in msg
    # No OR explanation for a single token.
    assert "Tokens are matched independently" not in msg


def test_empty_query_still_asks_for_one(project):
    # Needs a loaded graph: the requires_index guard answers first otherwise.
    from coderadar.mcp.server import _search
    import coderadar
    msg = _search(coderadar.CodeGraph(), "   ", None, 10)
    assert "Please provide a query" in msg


@pytest.fixture
def project(tmp_path):
    (tmp_path / "lib.py").write_text(
        "def real_symbol():\n    return 1\n", encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


def test_real_miss_message_end_to_end(project):
    import coderadar
    from coderadar.mcp.server import _search
    graph = coderadar.CodeGraph()

    msg = _search(graph, "zzzqqq xynx", None, 10)
    assert "No results found for 'zzzqqq xynx'" in msg
    assert "`zzzqqq`" in msg and "`xynx`" in msg
    assert "codegraph_search_similar" in msg

    # A real hit is unaffected by the miss wording.
    msg = _search(graph, "real_symbol", None, 10)
    assert "No results found" not in msg
    assert "real_symbol" in msg
