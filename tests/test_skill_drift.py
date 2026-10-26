"""P2 skill drift tests: the agent skills cannot rot.

`coderadar/skills/coderadar-{mcp,cli}/SKILL.md` are the source of truth
agents read. They live inside the package (not at the repo root) so maturin
copies them into the wheel; these tests read them through
`importlib.resources`, the same path an installed release uses.

Every `coderadar_*` tool the MCP skill names must exist in the registry;
every top-level `coderadar <cmd>` the CLI skill teaches must exist in
click. A doc edit naming a phantom tool fails here, not in production.
"""

from __future__ import annotations

import asyncio
import re
from pathlib import Path

import pytest

SKILLS = Path(__file__).parent.parent / "py_agent" / "src" / "coderadar" / "skills"

try:
    from coderadar.cli import main as _main
    from coderadar.docs import DocNotFound, doc_names, doc_text
    from coderadar.mcp.server import SERVER_INSTRUCTIONS
    from coderadar.mcp.server import create_server as _create_server
    from coderadar.skills import (
        SkillNotFound,
        default_destinations,
        install,
        iter_skills,
        skill_description,
        skill_names,
        skill_text,
    )
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="coderadar package not importable")


def _skill(name: str) -> str:
    return skill_text(name)


def _cli(*args: str):
    from click.testing import CliRunner

    return CliRunner().invoke(_main, list(args))


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

    def test_every_tool_has_a_call_signature(self):
        # The skill is the agent's only reference: a tool named in the table
        # but with no argument shape is a tool the agent has to guess at.
        tools = {t.name for t in _create_server(None)._tool_manager.list_tools()}
        text = _skill("coderadar-mcp")
        missing = sorted(name for name in tools if f"{name}(" not in text)
        assert not missing, f"skill documents no signature for: {missing}"

    def test_hidden_old_spellings_not_taught_as_tools(self):
        assert "coderadar_analyze" not in _skill("coderadar-mcp")

    def test_traverse_edge_kinds_are_current(self):
        text = _skill("coderadar-mcp")
        for kind in (
            "calls", "imports", "extends", "implements",
            "decorates", "instantiates", "references", "overrides",
        ):
            assert kind in text, f"MCP skill omits traverse edge kind: {kind}"

    def test_mutation_outcomes_and_recovery_are_taught(self):
        text = _skill("coderadar-mcp")
        for status in (
            "Applied",
            "RolledBack",
            "RejectedStale",
            "RejectedPolicy",
            "ApplyIncomplete",
            "RollbackIncomplete",
        ):
            assert status in text
        assert "recovery_path" in text
        assert "Synchronized" in text


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

    def test_every_top_level_command_is_documented(self):
        # Hidden legacy spellings are exempt; every current command must be
        # taught, so a new command cannot ship without its syntax.
        text = _skill("coderadar-cli")
        missing = sorted(
            name
            for name, cmd in _main.commands.items()
            if not getattr(cmd, "hidden", False) and f"coderadar {name}" not in text
        )
        assert not missing, f"CLI skill teaches no syntax for: {missing}"

    def test_removed_spellings_are_not_taught_as_commands(self):
        text = _skill("coderadar-cli")
        for old in ("analyze", "rebuild", "update", "stats",
                      "blame", "git-clean", "git-diff"):
            assert not re.search(
                rf"(?m)^coderadar {re.escape(old)}(?:\s|$)", text
            ), f"CLI skill teaches removed {old}"
        assert "Removed in 0.13" in text

    def test_teaches_the_four_mutations(self):
        text = _skill("coderadar-cli")
        for cmd in ("replace-body", "update-signature", "rename", "create-entity"):
            assert f"coderadar {cmd}" in text, f"CLI skill does not teach {cmd}"

    def test_traverse_edge_kinds_are_current(self):
        text = _skill("coderadar-cli")
        for kind in (
            "calls", "imports", "extends", "implements",
            "decorates", "instantiates", "references", "overrides",
        ):
            assert kind in text, f"CLI skill omits traverse edge kind: {kind}"

    def test_mutation_outcomes_and_exit_codes_are_taught(self):
        text = _skill("coderadar-cli")
        for status in (
            "Applied",
            "RolledBack",
            "RejectedStale",
            "RejectedPolicy",
            "ApplyIncomplete",
            "RollbackIncomplete",
        ):
            assert status in text
        assert "Exit" in text and "0" in text and "1" in text and "2" in text

    def test_set_project_exception_is_taught(self):
        # MCP's coderadar_set_project has no CLI command; the CLI spelling is -C.
        text = _skill("coderadar-cli")
        assert "coderadar_set_project" in text
        assert "-C/--project" in text


class TestMcpInstructionsDrift:
    def test_instructions_name_every_registered_tool(self):
        tools = {t.name for t in _create_server(None)._tool_manager.list_tools()}
        named = set(re.findall(r"coderadar_[a-z_]+", SERVER_INSTRUCTIONS))
        assert named == tools, (
            f"server instructions drift: missing={sorted(tools - named)}, "
            f"phantom={sorted(named - tools)}"
        )

    def test_instructions_teach_skill_installation(self):
        assert "coderadar skills install" in SERVER_INSTRUCTIONS
        assert "coderadar skills show" in SERVER_INSTRUCTIONS

    def test_instructions_name_the_bundled_resources(self):
        for uri in (
            "skills://coderadar-mcp",
            "skills://coderadar-cli",
            "docs://query-language",
        ):
            assert uri in SERVER_INSTRUCTIONS, f"server instructions omit {uri}"

    def test_instructions_state_the_set_project_exception(self):
        # coderadar_set_project has no CLI command; the shell spelling is -C.
        assert "coderadar_set_project" in SERVER_INSTRUCTIONS
        assert "-C/--project" in SERVER_INSTRUCTIONS


class TestMcpResources:
    def test_resources_are_listed(self):
        server = _create_server(None)
        listed = asyncio.run(server.list_resources())
        assert {str(r.uri) for r in listed} == {
            "skills://coderadar-mcp",
            "skills://coderadar-cli",
            "docs://query-language",
        }
        assert {r.name for r in listed} == {
            "coderadar-mcp",
            "coderadar-cli",
            "query-language",
        }

    def test_both_packs_point_at_the_bundled_docs(self):
        for name in skill_names():
            text = _skill(name)
            assert "coderadar docs query-language" in text
            assert "docs://query-language" in text

    def test_resource_text_matches_the_bundled_files(self):
        server = _create_server(None)
        expected = {
            "skills://coderadar-mcp": _skill("coderadar-mcp"),
            "skills://coderadar-cli": _skill("coderadar-cli"),
            "docs://query-language": doc_text("query-language"),
        }
        for uri, text in expected.items():
            contents = asyncio.run(server.read_resource(uri))
            assert len(contents) == 1
            assert contents[0].content == text
            assert contents[0].mime_type == "text/markdown"


class TestBundledDocs:
    def test_query_language_is_bundled(self):
        assert doc_names() == ("query-language",)
        text = doc_text("query-language")
        assert "## Query shape" in text
        assert "## Examples" in text

    def test_unknown_doc_is_rejected(self):
        with pytest.raises(DocNotFound):
            doc_text("not-a-doc")

    def test_cli_prints_the_doc_verbatim(self):
        result = _cli("docs", "query-language")
        assert result.exit_code == 0, result.output
        assert result.output == doc_text("query-language")

    def test_cli_lists_docs_without_a_name(self):
        result = _cli("docs")
        assert result.exit_code == 0, result.output
        assert "query-language" in result.output

    def test_cli_rejects_unknown_doc(self):
        result = _cli("docs", "not-a-doc")
        assert result.exit_code == 1
        assert "unknown document" in result.output


class TestHelpSurfaceParity:
    def test_cli_help_states_the_set_project_exception(self):
        result = _cli("--help")
        assert result.exit_code == 0, result.output
        assert "coderadar_set_project" in result.output
        assert "-C/--project" in result.output

    def test_query_help_points_at_the_bundled_reference(self):
        result = _cli("query", "--help")
        assert result.exit_code == 0, result.output
        assert "coderadar docs query-language" in result.output


class TestSkillPackaging:
    def test_both_packs_ship_inside_the_package(self):
        assert skill_names() == ("coderadar-cli", "coderadar-mcp")
        for name in skill_names():
            assert (SKILLS / name / "SKILL.md").is_file()

    def test_both_packs_teach_skill_installation(self):
        for name in skill_names():
            text = _skill(name)
            assert "coderadar skills install" in text
            assert "coderadar skills show" in text

    def test_frontmatter_name_matches_directory(self):
        for name in skill_names():
            assert re.search(rf"(?m)^name:\s*{re.escape(name)}\s*$", _skill(name)), (
                f"{name}/SKILL.md frontmatter name does not match its directory")

    def test_every_skill_has_a_trigger_description(self):
        descriptions = dict(iter_skills())
        assert set(descriptions) == set(skill_names())
        for name, description in descriptions.items():
            assert len(description) >= 80, f"{name} description is too short to trigger on"
            assert description == skill_description(name)

    def test_install_copies_both_packs(self, tmp_path):
        results = install(tmp_path)
        assert {r.status for r in results} == {"installed"}
        for name in skill_names():
            copied = (tmp_path / name / "SKILL.md").read_text(encoding="utf-8")
            assert copied == _skill(name)

    def test_reinstall_is_non_destructive_and_force_wins(self, tmp_path):
        install(tmp_path)
        (tmp_path / "coderadar-mcp" / "SKILL.md").write_text("local edit", encoding="utf-8")

        skipped = install(tmp_path)
        assert {r.status for r in skipped} == {"skipped"}
        assert (tmp_path / "coderadar-mcp" / "SKILL.md").read_text(encoding="utf-8") == "local edit"

        updated = install(tmp_path, force=True)
        assert {r.status for r in updated} == {"updated"}
        assert (tmp_path / "coderadar-mcp" / "SKILL.md").read_text(encoding="utf-8") == _skill(
            "coderadar-mcp"
        )

    def test_unknown_skill_is_rejected(self, tmp_path):
        with pytest.raises(SkillNotFound):
            install(tmp_path, names=["not-a-skill"])
        with pytest.raises(SkillNotFound):
            skill_text("not-a-skill")

    def test_default_destinations_cover_both_harnesses(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        project = default_destinations(project=True)
        assert project == (tmp_path / ".claude" / "skills", tmp_path / ".opencode" / "skills")


class TestSkillsCli:
    def test_list_shows_both_skills(self):
        result = _cli("skills", "list")
        assert result.exit_code == 0, result.output
        for name in skill_names():
            assert name in result.output
        assert "Trigger" in result.output

    def test_show_prints_verbatim(self):
        result = _cli("skills", "show", "coderadar-mcp")
        assert result.exit_code == 0, result.output
        assert result.output == _skill("coderadar-mcp")

    def test_install_writes_to_dest(self, tmp_path):
        result = _cli("skills", "install", "--dest", str(tmp_path), "--force")
        assert result.exit_code == 0, result.output
        for name in skill_names():
            assert (tmp_path / name / "SKILL.md").is_file()
            assert name in result.output

    def test_install_rejects_dest_and_project_together(self, tmp_path):
        result = _cli("skills", "install", "--dest", str(tmp_path), "--project")
        assert result.exit_code == 2
        assert "mutually exclusive" in result.output

    def test_show_rejects_unknown_skill(self):
        result = _cli("skills", "show", "not-a-skill")
        assert result.exit_code == 1
        assert "unknown skill" in result.output
