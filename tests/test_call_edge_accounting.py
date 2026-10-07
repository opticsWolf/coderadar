"""Call-edge accounting E2E (DR-32 counting truth).

`graph_stats.call_edges` counts every projection index pair, but the
ledger only holds the asserted share (resolved CALLS rows). The
projection follows three populations the ledger never asserted —
route→handler follows (§1.3), `external::` halves (R2-1), and symbolic
unresolved targets — and the stats now break them out so the invariant
closes from the stats alone:

    call_edges == asserted + external + unresolved + route_edges

with `asserted_call_edges` equal to live ledger CALLS rows.
"""

from __future__ import annotations

import sqlite3

import coderadar
import pytest
from coderadar import _core

try:
    from coderadar import _core as _c  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

A = b"""import os
from b import helper

def run():
    helper()
    os.getcwd()
    missing_fn()
    return 1
"""
B = b"""def helper():
    return 2
"""
APP = b"""from flask import Flask
app = Flask(__name__)

@app.route("/x")
def x():
    return 1
"""


@pytest.fixture()
def mixed(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_bytes(A)
    (tmp_path / "b.py").write_bytes(B)
    (tmp_path / "app.py").write_bytes(APP)
    g = coderadar.analyze(".", create_store=True)
    return g, tmp_path


def _ledger_calls(db_path) -> set:
    c = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return set(c.execute(
            "SELECT source_id, target_id FROM links_current "
            "WHERE edge_type = 'CALLS'").fetchall())
    finally:
        c.close()


class TestAccounting:
    def test_invariant_closes(self, mixed):
        _, _tmp = mixed
        s = _core.graph_stats()
        assert s["call_edges"] == (
            s["asserted_call_edges"] + s["external_call_edges"]
            + s["unresolved_call_edges"] + s["route_edges"]
        )

    def test_asserted_equals_ledger(self, mixed):
        _, tmp = mixed
        s = _core.graph_stats()
        db = tmp / ".coderadar" / "store" / "coderadar.db"
        assert s["asserted_call_edges"] == len(_ledger_calls(db)) == 1

    def test_buckets(self, mixed):
        _, _tmp = mixed
        s = _core.graph_stats()
        # run→helper asserted; run→external::missing_fn (os.getcwd drops
        # entirely — an external import call with no followable target);
        # the flask route→handler follow.
        assert (s["asserted_call_edges"], s["external_call_edges"],
                s["route_edges"]) == (1, 1, 1)
        # Every invented target currently prefixes external::, so the
        # backstop bucket is empty — and the key exists for when a path
        # fills it.
        assert s["unresolved_call_edges"] == 0
