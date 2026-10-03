"""Phase 2 — dead-code precision (plan §2.1, §2.2)."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, find_dead_code, get_smells
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

pytestmark = pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")

SRC = '''
import pytest


def main():
    return 1


def _tiny_dead():
    return 1


def _big_dead():
{big}
    return 0


@pytest.fixture
def fx():
    return 1


def test_uses_helper(fx):
    helper()


def helper():
    return 2
'''


@pytest.fixture
def project(tmp_path):
    body = "\n".join(f"    x{i} = {i}" for i in range(80))
    (tmp_path / "app.py").write_text(SRC.format(big=body), encoding="utf-8")
    analyze(str(tmp_path))
    return tmp_path


def _smell_names():
    return {f["entity_id"].rsplit(".", 1)[-1].rsplit("::", 1)[-1]
            for f in get_smells(None, "dead-code")}


def test_smell_and_find_dead_code_agree(project):
    found = {f["entity_name"] for f in find_dead_code(0.0, False, 1000)}
    assert _smell_names() == found
    # Tests, fixtures and test-only helpers are not dead code.
    for live in ("test_uses_helper", "fx", "helper", "main"):
        assert live not in found


def test_size_does_not_change_confidence(project):
    scores = {f["entity_name"]: f["score"] for f in find_dead_code(0.0, False, 1000)}
    assert scores["_tiny_dead"] == pytest.approx(scores["_big_dead"])


def test_smell_carries_score_signal(project):
    by_name = {f["entity_id"].rsplit("::", 1)[-1]: f for f in get_smells(None, "dead-code")}
    sig = by_name["_big_dead"]["signals"]
    assert sig["score"] > 0 and sig["removable_lines"] > 80
