"""Stage 4 golden tests: CFG refinement behind analysis.use_cfg_metrics.

The strangler flag defaults OFF (plan §4 principle 6); these tests pin both
the honest absence of CFG-only signals when off and the refined values when
on.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, get_smells, set_config
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

# 8 ifs + one `and`. The AST approximation counts 9 decisions (misses the
# short-circuit) -> cyclomatic 9 is under the threshold of 10 and nothing
# fires. The CFG refinement counts the short-circuit: McCabe 10 -> fires.
G_SOURCE = "def g(a, b, c, d, e, f, gg, h2, i2):\n    if a and b:\n        return 1\n"
for _v in ("c", "d", "e", "f", "gg", "h2", "i2"):
    G_SOURCE += f"    if {_v}:\n        return 2\n"
G_SOURCE += "    return 3\n"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestCfgRefinement:
    @pytest.fixture(autouse=True)
    def _index_and_reset(self, tmp_path):
        (tmp_path / "dead_after.py").write_text(
            "def f(x):\n"
            "    return 1\n"
            "    print('never')\n",
            encoding="utf-8",
        )
        (tmp_path / "short_circuit.py").write_text(G_SOURCE, encoding="utf-8")
        analyze(str(tmp_path))
        yield
        # Restore the default so other suites never see the flag.
        set_config({"analysis": {"use_cfg_metrics": False}})

    def _rule_findings(self):
        return get_smells(None, "intra-dead-statements")

    def test_flag_off_means_honest_absence(self):
        assert self._rule_findings() == []
        # ...and no unreachable_blocks signal anywhere either.
        assert all(
            "unreachable_blocks" not in f.get("signals", {})
            for f in get_smells(None, None)
        )

    def test_flag_on_finds_code_after_return(self):
        set_config({"analysis": {"use_cfg_metrics": True}})
        findings = self._rule_findings()
        assert len(findings) == 1
        assert findings[0]["entity_name"] == "f"
        assert findings[0]["signals"]["unreachable_blocks"] == 1.0

    def test_flag_on_refines_short_circuit_cyclomatic(self):
        set_config({"analysis": {"use_cfg_metrics": False}})
        off = [
            f for f in get_smells(None, None)
            if f.get("entity_name") == "g" and f["rule_id"] == "high-cyclomatic-complexity"
        ]
        assert off == [], "AST approximation must miss the short-circuit"

        set_config({"analysis": {"use_cfg_metrics": True}})
        on = [
            f for f in get_smells(None, None)
            if f.get("entity_name") == "g" and f["rule_id"] == "high-cyclomatic-complexity"
        ]
        assert len(on) == 1, "CFG refinement must count `a and b`: McCabe 10"
        assert on[0]["signals"]["cyclomatic"] == 10.0
