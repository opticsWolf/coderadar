"""The content-version spine (plan §5 P3, DR-38).

Done clause: `history_of` returns the full chain for a file edited three
times, after an analyze → edit → update → edit → update sequence, and the
unchanged-reindex test shows zero spine growth.
"""

from __future__ import annotations

import time
from pathlib import Path

import coderadar
import pytest
from click.testing import CliRunner
from coderadar import ops
from coderadar.cli import main as _main
from coderadar.render import history_of as render_history_of

try:
    from coderadar._core import analyze as _analyze  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

V1 = 'x = 1\n'
V2 = 'x = 1\ny = 2\n'
V3 = 'x = 1\ny = 2\nz = 3\n'


@pytest.fixture
def spine_project(tmp_path, monkeypatch):
    root = tmp_path / "spine"
    root.mkdir()
    (root / ".coderadar").mkdir()
    (root / "a.py").write_text(V1, encoding="utf-8")
    monkeypatch.chdir(root)
    coderadar.analyze(".", create_store=True)
    return root


def _chain():
    return ops.history_of("a.py")


def _rewrite_then_update(root: Path, content: str):
    (root / "a.py").write_text(content, encoding="utf-8")
    time.sleep(0.05)
    coderadar.CodeGraph().update_file("a.py")


class TestSpineContract:
    def test_first_index_mints_one_open_version(self, spine_project):
        report = _chain()
        assert report["count"] == 1
        (v,) = report["versions"]
        assert v["parent_digest"] is None
        assert v["open"] is True
        assert v["size_bytes"] == (spine_project / "a.py").stat().st_size
        assert v["line_count"] == 1
        assert v["digest"]

    def test_index_twice_unchanged_writes_zero_spine_rows(self, spine_project):
        before = _chain()
        coderadar.analyze(".", create_store=True)
        after = _chain()
        assert after == before, "unchanged reindex must not grow the spine"

    def test_edit_mints_one_concept_one_close_one_open(self, spine_project):
        before = _chain()
        _rewrite_then_update(spine_project, V2)
        report = _chain()
        assert report["count"] == 2
        head, tail = report["versions"]
        assert head["open"] is True
        assert tail["open"] is False
        assert head["parent_digest"] == tail["digest"] == before["versions"][0]["digest"]
        assert head["size_bytes"] == (spine_project / "a.py").stat().st_size
        assert head["line_count"] == 2
        # Close and open share one stamp: contiguous by construction.
        assert tail["valid_to"] == head["valid_from"]

    def test_three_edits_yield_full_chain_newest_first(self, spine_project):
        _rewrite_then_update(spine_project, V2)
        _rewrite_then_update(spine_project, V3)
        report = _chain()
        assert report["count"] == 3
        v3, v2, v1 = report["versions"]
        assert [v["open"] for v in (v3, v2, v1)] == [True, False, False]
        assert v3["parent_digest"] == v2["digest"]
        assert v2["parent_digest"] == v1["digest"]
        assert v1["parent_digest"] is None
        assert v2["valid_to"] == v3["valid_from"]
        assert v1["valid_to"] == v2["valid_from"]
        assert v3["size_bytes"] == (spine_project / "a.py").stat().st_size
        assert v3["line_count"] == 3

    def test_delete_keeps_chain_and_closes_head(self, spine_project):
        _rewrite_then_update(spine_project, V2)
        expected = [v["digest"] for v in _chain()["versions"]]
        (spine_project / "a.py").unlink()
        coderadar.CodeGraph().update_file("a.py")
        report = _chain()
        # The chain is intact: same digests, same order, queryable still.
        assert [v["digest"] for v in report["versions"]] == expected
        # The retire cascade closed the head: the file's last content
        # ended when the file did — but no spine row was retired.
        assert all(v["open"] is False for v in report["versions"])

    def test_framework_pass_does_not_retire_spine(self, tmp_path, monkeypatch):
        # H-3 regression guard: the §1.3 hook runs on every update_file;
        # it must never close a HAD_CONTENT edge (DR-42 excludes the
        # non-synthetic namespace from its sweep, and the scope filter
        # only ever matches route-id sources).
        root = tmp_path / "shop"
        root.mkdir()
        (root / ".coderadar").mkdir()
        (root / "app.py").write_text(
            'from flask import Flask\napp = Flask(__name__)\n\n'
            '@app.route("/health")\ndef health():\n    return {"ok": True}\n',
            encoding="utf-8",
        )
        monkeypatch.chdir(root)
        coderadar.analyze(".", create_store=True)
        before = ops.history_of("app.py")
        assert before["count"] == 1
        coderadar.CodeGraph().update_file("app.py")
        after = ops.history_of("app.py")
        assert after == before

    def test_spine_ids_excluded_from_diff_universe(self, spine_project):
        # P2's ever-universe must never list ledger infrastructure:
        # `all_concept_ids` excludes `content_version` by kind (the
        # live-ids exclusion is pinned at the Rust level, where the
        # sweeps consume it).
        _rewrite_then_update(spine_project, V2)
        from coderadar._core import all_concept_ids
        assert all_concept_ids("main")
        assert not [i for i in all_concept_ids("main") if "@" in i]

    def test_v1_downgrade_heals_spine_rows(self, spine_project):
        # The upgrade regression (cf. test_ensure_graph_falls_back_to_
        # analyze_on_v1_store): a v1-style store (meta_version stripped)
        # must heal spine rows on re-analyze. The timeless id once made
        # "same digest" skip the row entirely; now the gate — not a
        # digest pre-check — decides, and the stripped row rewrites with
        # fresh metadata instead of staying v1 forever.
        import datetime
        import json
        import sqlite3

        _rewrite_then_update(spine_project, V2)
        before = _chain()
        assert before["count"] == 2
        db = spine_project / ".coderadar" / "store" / "coderadar.db"
        con = sqlite3.connect(str(db))
        try:
            rows = con.execute(
                "SELECT id, content, recorded_at FROM concepts WHERE retired = 0"
            ).fetchall()
            fmt = "%Y-%m-%dT%H:%M:%S.%fZ"
            base = max(
                # Stored ledger timestamps are naive by schema; attaching
                # a zone would corrupt the round-trip under test.
                datetime.datetime.strptime(ra, fmt)  # noqa: DTZ007
                for _, _, ra in rows
            )
            for i, (cid, content, _) in enumerate(rows):
                data = json.loads(content)
                if "meta_version" not in data:
                    continue
                data.pop("meta_version")
                ts = (base + datetime.timedelta(microseconds=i + 1)).strftime(fmt)
                con.execute(
                    "UPDATE concepts SET content = ?, recorded_at = ? WHERE id = ?",
                    (json.dumps(data), ts, cid),
                )
            con.commit()
        finally:
            con.close()
        coderadar.analyze(".")
        after = _chain()
        assert [v["digest"] for v in after["versions"]] == [
            v["digest"] for v in before["versions"]
        ]
        con = sqlite3.connect(str(db))
        try:
            (spine_count,) = con.execute(
                "SELECT COUNT(*) FROM concepts WHERE retired = 0 AND id LIKE 'a.py@%'"
            ).fetchone()
            bad = con.execute(
                "SELECT id FROM concepts WHERE retired = 0 AND id LIKE 'a.py@%'"
                " AND json_extract(content, '$.meta_version') != 2"
            ).fetchall()
        finally:
            con.close()
        assert spine_count == 2
        assert bad == []

    def test_history_of_report_shape_contract(self, spine_project):
        _rewrite_then_update(spine_project, V2)
        report = _chain()
        assert tuple(sorted(report.keys())) == tuple(sorted(ops.HISTORY_OF_CONTRACT_KEYS))
        for v in report["versions"]:
            assert tuple(sorted(v.keys())) == tuple(sorted(ops.HISTORY_OF_VERSION_KEYS))

    def test_history_of_rejects_blank_file(self, spine_project):
        with pytest.raises(Exception, match="file path"):
            ops.history_of("   ")

    def test_cold_load_skips_spine_concepts(self, spine_project):
        # The classifier variant must divert spine rows before
        # `parse_v2_concept`: a fresh load with spine concepts present
        # restores entities and keeps the chain queryable (no V1Leftover
        # hard-fail, no entity pollution).
        _rewrite_then_update(spine_project, V2)
        before = _chain()
        coderadar.load(
            str(spine_project / ".coderadar" / "store" / "coderadar.db"),
            str(spine_project),
        )
        assert _chain() == before
        assert ops.node("a.py::module")["kind"] == "module"

    def test_render_and_surfaces(self, spine_project):
        _rewrite_then_update(spine_project, V2)
        text = render_history_of(_chain())
        assert "a.py" in text
        assert "(open)" in text
        assert str(_chain()["count"]) in text
        # CodeGraph method agrees with the op.
        assert coderadar.CodeGraph().history_of("a.py") == _chain()
        # CLI history exits 0 and names the file.
        out = CliRunner().invoke(_main, ["history", "a.py"])
        assert out.exit_code == 0, out.output
        assert "a.py" in out.output
