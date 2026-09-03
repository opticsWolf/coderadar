"""v0.8 P2-1: multi-token search (guide: docs/v0.8-p2-agent-ux-guide.md).

Field evidence (Süvea session, report 1): ``codegraph_search("SyncNode
exchange_with")`` and ``codegraph_search("Store export_snapshot
import_snapshot")`` returned zero results while the index was fully ready —
the Rust scorer matched the *whole* lowercased query string against entity
names, so no ≥2-token query could ever hit. Tokens are now the unit of
matching: an entity's score is the sum of per-token contributions (name tiers
100/50/25, signature 10, docstring 5), so each token finds its own entities
and entities matching several tokens rank first.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

try:
    from coderadar._core import analyze as _analyze
    from coderadar._core import search_entities
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


SOURCE = '''\
"""Fixture library for multi-token search."""


def export_snapshot(path):
    """Serialize the graph to a file."""
    return path


def import_snapshot(path):
    """Read a graph back from a file."""
    return path


def touch_ledger(store, mode="rw"):
    return mode


def apply_patch(diff):
    return diff


def rebuild_everything():
    """Writes the ledger to disk."""
    return None
'''


@pytest.fixture
def project(tmp_path):
    (tmp_path / "fixture_lib.py").write_text(SOURCE, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


def _names(rows):
    return [r.get("name") for r in rows]


def test_two_tokens_find_two_different_entities(project):
    # The report's exact shape: two tokens that never appear in one name.
    rows = search_entities("export_snapshot import_snapshot", 20)
    names = _names(rows)
    assert "export_snapshot" in names, names
    assert "import_snapshot" in names, names


def test_multi_token_entity_outranks_single_token_one(project):
    # "touch_ledger" matches token "ledger" in its name (25) *and* "store" in
    # its signature (+10); "rebuild_everything" matches "ledger" only in its
    # docstring (5). The double match must sort first.
    rows = search_entities("ledger store", 20)
    names = _names(rows)
    assert names.index("touch_ledger") < names.index("rebuild_everything"), names


def test_token_matched_only_in_signature_is_returned(project):
    # "diff" occurs nowhere in any entity name — only in apply_patch's
    # parameter list. The old matcher returned nothing here.
    rows = search_entities("diff", 20)
    assert "apply_patch" in _names(rows), _names(rows)


def test_token_matched_only_in_docstring_is_returned(project):
    # "disk" appears only in rebuild_everything's docstring.
    rows = search_entities("disk", 20)
    assert "rebuild_everything" in _names(rows), _names(rows)


def test_single_token_exact_match_stays_first(project):
    # Regression guard: single-token behaviour is the old matcher's — the
    # exact-name hit is the top result.
    rows = search_entities("export_snapshot", 20)
    assert rows, "no results for an exact name"
    assert rows[0].get("name") == "export_snapshot", _names(rows)


def test_multi_token_query_ranks_multi_token_hits_at_the_top(project):
    # "snapshot" is contained in both snapshot helpers (25 each); "store"
    # only in touch_ledger's signature (10). The two containment hits tie at
    # the top; assert the tie set, not an order between equal scores.
    rows = search_entities("snapshot store", 20)
    assert set(_names(rows[:2])) == {"export_snapshot", "import_snapshot"}, _names(rows)
    assert "touch_ledger" in _names(rows[2:]), _names(rows)


def test_kind_filter_composes_with_tokens(project):
    rows = search_entities("export_snapshot import_snapshot", 20, "function")
    names = _names(rows)
    assert {"export_snapshot", "import_snapshot"} <= set(names), names
    for r in rows:
        assert r.get("kind") == "function"
    # A kind that matches neither token-carrying entity comes back empty.
    rows = search_entities("export_snapshot import_snapshot", 20, "module")
    assert "export_snapshot" not in _names(rows)


def test_empty_query_enumerates_the_kind(project):
    # Enumeration contract (pre-existing, relied on by the visualizers'
    # `_entities_of_kind`): empty query + kind filter = every entity of
    # that kind. Whitespace-only queries are empty too.
    rows = search_entities("", 50, "function")
    names = set(_names(rows))
    assert {"export_snapshot", "import_snapshot", "touch_ledger",
            "apply_patch", "rebuild_everything"} <= names, names
    rows = search_entities("", 50, "class")
    assert rows == [], _names(rows)  # the fixture has no classes
    assert search_entities("   ", 50, "function")  # still enumerates


def test_short_tokens_do_not_light_up_docstrings(project):
    # One/two-character tokens skip the signature/docstring tiers: "rw"
    # (a default value / docstring fragment) must not match by containment
    # alone — only name tiers count for it.
    rows = search_entities("rw", 50)
    for r in rows:
        assert "rw" in r.get("name", "").lower(), (r.get("name"), r.get("kind"))
