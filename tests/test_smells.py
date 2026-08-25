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


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestFindingDedupe:
    """G0-B (CODERADAR_BUGS_QUIRKS.md #9): one finding per entity per rule.

    Stale + fresh entity versions used to coexist after incremental updates,
    reporting the same violation two or three times.
    """

    def test_no_duplicate_findings_across_edit_cycle(self, tmp_path):
        from coderadar._core import analyze, get_smells, update_file

        body = "\n".join(f"    x{i} = {i}" for i in range(60))
        src = tmp_path / "big.py"
        src.write_text(f"def huge():\n{body}\n    return x0\n", encoding="utf-8")

        analyze(str(tmp_path))
        findings = [f for f in get_smells(None, "long-method") if "huge" in f["entity_id"]]
        assert len(findings) >= 1, "fixture must trigger long-method"
        assert len(findings) == 1, f"exactly one finding expected, got {len(findings)}"

        # Edit cycle: rewrite the function body, re-index incrementally.
        src.write_text(f"def huge():\n{body}\n    return x1\n", encoding="utf-8")
        update_file(str(src), None, None)

        findings_after = [
            f for f in get_smells(None, "long-method") if "huge" in f["entity_id"]
        ]
        assert len(findings_after) == 1, (
            f"edit cycle must not duplicate findings, got {len(findings_after)}"
        )


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestStrictnessProfiles:
    """Stage 0.4: strict/normal/loose threshold profiles on get_smells."""

    def _ids(self, findings):
        return {(f["rule_id"], f["entity_id"]) for f in findings}

    def test_normal_default_matches_explicit_normal(self):
        from coderadar._core import get_smells
        assert self._ids(get_smells(None, None)) == self._ids(get_smells(None, None, "normal"))

    def test_monotonicity_strict_superset_of_loose(self):
        # findings(Strict) ⊇ findings(Normal) ⊇ findings(Loose) — the whole
        # correctness story of the profile feature.
        from coderadar._core import get_smells
        strict = self._ids(get_smells(None, None, "strict"))
        normal = self._ids(get_smells(None, None, "normal"))
        loose = self._ids(get_smells(None, None, "loose"))
        assert loose <= normal <= strict, (
            f"monotonicity broken: |strict|={len(strict)} |normal|={len(normal)} |loose|={len(loose)}"
        )

    def test_unknown_strictness_is_a_loud_error(self):
        from coderadar._core import get_smells
        with pytest.raises(ValueError, match="maximal"):
            get_smells(None, None, "maximal")
