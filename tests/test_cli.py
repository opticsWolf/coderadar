"""The CLI answers with real data, or fails.

Nothing in the suite imported `coderadar.cli` before this file, which is how
`rebuild` came to print "Rebuilding..." and return without indexing, how
`status` came to report "CodeRadar is running" unconditionally, how
`diagnose` printed headers and no rows, how two different commands were both
registered as `watch` (click keys on the function name, so the first was
unreachable), and how every read-only command came to answer "No graph
loaded — run coderadar init first" to a user who had just run it.

These tests drive the commands through click's runner against a real
project, and assert on what comes back rather than on the exit code alone.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest
from click.testing import CliRunner

from coderadar.cli import main

try:
    import coderadar._core  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


SOURCE = '''\
class Base:
    def describe(self):
        return "base"


class Derived(Base):
    def describe(self):
        return helper() + "derived"


def helper():
    return "x"


async def fetch(url):
    return url


def gen():
    yield 1
'''


@pytest.fixture
def project(tmp_path):
    (tmp_path / "app.py").write_text(SOURCE, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        yield tmp_path
    finally:
        os.chdir(previous)


@pytest.fixture
def run(project):
    def _run(*args):
        return CliRunner().invoke(main, list(args), catch_exceptions=False)
    return _run


class TestOneCommandPerName:
    def test_watch_is_registered_once_with_debounce(self):
        """Two functions were both named `watch`; the second won silently.

        The losing one carried the config activation, so the surviving
        command ran without ever reading `.coderadar.toml`.
        """
        watch = main.commands["watch"]
        assert {p.name for p in watch.params} == {"paths", "debounce"}

    def test_the_removed_commands_are_gone(self):
        # `mutations` reported an "audit trail from MutationLog" for a
        # MutationLog that does not exist anywhere in the codebase.
        assert "mutations" not in main.commands


class TestCommandsThatUsedToAnswerNothing:
    def test_rebuild_actually_indexes(self, run):
        result = run("rebuild", ".")

        assert result.exit_code == 0, result.output
        # It used to print "Rebuilding..." and return.
        assert "function(s)" in result.output
        assert "0 function(s)" not in result.output

    def test_status_reports_the_project_not_a_slogan(self, run):
        result = run("status")

        assert result.exit_code == 0, result.output
        assert "CodeRadar is running" not in result.output
        assert "Project:" in result.output

    def test_diagnose_reports_rows_or_says_none(self, run):
        result = run("diagnose", "--low-confidence")

        assert result.exit_code == 0, result.output
        assert "Ambiguous base classes:" in result.output
        # A header with nothing under it reads as a clean bill of health.
        assert "none" in result.output or "ambiguous" in result.output

    def test_stats_answers_after_a_bare_init(self, run):
        """The whole read-only surface used to dead-end here.

        The graph lives in the process that built it, so a fresh `stats`
        process found an empty core and told the user to run the command
        they had already run.
        """
        result = run("stats")

        assert result.exit_code == 0, result.output
        assert "No graph loaded" not in result.output
        assert "functions" in result.output


class TestQueriesSeeRealFields:
    def test_is_async_matches(self, run):
        """`is_async` was hardcoded false at extraction for every language."""
        result = run("query", "functions where is_async == true")

        assert result.exit_code == 0, result.output
        # `fetch` is the only async function in the fixture. The rendered
        # table truncates names at narrow widths, so assert on the count.
        assert "No results" not in result.output
        assert "1 result(s)" in result.output

    def test_is_async_does_not_match_everything(self, run):
        result = run("query", "functions where is_async == false")

        assert result.exit_code == 0, result.output
        assert "1 result(s)" not in result.output

    def test_callers_finds_the_call_site(self, run):
        result = run("callers", ".{sep}app.py::helper".format(sep=os.sep))

        assert result.exit_code == 0, result.output
        assert "describe" in result.output


class TestVisualizeFailsInsteadOfInventing:
    def test_it_draws_the_real_hierarchy(self, run):
        result = run("visualize", "hierarchy", "--format", "graphviz")

        assert result.exit_code == 0, result.output
        assert "Derived" in result.output and "Base" in result.output
        assert "BaseModel" not in result.output, "demo data came back"

    def test_an_entity_with_no_calls_is_an_error_not_a_diagram(self, run):
        run("rebuild", ".")
        result = CliRunner().invoke(
            main, ["visualize", "call-graph", "gen"], catch_exceptions=False)

        assert result.exit_code == 1
        assert "Nothing to visualize" in result.output
        assert "validate_input" not in result.output


class TestExitCodes:
    def test_a_failed_update_does_not_report_success(self, run):
        result = run("update", "does_not_exist.py")

        assert result.exit_code != 0, result.output

    def test_the_removed_export_command_is_not_a_success(self, run):
        # v0.8 P1 retired `coderadar export` along with the Python-side
        # snapshot format; the store is the snapshot now.
        result = run("export", "snap.bin")

        assert result.exit_code == 2
        assert "No such command" in result.output
