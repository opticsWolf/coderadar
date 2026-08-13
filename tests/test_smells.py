"""CodeRadar — native code smell engine end-to-end tests.

Exercises the Rust `SmellEngine` through the `get_smells` pyfunction against a
fixture with known architectural smells (see fixtures/python/smells/smelly.py).
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

SMELLS_DIR = Path(__file__).parent / "fixtures" / "python" / "smells"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestSmellEngine:
    @pytest.fixture(autouse=True)
    def _index(self):
        analyze(str(SMELLS_DIR))

    def test_get_all_smells_detects_expected_rules(self):
        findings = get_smells(None, None)
        rule_ids = {f["rule_id"] for f in findings}
        # Method-scope smells
        assert "long-parameter-list" in rule_ids
        assert "high-cyclomatic-complexity" in rule_ids
        assert "long-method" in rule_ids
        # Class-scope smells (requires field extraction)
        assert "too-many-fields" in rule_ids
        assert "data-class" in rule_ids

    def test_filter_by_rule_carries_signals(self):
        findings = get_smells(None, "long-parameter-list")
        assert findings
        assert all(f["rule_id"] == "long-parameter-list" for f in findings)
        assert any(f["signals"].get("param_count", 0) >= 5 for f in findings)

    def test_filter_by_rule_too_many_fields(self):
        findings = get_smells(None, "too-many-fields")
        assert findings
        assert any(f["signals"].get("field_count", 0) >= 10 for f in findings)

    def test_findings_are_enriched_with_entity_name(self):
        findings = get_smells(None, "too-many-fields")
        assert findings
        assert all("entity_name" in f and f["entity_name"] for f in findings)
        assert any(f["entity_name"] == "BigDataClass" for f in findings)

    def test_filter_by_entity_id(self):
        # The too-many-params function id is "<file>::too_many_params"
        findings = get_smells(None, None)
        assert findings  # sanity: indexed and smells exist
        by_entity = [f for f in findings if f["entity_name"] == "too_many_params"]
        assert by_entity, "expected a finding attributed to too_many_params"
