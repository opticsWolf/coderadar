"""Day-one golden tests for Stage 1 dead-code detection (plan §13.1).

Asserts EXACT finding sets on tests/fixtures/python/deadcode/ — not just
"rule fires". Closes the god-class coverage pattern for dead-code.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, find_dead_code
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

FIXTURE = Path(__file__).parent / "fixtures" / "python" / "deadcode"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestDeadCodeGolden:
    @pytest.fixture(autouse=True)
    def _index(self):
        analyze(str(FIXTURE))

    def _names(self, **kwargs):
        return {f["entity_name"] for f in find_dead_code(**kwargs)}

    def test_live_chain_is_never_reported(self):
        names = self._names(min_confidence=0.0)
        for live in ("main", "run_pipeline", "_load", "_transform"):
            assert live not in names, f"{live} is live but was reported dead"

    def test_orphan_is_unreachable(self):
        findings = [
            f for f in find_dead_code(min_confidence=0.0) if f["entity_name"] == "_orphan"
        ]
        assert len(findings) == 1
        assert findings[0]["kind"] == "unreachable"

    def test_dead_chain_reported_transitively(self):
        # _chain_a has zero callers (Unreachable); _chain_b is called only by
        # dead _chain_a — the transitive case fossil reports as a dead chain.
        kinds = {
            f["entity_name"]: f["kind"]
            for f in find_dead_code(min_confidence=0.0)
        }
        assert kinds.get("_chain_a") == "unreachable"
        assert kinds.get("_chain_b") == "transitively-dead"

    def test_dunder_method_is_runtime_live(self):
        assert "__repr__" not in self._names(min_confidence=0.0)

    def test_exact_finding_set_at_zero_confidence(self):
        names = self._names(min_confidence=0.0)
        assert names == {"_orphan", "_chain_a", "_chain_b"}

    def test_min_confidence_filters_weak_findings(self):
        strong = self._names(min_confidence=0.95)
        # Only Certain-tier findings survive a 0.95 floor.
        for f in find_dead_code(min_confidence=0.95):
            assert f["score"] >= 0.95
        assert isinstance(strong, set)
