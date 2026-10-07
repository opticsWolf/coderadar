"""The MCP instructions, MCP tool descriptions and CLI help describe one surface.

Each test pins a fact the texts used to get wrong: a stale tool count, tools
that do not exist, missing smell rules, and edge kinds the engine rejects.
"""

import asyncio
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

from click.testing import CliRunner
from coderadar.cli import main
from coderadar.mcp.server import SERVER_INSTRUCTIONS, create_server

ROOT = Path(__file__).parent.parent
TOOL_NAME = re.compile(r"\b(?:codegraph|coderadar)_[a-z_]+\b")


def _tools():
    return {t.name: t for t in asyncio.run(create_server(None).list_tools())}


def test_instructions_name_exactly_the_registered_tools():
    assert set(TOOL_NAME.findall(SERVER_INSTRUCTIONS)) == set(_tools())


def test_tool_descriptions_only_reference_real_tools():
    tools = _tools()
    for name, tool in tools.items():
        unknown = set(TOOL_NAME.findall(tool.description or "")) - set(tools)
        assert not unknown, f"{name} mentions {unknown}"


def test_smells_description_lists_every_rule():
    rule_ids = set()
    for rs in (ROOT / "core_indexer/src/smells/rules").glob("*.rs"):
        rule_ids.update(re.findall(r'fn id\(&self\)[^{]*\{\s*"([a-z-]+)"', rs.read_text(encoding="utf-8")))
    desc = _tools()["coderadar_get_smells"].description
    assert rule_ids and all(r in desc for r in rule_ids), rule_ids - {r for r in rule_ids if r in desc}


def test_edge_kinds_match_the_engine():
    lib = (ROOT / "core_indexer/src/lib.rs").read_text(encoding="utf-8")
    kinds = set(re.search(r"ALL_EDGE_KINDS: \[&str; \d+\] = \[([^\]]+)\]", lib).group(1).replace('"', "").replace(" ", "").split(","))
    cli_help = CliRunner().invoke(main, ["traverse", "--help"]).output
    mcp_desc = _tools()["coderadar_traverse"].description
    for text in (cli_help, mcp_desc):
        for kind in kinds:
            assert kind in text
        for bogus in ("handles", "declares", "navigation"):
            assert bogus not in text


def test_cli_help_has_no_stale_counts_or_history():
    runner = CliRunner()
    serve = runner.invoke(main, ["mcp", "serve", "--help"]).output
    assert not re.search(r"\b\d+ tools\b", serve)
    for cmd in ("status", "diagnose", "rebuild", "query", "traverse"):
        out = runner.invoke(main, [cmd, "--help"]).output
        assert "used to" not in out and "This printed" not in out and "Pest" not in out, cmd
