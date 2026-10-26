"""Direct unit tests for `coderadar.render` text formatters — §3.2.

These are pure functions (dicts in, markdown out): no `_core`, no I/O,
milliseconds each. Before this file the formatters were covered only
indirectly through CLI/MCP text assertions, so a formatter silently
dropping a section ( Plausible output, exit 0 ) had no precise pin.
Each test feeds a realistic row set and asserts the content lands;
each empty case pins the documented empty contract (headers + an
explicit clean/empty reading, never a crash or a bare header).
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

from coderadar import render


def test_search_names_rows_with_ids_files_lines():
    out = render.search("Store", None, [
        {"name": "Store", "kind": "class", "id": "app.py::Store",
         "file_path": "app.py", "start_line": 3, "signature": "class Store:"},
    ])
    assert "## Search: `Store`" in out
    assert "app.py::Store" in out and "app.py" in out
    assert "Line:" in out and "Signature:" in out


def test_search_empty_delegates_to_miss():
    out = render.search("zzz", "function", [])
    assert "zzz" in out and "No" in out


def test_callers_lists_rows_and_counts():
    out = render.callers("app.py::helper", [
        {"id": "app.py::Store.load", "file": "app.py", "line": 9},
        {"id": "external::os.path", "file": "", "line": 0},
    ])
    assert "## Callers of `app.py::helper` (2)" in out
    assert "app.py::Store.load" in out
    assert "external" in out


def test_callees_empty_is_an_empty_list_not_an_error():
    out = render.callees("app.py::leaf", [])
    assert "## Callees from `app.py::leaf` (0)" in out


def test_node_shows_metadata_and_neighbor_sections():
    out = render.node({
        "name": "Store.load", "id": "app.py::Store.load", "kind": "method",
        "file_path": "app.py", "start_line": 5, "end_line": 9,
        "signature": "def load(self)", "decorators": ["@traced"],
        "callers": [{"name": "entry", "kind": "function", "id": "app.py::entry"}],
        "callees": [],
    })
    assert "## Store.load" in out and "5–9" in out
    assert "@traced" in out and "## Callers (1)" in out


def test_affected_groups_by_depth_with_total():
    out = render.affected("app.py::helper", {
        "entity": {"name": "helper"}, "entity_id": "app.py::helper",
        "max_depth": 5,
        "depths": {1: [{"name": "Store.load", "kind": "method", "id": "app.py::Store.load"}]},
    })
    assert "**Total dependents:** 1" in out and "Depth 1" in out
    assert "Store.load" in out


def test_affected_empty_says_no_dependents():
    out = render.affected("x", {"entity": {"name": "x"}, "entity_id": "x",
                                "max_depth": 5, "depths": {}})
    assert "No dependents found" in out


def test_module_children_counts_categories():
    out = render.module_children({
        "module_id": "app.py::module",
        "classes": [{"name": "Store", "id": "app.py::Store", "line": 3}],
        "functions": [{"name": "helper", "id": "app.py::helper", "line": 12}],
        "imports": [], "constants": [],
    })
    assert "## Module: `app.py::module`" in out and "2 children" in out
    assert "### Classes (1)" in out and "### Functions (1)" in out


def test_resolve_route_names_handler_and_confidence():
    out = render.resolve({"name": "/users/:id", "mode": "route", "results": [
        {"name": "get_user", "kind": "function", "id": "app.py::get_user",
         "file_path": "app.py", "confidence": 0.95,
         "route": {"name": "/users/:id"}},
    ]})
    assert "Route Resolution" in out and "get_user" in out
    assert "0.95" in out and "/users/:id" in out


def test_resolve_reference_empty_suggests_search():
    out = render.resolve({"name": "Nope", "mode": "reference", "results": []})
    assert "Nope" in out and "coderadar_search" in out


def test_dead_code_empty_is_clean_not_failure():
    out = render.dead_code([], 0.6)
    assert "clean result" in out and "indexing failure" in out


def test_dead_code_names_evidence_and_distance():
    out = render.dead_code([{
        "entity_name": "old_fn", "entity_id": "a.py::old_fn", "kind": "function",
        "tier": "T1", "score": 0.9, "removable_lines": 4, "file": "a.py",
        "line": 10, "evidence": ["no callers"], "nearest_root_distance": 3,
    }], 0.6)
    assert "old_fn" in out and "no callers" in out and "3 hop(s)" in out


def test_get_smells_empty_with_scope():
    out = render.get_smells([], "a.py::C", "long-method")
    assert "No findings" in out and "long-method" in out


def test_get_smells_shows_rule_severity_signals():
    out = render.get_smells([{
        "entity_name": "C", "rule_id": "god-class", "severity": "high",
        "message": "too big", "signals": {"wmc": 60},
    }], None, None)
    assert "god-class" in out and "high" in out and "wmc=60" in out


def test_find_clones_empty_is_clean():
    out = render.find_clones([], 10, 0.8)
    assert "Clean result" in out


def test_find_clones_names_instances_with_lines():
    out = render.find_clones([{
        "clone_type": "Type-1", "similarity": 1.0, "confidence_tier": "high",
        "instances": [
            {"entity_id": "d.py::dup_a", "file": "d.py",
             "start_line": 1, "end_line": 6, "span_start": 0, "span_end": 90},
            {"entity_id": "d.py::dup_b", "file": "d.py",
             "start_line": 9, "end_line": 14, "span_start": 91, "span_end": 180},
        ],
    }], 3, 0.8)
    assert "d.py::dup_a" in out and "d.py::dup_b" in out
    assert "lines 1-6" in out and "Type-1" in out


def test_find_scaffolding_empty_is_clean():
    out = render.find_scaffolding([], False)
    assert "Clean result" in out


def test_find_scaffolding_groups_by_kind_with_stats_footer():
    out = render.find_scaffolding([
        {"kind": "comment-marker", "file": "e.py", "line": 1,
         "label": "TODO", "snippet": "TODO: wire this up"},
        {"kind": "scan-stats", "file": "", "line": 0,
         "label": "3 files scanned", "snippet": ""},
    ], False)
    assert "Comment Marker (1)" in out and "TODO" in out
    assert "3 files scanned" in out


def test_explore_groups_files_and_relationships():
    out = render.explore({
        "files": [{"file_path": "app.py", "entities": [
            {"name": "Store", "kind": "class", "source": "class Store:"}]}],
        "relationships": [{"kind": "callee", "entity_name": "entry",
                           "other_name": "Store.load"}],
    })
    assert "**app.py**" in out and "class Store:" in out
    assert "Store.load" in out and "[callee]" in out


def test_traverse_names_depths_and_warns_unresolved():
    out = render.traverse({
        "entity": {"name": "helper"}, "entity_id": "app.py::helper",
        "direction": "upstream", "max_depth": 3, "edge_kinds": ["calls"],
        "results": [{"name": "Store.load", "kind": "function",
                     "id": "app.py::Store.load", "depth": 1}],
        "unresolved": 2,
    })
    assert "Traverse from `helper`" in out and "Depth 1" in out
    assert "Traversal incomplete" in out and "2 outgoing" in out


def test_traverse_empty_names_direction_and_depth():
    out = render.traverse({
        "entity": {"name": "leaf"}, "entity_id": "x", "direction": "both",
        "max_depth": 3, "edge_kinds": None, "results": [],
    })
    assert "No neighbors found" in out and "direction=both" in out


def test_query_numbers_rows_and_truncates():
    rows = [{"name": f"f{i}", "kind": "function", "id": f"a.py::f{i}",
             "file_path": "a.py", "start_line": i} for i in range(1, 4)]
    out = render.query("functions where name contains 'f'", rows, limit=2)
    assert "Found 3 result(s)" in out and "... and 1 more" in out


def test_query_empty():
    assert "no results" in render.query("zzz", []).lower()


def test_search_similar_shows_similarity():
    out = render.search_similar("compute", [
        {"name": "alpha", "kind": "function", "id": "a.py::alpha",
         "file_path": "a.py", "similarity": 0.912},
    ])
    assert "alpha" in out and "0.912" in out
    assert "No semantically similar" not in out


def test_search_similar_empty():
    assert "No semantically similar" in render.search_similar("zzz", [])


def test_search_hybrid_shows_per_arm_ranks():
    out = render.search_hybrid("compute", [
        {"name": "alpha", "kind": "function", "id": "a.py::alpha",
         "file_path": "a.py", "score": 0.0833,
         "vector_rank": 1, "keyword_rank": 2},
    ])
    assert "alpha" in out and "vector #1" in out and "keyword #2" in out
    assert "RRF 0.0833" in out


def test_search_hybrid_empty():
    assert "No hybrid results" in render.search_hybrid("zzz", [])
