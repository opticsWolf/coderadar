"""S6/S5 shims: embedding dedup helpers, the query planner and GraphRAG."""

from __future__ import annotations

import hashlib
import importlib

import pytest

from tests._rust_bridge import rust_bridge


def module(name: str):
    """Import a bridge submodule under the private alias."""
    rust_bridge()
    return importlib.import_module(f"coderadar_rs.{name}")


def test_content_hash_matches_the_python_fallback_rule() -> None:
    dedup = module("embedding.dedup")
    data = b"def greet():\n    return 1\n"
    try:
        import xxhash
    except ImportError:
        expected = hashlib.sha256(data).hexdigest()[:16]
    else:
        expected = xxhash.xxh3_64(data).hexdigest()
    assert dedup.compute_content_hash(data) == expected


def test_embed_text_key_and_model_round_trip() -> None:
    dedup = module("embedding.dedup")
    row = {"name": "greet", "signature": "def greet()", "docstring": "Say hi."}
    assert dedup.embed_body(row, "function") == "def greet()\nSay hi."
    assert dedup.embed_body({"name": "Widget", "signature": "class Widget"}, "class") == (
        "class Widget"
    )
    key = dedup.embed_cache_key("abc123", "BAAI/bge-small-en-v1.5")
    assert key == f"BAAI/bge-small-en-v1.5#pp{dedup.PREPROCESS_VERSION}#abc123"
    assert dedup.stored_key_model(key) == "BAAI/bge-small-en-v1.5"
    assert dedup.stored_key_model("abc123") is None


def test_planner_routes_every_intent_family() -> None:
    planner = module("query.planner")
    cases = {
        "who calls parse": ("impact_analysis", "callers_of"),
        "path from a to b": ("call_chain", "traverse"),
        "find similar functions": ("similarity_search", "search_similar"),
        "module dependencies": ("dependency_graph", "traverse"),
        "what is foo": ("definition_lookup", "find"),
        "list everything": ("scope_exploration", "list_by_kind"),
    }
    for text, (intent, method) in cases.items():
        plan = planner.plan_query(text)
        assert plan.intent.value == intent, text
        assert plan.method == method, text


def test_graphrag_context_compresses_and_respects_the_budget() -> None:
    graphrag = module("agent.graphrag")
    intent = module("query.planner").QueryIntent
    rows = [
        {
            "id": "a.py::f",
            "name": "f",
            "signature": "def f()",
            "body": "if x:\n  return 1\nelse:\n  pass",
        }
    ]
    built = graphrag.GraphRAGContextBuilder(max_tokens=8192).build_context(
        rows, intent.DEFINITION_LOOKUP
    )
    assert built.rows[0]["body_skeleton"] == "if x:\n  return 1"
    assert built.tokens_used > 0

    tiny = graphrag.GraphRAGContextBuilder(max_tokens=1).build_context(
        rows, intent.DEFINITION_LOOKUP
    )
    assert tiny.rows == []

    signatures = graphrag.GraphRAGContextBuilder(
        max_tokens=8192, strategy=graphrag.ContextStrategy.SIGNATURES_ONLY
    ).build_context(rows, intent.CALL_CHAIN)
    assert sorted(signatures.rows[0]) == ["docstring", "name", "signature"]


def test_embedding_dedup_requires_a_loaded_graph() -> None:
    package = module("embedding")
    dedup = package.EmbeddingDedup()
    target = package.EmbedTarget("a.py::f", "def f()", "hash")
    with pytest.raises(RuntimeError, match="No graph loaded"):
        dedup.embed_batch([target])
