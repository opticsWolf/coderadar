"""P2 skill drift tests: the agent skills cannot rot.

`skills/coderadar-{mcp,cli}/SKILL.md` are the source of truth agents read.
Every `coderadar_*` tool the MCP skill names must exist in the registry;
every top-level `coderadar <cmd>` the CLI skill teaches must exist in
click. A doc edit naming a phantom tool fails here, not in production.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

SKILLS = Path(__file__).parent.parent / "skills"

try:
    from coderadar.mcp.server import create_server as _create_server
    from coderadar.cli import main as _main
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="coderadar package not importable")


def _skill(name: str) -> str:
    return (SKILLS / name / "SKILL.md").read_text(encoding="utf-8")


class TestMcpSkillDrift:
    def test_every_taught_tool_exists(self):
        tools = {t.name for t in _create_server(None)._tool_manager.list_tools()}
        taught = set(re.findall(r"`(coderadar_[a-z_]+)`", _skill("coderadar-mcp")))
        # `ops.`-qualified API names are not MCP tools; the rest must be.
        taught_tools = {t for t in taught if not t.startswith("coderadar_ops")}
        assert taught_tools, "skill names no tools?"
        missing = taught_tools - tools
        assert not missing, f"skill teaches phantom tools: {sorted(missing)}"

    def test_skill_names_the_recompute_flag(self):
        # DR-11 surface proof: the model-switch flag must be discoverable.
        assert "recompute" in _skill("coderadar-mcp")

    def test_hidden_old_spellings_not_taught_as_tools(self):
        assert "coderadar_analyze" not in _skill("coderadar-mcp")


class TestCliSkillDrift:
    def test_every_taught_command_exists(self):
        taught = set(re.findall(r"coderadar ([a-z][a-z-]+)", _skill("coderadar-cli")))
        # Drop the `-C <path>` flag form and prose mentions.
        taught = {c for c in taught if c in _main.commands}
        assert taught, "skill teaches no commands?"
        # Every command-like mention that IS a real command must exist
        # (checked by construction above); now the reverse: no phantom.
        for cmd in re.findall(r"coderadar ([a-z][a-z-]+)", _skill("coderadar-cli")):
            if cmd in {"old", "new"}:  # prose in the Renamed section
                continue
            assert cmd in _main.commands or cmd in {"search-symbols"}, (
                f"skill teaches phantom command: {cmd}")

    def test_search_symbols_marked_api_only(self):
        # §1.10 binding is deferred: the skill must say so, not imply a CLI.
        assert "API-only" in _skill("coderadar-cli")

    def test_old_spellings_carry_the_notice(self):
        text = _skill("coderadar-cli")
        for old in ("analyze", "rebuild", "update", "stats"):
            assert old in text
        assert "0.13" in text
