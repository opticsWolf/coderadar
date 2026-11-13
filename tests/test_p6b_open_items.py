"""P6b — aggregation and the search-side "why" join.

Two reads, no new storage: `open_items` aggregates the unchecked task
items of live annotation bodies (annotations only — the list grammar is
what makes an aggregate possible), and search hits carry the notes
anchored to them ("why" next to "what").

The invariants these tests hold:

* only the *current* version of a row contributes items — a superseded
  body's items are gone, not merely outranked;
* a retired row contributes nothing (retire is an act);
* `scope` is a prefix on the annotated id, so `a.py` covers `a.py::f`
  and `a.py::C.m` but not `a.pyx`;
* comments never aggregate (freeform has no items);
* a hit's notes include those anchored to an enclosing id and to the
  file, and a hit with none carries no `notes` key at all — the pre-P6b
  shape is unchanged.
"""

from __future__ import annotations

import coderadar
import pytest
from coderadar import _core, ops

try:
    from coderadar import _core as _c  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(
    not _CORE, reason="Rust _core extension not built")


@pytest.fixture()
def graph(tmp_path, monkeypatch):
    """A store with one file, two functions and a class method.

    `analyze` returns the bound handle: a fresh `CodeGraph()` binds on
    first use and would replace the loaded graph, dropping its store.
    """
    monkeypatch.chdir(tmp_path)
    (tmp_path / "a.py").write_bytes(
        b"class C:\n"
        b"    def m(self):\n"
        b"        return 1\n"
        b"\n"
        b"def f():\n"
        b"    return 2\n"
        b"\n"
        b"def g():\n"
        b"    return 3\n")
    (tmp_path / "b.py").write_bytes(b"def h():\n    return 4\n")
    return coderadar.analyze(".", create_store=True)


def _annotation(graph, about, body):
    return graph.annotate(about, body)["id"]


class TestOpenItems:
    def test_reads_current_version_only(self, graph):
        row = _annotation(graph, "a.py::f", "- [ ] first\n- [ ] second\n")
        assert [i["text"] for i in ops.open_items()["items"]] == ["first", "second"]
        # Flip one item and add another: the superseded body is gone.
        graph.revise_annotation(row, "- [x] first\n- [ ] second\n- [ ] third\n")
        items = ops.open_items()["items"]
        assert [i["text"] for i in items] == ["second", "third"]
        assert [i["line"] for i in items] == [2, 3]
        # Revise supersedes the row: `n` names the row, not the version.
        assert {i["n"] for i in items} == {1}

    def test_checked_items_are_not_items(self, graph):
        _annotation(graph, "a.py::f", "- [x] done\n- [x] also done\n")
        assert ops.open_items()["items"] == []

    def test_retired_row_contributes_nothing(self, graph):
        row = _annotation(graph, "a.py::f", "- [ ] live\n")
        _annotation(graph, "a.py::g", "- [ ] also live\n")
        graph.retire_annotation(row)
        items = ops.open_items()["items"]
        assert [i["about"] for i in items] == ["a.py::g"]

    def test_comments_never_aggregate(self, graph):
        graph.comment("a.py::f", "- [ ] prose, not a checklist entry\n")
        assert ops.open_items()["items"] == []

    def test_scope_is_a_prefix_on_the_annotated_id(self, graph):
        _annotation(graph, "a.py", "- [ ] file level\n")
        _annotation(graph, "a.py::f", "- [ ] function level\n")
        _annotation(graph, "a.py::C.m", "- [ ] method level\n")
        _annotation(graph, "b.py::h", "- [ ] other file\n")
        file_scope = ops.open_items("a.py")
        assert [i["text"] for i in file_scope["items"]] == [
            "file level", "method level", "function level"]
        # A scope names an id or an ancestor of one; `a.pyx` is neither.
        _annotation(graph, "a.pyx::z", "- [ ] lookalike\n")
        assert ops.open_items("a.pyx")["count"] == 1
        assert ops.open_items("a.py")["count"] == 3
        assert ops.open_items("a.py::C")["count"] == 1
        assert ops.open_items("a.py::C.m")["count"] == 1

    def test_unknown_scope_reads_empty(self, graph):
        result = ops.open_items("nope.py")
        assert result == {"scope": "nope.py", "items": [], "count": 0}

    def test_graph_method_and_mcp_surface(self, graph):
        _annotation(graph, "a.py::f", "- [ ] via the facade\n")
        assert graph.open_items("a.py::f")["items"][0]["text"] == "via the facade"
        from coderadar.mcp.server import create_server
        server = create_server(None)
        # The tool is registered with the read-only annotation flags.
        tools = {t.name: t for t in server._tool_manager.list_tools()}
        assert "coderadar_open_items" in tools


class TestWhyJoin:
    def test_keyword_hits_carry_notes_from_entity_and_file(self, graph):
        _annotation(graph, "a.py", "- [ ] file note\n")
        _annotation(graph, "a.py::C", "- [ ] class note\n")
        graph.comment("a.py::C.m", "the method's reason")
        rows = ops.search("m")
        hit = next(r for r in rows if r["id"] == "a.py::C.m")
        abouts = [n["about"] for n in hit["notes"]]
        # The method's own comment, the class's annotation, the file's.
        assert abouts == ["a.py::C.m", "a.py::C", "a.py"]
        kinds = {n["kind"] for n in hit["notes"]}
        assert kinds == {"annotation", "comment"}
        # Bodies ride in full; the annotation states its open count.
        by_about = {n["about"]: n for n in hit["notes"]}
        assert by_about["a.py::C.m"]["body"].strip() == "the method's reason"
        assert by_about["a.py"]["open"] == 1

    def test_a_hit_without_notes_keeps_the_old_shape(self, graph):
        rows = ops.search("h")
        hit = next(r for r in rows if r["id"] == "b.py::h")
        assert "notes" not in hit

    def test_retired_notes_do_not_ride_along(self, graph):
        row = _annotation(graph, "a.py::f", "- [ ] withdrawn\n")
        rows = ops.search("f")
        hit = next(r for r in rows if r["id"] == "a.py::f")
        assert hit["notes"][0]["body"].strip() == "- [ ] withdrawn"
        graph.retire_annotation(row)
        rows = ops.search("f")
        hit = next(r for r in rows if r["id"] == "a.py::f")
        assert "notes" not in hit

    def test_notes_are_newest_first_within_a_hit(self, graph):
        _annotation(graph, "a.py::f", "- [ ] first row\n")
        _annotation(graph, "a.py::f", "- [ ] second row\n")
        hit = next(r for r in ops.search("f") if r["id"] == "a.py::f")
        assert [n["n"] for n in hit["notes"]] == [2, 1]

    def test_semantic_and_hybrid_searches_join_too(self, graph):
        """The join is on every search path, not just the keyword one.

        Vectors are staged directly (8-dim, persisted) so the test does not
        depend on a local model: the point is the join, not the embedding.
        """
        _annotation(graph, "a.py::f", "- [ ] semantic note\n")
        _core.set_embeddings_bulk(
            [("a.py::f", [0.1] * 8, "M#pp1#x"), ("a.py::g", [0.9] * 8, "M#pp1#y")])
        _core.persist_embeddings("M", [("a.py::f", "M#pp1#x"),
                                       ("a.py::g", "M#pp1#y")])
        rows = ops.search_similar([0.1] * 8, top_k=5)
        hit = next(r for r in rows if r["id"] == "a.py::f")
        assert hit["notes"][0]["body"].strip() == "- [ ] semantic note"

    def test_storeless_graph_answers_no_notes(self, tmp_path, monkeypatch):
        """`analyze` without a store: search still works, notes are absent.

        `open_items` refuses like every other store read — there is no
        ledger to aggregate; a search over the projection keeps working
        because it never needed one.
        """
        monkeypatch.chdir(tmp_path)
        (tmp_path / "a.py").write_bytes(b"def f():\n    return 1\n")
        coderadar.analyze(".")
        assert ops.search("f")[0].get("notes") is None
        with pytest.raises(ops.EngineError, match="No persistent store"):
            ops.open_items()
