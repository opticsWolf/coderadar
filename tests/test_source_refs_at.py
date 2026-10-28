"""P0 (DR-36): `source_refs_at` batched primitive.

Anti-divergence pin at the FFI boundary: the batched path returns exactly
what N single `entity_source_ref_at` calls return, for present, retired and
absent ids, at two generations — plus the absent-at-T omission rule and the
predates stop condition. No `ops` surface exists by design (P1/P2 are the
only callers), so these tests drive `_core` directly.
"""
from __future__ import annotations

import time

import coderadar
import pytest
from coderadar import ops

try:
    from coderadar import _core

    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


ONE_V1 = b'def greet(name):\n    return f"hi {name}"\n'
ONE_V2 = b'def greet(name):\n    return f"HELLO {name}"\n'
TWO = b'import os\n\n\ndef alpha():\n    return 1\n'

IDS = [
    "one.py::greet",
    "one.py::module",
    "two.py::alpha",
    "two.py::module",
    "one.py::ghost",
]


@pytest.fixture()
def history(tmp_path, monkeypatch):
    """one.py (V1 -> V2) + two.py, persisted store. Returns (t0, t1)."""
    import sqlite3

    monkeypatch.chdir(tmp_path)
    (tmp_path / "one.py").write_bytes(ONE_V1)
    (tmp_path / "two.py").write_bytes(TWO)
    g = coderadar.analyze(".", create_store=True)
    db = tmp_path / ".coderadar" / "store" / "coderadar.db"

    def recorded_max():
        c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            return c.execute("select max(recorded_at) from transaction_log").fetchone()[0]
        finally:
            c.close()

    t0 = recorded_max()
    time.sleep(0.01)
    (tmp_path / "one.py").write_bytes(ONE_V2)
    ops.update_file("one.py", graph=g)
    t1 = recorded_max()
    assert t1 > t0
    return t0, t1


def _singles(ids, ts):
    return {i: _core.entity_source_ref_at(i, ts) for i in ids}


class TestBatchMatchesSingles:
    def test_t0_parity(self, history):
        t0, _t1 = history
        out = _core.source_refs_at(IDS, t0)
        assert out["predates_recorded_history"] is False
        assert set(out["refs"]) == set(IDS) - {"one.py::ghost"}
        assert _singles(IDS, t0) == {i: out["refs"].get(i) for i in IDS}

    def test_t1_parity_after_edit(self, history):
        _t0, t1 = history
        out = _core.source_refs_at(IDS, t1)
        assert out["predates_recorded_history"] is False
        assert set(out["refs"]) == set(IDS) - {"one.py::ghost"}
        assert _singles(IDS, t1) == {i: out["refs"].get(i) for i in IDS}
        # The edit moved the span: batch and single agree on the new bytes.
        assert out["refs"]["one.py::greet"]["digest"] is not None

    def test_absent_ids_omitted_never_none(self, history):
        t0, _t1 = history
        out = _core.source_refs_at(["one.py::ghost", "nope.py::module"], t0)
        assert out["refs"] == {}
        assert out["predates_recorded_history"] is False

    def test_predates_history_is_empty_map(self, history):
        _t0, _t1 = history
        out = _core.source_refs_at(["one.py::greet"], "2000-01-01T00:00:00Z")
        assert out["predates_recorded_history"] is True
        assert out["refs"] == {}

    def test_garbage_timestamp_is_value_error(self, history):
        _t0, _t1 = history
        with pytest.raises(ValueError):
            _core.source_refs_at(["one.py::greet"], "not-a-timestamp")
