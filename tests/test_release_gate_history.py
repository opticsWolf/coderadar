"""Release-gate proof: `as_of` serves old names AND old bytes.

The v0.12 release gate requires: `as_of(before)` → old name and old
bytes; `as_of(after)` → new name and bytes; pre-blob T →
content-unavailable. Names and bytes are asserted in ONE flow (rename +
body change across two generations), so the gate clause is recorded, not
implied by separate suites.
"""

from __future__ import annotations

import coderadar
import pytest

try:
    from coderadar import _core as _c  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

V1 = b"def combine(a, b):\n    return a + b\n"
V2 = b"def merge(a, b):\n    return a + b + 0\n"


def _max_recorded(root) -> str:
    import sqlite3
    db = root / ".coderadar" / "store" / "coderadar.db"
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        return c.execute("SELECT MAX(recorded_at) FROM transaction_log").fetchone()[0]
    finally:
        c.close()


@pytest.fixture()
def renamed(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "m.py").write_bytes(V1)
    g = coderadar.analyze(".", create_store=True)
    # Ledger stamps, not wall time: truncating microseconds to zero lands
    # BEFORE the generation's own recorded stamp within the same second.
    T0 = _max_recorded(tmp_path)
    (tmp_path / "m.py").write_bytes(V2)
    g.update_file("m.py")
    T1 = _max_recorded(tmp_path)
    assert T1 >= T0
    return g, T0, T1


class TestReleaseGateHistory:
    def test_before_gives_old_name_and_old_bytes(self, renamed):
        g, T0, _T1 = renamed
        snap = g.as_of(T0)
        ent = snap.find("m.py::combine")
        assert ent is not None and ent["name"] == "combine"
        assert snap.find("m.py::merge") is None
        assert snap.read_bytes("m.py::combine") == V1[:-1]

    def test_after_gives_new_name_and_new_bytes(self, renamed):
        g, _T0, T1 = renamed
        snap = g.as_of(T1)
        assert snap.find("m.py::combine") is None
        ent = snap.find("m.py::merge")
        assert ent is not None and ent["name"] == "merge"
        # Node extent, not the file: the trailing newline sits outside the
        # tree-sitter span (§3.0 proven [0,44)-of-46 shape).
        assert snap.read_bytes("m.py::merge") == V2[:-1]
        assert snap.read_bytes("m.py::module") == V2

    def test_predates_history_is_nothing_not_something(self, renamed):
        g, _T0, _T1 = renamed
        snap = g.as_of("2000-01-01T00:00:00.000000Z")
        assert snap.find("m.py::combine") is None
        assert snap.read_bytes("m.py::combine") is None
