"""P2 (DR-37): `ops.changes_between` over a real stored project.

Gen1 -> gen2 exercises every class: a body edit (modified), a rename
(removed + added -- the hash covers the name), a deleted file (removed),
a new file (added), a reorder (moved, same id), a file move with
identical text (moved, cross-id), an untouched file (unmodified) and a
hash-less import (uncompared). Plus the report-shape contract, scope
filtering, request validation, the exactly-two-folds assertion, and an
H-11 merge driven through monkeypatched folds.
"""
from __future__ import annotations

import sqlite3
import time

import coderadar
import pytest
from coderadar import ops, render

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

A_V1 = b"import sys\n\n\ndef alpha():\n    return 1\n\n\ndef beta():\n    return 2\n"
A_V2 = b"import sys\n\n\ndef alpha():\n    return 10\n\n\ndef beta2():\n    return 2\n"
B = b"def gamma():\n    return 3\n"
D_V1 = b"def one():\n    return 1\n\n\ndef two():\n    return 2\n"
D_V2 = b"def two():\n    return 2\n\n\ndef one():\n    return 1\n"
E = b"def eco():\n    return 7\n"
F = b"# moved from e.py\n\n\ndef eco():\n    return 7\n"
C = b"def delta():\n    return 4\n"
Z = b"def zeta():\n    return 0\n"

CONTRACT_KEYS = {
    "store", "root", "branch", "t1", "t2", "scope",
    "normalized_ids", "normalized_groups",
    "added", "removed", "modified", "moved", "uncompared",
    "counts", "unmodified", "predates",
}

EMPTY_COUNTS = {
    "added": 0, "removed": 0, "modified": 0, "moved": 0, "uncompared": 0,
}


def _recorded_max(db_path):
    c = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return c.execute("select max(recorded_at) from transaction_log").fetchone()[0]
    finally:
        c.close()


@pytest.fixture()
def history(tmp_path, monkeypatch):
    """Two generations over six files; returns (t0, t1)."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_bytes(A_V1)
    (tmp_path / "b.py").write_bytes(B)
    (tmp_path / "d.py").write_bytes(D_V1)
    (tmp_path / "e.py").write_bytes(E)
    (tmp_path / "z.py").write_bytes(Z)
    coderadar.analyze(".", create_store=True)
    hot = tmp_path / ".coderadar" / "store" / "coderadar.db"
    t0 = _recorded_max(hot)
    time.sleep(0.01)
    (tmp_path / "a.py").write_bytes(A_V2)
    (tmp_path / "b.py").unlink()
    (tmp_path / "c.py").write_bytes(C)
    (tmp_path / "d.py").write_bytes(D_V2)
    (tmp_path / "e.py").unlink()
    (tmp_path / "f.py").write_bytes(F)
    for changed in ("a.py", "b.py", "c.py", "d.py", "e.py", "f.py"):
        ops.update_file(changed)
    t1 = _recorded_max(hot)
    assert t1 > t0
    return t0, t1


def _ids(entries):
    return sorted(e.get("id", e.get("id_after", "?")) for e in entries)


class TestFourClasses:
    def test_all_classes_nonempty(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1)
        assert all(report["counts"][k] > 0
                   for k in ("added", "removed", "modified", "moved"))
        assert report["counts"]["uncompared"] == 1
        # z.py (module + zeta) plus the reorder-invisible d.py pair.
        assert report["unmodified"] == 4

    def test_added(self, history):
        t0, t1 = history
        assert _ids(ops.changes_between(t0, t1)["added"]) == [
            "a.py::beta2", "c.py::delta", "c.py::module", "f.py::module"]

    def test_removed(self, history):
        t0, t1 = history
        assert _ids(ops.changes_between(t0, t1)["removed"]) == [
            "a.py::beta", "b.py::gamma", "b.py::module", "e.py::module"]

    def test_modified(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1)
        assert _ids(report["modified"]) == [
            "a.py::alpha", "a.py::module", "d.py::module"]
        by_id = {e["id"]: e for e in report["modified"]}
        alpha = by_id["a.py::alpha"]
        assert alpha["hash_before"] != alpha["hash_after"]

    def test_pure_reorder_is_invisible_to_the_update_path(self, history):
        # The update path rewrites a function only on signature/body
        # change (projection_ops needs_insert), so a pure reorder mints
        # no row and the ledger keeps the write-time spans: one/two read
        # as unmodified, not moved. Same-id moved surfaces only after a
        # full re-analyze rewrite — the read side is ready, the write
        # side is a follow-up DR, not P2.
        t0, t1 = history
        report = ops.changes_between(t0, t1, scope="d.py::")
        assert report["counts"]["moved"] == 0
        assert _ids(report["modified"]) == ["d.py::module"]
        assert report["unmodified"] == 2

    def test_moved_same_id_from_rewritten_spans(self, history, monkeypatch):
        # What a full re-analyze rewrite would fold: equal hash, new
        # spans. The classification is data-driven; the fixture above
        # proves the update path never produces this shape.
        t0, _t1 = history
        real = _core.content_hashes_at
        seen: list[str] = []

        def rewritten(ids, ts, branch="main"):
            out = real(ids, ts, branch)
            seen.append(ts)
            if len(seen) == 2:
                for raw, entry in out["hashes"].items():
                    if entry is not None and raw == "z.py::zeta":
                        entry = dict(entry)
                        entry["start_line"] = 10
                        entry["end_line"] = 12
                        out["hashes"][raw] = entry
            return out

        monkeypatch.setattr(_core, "content_hashes_at", rewritten)
        report = ops.changes_between(t0, t0)
        assert [e["id"] for e in report["moved"]] == ["z.py::zeta"]

    def test_moved_cross_id(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1)
        cross = [e for e in report["moved"] if "id_before" in e]
        assert len(cross) == 1
        assert (cross[0]["id_before"], cross[0]["id_after"]) == (
            "e.py::eco", "f.py::eco")
        assert cross[0]["span_before"] != cross[0]["span_after"]

    def test_uncompared_is_presence_only(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1)
        assert len(report["uncompared"]) == 1
        entry = report["uncompared"][0]
        assert set(entry) == {"id"}
        assert entry["id"].startswith("a.py::")

    def test_normalization_quiet_on_single_spelling(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1)
        assert report["normalized_ids"] is False
        assert report["normalized_groups"] == 0

    def test_equal_instants_still_report_presence(self, history):
        t0, _t1 = history
        report = ops.changes_between(t0, t0)
        # A zero-width window over an ever-universe: the five T1-only ids
        # are absent at both folds, so they are out of scope (dropped,
        # not removed); the hash-less import is presence-only at any
        # window, even a zero-width one.
        assert report["counts"] == {**dict(EMPTY_COUNTS), "uncompared": 1}
        ever = _core.all_concept_ids("main")
        assert report["unmodified"] == len(ever) - 6

    def test_report_shape_is_the_contract(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1)
        assert set(report) == CONTRACT_KEYS
        assert report["predates"] == {"t1": False, "t2": False}


class TestScope:
    def test_file_scope(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1, scope="a.py")
        assert report["scope"] == "a.py"
        assert _ids(report["added"]) == ["a.py::beta2"]
        assert _ids(report["removed"]) == ["a.py::beta"]
        assert _ids(report["modified"]) == ["a.py::alpha", "a.py::module"]
        assert report["counts"]["moved"] == 0

    def test_prefix_scope(self, history):
        t0, t1 = history
        scoped = ops.changes_between(t0, t1, scope="d.py::")
        whole = ops.changes_between(t0, t1)
        assert scoped["counts"]["moved"] == 0
        assert scoped["unmodified"] == 2
        assert whole["counts"]["moved"] == 1

    def test_scope_with_no_match_is_empty(self, history):
        t0, t1 = history
        report = ops.changes_between(t0, t1, scope="nope.py")
        assert report["counts"] == dict(EMPTY_COUNTS)
        assert report["unmodified"] == 0


class TestRequestValidation:
    def test_garbage_stamp_is_invalid_request(self, history):
        _t0, t1 = history
        with pytest.raises(ops.InvalidRequest):
            ops.changes_between("not-a-timestamp", t1)

    def test_inverted_window_is_invalid_request(self, history):
        t0, t1 = history
        with pytest.raises(ops.InvalidRequest, match="sorts after"):
            ops.changes_between(t1, t0)


class TestFoldDiscipline:
    def test_exactly_two_folds(self, history, monkeypatch):
        t0, t1 = history
        calls = []
        real = _core.content_hashes_at

        def counting(ids, ts, branch="main"):
            calls.append(ts)
            return real(ids, ts, branch)

        monkeypatch.setattr(_core, "content_hashes_at", counting)
        ops.changes_between(t0, t1)
        assert sorted(calls) == sorted([t0, t1])


class TestRespellingMerge:
    def test_old_and_new_spelling_merge_to_unmodified(self, history, monkeypatch):
        # H-11 without a time machine: both spellings resolve at both T in
        # the crafted folds; the join must merge them, not report churn.
        t0, _t1 = history
        old = ".\\w.py::w"
        new = "w.py::w"
        assert _core.canonical_id(old) == _core.canonical_id(new) == new
        real = _core.content_hashes_at

        def crafted(ids, ts, branch="main"):
            out = real(ids, ts, branch)
            entry = {"id": new, "content_hash": 12345,
                     "start_line": 1, "end_line": 3}
            out["hashes"][old] = dict(entry, id=old)
            out["hashes"][new] = dict(entry)
            return out

        monkeypatch.setattr(_core, "content_hashes_at", crafted)
        monkeypatch.setattr(_core, "visible_all_concept_ids", lambda branch: [old, new])
        report = ops.changes_between(t0, t0)
        assert report["normalized_ids"] is True
        assert report["normalized_groups"] == 1
        assert report["counts"] == dict(EMPTY_COUNTS)
        assert report["unmodified"] == 1


class TestRender:
    def test_names_counts_and_cross_id_move(self, history):
        t0, t1 = history
        text = render.changes_between(ops.changes_between(t0, t1))
        assert "added=4  removed=4  modified=3  moved=1" in text
        assert "e.py::eco -> f.py::eco" in text
        assert "single spelling" in text
