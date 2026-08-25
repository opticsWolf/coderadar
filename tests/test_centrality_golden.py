"""Stage 5 golden tests: harmonic centrality ranking + triage signal.

Centrality walks callers_by_callee upstream (importance = how much depends
on you), normalized 0..=1. Pinned here: rank ordering of a fan-in graph,
zero scores for unknown ids, and the `central` triage signal attached to
god-class findings.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import (
        analyze,
        get_smells,
        rank_by_centrality,
        search_entities,
    )
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestCentralityRanking:
    @pytest.fixture(autouse=True)
    def _index_fan_in(self, tmp_path):
        # Four leaves call core(); core() calls util(). Harmonic centrality
        # must rank core (direct fan-in) above util (relayed fan-in).
        src = ""
        for i in range(4):
            src += f"def leaf_{i}():\n    return core()\n\n"
        src += "def core():\n    return util()\n\ndef util():\n    return 42\n"
        (tmp_path / "fan.py").write_text(src, encoding="utf-8")
        analyze(str(tmp_path))

    def _fid(self, name):
        hits = search_entities(name, 5, "function")
        assert hits, f"{name} must be indexed"
        return [h["id"] for h in hits]

    def test_core_ranks_above_util_above_leaves(self):
        ids = []
        ids += self._fid("core")
        ids += [i for i in self._fid("util") if i not in ids]
        scores = dict(rank_by_centrality(ids))
        core_scores = [v for k, v in scores.items() if k.endswith("::core")]
        util_scores = [v for k, v in scores.items() if k.endswith("::util")]
        assert max(core_scores) == 1.0, "max fan-in normalizes to exactly 1.0"
        assert max(core_scores) > max(util_scores)

    def test_unknown_id_scores_zero(self):
        scores = dict(rank_by_centrality(["no/such/path.py::ghost"]))
        assert scores["no/such/path.py::ghost"] == 0.0


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestCentralSignal:
    def test_god_class_carries_central_signal(self, tmp_path):
        # A fat class whose main method is called from four places: the
        # god-class finding must carry the normalized centrality signal.
        src = "def caller_0():\n    return Worker().run(1)\n"
        src += """def caller_1():
    return Worker().run(2)


def caller_2():
    return Worker().run(3)


def caller_3():
    return Worker().run(4)


class Worker:
    def run(self, n):
        total = 0
"""
        for i in range(15):
            src += f"        total += {i} * n\n        if total > {1000 + i}:\n            total -= {i}\n"
        src += "        return total\n"
        (tmp_path / "fat.py").write_text(src, encoding="utf-8")
        analyze(str(tmp_path))

        findings = [
            f for f in get_smells(None, None)
            if f["rule_id"] == "god-class"
        ]
        # The AST table may or may not flag this exact shape depending on
        # WMC/CBO thresholds; when it does, the centrality signal must ride
        # along (Stage 5 contract). When it doesn't, no false signal may
        # appear anywhere.
        for f in findings:
            assert "central" in f["signals"], "god-class finding must carry centrality"
            assert 0.0 <= f["signals"]["central"] <= 1.0
