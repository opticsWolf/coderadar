"""Embeddings Phase 1 E2E (DR-11, §3.1).

Model id + preprocessing live IN the dedup key (a switch retires vectors
by missing, never by silent reuse); one dimension per projection enforced
at write and query time with explicit errors; embed-what is
signature+docstring; enumeration is exact-count (no silent 10k cap).
"""

from __future__ import annotations

import pytest

import coderadar
from coderadar import _core, ops
from coderadar.embedding import (
    PREPROCESS_VERSION,
    embed_body,
    embed_cache_key,
    stored_key_model,
)

try:
    from coderadar import _core as _c  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = [
    pytest.mark.skipif(not _CORE, reason="Rust _core extension not built"),
    pytest.mark.skipif(
        __import__("importlib").util.find_spec("fastembed") is None,
        reason="fastembed not installed"),
]


@pytest.fixture()
def tiny(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_bytes(
        b'def alpha():\n    """Compute the alpha."""\n    return 1\n\n'
        b'def beta():\n    return 2\n')
    coderadar.analyze(".", create_store=True)
    return tmp_path


class TestEmbedWhat:
    def test_signature_plus_docstring(self):
        entity = {"signature": "def alpha():", "name": "alpha",
                  "docstring": "Compute the alpha."}
        assert embed_body(entity, "function") == "def alpha():\nCompute the alpha."

    def test_fallbacks(self):
        assert embed_body({"signature": "", "name": "x"}, "function") == "x"
        assert embed_body({"signature": "import os", "name": ""}, "import") == "import os"
        # Non-doc kinds ignore docstrings even when present.
        assert embed_body({"signature": "C = 1", "name": "C",
                           "docstring": "ignored"}, "constant") == "C = 1"

    def test_key_format_pinned(self):
        assert embed_cache_key("ab12", "M") == f"M#pp{PREPROCESS_VERSION}#ab12"
        assert stored_key_model("M#pp1#ab12") == "M"
        assert stored_key_model("barehash") is None


class TestKeyedDedup:
    def test_generate_then_cached(self, tiny):
        first = ops.compute_embeddings()
        assert first["generated"] == first["total"] > 0
        assert first["errors"] == 0
        second = ops.compute_embeddings()
        assert second == {"generated": 0, "cached": first["total"],
                          "total": first["total"], "errors": 0}

    def test_legacy_bare_hash_regenerates_once(self, tiny):
        ops.compute_embeddings()
        key_before = _core.lookup_entity("a.py::alpha")["embedding_hash"]
        assert "#" in key_before
        # Stage a legacy vector: right width (passes the dim gate), bare key.
        _core.clear_all_embeddings()
        _core.set_embeddings_bulk(
            [("a.py::alpha", [0.1] * 384, "barelegacyhash")])
        regen = ops.compute_embeddings()
        assert regen["generated"] >= 1
        key_after = _core.lookup_entity("a.py::alpha")["embedding_hash"]
        assert "#" in key_after and key_after != "barelegacyhash"
        rerun = ops.compute_embeddings()
        assert rerun["generated"] == 0

    def test_docstring_change_rotates_key(self, tiny):
        # Vectors are projection-side (ledger persistence is 0.13), so the
        # precise docstring assertion is key rotation, not regen counts:
        # alpha's key must change (docstring is IN the embed text) while
        # beta's key stays stable across an incremental file update.
        ops.compute_embeddings()
        alpha_before = _core.lookup_entity("a.py::alpha")["embedding_hash"]
        beta_before = _core.lookup_entity("a.py::beta")["embedding_hash"]
        (tiny / "a.py").write_bytes(
            b'def alpha():\n    """Compute the alpha, revised.\"\"\"\n    return 1\n\n'
            b'def beta():\n    return 2\n')
        coderadar.CodeGraph().update_file("a.py")
        report = ops.compute_embeddings()
        # The file's vectors cleared on update; both regenerate, the
        # untouched module stays cached.
        assert report["generated"] == 2
        assert report["cached"] == report["total"] - 2
        alpha_after = _core.lookup_entity("a.py::alpha")["embedding_hash"]
        beta_after = _core.lookup_entity("a.py::beta")["embedding_hash"]
        assert alpha_after != alpha_before
        assert beta_after == beta_before


class TestDimensionGates:
    def test_write_gate_rejects_mixed_width(self, tiny):
        ops.compute_embeddings()
        with pytest.raises(RuntimeError, match="embedding dimension mismatch"):
            _core.set_embeddings_bulk([("a.py::alpha", [0.1] * 8, "k")])

    def test_write_gate_error_names_remedy(self, tiny):
        ops.compute_embeddings()
        with pytest.raises(RuntimeError, match="recompute=True"):
            _core.set_embeddings_bulk([("a.py::alpha", [0.1] * 8, "k")])

    def test_query_gate_is_explicit(self, tiny, monkeypatch):
        _core.set_embeddings_bulk([("a.py::alpha", [0.1] * 8, "X#pp1#y")])
        calls = []
        real_compute = ops.compute_embeddings
        monkeypatch.setattr(
            ops, "compute_embeddings",
            lambda *a, **k: (calls.append(1), real_compute(*a, **k))[1])
        with pytest.raises(ops.EngineError, match="embedding dimension"):
            ops.search_similar("hello world")
        # A width conflict cannot be computed away — no futile recompute.
        assert calls == []

    def test_configured_dimension_checked(self, tiny, monkeypatch):
        import coderadar.embedding as emb_mod
        real_settings = emb_mod.embedding_settings
        monkeypatch.setattr(emb_mod, "embedding_settings",
                            lambda: (real_settings()[0], 999))
        with pytest.raises(ops.EngineError, match="999"):
            ops.compute_embeddings()


class TestModelSwitch:
    def test_switch_refuses_without_flag(self, tiny):
        ops.compute_embeddings()
        with pytest.raises(ops.EngineError, match="recompute=True"):
            ops.compute_embeddings(model_name="FAKE-MODEL")

    def test_switch_error_names_both_models(self, tiny):
        ops.compute_embeddings()
        with pytest.raises(ops.EngineError, match="FAKE-MODEL"):
            ops.compute_embeddings(model_name="FAKE-MODEL")

    def test_recompute_clears_and_regenerates(self, tiny):
        first = ops.compute_embeddings()
        report = ops.compute_embeddings(recompute=True)
        assert report["generated"] == first["total"]
        assert report["cached"] == 0


class TestNoSilentCap:
    def test_eleven_thousand_functions_all_collected(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        lines = "".join(f"def f{i}():\n    return {i}\n" for i in range(10005))
        (tmp_path / "big.py").write_bytes(lines.encode())
        coderadar.analyze(".", create_store=True)
        stats = _core.graph_stats()
        assert stats["functions"] == 10005
        targets = ops._collect_embed_targets("M")
        assert (sum(1 for t in targets if t.kind == "function")
                == stats["functions"])


class TestSimilarityStillWorks:
    def test_docstring_drives_ranking(self, tiny):
        ops.compute_embeddings()
        rows = ops.search_similar("alpha computation")
        assert rows and rows[0]["id"] == "a.py::alpha"
