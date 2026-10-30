"""Annotation + comment rows (P6a, DR-44).

Kinds, the write boundary (lint Gate 1 / list-grammar Gate 2), reads,
retire-first deletion, and the INV-13 exclusion pins: a store with
annotations verifies, diffs, and embeds exactly like one without.
The linter itself is pinned in Rust (`annotations::lint_tests`); here:
the ops surface, reports, refusals, CLI/MCP existence, and the
universes that must not see the new kinds.
"""

from __future__ import annotations

import coderadar
import pytest
from coderadar import ops

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover - gate for CI without the extension
    _core = None  # type: ignore[assignment]
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

A = "def alpha():\n    return 1\n"


@pytest.fixture()
def project(tmp_path, monkeypatch):
    """A one-file indexed project with its store loaded."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_text(A, encoding="utf-8", newline="\n")
    coderadar.analyze(".", create_store=True)
    return tmp_path


class TestAnnotate:
    def test_round_trip(self, project):
        created = ops.annotate("a.py::alpha", "- [ ] review\n- [x] indexed\n")
        assert created["id"] == "annotation:a.py::alpha:1"
        assert created["about"] == "a.py::alpha"
        assert created["n"] == 1
        assert created["open"] == 1
        assert created["repaired"] is False
        assert created["body"] == "- [ ] review\n- [x] indexed\n"
        result = ops.annotations_on("a.py::alpha")
        assert result["target"] == "a.py::alpha"
        assert result["count"] == 1
        (version,) = result["versions"]
        assert version["id"] == created["id"]
        assert version["current"] is True
        assert version["retired"] is False

    def test_repair_is_visible_in_the_report(self, project):
        created = ops.annotate("a.py::alpha", "* sloppy\n+spacing\n")
        assert created["repaired"] is True
        assert created["body"] == "- sloppy\n- spacing\n"

    def test_comment_takes_prose(self, project):
        created = ops.comment("a.py::alpha", "# Why\n\nBecause reasons.\n")
        assert created["id"] == "comment:a.py::alpha:1"
        assert created["open"] == 0
        result = ops.comments_on("a.py::alpha")
        assert result["count"] == 1
        # Kinds never mix: the annotation scan stays empty.
        assert ops.annotations_on("a.py::alpha")["count"] == 0

    def test_grammar_refuses_with_line_cited(self, project):
        with pytest.raises(ops.InvalidRequest, match="line 3"):
            ops.annotate("a.py::alpha", "- ok\n\njust prose\n")
        # The same body is a fine comment (Gate 2 is annotations-only).
        created = ops.comment("a.py::alpha", "- ok\n\njust prose\n")
        assert created["id"] == "comment:a.py::alpha:1"

    def test_dangling_about_is_legal(self, project):
        # Annotations outlive their target (retire-first rationale), and
        # existence is branch-relative while rows are trunk-global — so no
        # write-time existence gate. A never-indexed id reads back fine.
        created = ops.annotate("ghost.py::missing", "- [ ] backfill me\n")
        assert created["id"] == "annotation:ghost.py::missing:1"
        assert ops.annotations_on("ghost.py::missing")["count"] == 1

    def test_nesting_and_empties_refuse(self, project):
        with pytest.raises(ops.InvalidRequest, match="never another"):
            ops.annotate("annotation:a.py::alpha:1", "- x\n")
        with pytest.raises(ops.InvalidRequest, match="target concept id"):
            ops.annotate("   ", "- x\n")
        with pytest.raises(ops.InvalidRequest, match="nothing storable|storable"):
            ops.annotate("a.py::alpha", "   \n")


class TestReviseRetire:
    def test_toggle_is_a_version(self, project):
        created = ops.annotate("a.py::alpha", "- [ ] task\n")
        revised = ops.revise_annotation(created["id"], "- [x] task\n")
        assert revised["revised"] is True
        assert revised["open"] == 0
        chain = ops.annotation_history(created["id"])
        assert chain["count"] == 2
        # Newest first (Q-B decided): the current version reads first.
        assert chain["versions"][0]["current"] is True
        assert chain["versions"][0]["open"] == 0
        assert chain["versions"][1]["current"] is False
        assert chain["versions"][1]["open"] == 1
        # Contiguity derives: the older version ends where the newer starts.
        assert (chain["versions"][1]["valid_to"]
                == chain["versions"][0]["valid_from"])

    def test_identical_revise_is_a_noop(self, project):
        created = ops.comment("a.py::alpha", "rationale\n")
        revised = ops.revise_annotation(created["id"], "rationale\n")
        assert revised["revised"] is False
        assert ops.comment_history(created["id"])["count"] == 1

    def test_retire_first_then_closed(self, project):
        created = ops.annotate("a.py::alpha", "- one\n")
        assert ops.retire_annotation(created["id"]) == {
            "id": created["id"], "retired": True}
        # A second retire is a no-op report, not an error.
        assert ops.retire_annotation(created["id"])["retired"] is False
        chain = ops.annotation_history(created["id"])
        assert chain["count"] == 2
        assert chain["versions"][0]["retired"] is True
        assert chain["versions"][0]["current"] is False
        # …but the audit trail still reads, and the row is closed to edits.
        assert ops.annotations_on("a.py::alpha")["count"] == 2
        with pytest.raises(ops.InvalidRequest, match="retired"):
            ops.revise_annotation(created["id"], "- two\n")

    def test_sequences_never_reuse_retired_ids(self, project):
        first = ops.annotate("a.py::alpha", "- one\n")
        ops.retire_annotation(first["id"])
        second = ops.annotate("a.py::alpha", "- two\n")
        assert second["id"] == "annotation:a.py::alpha:2"

    def test_user_writes_never_touch_code_rows(self, project):
        with pytest.raises(ops.InvalidRequest):
            ops.revise_annotation("a.py::alpha", "- x\n")
        with pytest.raises(ops.InvalidRequest):
            ops.retire_annotation("a.py::alpha")
        # Well-formed-but-absent reads empty (the history_of honesty).
        assert ops.annotation_history("annotation:nope:1")["count"] == 0
        # Malformed refuses instead.
        with pytest.raises(ops.InvalidRequest, match="not an annotation"):
            ops.annotation_history("bogus")

    def test_unknown_about_reads_empty(self, project):
        assert ops.annotations_on("ghost.py::missing")["count"] == 0
        assert ops.comments_on("ghost.py::missing")["count"] == 0


class TestSurfaces:
    def test_codegraph_methods(self, project):
        g = coderadar.CodeGraph(
            str(project / ".coderadar" / "store" / "coderadar.db"))
        created = g.annotate("a.py::alpha", "- x\n")
        assert created["id"] == "annotation:a.py::alpha:1"
        assert g.annotations_on("a.py::alpha")["count"] == 1
        assert g.annotation_history(created["id"])["count"] == 1
        assert g.comment("a.py::alpha", "why\n")["id"] == \
            "comment:a.py::alpha:1"
        assert g.comments_on("a.py::alpha")["count"] == 1
        assert g.comment_history("comment:a.py::alpha:1")["count"] == 1
        assert g.revise_annotation(created["id"], "- y\n")["revised"] is True
        assert g.retire_annotation(created["id"])["retired"] is True

    def test_render(self, project):
        from coderadar import render
        created = ops.annotate("a.py::alpha", "- [ ] x\n")
        text = render.annotation_stored(created)
        assert created["id"] in text and "minted" in text
        chain = render.annotation_versions(ops.annotations_on("a.py::alpha"))
        assert created["id"] in chain and "current" in chain

    def test_cli(self, project):
        from click.testing import CliRunner
        from coderadar.cli import main as cli_main
        runner = CliRunner()
        # A body starting with '-' needs the '--' separator (click
        # would otherwise read it as an option flag).
        result = runner.invoke(cli_main, ["annotate", "a.py::alpha",
                                           "--", "- [ ] x"])
        assert result.exit_code == 0, result.output
        assert "annotation:a.py::alpha:1" in result.output
        result = runner.invoke(cli_main, ["annotations-on", "a.py::alpha"])
        assert result.exit_code == 0, result.output
        assert "annotation:a.py::alpha:1" in result.output
        # Bare '-' reads the body from stdin (the _text_arg convention).
        result = runner.invoke(cli_main, ["comment", "a.py::alpha", "-"],
                               input="why\n")
        assert result.exit_code == 0, result.output
        assert "comment:a.py::alpha:1" in result.output
        result = runner.invoke(cli_main, ["comments-on", "a.py::alpha"])
        assert result.exit_code == 0, result.output
        result = runner.invoke(cli_main, ["revise-annotation",
                                          "annotation:a.py::alpha:1",
                                          "--", "- [x] x"])
        assert result.exit_code == 0, result.output
        result = runner.invoke(cli_main, ["annotation-history",
                                          "annotation:a.py::alpha:1"])
        assert result.exit_code == 0, result.output
        result = runner.invoke(cli_main, ["retire-annotation",
                                          "annotation:a.py::alpha:1"])
        assert result.exit_code == 0, result.output

    def test_mcp_read_tools_exist(self):
        from coderadar.mcp.server import create_server
        tools = {t.name for t in create_server(None)._tool_manager.list_tools()}
        assert {"coderadar_annotations_on", "coderadar_comments_on",
                "coderadar_annotation_history",
                "coderadar_comment_history"} <= tools
        # And the writes stayed out (H-13): no MCP tool mints, revises,
        # or retires annotations.
        assert not (tools & {"coderadar_annotate", "coderadar_comment",
                              "coderadar_revise_annotation",
                              "coderadar_retire_annotation"})


class TestInv13:
    """A store with annotations verifies, diffs, and embeds exactly
    like one without (INV-13)."""

    @pytest.fixture()
    def analyzed(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        (tmp_path / "a.py").write_text(A, encoding="utf-8", newline="\n")
        coderadar.analyze(".", create_store=True)
        return tmp_path

    def _verdict(self):
        # Writes mint generations (every ledger write does), so the
        # generation list legitimately grows: INV-13 pins the verdict —
        # the same coverage for every pre-existing generation, the same
        # totals shape, the same offenders.
        report = ops.verify_history()
        return (report["byte_coverage_pct"],
                {g["timestamp"]: (g["counts"], g["modules"])
                 for g in report["generations"]},
                sorted(report["offender_distinct"]))

    def test_verify_verdict_identical(self, analyzed):
        pct_before, gens_before, offenders_before = self._verdict()
        ops.annotate("a.py::alpha", "- [ ] check coverage\n")
        ops.comment("a.py::alpha", "coverage rationale\n")
        ops.annotate("ghost.py::missing", "- [ ] dangling\n")
        pct_after, gens_after, offenders_after = self._verdict()
        assert pct_before == pct_after
        assert offenders_before == offenders_after
        for ts, verdict in gens_before.items():
            assert gens_after[ts] == verdict

    def test_changes_between_absent(self, analyzed):
        from coderadar import _core
        stamps = _core.recorded_generations(None, None, "main")
        lo, hi = stamps[0], stamps[-1]
        before = ops.changes_between(lo, hi)
        ops.annotate("a.py::alpha", "- [ ] x\n")
        ops.comment("a.py::alpha", "y\n")
        after = ops.changes_between(lo, hi)
        assert before == after
        flat = [e.get("id", e.get("id_after", "?"))
                for group in ("added", "removed", "modified", "moved",
                              "uncompared")
                for e in after[group]]
        assert not [i for i in flat
                    if i.startswith(("annotation:", "comment:"))]

    def test_entity_universe_unchanged(self, analyzed):
        from coderadar import _core
        before = _core.graph_stats()
        ops.annotate("a.py::alpha", "- [ ] x\n")
        ops.comment("a.py::alpha", "y\n")
        assert _core.graph_stats() == before
