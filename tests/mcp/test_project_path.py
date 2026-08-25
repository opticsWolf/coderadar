"""Every tool takes `project_path`, and refuses rather than misanswers.

Agents working across repositories will pass a project path. A tool that
accepts it and ignores it answers a question nobody asked — about the wrong
codebase, with nothing in the reply to say so. This build serves one project
(the core keeps a single GLOBAL_GRAPH and `analyze` replaces it wholesale),
so the honest behaviour is to accept a matching path and refuse anything
else with the reason.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from coderadar.mcp import lazy, server as server_mod
from coderadar.mcp.lazy import LazyRootRetry
from coderadar.mcp.roots import ResolvedRoot
from coderadar.mcp.startup import BackgroundIndex


@pytest.fixture(autouse=True)
def _no_leftover_handles():
    lazy.configure(None)
    yield
    lazy.configure(None)


def _serving(path: Path) -> None:
    lazy.configure(LazyRootRetry(
        ResolvedRoot(path=path.resolve(), source="--path", marker=None),
        BackgroundIndex(analyze=lambda root: None),
    ))


class TestTheSchema:
    #: `codegraph_set_project` names a project rather than asking about one,
    #: so it has no project_path argument of its own.
    NO_PROJECT_PATH_TOOLS = {"codegraph_set_project"}

    def test_every_tool_offers_project_path(self):
        tools = server_mod.create_server(None)._tool_manager.list_tools()
        # 22 tools since codegraph_find_scaffolding (Stage 3) joined the surface.
        assert len(tools) == 22

        without = [
            t.name for t in tools
            if "project_path" not in (t.parameters or {}).get("properties", {})
            and t.name not in self.NO_PROJECT_PATH_TOOLS
        ]
        assert without == []

    def test_project_path_is_never_required(self):
        # An agent that does not care which project must not have to say so.
        # The exception is codegraph_set_project: there, the argument does
        # not ask about a project — it names the one to switch to.
        tools = server_mod.create_server(None)._tool_manager.list_tools()
        required = [
            t.name for t in tools
            if "project_path" in ((t.parameters or {}).get("required") or [])
            and t.name not in self.NO_PROJECT_PATH_TOOLS
        ]
        assert required == []


class TestTheGuard:
    def test_no_project_path_means_proceed(self, tmp_path):
        _serving(tmp_path)
        assert server_mod._wrong_project(None) is None
        assert server_mod._wrong_project("") is None

    def test_the_served_project_is_accepted(self, tmp_path):
        _serving(tmp_path)
        assert server_mod._wrong_project(str(tmp_path)) is None

    def test_a_different_spelling_of_the_served_project_is_accepted(self, tmp_path):
        nested = tmp_path / "src"
        nested.mkdir()
        _serving(tmp_path)
        assert server_mod._wrong_project(str(nested / "..")) is None

    def test_a_subdirectory_of_the_served_project_is_accepted(self, tmp_path):
        """Agents pass directories they explored, not canonical roots."""
        (tmp_path / ".coderadar").mkdir()
        deep = tmp_path / "src" / "deep"
        deep.mkdir(parents=True)
        _serving(tmp_path)
        assert server_mod._wrong_project(str(deep)) is None

    def test_a_file_inside_the_served_project_is_accepted(self, tmp_path):
        """An editor tab's path means "the project this file lives in"."""
        (tmp_path / ".coderadar").mkdir()
        source = tmp_path / "m.py"
        source.write_text("x = 1\n", encoding="utf-8")
        _serving(tmp_path)
        assert server_mod._wrong_project(str(source)) is None

    def test_another_project_is_refused_with_a_reason(self, tmp_path):
        served = tmp_path / "served"
        other = tmp_path / "other"
        served.mkdir()
        other.mkdir()
        _serving(served)

        message = server_mod._wrong_project(str(other))

        assert message is not None
        assert str(served.resolve()) in message
        assert str(other.resolve()) in message
        # Refusing without a way forward is just a dead end.
        assert "codegraph_set_project" in message

    def test_with_no_retry_handle_the_cwd_is_what_we_serve(self, tmp_path):
        # A directly constructed server has no root handle; the process cwd
        # is the project by the same reasoning that made serve chdir onto it.
        assert server_mod._wrong_project(os.getcwd()) is None

    def test_an_unreadable_path_is_named_as_the_problem(self, tmp_path):
        message = server_mod._wrong_project(str(tmp_path / "a" / "b" / "c"))
        assert message is not None
        assert "no readable directory" in message


class TestATool:
    def test_a_tool_asked_about_another_project_refuses(self, tmp_path, monkeypatch):
        import coderadar._core as core

        served = tmp_path / "served"
        other = tmp_path / "other"
        served.mkdir()
        other.mkdir()
        _serving(served)
        monkeypatch.setattr(core, "graph_stats", lambda: {"modules": 3})

        tools = {
            t.name: t for t in
            server_mod.create_server(object())._tool_manager.list_tools()
        }
        assert "codegraph_search" in tools

        answer = tools["codegraph_search"].fn(query="anything", project_path=str(other))
        assert "cannot answer" in answer
