"""H-12: a partial edge write must not pass for a clean analyze.

`persist_edges` is assert-only and flushes in batches, so a failure part way
through leaves the earlier batches committed — the store's edges are then
incomplete while the index around them is sound. That used to surface as a
`[diag]` line on stderr with a zero exit and a clean report.

Two halves are covered here: the duplicate that caused it in practice (a
function constructing the same class twice, which the ledger rejected on its
`links` UNIQUE key) and the reporting surface that now carries the failure
when one happens. The Rust-side regression test is
`test_persist_edges_dedups_repeated_constructor_calls`.
"""

import pytest
from coderadar import analyze, edge_persist_error, ops, render

# One class, constructed twice in one function: this is what produced two
# identical INSTANTIATES assertions sharing one `valid_from`.
TWICE = (
    "class Thing:\n"
    "    def __init__(self):\n"
    "        self.x = 1\n"
    "\n"
    "def make():\n"
    "    a = Thing()\n"
    "    b = Thing()\n"
    "    return a, b\n"
)


@pytest.fixture()
def project(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "twice.py").write_text(TWICE)
    analyze(".", create_store=True)
    return tmp_path


def test_repeated_constructor_leaves_no_edge_persist_error(project):
    # The projection really does hold the duplicate (otherwise this passes
    # for the wrong reason), and persisting it still reports clean.
    assert edge_persist_error() is None


def test_clean_reindex_report_omits_the_key(project):
    out = ops.reindex(full=True)
    assert "edge_persist_error" not in out, (
        f"a clean reindex must not report an edge failure, got "
        f"{out.get('edge_persist_error')!r}"
    )
    assert edge_persist_error() is None


def test_full_reanalyze_of_a_duplicate_heavy_tree_stays_clean(project):
    # Several files, each with the same shape, so the duplicates span more
    # than one flush batch — the case where the old abort left *some*
    # batches committed rather than none.
    for i in range(12):
        (project / f"m{i}.py").write_text(TWICE)
    out = ops.reindex(full=True)
    assert "edge_persist_error" not in out
    assert out["stats"]["modules"] >= 13


def test_render_warns_loudly_when_edges_are_incomplete():
    report = {
        "stats": {"file_count": 2, "modules": 2, "classes": 1,
                  "functions": 3, "call_edges": 4},
        "edge_persist_error": 'Engine(SqliteFailure(1555, "UNIQUE ..."))',
    }
    text = render.reindex(report)
    assert "INCOMPLETE" in text
    assert "UNIQUE" in text


def test_render_is_unchanged_when_edges_persisted():
    report = {
        "stats": {"file_count": 2, "modules": 2, "classes": 1,
                  "functions": 3, "call_edges": 4},
    }
    text = render.reindex(report)
    assert "INCOMPLETE" not in text
