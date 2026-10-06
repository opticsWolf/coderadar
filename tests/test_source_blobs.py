"""§1.9 put-on-index (DR-25/DR-30): file bytes land in Macrame blobs.

Covers the plan's done-clause: bytes written on analyze/update, digest at
`extra.coderadar.source_blob` on the file's module concept, counts on every
report surface, kill-switch off, secret defaults excluded, reindex/cheap
paths accumulate, zero-growth on unchanged reindex. Byte reads at-T wait on
§0.1(b)/§3.0 — here digests are proven content-addressed by recomputation
(hashlib), not by reading blob bytes back through a surface that lands
later.
"""

import hashlib
import json
import sqlite3

import pytest
from coderadar import analyze, blob_stats, ops

V1 = "def f():\n    return 1\n"


def _module_extra(db_path, mod_id):
    c = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = c.execute(
            "SELECT extra FROM concepts WHERE id = ?", (mod_id,)
        ).fetchone()
    finally:
        c.close()
    assert row is not None, f"module concept {mod_id} must exist"
    return json.loads(row[0])["coderadar"]


def _digest_ok(digest, payload):
    assert isinstance(digest, str) and len(digest) == 64
    assert digest == digest.lower() and all(
        ch in "0123456789abcdef" for ch in digest
    )
    assert digest == hashlib.sha256(payload).hexdigest()


@pytest.fixture()
def project(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_text(V1)
    g = analyze(".", create_store=True)
    db = tmp_path / ".coderadar" / "store" / "coderadar.db"
    return g, db


def test_analyze_stores_blob_and_asserts_digest(project, tmp_path):
    g, db = project
    stats = blob_stats()
    assert stats["stored"] == 1
    assert stats["skipped_oversize"] == 0
    assert stats["skipped_excluded"] == 0
    assert stats["bytes"] == len((tmp_path / "a.py").read_bytes())
    extra = _module_extra(db, "a.py::module")
    assert extra["format"] == 1  # additive: format stays 1
    assert extra["content_hash"]  # xxh3 staleness signal untouched
    # Content-addressed means the DISK bytes (CRLF on Windows), not V1.
    _digest_ok(extra["source_blob"], (tmp_path / "a.py").read_bytes())


def test_reindex_reports_blob_counts(project, tmp_path):
    # Cheap path with no changes: the key rides along, all zeros (the
    # counter is activity-since-load, not inventory).
    out = ops.reindex()
    assert out["blobs"] == {
        "stored": 0,
        "skipped_oversize": 0,
        "skipped_excluded": 0,
        "bytes": 0,
    }
    # Touch a file: the cheap path updates it, the put accumulates.
    # (os.utime past the 2 s staleness grace — the heuristic absorbs
    # analyze-vs-commit skew, so a just-written file reads as fresh.)
    import os
    import time

    (tmp_path / "a.py").write_text("def f():\n    return 9\n")
    db = tmp_path / ".coderadar" / "store" / "coderadar.db"
    fresh = time.time()
    os.utime(tmp_path / "a.py", (fresh + 5, fresh + 5))
    out = ops.reindex()
    assert out["blobs"]["stored"] == 1


def test_unchanged_reanalyze_adds_no_blob_bytes(project, tmp_path):
    _, db = project
    before = db.stat().st_size
    analyze(".", create_store=True)
    after = db.stat().st_size
    assert after == before, "re-put refreshes put_at, writes no new bytes"
    # The digest is stable across runs (same bytes, same address).
    assert _module_extra(db, "a.py::module")["source_blob"] == hashlib.sha256(
        (tmp_path / "a.py").read_bytes()
    ).hexdigest()


def test_update_file_reports_blob_outcome(project):
    g, db = project
    v2 = "def f():\n    return 2\n"
    import pathlib

    pathlib.Path("a.py").write_text(v2)
    rep = g.update_file("a.py")
    assert rep.blobs_stored == 1
    disk = pathlib.Path("a.py").read_bytes()
    assert rep.blobs_bytes == len(disk)
    extra = _module_extra(db, "a.py::module")
    _digest_ok(extra["source_blob"], disk)


def test_secret_default_gets_graph_but_no_blob(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "mysecret.py").write_text(V1)
    g = analyze(".", create_store=True)
    stats = blob_stats()
    assert stats["skipped_excluded"] == 1
    assert stats["stored"] == 0
    # Graph coverage is untouched — only the blob is withheld (the module
    # concept asserts with a digest-less extra).
    extra = _module_extra(
        tmp_path / ".coderadar" / "store" / "coderadar.db",
        "mysecret.py::module",
    )
    assert "source_blob" not in extra


def test_kill_switch_disables_blobs(tmp_path, monkeypatch):
    from coderadar._core import set_config

    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_text(V1)
    set_config({"database": {"store_source_blobs": False}})
    try:
        analyze(".", create_store=True)
        stats = blob_stats()
        assert stats == {
            "stored": 0,
            "skipped_oversize": 0,
            "skipped_excluded": 0,
            "bytes": 0,
        }
        extra = _module_extra(
            tmp_path / ".coderadar" / "store" / "coderadar.db",
            "a.py::module",
        )
        assert "source_blob" not in extra
    finally:
        set_config({"database": {"store_source_blobs": True}})


def test_watcher_batch_folds_blob_counts():
    from coderadar import UpdateReport, Watcher

    made = []

    class _StubGraph:
        def update_file(self, file_path):
            made.append(file_path)
            return UpdateReport(
                affected_files=[file_path],
                changed_symbols=[],
                new_unresolved_references=[],
                newly_resolved_references=[],
                elapsed_ms=1.0,
                parse_quality="Clean",
                parse_errors=0,
                fully_applied=True,
                epoch_before=1,
                epoch_after=2,
                blobs_stored=1,
                blobs_bytes=10,
            )

        def remove_file(self, file_path):
            return 0

    w = Watcher.__new__(Watcher)
    w._graph = _StubGraph()
    rep = w._apply([("a.py", "Modify"), ("b.py", "Modify")], echo=False)
    assert made == ["a.py", "b.py"]
    assert rep.blobs_stored == 2
    assert rep.blobs_bytes == 20
