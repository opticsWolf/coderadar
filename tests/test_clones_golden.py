"""Day-one golden tests for Stage 2 clone detection (plan §13.1).

Asserts exact grouping on tests/fixtures/python/clones/ — a Type-1 pair, a
renamed Type-2 body, and a structurally unrelated control that must never be
grouped.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, find_clones
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

FIXTURE = Path(__file__).parent / "fixtures" / "python" / "clones"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestClonesGolden:
    @pytest.fixture(autouse=True)
    def _index(self):
        analyze(str(FIXTURE))

    def _groups(self, **kwargs):
        groups = find_clones(**kwargs)
        # entity name -> the type of the group containing it
        out = {}
        for g in groups:
            for inst in g["instances"]:
                name = inst["entity_id"].split("::")[-1]
                out[name] = g["clone_type"]
        return out, groups

    def test_identical_bodies_are_type1(self):
        mapping, _ = self._groups(min_lines=4)
        assert mapping.get("clone_a") == "type-1"
        assert mapping.get("clone_b") == "type-1"

    def test_renamed_body_is_type2(self):
        mapping, _ = self._groups(min_lines=4)
        assert mapping.get("clone_c") == "type-2"

    def test_unrelated_body_is_never_grouped(self):
        mapping, _ = self._groups(min_lines=4)
        assert "unrelated" not in mapping

    def test_type1_similarity_is_exact(self):
        _, groups = self._groups(min_lines=4)
        t1 = [g for g in groups if g["clone_type"] == "type-1"]
        assert all(g["similarity"] == 1.0 for g in t1)

    def test_min_lines_filters_short_bodies(self):
        _, groups = self._groups(min_lines=10_000)
        assert groups == []
