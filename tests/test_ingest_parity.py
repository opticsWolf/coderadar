"""Parity between the two ingest paths.

`analyze()` builds entities through `build_fragment`; `update_file()` builds
them through `apply_diff_update`. The two construct `Function` and `Class`
independently, and drift between them is silent — a field dropped on one side
shows up as a signature that renders without parameters, smells that stop
firing, and embeddings over a truncated signature, with nothing failing.

These tests pin the two paths to the same output.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, search_entities, update_file
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

pytestmark = pytest.mark.skipif(
    not _CORE_AVAILABLE, reason="Rust _core extension not built"
)

SOURCE = '''\
def no_args():
    return 1


def positional(alpha, beta):
    return alpha + beta


def annotated(count: int, label: str = "x", *rest, **kw) -> str:
    return f"{label}{count}"


class Holder:
    def method(self, value, flag=False):
        return value if flag else None
'''


def _norm(entity_id: str) -> str:
    """Path-separator-insensitive id, so spelling drift does not mask field drift."""
    return entity_id.replace("\\", "/").lstrip("./")


def _entities() -> dict:
    """Every function and class in the graph, keyed by normalized id."""
    out = {}
    for kind in ("function", "class"):
        for e in search_entities("", 200, kind):
            out[_norm(e["id"])] = e
    return out


@pytest.fixture
def indexed(tmp_path):
    target = tmp_path / "parity_mod.py"
    target.write_text(SOURCE, encoding="utf-8")
    analyze(str(tmp_path))
    return target


def test_update_file_preserves_parameters(indexed):
    """apply_diff_update used to construct Function with `parameters: vec![]`.

    Every function touched by update_file — i.e. every function in watch mode,
    and every function after a mutation, since apply() reindexes affected
    files — silently lost its parameter list.
    """
    before = _entities()
    assert before, "fixture should have produced entities"

    # Force the diff path over unchanged content.
    update_file(str(indexed), None, True)
    after = _entities()

    assert set(after) == set(before), "the same entities should survive a re-ingest"

    for eid, entity in before.items():
        if "signature" in entity:
            assert after[eid].get("signature") == entity["signature"], (
                f"{eid}: signature changed across ingest paths"
            )


def test_update_file_matches_analyze_field_for_field(indexed):
    """Full-dict parity, so the next dropped field fails here rather than silently.

    Path-shaped fields are normalized before comparing: the two paths spell
    separators differently, which is its own defect and has its own test below.
    """
    path_fields = ("id", "parent_id", "parent_module", "file_path")

    def flatten(entity: dict) -> dict:
        return {
            k: (_norm(v) if k in path_fields and isinstance(v, str) else v)
            for k, v in entity.items()
        }

    before = {k: flatten(v) for k, v in _entities().items()}

    update_file(str(indexed), None, True)
    after = {k: flatten(v) for k, v in _entities().items()}

    for eid, entity in before.items():
        assert after[eid] == entity, f"{eid} differs after update_file"


@pytest.mark.xfail(
    reason="apply_diff_update re-inserts under a normalized id while the walker "
           "stores the OS spelling, so every updated entity is duplicated",
    strict=True,
)
def test_update_file_does_not_duplicate_entities(indexed):
    """Ids must not fork across the two ingest paths.

    `analyze` stores `.\\p.py::f`; `update_file` inserts `p.py::f` alongside it
    instead of replacing it, so watch mode and post-mutation reindex both grow
    a second copy of every entity they touch.
    """
    before = len(_entities())
    update_file(str(indexed), None, True)

    raw = [e["id"] for kind in ("function", "class") for e in search_entities("", 200, kind)]
    assert len(raw) == before, f"entity count grew from {before} to {len(raw)}: {raw}"


def test_signatures_carry_their_parameters(indexed):
    """A signature rendered without parameters is the visible symptom."""
    update_file(str(indexed), None, True)
    by_name = {e["name"]: e for e in _entities().values()}

    assert by_name["positional"]["signature"] == "def positional(alpha, beta)"
    assert by_name["no_args"]["signature"] == "def no_args()"
    assert "count" in by_name["annotated"]["signature"]
    assert "value" in by_name["method"]["signature"]
