"""Retention + backup E2E (plan §3.4, DR-25).

`ops.archive` over a real stored project with blob history: unreferenced
blobs move cold yet stay `blob_get`-readable (through CodeRadar's own
`Snapshot.read_bytes`), a lost cold blob is the honest `ContentUnavailable`
(relocated from `test_blob_reads`: macrame guards hot deletes, so the
staging lives here beside the archive seam), and the hot+cold backup pair
restores every retained digest — including against a pre-0.19 cold file
(no `blobs` table: absent, never an error).
"""

from __future__ import annotations

import shutil
import sqlite3
import time
from pathlib import Path

import pytest

import coderadar
from coderadar import ops

try:
    from coderadar import _core  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

A_V1 = b'def alpha():\r\n    """First."""\r\n    return 1\r\n'
A_V2 = b'def alpha():\r\n    """First (revised)."""\r\n    return 1\r\n'
B = b'def beta():\r\n    """Second."""\r\n    return 2\r\n'

FUTURE = "2999-01-01T00:00:00.000000Z"


def _recorded_max(db_path):
    c = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return c.execute("select max(recorded_at) from transaction_log").fetchone()[0]
    finally:
        c.close()


def _store_files(root: Path) -> tuple[Path, Path]:
    hot = root / ".coderadar" / "store" / "coderadar.db"
    cold = root / ".coderadar" / "store" / "coderadar_archive.db"
    return hot, cold


def _checkpoint(hot: Path) -> None:
    # The store is WAL-mode: a main-file copy without its -wal misses
    # un-checkpointed rows. The backup procedure checkpoints first, so
    # the main file alone is a consistent snapshot (then + cold = pair).
    c = sqlite3.connect(str(hot))
    try:
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    finally:
        c.close()


@pytest.fixture()
def generations(tmp_path, monkeypatch):
    """a.py (V1→V2) + b.py (V1 once): two blob generations for a.py.

    Returns (graph, t0, t1, digests): digests maps (file, gen) → (digest, bytes).
    """
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_bytes(A_V1)
    (tmp_path / "b.py").write_bytes(B)
    g = coderadar.analyze(".", create_store=True)
    hot, _cold = _store_files(tmp_path)
    t0 = _recorded_max(hot)
    time.sleep(0.01)
    (tmp_path / "a.py").write_bytes(A_V2)
    ops.update_file("a.py", graph=g)
    t1 = _recorded_max(hot)
    assert t1 > t0
    digests = {}
    for fid, gen, ts, expect in (("a.py::alpha", "v1", t0, A_V1),
                                 ("a.py::alpha", "v2", t1, A_V2),
                                 ("b.py::beta", "v1", t1, B)):
        ref = _core.entity_source_ref_at(fid, ts)
        assert ref is not None and ref["digest"], f"no digest for {fid}@{gen}"
        digests[(fid, gen)] = (ref["digest"], expect)
    return g, t0, t1, digests


class TestArchive:
    def test_report_shape(self, generations):
        g, _t0, _t1, _digests = generations
        report = ops.archive(FUTURE)
        assert set(report) == {"links_archived", "concepts_archived",
                               "log_entries_archived", "horizon",
                               "blobs_archived", "blobs_restored",
                               "blob_scan_bytes", "cutoff"}
        assert report["cutoff"] == FUTURE
        assert report["blobs_archived"] >= 1, "a.py V1 bytes must go cold"

    def test_cold_yet_readable(self, generations):
        g, t0, t1, digests = generations
        ops.archive(FUTURE)
        hot, cold = _store_files(Path.cwd())
        assert cold.exists(), "archive must create the cold sibling"
        a_v1, _ = digests[("a.py::alpha", "v1")]
        # The PyO3 round-trip through the cold fallback, not just Rust.
        assert ops.blob_get(a_v1) == A_V1
        # And through CodeRadar's own read path, at both generations.
        # Entity spans are node extents (trailing break belongs to no
        # entity); whole-file exactness is the module's proof below.
        assert g.as_of(t0).read_bytes("a.py::alpha") == A_V1[:-2]
        assert g.as_of(t1).read_bytes("a.py::alpha") == A_V2[:-2]
        assert g.as_of(t1).read_bytes("b.py::beta") == B[:-2]

    def test_module_reads_whole_file(self, generations):
        g, t0, t1, _digests = generations
        ops.archive(FUTURE)
        assert g.as_of(t0).read_bytes("a.py::module") == A_V1
        assert g.as_of(t1).read_bytes("a.py::module") == A_V2
        text = g.as_of(t0).read_source("a.py::module")
        assert text.startswith("1\tdef alpha():\n") and "return 1\n" in text

    def test_graph_archive_method(self, generations):
        g, _t0, _t1, _digests = generations
        assert g.archive(FUTURE)["blobs_archived"] >= 1

    def test_cutoff_from_toml(self, generations):
        (Path.cwd() / ".coderadar.toml").write_text(
            "[retention]\narchive_after_days = 0\n", encoding="utf-8")
        report = ops.archive()
        assert report["blobs_archived"] >= 1

    def test_no_cutoff_is_invalid_request(self, generations):
        with pytest.raises(ops.InvalidRequest, match="no archive cutoff"):
            ops.archive()

    def test_garbage_cutoff_is_invalid_request(self, generations):
        with pytest.raises(ops.InvalidRequest, match="invalid timestamp"):
            ops.archive("yesterday-ish")

    def test_negative_days_is_invalid_request(self, generations):
        (Path.cwd() / ".coderadar.toml").write_text(
            "[retention]\narchive_after_days = -3\n", encoding="utf-8")
        with pytest.raises(ops.InvalidRequest, match="negative"):
            ops.archive()


class TestColdLoss:
    def test_lost_cold_blob_names_digest(self, generations):
        # Production condition: ledger intact, cold bytes gone (the cold
        # file is trigger-free by macrame design; hot deletes stay guarded).
        g, t0, _t1, digests = generations
        ops.archive(FUTURE)
        _hot, cold = _store_files(Path.cwd())
        a_v1, _ = digests[("a.py::alpha", "v1")]
        c = sqlite3.connect(str(cold))
        try:
            c.execute("DELETE FROM blobs WHERE sha256 = ?", (a_v1,))
            c.commit()
        finally:
            c.close()
        # Ledger still answers (hot+cold history)…
        assert g.as_of(t0).find("a.py::alpha") is not None
        # …but the bytes are honestly unavailable, naming the digest.
        with pytest.raises(ops.ContentUnavailable, match=a_v1):
            g.as_of(t0).read_bytes("a.py::alpha")
        # The module (whole-file) path fails the same way, not silently.
        with pytest.raises(ops.ContentUnavailable, match=a_v1):
            g.as_of(t0).read_bytes("a.py::module")


class TestBackup:
    def test_hot_cold_pair_restores_every_digest(self, generations, tmp_path):
        g, t0, t1, digests = generations
        ops.archive(FUTURE)
        hot, cold = _store_files(Path.cwd())
        # Quiesced copy: the suite holds no writers (documented procedure).
        _checkpoint(hot)
        backup = tmp_path / "backup"
        backup.mkdir()
        shutil.copy2(hot, backup / "coderadar.db")
        shutil.copy2(cold, backup / "coderadar_archive.db")
        g2 = coderadar.load(str(backup / "coderadar.db"), str(tmp_path))
        for (fid, _gen), (digest, expect) in digests.items():
            assert ops.blob_get(digest) == expect, fid
        # History reads work off the pair too (hot+cold union).
        assert g2.as_of(t0).read_bytes("a.py::alpha") == A_V1[:-2]
        assert g2.as_of(t1).read_bytes("a.py::alpha") == A_V2[:-2]
        assert g2.as_of(t0).read_bytes("a.py::module") == A_V1

    def test_hot_only_is_pointers(self, generations, tmp_path):
        # The documented warning, pinned: without the cold file, archived
        # blob addresses resolve to None, current reads still work (hot),
        # and archived-T reconstruction errors naming the missing half —
        # never a silent present-tense answer.
        g, t0, t1, digests = generations
        ops.archive(FUTURE)
        hot, _cold = _store_files(Path.cwd())
        _checkpoint(hot)
        lonely = tmp_path / "lonely"
        lonely.mkdir()
        shutil.copy2(hot, lonely / "coderadar.db")
        g2 = coderadar.load(str(lonely / "coderadar.db"), str(tmp_path))
        a_v1, _ = digests[("a.py::alpha", "v1")]
        assert ops.blob_get(a_v1) is None
        assert g2.as_of(t1).find("a.py::alpha")["name"] == "alpha"
        with pytest.raises(ops.EngineError, match="archive database file"):
            g2.as_of(t0).find("a.py::alpha")

    def test_pre_019_cold_file_is_absent_not_error(self, generations, tmp_path):
        # A cold file from before 0.19 has no `blobs` table: the reader
        # must treat that as absent. Craft one (valid sqlite, no blobs).
        g, _t0, t1, digests = generations
        _hot, cold = _store_files(Path.cwd())
        assert not cold.exists()
        c = sqlite3.connect(str(cold))
        try:
            c.execute("CREATE TABLE dummy (x TEXT)")
            c.commit()
        finally:
            c.close()
        _b_v1, _ = digests[("b.py::beta", "v1")]
        assert ops.blob_get(_b_v1) == B, "hot bytes must still resolve"
        assert g.as_of(t1).read_bytes("b.py::beta") == B[:-2]
