"""Golden snapshot tests for the native smell engine.

Locks the exact (rule_id, severity, entity_name, signals) output for the four
rules that previously had no golden coverage:
  - deep-nesting     (4 nested ifs → depth 4, Medium)
  - excessive-returns (6 returns → Medium)
  - brain-method     (max cyclomatic 15, WMC 20 → High)
  - god-class        (positive: WMC 52, CBO 5 → Medium)
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, get_smells
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

GOLDEN_DIR = Path(__file__).parent / "fixtures" / "python" / "smells" / "golden"


def _subset_in(expected, findings):
    """True if some finding contains all expected key/value pairs exactly."""
    return any(all(f.get(k) == v for k, v in expected.items()) for f in findings)


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestSmellGolden:
    @pytest.fixture(autouse=True)
    def _index(self):
        analyze(str(GOLDEN_DIR))

    def _findings(self):
        return get_smells(None, None)

    def test_deep_nesting_golden(self):
        assert _subset_in(
            {
                "rule_id": "deep-nesting",
                "severity": "Medium",
                "entity_name": "deeply_nested",
                "signals": {"nesting_depth": 4.0},
            },
            self._findings(),
        )

    def test_excessive_returns_golden(self):
        assert _subset_in(
            {
                "rule_id": "excessive-returns",
                "severity": "Medium",
                "entity_name": "many_returns",
                "signals": {"return_count": 6.0},
            },
            self._findings(),
        )

    def test_brain_method_golden(self):
        assert _subset_in(
            {
                "rule_id": "brain-method",
                "severity": "High",
                "entity_name": "BrainMethod",
                "signals": {"max_method_cyclomatic": 15.0, "WMC": 20.0},
            },
            self._findings(),
        )

    def test_god_class_positive_golden(self):
        assert _subset_in(
            {
                "rule_id": "god-class",
                "severity": "Medium",
                "entity_name": "God",
                "signals": {"WMC": 52.0, "CBO": 5.0},
            },
            self._findings(),
        )
