"""§0.1(a) structural honesty (DR-9): the temporal surface answers from T.

Covers the plan's done-clause: the rename-a-function test (`as_of(before)`
sees the old name, `as_of(after)` the new one); `Snapshot.query` raises;
MCP/CLI/`CodeGraph.as_of` agree; naive/offset/pre-history cases pinned.
Bytes-at-T assertions wait on §0.1(b) — entities carry `ByteSpan`s here,
not source bodies.
"""

import sqlite3
import time

import pytest
from coderadar import analyze, ops

V1 = 'def old_name():\n    return 1\n\n\ndef caller():\n    return old_name()\n'
V2 = 'def new_name():\n    return 1\n\n\ndef caller():\n    return new_name()\n'


def _recorded_max(db_path):
    c = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return c.execute("select max(recorded_at) from transaction_log").fetchone()[0]
    finally:
        c.close()


@pytest.fixture()
def renamed(tmp_path, monkeypatch):
    """tmp project indexed as v1, then renamed to v2: returns (graph, t1, t2).

    Timestamps come straight from the ledger (race-free): t1 is the newest
    stamp after the v1 index, t2 after the rename update.
    """
    monkeypatch.chdir(tmp_path)
    (tmp_path / "mod.py").write_text(V1)
    g = analyze(".", create_store=True)
    db = tmp_path / ".coderadar" / "store" / "coderadar.db"
    t1 = _recorded_max(db)
    time.sleep(0.01)  # separate recorded stamps across the rename
    (tmp_path / "mod.py").write_text(V2)
    ops.update_file("mod.py", graph=g)
    t2 = _recorded_max(db)
    assert t2 > t1, "rename must record after the v1 index"
    return g, t1, t2


def _fid(mod_path, func):
    # Entity ids are deterministic (`file::name` for module-level
    # functions) — no live-graph query needed, which is exactly what lets
    # the pre-rename tests address the retired name.
    return f"{mod_path}::{func}"


def test_as_of_before_sees_old_name(renamed):
    g, t1, _t2 = renamed
    old_id = _fid("mod.py", "old_name")
    snap = g.as_of(t1)
    ent = snap.find(old_id)
    assert ent is not None, "v1 entity must resolve at recorded-time t1"
    assert ent["name"] == "old_name"


def test_as_of_after_sees_new_name_not_old(renamed):
    g, _t1, t2 = renamed
    old_id = _fid("mod.py", "old_name")
    snap = g.as_of(t2)
    assert snap.find(old_id) is None, "retired name must be absent at t2"
    ent = snap.find(_fid("mod.py", "new_name"))
    assert ent is not None and ent["name"] == "new_name"


def test_ops_as_of_entities_are_found_not_none(renamed):
    """The old all-`None` bug (`Snapshot` had no `find`) stays dead."""
    _, t1, _t2 = renamed
    old_id = _fid("mod.py", "old_name")
    result = ops.as_of(t1, symbols=[old_id, "mod.py::never_existed"])
    assert result["entities"][old_id] is not None
    assert result["entities"][old_id]["name"] == "old_name"
    assert result["entities"]["mod.py::never_existed"] is None
    assert result["predates_recorded_history"] is False
    assert result["timestamp"] == t1  # already canonical: echoed unchanged


def test_snapshot_query_and_callers_raise(renamed):
    g, t1, _t2 = renamed
    snap = g.as_of(t1)
    with pytest.raises(ops.TemporalUnsupported):
        snap.query("functions where name contains 'old_name'")
    with pytest.raises(ops.TemporalUnsupported):
        snap.callers("mod.py::caller")
    with pytest.raises(ops.TemporalUnsupported):
        snap.traverse("mod.py::caller", direction="both")
    with pytest.raises(ops.TemporalUnsupported):
        snap.traverse("mod.py::caller", direction="in")


def test_snapshot_traverse_out_and_callees_are_real(renamed):
    g, t1, _t2 = renamed
    snap = g.as_of(t1)
    caller_id = _fid("mod.py", "caller")
    walked = snap.traverse(caller_id, direction="out", max_depth=2)
    assert walked, "downstream walk at t1 must reach the v1 callee"
    names = [n.get("name") for n in walked if isinstance(n, dict)]
    assert "old_name" in names, f"at-T bodies, not present-tense: {names}"
    assert "new_name" not in names
    callees = snap.callees(caller_id)
    assert [c["name"] for c in callees] == ["old_name"]


def test_garbage_timestamps_rejected(renamed):
    g, _t1, _t2 = renamed
    for bad in ["now", "2026-10-06T15:23:34+01:00",
                "2026-10-06T15:23:34.123Z", "2026-10-06 15:23:34", ""]:
        with pytest.raises(ops.InvalidRequest):
            g.as_of(bad)
        with pytest.raises(ops.InvalidRequest):
            ops.as_of(bad, symbols=["mod.py::caller"])
    # Legacy second-precision widens to canonical instead of rejecting.
    snap = g.as_of("2026-10-06T15:23:34Z")
    assert snap.timestamp == "2026-10-06T15:23:34.000000Z"


def test_pre_history_reports_predates_distinctly(renamed):
    g, _t1, _t2 = renamed
    pre = "2000-01-01T00:00:00.000000Z"
    snap = g.as_of(pre)
    assert snap.predates_recorded_history is True
    assert snap.find("mod.py::caller") is None
    result = ops.as_of(pre, symbols=["mod.py::caller"])
    assert result["predates_recorded_history"] is True
    assert result["entities"] == {"mod.py::caller": None}


def test_entry_points_agree(renamed):
    """MCP/CLI/`CodeGraph.as_of` answer the same lookup the same way."""
    g, t1, _t2 = renamed
    old_id = _fid("mod.py", "old_name")
    via_python = ops.as_of(t1, symbols=[old_id])["entities"][old_id]
    assert via_python["name"] == "old_name"
    # CLI renders the same entity (name + file reach the report).
    from click.testing import CliRunner
    from coderadar.cli import main as cli_main
    res = CliRunner().invoke(cli_main, ["as-of", t1, old_id])
    assert res.exit_code == 0, res.output
    assert "old_name" in res.output and "mod.py" in res.output
    # MCP funnels the same `ops.as_of` (no separate temporal logic).
    from coderadar.mcp.server import _as_of as mcp_as_of
    rendered = mcp_as_of(g, t1, "", [old_id])
    assert "old_name" in rendered
