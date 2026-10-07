"""Blob read path E2E (plan §3.0, DR-25).

`Snapshot.read_bytes` (raw, byte-exact) + `Snapshot.read_source` (display)
over a real stored project with history: exact bytes at T0 and T1 across
an update, CRLF + non-ASCII preservation, range slicing, and every honest
edge — absent-at-T (`None`), in-graph-but-byteless (`ContentUnavailable`
in its three shapes), malformed digests (`InvalidRequest`).
"""

from __future__ import annotations

import hashlib
import sqlite3
import time

import pytest

import coderadar
from coderadar import ops

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

# Single function, starts at offset 0, no trailing newline: the entity span
# is the whole file, so byte-equality is assertable without span knowledge.
ONE_V1 = (
    b'def greet(name):\r\n'
    b'    """Say hello \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x91\x8b."""\r\n'
    b'    return f"hi {name}"'
)
ONE_V2 = (
    b'def greet(name):\r\n'
    b'    """Say hello \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x91\x8b."""\r\n'
    b'    return f"HELLO {name}"'
)

TWO = (
    b'import os\r\n'
    b'\r\n'
    b'def alpha():\r\n'
    b'    """First: caf\xc3\xa9."""\r\n'
    b'    return 1\r\n'
    b'\r\n'
    b'def beta():\r\n'
    b'    """Second."""\r\n'
    b'    return 2\r\n'
)

FLASK_APP = (
    b'from flask import Flask\r\n'
    b'app = Flask(__name__)\r\n'
    b'\r\n'
    b"@app.route('/users/<id>')\r\n"
    b'def get_user(id):\r\n'
    b'    """Fetch one user."""\r\n'
    b'    return id\r\n'
)


def _recorded_max(db_path):
    c = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return c.execute("select max(recorded_at) from transaction_log").fetchone()[0]
    finally:
        c.close()


@pytest.fixture()
def history(tmp_path, monkeypatch):
    """one.py (V1, CRLF+CJK) + two.py, indexed; then one.py → V2.

    Returns (graph, t0, t1): ledger stamps, race-free.
    """
    monkeypatch.chdir(tmp_path)
    (tmp_path / "one.py").write_bytes(ONE_V1)
    (tmp_path / "two.py").write_bytes(TWO)
    g = coderadar.analyze(".", create_store=True)
    db = tmp_path / ".coderadar" / "store" / "coderadar.db"
    t0 = _recorded_max(db)
    time.sleep(0.01)
    (tmp_path / "one.py").write_bytes(ONE_V2)
    ops.update_file("one.py", graph=g)
    t1 = _recorded_max(db)
    assert t1 > t0
    return g, t0, t1


class TestRawBytes:
    def test_t0_is_original_bytes_exactly(self, history):
        g, t0, _t1 = history
        snap = g.as_of(t0)
        assert snap.read_bytes("one.py::greet") == ONE_V1

    def test_t1_is_new_bytes(self, history):
        g, _t0, t1 = history
        snap = g.as_of(t1)
        assert snap.read_bytes("one.py::greet") == ONE_V2

    def test_history_not_present_tense(self, history):
        g, t0, _t1 = history
        snap = g.as_of(t0)
        raw = snap.read_bytes("one.py::greet")
        assert b"HELLO" not in raw and b"hi {name}" in raw

    def test_multifunc_slice_preserves_crlf_and_cjk(self, history):
        g, t0, _t1 = history
        raw = g.as_of(t0).read_bytes("two.py::alpha")
        assert b"\r\n" in raw, "CRLF must survive bit-for-bit"
        assert "café".encode("utf-8") in raw, "non-ASCII must survive bit-for-bit"
        assert raw in TWO
        assert b"def beta" not in raw, "slice must not leak the sibling"

    def test_range_slicing(self, history):
        g, t0, _t1 = history
        snap = g.as_of(t0)
        full = snap.read_bytes("one.py::greet")
        assert snap.read_bytes("one.py::greet", 0, 3) == b"def"
        assert snap.read_bytes("one.py::greet", 4, 9) == full[4:9]

    def test_bad_range_is_invalid_request(self, history):
        g, t0, _t1 = history
        snap = g.as_of(t0)
        n = len(snap.read_bytes("one.py::greet"))
        with pytest.raises(ops.InvalidRequest):
            snap.read_bytes("one.py::greet", 5, 2)
        with pytest.raises(ops.InvalidRequest):
            snap.read_bytes("one.py::greet", 0, n + 1)
        with pytest.raises(ops.InvalidRequest):
            snap.read_bytes("one.py::greet", -1, 3)


class TestAbsent:
    def test_unknown_entity_is_none(self, history):
        g, t0, _t1 = history
        snap = g.as_of(t0)
        assert snap.read_bytes("nope.py::nope") is None
        assert snap.read_source("nope.py::nope") is None

    def test_predates_history_is_none(self, history):
        g, _t0, _t1 = history
        snap = g.as_of("2000-01-01T00:00:00.000000Z")
        assert snap.read_bytes("one.py::greet") is None

    def test_retired_is_none_not_unavailable(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        (tmp_path / "gone.py").write_bytes(ONE_V1)
        g = coderadar.analyze(".", create_store=True)
        (tmp_path / "gone.py").unlink()
        coderadar.CodeGraph().update_file("gone.py")
        db = tmp_path / ".coderadar" / "store" / "coderadar.db"
        snap = g.as_of(_recorded_max(db))
        assert snap.find("gone.py::greet") is None
        assert snap.read_bytes("gone.py::greet") is None


class TestContentUnavailable:
    def test_blobs_disabled(self, tmp_path, monkeypatch):
        # Library analyze does not auto-read TOML (CLI/MCP activate it):
        # push the kill-switch explicitly, restore after.
        from coderadar._core import set_config as _set_config
        monkeypatch.chdir(tmp_path)
        (tmp_path / "plain.py").write_bytes(ONE_V1)
        _set_config({"database": {"store_source_blobs": False}})
        try:
            g = coderadar.analyze(".", create_store=True)
        finally:
            _set_config({"database": {"store_source_blobs": True}})
        db = tmp_path / ".coderadar" / "store" / "coderadar.db"
        snap = g.as_of(_recorded_max(db))
        assert snap.find("plain.py::greet") is not None
        with pytest.raises(ops.ContentUnavailable, match="no source blob recorded"):
            snap.read_bytes("plain.py::greet")

    # "Recorded but unresolvable" needs a cold-only blob with no cold
    # file — a retention scenario, staged in tests/test_retention.py via
    # ops.archive (macrame guards blob deletes outside archive sessions).

    def test_route_names_handler(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        (tmp_path / "app.py").write_bytes(FLASK_APP)
        g = coderadar.analyze(".", create_store=True)
        resolved = ops.resolve("/users/42")
        route_id = _find_route_id(resolved)
        assert route_id is not None, f"flask route must resolve: {resolved!r}"
        db = tmp_path / ".coderadar" / "store" / "coderadar.db"
        snap = g.as_of(_recorded_max(db))
        with pytest.raises(ops.ContentUnavailable, match="no source span"):
            snap.read_bytes(route_id)


def _find_route_id(obj):
    """First id in a resolve payload that names a route concept."""
    if isinstance(obj, dict):
        for key in ("id", "route", "pattern"):
            val = obj.get(key)
            if isinstance(val, str) and ":route:" in val:
                return val
        for val in obj.values():
            found = _find_route_id(val)
            if found is not None:
                return found
    elif isinstance(obj, list):
        for val in obj:
            found = _find_route_id(val)
            if found is not None:
                return found
    return None


class TestBlobGet:
    def test_round_trip(self, history):
        g, t0, _t1 = history
        ref = _core.entity_source_ref_at("one.py::greet", t0)
        assert ops.blob_get(ref["digest"]) == ONE_V1

    def test_absent_digest_is_none(self, history):
        missing = hashlib.sha256(b"never indexed anywhere").hexdigest()
        assert ops.blob_get(missing) is None

    def test_malformed_digest_is_invalid_request(self, history):
        with pytest.raises(ops.InvalidRequest, match="malformed blob digest"):
            ops.blob_get("zz")
        with pytest.raises(ops.InvalidRequest, match="malformed blob digest"):
            ops.blob_get("A" * 64)


class TestDisplay:
    def test_read_source_is_numbered_slice(self, history):
        g, t0, _t1 = history
        text = g.as_of(t0).read_source("one.py::greet")
        assert text == (
            "1\tdef greet(name):\n"
            "2\t    \"\"\"Say hello \u65e5\u672c\u8a9e \U0001f44b.\"\"\"\n"
            "3\t    return f\"hi {name}\"\n"
        )

    def test_display_normalizes_display_raw_preserves(self, history):
        g, t0, _t1 = history
        snap = g.as_of(t0)
        assert b"\r\n" in snap.read_bytes("one.py::greet")
        assert "\r" not in snap.read_source("one.py::greet")
