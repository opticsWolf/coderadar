"""Issue 5: the entity-id grammar is a contract, not folklore.

Every spelling the README/MCP descriptions promise must resolve to the
same entity: canonical dot-prefix, absolute path, forward slashes, and
missing dot-prefix. And the enum halves must refuse loudly: unknown search
kinds (Rust + MCP) error instead of reading as "no results".
"""
import os

import coderadar
import pytest
from coderadar._core import callees_of, callers_of, search_entities
from coderadar.mcp import server as mcp


@pytest.fixture()
def chain(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "helpers.py").write_text(
        "def combine(items):\n    return items\n", encoding="utf-8"
    )
    (pkg / "__init__.py").write_text(
        "from .helpers import combine\n", encoding="utf-8"
    )
    (tmp_path / "main.py").write_text(
        "from pkg import combine\ndef run(items):\n    return combine(items)\n",
        encoding="utf-8",
    )
    graph = coderadar.analyze(str(tmp_path))
    run_id = next(
        h["id"] for h in search_entities("run", 10, "function") if "::run" in h["id"]
    )
    comb_id = next(
        h["id"]
        for h in search_entities("combine", 10, "function")
        if "helpers" in h.get("id", "")
    )
    return graph, str(tmp_path), run_id, comb_id


def _callee_ids(start_id):
    cal = callees_of(start_id)
    assert isinstance(cal, list)
    return [c.get("id", "") for c in cal]


def test_canonical_spelling_resolves_through_reexport(chain):
    _, _, run_id, comb_id = chain
    assert comb_id in _callee_ids(run_id)


def test_absolute_forward_slash_and_bare_spellings_agree(chain):
    _, root, run_id, comb_id = chain
    _, _, tail = run_id.partition("::")
    assert tail == "run"
    absolute = os.path.join(root, "main.py") + "::run"
    fwd = run_id.replace(os.sep, "/") if os.sep in run_id else run_id
    bare = run_id[2:] if run_id[:2] in (".\\", "./") else run_id
    for spelling in {absolute, fwd, bare} - {run_id}:
        assert comb_id in _callee_ids(spelling), spelling


def test_external_pseudo_namespace_passes_through(chain):
    # external:: targets are not entities — but naming one must not error
    # or canonicalize into a file path (R2-16 carve-out).
    assert callers_of("external::len") == []


def test_unknown_search_kind_refuses(chain):
    with pytest.raises(ValueError, match="unknown kind"):
        search_entities("combine", 5, "bogus_kind")
    with pytest.raises(ValueError, match="unknown kind"):
        search_entities("combine", 5, "method")  # methods are functions here


def test_known_search_kinds_accepted(chain):
    for kind in ("function", "class", "type_alias", "constant", "module", "import"):
        assert isinstance(search_entities("combine", 5, kind), list)


def test_mcp_search_kind_refuses(chain):
    graph, _, _, _ = chain
    out = mcp._search(graph, "combine", "bogus_kind", 5)
    assert "Unknown kind" in out
    assert "function | class" in out
