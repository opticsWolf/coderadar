"""P1 (DR-36): `ops.verify_history` over a real stored project.

A two-generation store (a.py V1→V2 + b.py + a secret-named file with no
blob) exercises the sweep: mixed ok/no_digest counts, sampling-vs-all
agreement, unresolvable vs no_digest, doctored-bytes mismatch, the report
shape contract, and the request-validation gates. The committed pre-analyze
baseline fixture pins the no-coverage era as P1's regression input.
"""
from __future__ import annotations

import json
import sqlite3
import time
from pathlib import Path

import coderadar
import pytest
from coderadar import ops, render

try:
    from coderadar import _core
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

A_V1 = b'def alpha():\n    """First."""\n    return 1\n'
A_V2 = b'def alpha():\n    """First (revised)."""\n    return 1\n'
B = b'def beta():\n    """Second."""\n    return 2\n'
S = b'def hidden():\n    return 0\n'

CONTRACT_KEYS = {
    "store", "root", "branch", "window", "total_generations", "sampled",
    "offenders_per_outcome", "generations", "totals", "byte_coverage_pct",
    "distinct_digests", "offender_sample", "offender_distinct",
}

BASELINE = (
    Path(__file__).parent / "fixtures" / "temporal" / "verify_history_baseline.json"
)


def _recorded_max(db_path):
    c = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return c.execute("select max(recorded_at) from transaction_log").fetchone()[0]
    finally:
        c.close()


@pytest.fixture()
def history(tmp_path, monkeypatch):
    """a.py (V1→V2) + b.py (blobbed) + sk_secret.py (indexed, no blob)."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_bytes(A_V1)
    (tmp_path / "b.py").write_bytes(B)
    (tmp_path / "sk_secret.py").write_bytes(S)
    g = coderadar.analyze(".", create_store=True)
    hot = tmp_path / ".coderadar" / "store" / "coderadar.db"
    t0 = _recorded_max(hot)
    time.sleep(0.01)
    (tmp_path / "a.py").write_bytes(A_V2)
    ops.update_file("a.py", graph=g)
    t1 = _recorded_max(hot)
    assert t1 > t0
    return g, hot, t0, t1


class TestMixedStore:
    def test_counts_per_generation(self, history):
        g, _hot, _t0, _t1 = history
        report = ops.verify_history(all=True, graph=g)
        assert report["total_generations"] == 2
        assert report["sampled"] == 2
        for gen in report["generations"]:
            assert gen["modules"] == 3
            assert gen["counts"] == {"ok": 2, "no_digest": 1}
            assert set(gen) == {"timestamp", "modules", "counts", "offenders"}

    def test_totals_coverage_and_offenders(self, history):
        g, _hot, _t0, _t1 = history
        report = ops.verify_history(all=True, graph=g)
        assert report["totals"] == {"ok": 4, "no_digest": 2}
        assert report["byte_coverage_pct"] == round(4 / 6 * 100, 2)
        # a.py V1, a.py V2, b.py: the secret file contributes no digest.
        assert report["distinct_digests"] == 3
        assert report["offender_sample"] == {"no_digest": ["sk_secret.py::module"]}
        assert report["offender_distinct"] == {"no_digest": 1}

    def test_sampling_agrees_with_all_on_small_store(self, history):
        g, _hot, _t0, _t1 = history
        sampled = ops.verify_history(generations=25, graph=g)
        swept = ops.verify_history(all=True, graph=g)
        assert sampled["totals"] == swept["totals"]
        assert sampled["sampled"] == swept["sampled"] == 2

    def test_report_shape_is_the_contract(self, history):
        g, _hot, _t0, _t1 = history
        report = ops.verify_history(graph=g)
        assert set(report) == CONTRACT_KEYS
        assert report["branch"] == "main"
        assert report["window"][0] <= report["window"][1]

    def test_render_names_coverage_and_offenders(self, history):
        g, _hot, _t0, _t1 = history
        text = render.verify_history(ops.verify_history(graph=g))
        assert "byte coverage" in text
        assert "no_digest (1 distinct files):" in text
        assert "sk_secret.py::module" in text


class TestFailureModes:
    def _cold(self, history):
        g, hot, _t0, _t1 = history
        ops.archive("2999-01-01T00:00:00.000000Z")
        cold = hot.parent / "coderadar_archive.db"
        assert cold.exists()
        return g, cold

    def test_deleted_cold_blob_is_unresolvable_not_no_digest(self, history):
        g, cold = self._cold(history)
        _g, _hot, t0, _t1 = history
        ref = _core.entity_source_ref_at("a.py::module", t0)
        assert ref is not None and ref["digest"]
        # Cold is trigger-free by macrame design: bytes can go missing
        # there while the ledger stays intact (cf. test_retention).
        c = sqlite3.connect(str(cold))
        try:
            c.execute("DELETE FROM blobs WHERE sha256 = ?", (ref["digest"],))
            c.commit()
        finally:
            c.close()
        report = ops.verify_history(all=True, graph=g)
        assert report["totals"]["unresolvable"] == 1
        # Only the V1 generation referenced the deleted bytes; the secret
        # file still reports no_digest, never unresolvable.
        assert report["totals"]["no_digest"] == 2
        assert "a.py::module" in report["offender_sample"]["unresolvable"]

    def test_doctored_cold_bytes_are_mismatch(self, history):
        g, cold = self._cold(history)
        _g, _hot, t0, _t1 = history
        # Only superseded blobs go cold (still-referenced ones stay hot),
        # so the doctored address is V1's, read at the t0 generation.
        ref = _core.entity_source_ref_at("a.py::module", t0)
        assert ref is not None and ref["digest"]
        c = sqlite3.connect(str(cold))
        try:
            c.execute(
                "UPDATE blobs SET bytes = ? WHERE sha256 = ?",
                (b"\x00corrupt", ref["digest"]),
            )
            c.commit()
        finally:
            c.close()
        report = ops.verify_history(all=True, graph=g)
        assert report["totals"]["digest_mismatch"] == 1
        assert report["totals"]["ok"] == 3


class TestSecretOnlyStore:
    def test_every_file_no_digest(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        (tmp_path / "sk_secret.py").write_bytes(S)
        g = coderadar.analyze(".", create_store=True)
        report = ops.verify_history(all=True, graph=g)
        assert report["totals"] == {"no_digest": 1}
        assert report["byte_coverage_pct"] == 0.0
        assert report["distinct_digests"] == 0


class TestRequestValidation:
    def test_empty_window_is_invalid_request(self, history):
        g, _hot, _t0, _t1 = history
        with pytest.raises(ops.InvalidRequest, match="no recorded generations"):
            ops.verify_history(from_ts="2000-01-01T00:00:00Z",
                               to_ts="2000-01-02T00:00:00Z", graph=g)

    def test_inverted_window_is_invalid_request(self, history):
        g, _hot, _t0, _t1 = history
        with pytest.raises(ops.InvalidRequest, match="empty window"):
            ops.verify_history(from_ts="2026-10-02T00:00:00Z",
                               to_ts="2026-10-01T00:00:00Z", graph=g)

    def test_garbage_stamp_is_invalid_request(self, history):
        g, _hot, _t0, _t1 = history
        with pytest.raises(ops.InvalidRequest):
            ops.verify_history(from_ts="not-a-timestamp", graph=g)

    def test_bad_counts_are_invalid_request(self, history):
        g, _hot, _t0, _t1 = history
        with pytest.raises(ops.InvalidRequest, match="generations"):
            ops.verify_history(generations=0, graph=g)
        with pytest.raises(ops.InvalidRequest, match="offenders"):
            ops.verify_history(offenders=-1, graph=g)

    def test_scan_cap_binds_all(self, history):
        g, _hot, _t0, _t1 = history
        with pytest.raises(ops.InvalidRequest, match="max_generations"):
            ops.verify_history(all=True, max_generations=1, graph=g)


class TestBaselineFixture:
    def test_pre_analyze_era_is_pinned(self):
        baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
        assert baseline["total_generations"] > 6000
        assert baseline["sampled"] == 25
        # The 0%-coverage evidence: one surviving ok against thousands
        # of byte-less file-generations.
        assert baseline["totals"]["ok"] == 1
        assert baseline["totals"]["no_digest"] > 8000
        assert baseline["byte_coverage_pct"] < 0.1


class TestPrototypeAgreement:
    def test_op_matches_independent_replay(self, history, tmp_path, capsys):
        # Two implementations, one allowed to be wrong, is a test: the
        # shipped op must agree with the prototype's log replay.
        import importlib.util

        _g, hot, _t0, _t1 = history
        spec = importlib.util.spec_from_file_location(
            "verify_history_proto",
            Path(__file__).parent.parent / "scripts" / "verify_history.py",
        )
        proto = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(proto)
        assert proto.main(["--store", str(hot), "--root", ".",
                           "--all", "--json"]) == 0
        replayed = json.loads(capsys.readouterr().out)
        op = ops.verify_history(all=True)
        assert replayed["totals"] == op["totals"]
        assert replayed["byte_coverage_pct"] == op["byte_coverage_pct"]
        assert replayed["distinct_digests"] == op["distinct_digests"]
