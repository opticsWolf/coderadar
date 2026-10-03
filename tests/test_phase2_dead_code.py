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


# ── §2.3 — overrides of external bases are entry points ─────────────────

EXTERNAL_SRC = '''
from PySide6.QtWidgets import QWidget
from typing import Protocol


class DockWidget(QWidget):
    def eventFilter(self, obj, event):
        return False

    def _dead_helper(self):
        return 1


class MenuTarget(Protocol):
    def menu_target(self) -> str:
        ...


class Base(QWidget):
    def eventFilter(self, obj, event):
        return False


class Derived(Base):
    def eventFilter(self, obj, event):
        return False
'''


def test_external_overrides_are_not_dead(tmp_path):
    (tmp_path / "qt_app.py").write_text(EXTERNAL_SRC, encoding="utf-8")
    analyze(str(tmp_path))
    # entity_name is the bare method name, so key on the qualified id tail.
    found = {f["entity_id"].rsplit("::", 1)[-1]: f for f in find_dead_code(0.0, False, 1000)}
    # Virtual dispatch from outside the indexed root keeps these live.
    for dispatched in ("DockWidget.eventFilter", "Base.eventFilter", "MenuTarget.menu_target"):
        assert dispatched not in found, f"{dispatched} wrongly flagged dead"
    # A private helper of a framework subclass is never a dispatch target.
    assert "DockWidget._dead_helper" in found
    # The outermost in-repo override of the external base is the dispatch
    # target; the derived one is live only via never-instantiated dispatch.
    assert found["Derived.eventFilter"]["kind"] == "rta-dead"


ABC_SRC = '''
from abc import ABC, abstractmethod


class Shape(ABC):
    @abstractmethod
    def area(self):
        ...

    def describe(self):
        return "shape"


class Plain:
    def _orphan(self):
        return 1
'''


def test_abstract_declarations_are_not_dead(tmp_path):
    (tmp_path / "shapes.py").write_text(ABC_SRC, encoding="utf-8")
    analyze(str(tmp_path))
    found = {f["entity_id"].rsplit("::", 1)[-1] for f in find_dead_code(0.0, False, 1000)}
    assert "Shape.area" not in found
    # ABC subclasses an external base, so its public members are dispatch
    # candidates too (plan §2.3) — `describe` is not flagged.
    assert "Shape.describe" not in found
    # A plain class with no external base is not protected: `_orphan` stays
    # a real finding.
    assert "Plain._orphan" in found
