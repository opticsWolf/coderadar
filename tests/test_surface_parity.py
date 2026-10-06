"""One name per operation, on every surface.

Each op in `coderadar.ops.OPERATIONS` is the MCP tool `coderadar_<op>`, the
CLI command `coderadar <op>` (underscores become hyphens) and the
`CodeGraph` method `<op>`, and the CLI takes every argument the tool does.
`set_project` is the exception: on the CLI it is `-C/--project`, and the
Python API has no form of it (it works on the cwd).

The CLI half also checks the behaviour the shared ops promise: project
walk-up, JSON output, exit codes and dry-run-unless-applied edits.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import coderadar
import pytest
from click.testing import CliRunner
from coderadar import ops
from coderadar.cli import main
from coderadar.mcp.server import create_server

try:
    import coderadar._core
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

#: CLI parameter names that spell an MCP argument differently.
CLI_SPELLING = {"query_text": "query", "query_string": "query", "apply": "dry_run"}
#: MCP arguments with no CLI parameter, by design.
MCP_ONLY = {"project_path"}  # the CLI has -C/--project instead


def _tools():
    return {t.name: t for t in create_server(None)._tool_manager.list_tools()}


def _cli_name(op: str) -> str:
    return op.replace("_", "-")


class TestNames:
    def test_every_op_is_an_mcp_tool_and_nothing_else_is(self):
        assert set(_tools()) == {f"coderadar_{op}" for op in ops.OPERATIONS}

    def test_every_op_is_a_cli_command(self):
        missing = [_cli_name(op) for op in ops.OPERATIONS
                   if op != "set_project" and _cli_name(op) not in main.commands]
        assert missing == []
        assert any("--project" in p.opts for p in main.params)

    def test_every_op_is_a_codegraph_method(self):
        missing = [op for op in ops.OPERATIONS
                   if op != "set_project" and not callable(getattr(coderadar.CodeGraph, op, None))]
        assert missing == []

    def test_the_cli_takes_every_argument_the_tool_does(self):
        tools = _tools()
        gaps = {}
        for op in ops.OPERATIONS:
            if op == "set_project":
                continue
            mcp_args = set((tools[f"coderadar_{op}"].parameters or {}).get("properties", {}))
            cli_args = {CLI_SPELLING.get(p.name, p.name)
                        for p in main.commands[_cli_name(op)].params}
            missing = mcp_args - MCP_ONLY - cli_args
            if missing:
                gaps[op] = sorted(missing)
        assert gaps == {}

    def test_old_cli_names_still_work_but_are_hidden(self):
        for old in ("analyze", "rebuild", "update", "stats", "blame", "git-clean", "git-diff"):
            assert old in main.commands, old
            assert main.commands[old].hidden, old


SOURCE = '''\
def helper():
    return 1


def caller():
    return helper() + 1
'''


@pytest.fixture
def project(tmp_path):
    (tmp_path / ".coderadar").mkdir()
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "mod.py").write_text(SOURCE, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        yield tmp_path
    finally:
        os.chdir(previous)


def _run(*args):
    return CliRunner().invoke(main, list(args), catch_exceptions=False)


@pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")
class TestCli:
    def test_a_subdirectory_works_on_the_whole_project(self, project):
        os.chdir(project / "pkg")
        result = _run("search", "helper", "--format", "json")

        assert result.exit_code == 0, result.output
        assert Path.cwd() == project.resolve()
        assert any(r["name"] == "helper" for r in json.loads(result.stdout))

    def test_dash_c_names_the_project(self, project, tmp_path_factory):
        elsewhere = tmp_path_factory.mktemp("elsewhere")
        os.chdir(elsewhere)
        result = _run("-C", str(project), "status", "--format", "json")

        assert result.exit_code == 0, result.output
        assert Path(json.loads(result.stdout)["root"]) == project.resolve()

    def test_text_is_the_mcp_text(self, project):
        result = _run("node", "pkg/mod.py::helper")

        assert result.exit_code == 0, result.output
        assert "helper" in result.stdout

    def test_an_unknown_entity_exits_1_with_suggestions(self, project):
        result = _run("node", "pkg/mod.py::helpr")

        assert result.exit_code == 1
        assert "not found" in result.stderr
        assert "`coderadar search`" in result.stderr or "Did you mean" in result.stderr

    def test_bad_input_exits_2(self, project):
        result = _run("query", "functions where nonsense ==")

        assert result.exit_code == 2

    def test_traverse_follows_every_edge_kind_by_default(self, project):
        result = _run("traverse", "pkg/mod.py::helper", "--direction", "upstream",
                      "--format", "json")

        assert result.exit_code == 0, result.output
        assert json.loads(result.stdout)["edge_kinds"] in (None, [])

    def test_edits_are_dry_runs_unless_applied(self, project):
        target = project / "pkg" / "mod.py"
        dry = _run("rename", "pkg/mod.py::helper", "assist")

        assert dry.exit_code == 0, dry.output
        assert "--apply" in dry.stdout
        assert "def helper" in target.read_text(encoding="utf-8")

        applied = _run("rename", "pkg/mod.py::helper", "assist", "--apply")

        assert applied.exit_code == 0, applied.output
        text = target.read_text(encoding="utf-8")
        assert "def assist" in text and "assist() + 1" in text

    def test_update_file_drops_a_deleted_file(self, project):
        _run("reindex", "--full")
        (project / "pkg" / "mod.py").unlink()
        result = _run("update-file", "pkg/mod.py")

        assert result.exit_code == 0, result.output
        assert "File Removed" in result.stdout

    def test_an_old_name_says_its_new_one(self, project):
        result = _run("stats")

        assert result.exit_code == 0, result.output
        assert "coderadar status" in result.stderr


@pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")
class TestCallersCalleesParity:
    """`callers`/`callees` answer identically on ops, CLI and MCP."""

    def test_all_three_surfaces_agree(self, project):
        coderadar.analyze(".")
        target = "pkg/mod.py::helper"

        from coderadar.mcp import server as _server
        ops_callers = {r["id"] for r in ops.callers(target)}
        assert "pkg/mod.py::caller" in ops_callers
        ops_callees = {r["id"] for r in ops.callees("pkg/mod.py::caller")}
        assert target in ops_callees

        cli = _run("callers", target)
        assert cli.exit_code == 0, cli.output
        assert "pkg/mod.py::caller" in cli.output

        mcp_text = _server._callers(None, target)
        assert "pkg/mod.py::caller" in mcp_text
        mcp_text = _server._callees(None, "pkg/mod.py::caller")
        assert target in mcp_text

    def test_unknown_is_unknown_everywhere(self, project):
        coderadar.analyze(".")
        from coderadar.mcp import server as _server

        assert ops.callers("pkg/mod.py::nosuch") == []
        cli = _run("callers", "pkg/mod.py::nosuch")
        assert cli.exit_code == 0, cli.output
        assert "Unknown entity" in cli.output
        assert "Unknown entity" in _server._callers(None, "pkg/mod.py::nosuch")


class TestRecoveryArgsMatch:
    """The agent's recovery paths exist on MCP too: `reindex(full)`,
    `compute_embeddings(model_name)` — schema-pinned, not executed."""

    def test_mcp_reindex_takes_full(self):
        from coderadar.mcp.server import create_server

        tools = {t.name: t for t in create_server(None)._tool_manager.list_tools()}
        props = (tools["coderadar_reindex"].parameters or {}).get("properties", {})
        assert "full" in props
        assert "with_embeddings" in props

    def test_mcp_compute_embeddings_takes_model_name(self):
        from coderadar.mcp.server import create_server

        tools = {t.name: t for t in create_server(None)._tool_manager.list_tools()}
        props = (tools["coderadar_compute_embeddings"].parameters or {}).get(
            "properties", {})
        assert "model_name" in props


class TestStoreRepairOpsSeam:
    """`store-repair` answers from `ops.store_repair` (CLI-only surface,
    no MCP tool by design — destructive `--delete`, and re-analyze already
    retires automatically)."""

    def test_missing_store_reports(self, project):
        assert ops.store_repair("nope.db") == {"missing": "nope.db"}
        cli = _run("store-repair", "--db", "nope.db")
        assert cli.exit_code == 0, cli.output
        assert "nothing to repair" in cli.output

    def test_report_shape_on_real_store(self, project):
        coderadar.analyze(".", create_store=True)
        db = str(project / ".coderadar" / "store" / "coderadar.db")
        result = ops.store_repair(db)
        assert set(result) == {"v1_found", "v1_retired", "edges_retired",
                               "unreadable_live", "legacy_ids"}
        cli = _run("store-repair", "--db", db)
        assert cli.exit_code == 0, cli.output
        assert "Store repair:" in cli.output


DIAGNOSE_SOURCE = '''\
def ok():
    return helper() + missing_target()
'''


@pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")
class TestDiagnoseParity:
    """`diagnose` reports the same gap on ops, CLI and MCP."""

    @pytest.fixture
    def gappy(self, tmp_path):
        (tmp_path / ".coderadar").mkdir()
        (tmp_path / "g.py").write_text(DIAGNOSE_SOURCE, encoding="utf-8")
        previous = Path(os.getcwd())
        os.chdir(tmp_path)
        try:
            coderadar.analyze(".")
            yield tmp_path
        finally:
            os.chdir(previous)

    def test_all_three_surfaces_agree(self, gappy):
        from coderadar.mcp import server as _server

        result = ops.diagnose()
        gap = [r for r in result["unresolved"] if r["id"] == "g.py::ok"]
        assert gap and "missing_target" in gap[0]["targets"]

        cli = _run("diagnose")
        assert cli.exit_code == 0, cli.output
        assert "missing_target" in cli.output

        assert "missing_target" in _server._diagnose(None)

    def test_clean_graph_reports_none_everywhere(self, project):
        coderadar.analyze(".")
        from coderadar.mcp import server as _server

        assert ops.diagnose()["unresolved"] == []
        cli = _run("diagnose")
        assert cli.exit_code == 0, cli.output
        assert "none" in cli.output
        assert "none" in _server._diagnose(None)
