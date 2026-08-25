"""Stage 6.2 golden tests: statically-decided conditions (dead branches).

Scoped-down const-prop per plan section 11.2: only boolean literals and
literal-vs-literal comparisons (optionally under not/and/or with
short-circuit awareness) are decided; everything else stays unknown and
never produces findings. Rides the analysis.use_cfg_metrics strangler flag
like the rest of the Stage 4 family.
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


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestDeadBranch:
    """Stage 6.2: statically-decided conditions behind the CFG flag."""

    @pytest.fixture(autouse=True)
    def _reset_flag(self):
        yield
        set_config({"analysis": {"use_cfg_metrics": False}})

    def _findings(self):
        return [f for f in get_smells(None, None) if f["rule_id"] == "dead-branch"]

    def test_flag_off_means_honest_absence(self, tmp_path):
        (tmp_path / "flags.py").write_text(
            "def f(debug):\n"
            "    if True:\n"
            "        print('always')\n"
            "    while 1 > 2:\n"
            "        break\n",
            encoding="utf-8",
        )
        analyze(str(tmp_path))
        assert self._findings() == []

    def test_flag_on_fires_medium_with_count(self, tmp_path):
        set_config({"analysis": {"use_cfg_metrics": True}})
        (tmp_path / "flags.py").write_text(
            "def f(x):\n"
            "    if False:\n"
            "        print('never')\n"
            "    if x == 1 or True:\n"
            "        print('always via short-circuit')\n"
            "    if x > 3:\n"
            "        print('runtime-decided, not flagged')\n",
            encoding="utf-8",
        )
        analyze(str(tmp_path))
        findings = self._findings()
        assert len(findings) == 1
        f = findings[0]
        assert f["severity"] == "Medium"
        # `if False` and `x == 1 or True` are decided; `x > 3` is not.
        assert f["signals"]["dead_branches"] == 2.0
