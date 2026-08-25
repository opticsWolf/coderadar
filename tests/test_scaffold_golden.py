"""Day-one golden tests for Stage 3 scaffolding & secrets scanning."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, find_scaffolding
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

FIXTURE = Path(__file__).parent / "fixtures" / "python" / "scaffold"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestScaffoldGolden:
    @pytest.fixture(autouse=True)
    def _index(self):
        analyze(str(FIXTURE))

    def _kinds(self, include_secrets=False):
        fs = find_scaffolding(include_secrets, 500)
        return {(f["kind"], f["label"]) for f in fs}

    def test_comment_markers_found(self):
        kinds = self._kinds()
        assert any(k == "comment-marker" and "Phase 1" in l for k, l in kinds)
        assert any(k == "comment-marker" and "TODO" in l for k, l in kinds)

    def test_placeholder_body_found(self):
        kinds = self._kinds()
        assert any(k == "placeholder-body" for k, l in kinds), kinds

    def test_secret_hidden_without_opt_in(self):
        kinds = self._kinds(include_secrets=False)
        assert not any(k == "secret" for k, _ in kinds)

    def test_secret_redacted_with_opt_in(self):
        findings = find_scaffolding(True, 500)
        secrets = [f for f in findings if f["kind"] == "secret"]
        assert any(f["label"] == "aws_access_key" for f in secrets)
        # The full key must never leave the process.
        assert all("JKLMNOP" not in f["snippet"] for f in findings)
        assert all(f["snippet"].endswith("***") for f in secrets)

    def test_real_logic_is_not_flagged(self):
        findings = find_scaffolding(False, 500)
        flagged_names = {f.get("label", "") for f in findings}
        assert not any("real_logic" in s for s in
                       [f["snippet"] for f in findings if f["kind"] == "placeholder-body"])
