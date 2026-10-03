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


# ── §2.6 — every finding explains itself ─────────────────────────────────


def test_evidence_travels_with_the_finding(project):
    by_name = {f["entity_name"]: f for f in find_dead_code(0.0, False, 1000)}
    ev = by_name["_tiny_dead"]["evidence"]
    assert "no inbound callers" in ev
    assert "private name" in ev


def test_transitive_dead_carries_chain_distance(tmp_path):
    (tmp_path / "app.py").write_text(
        "def _orphan():\n"
        "    return _child()\n\n\n"
        "def _child():\n"
        "    return 1\n\n\n"
        "def main():\n"
        "    return 1\n",
        encoding="utf-8",
    )
    analyze(str(tmp_path))
    by_name = {f["entity_name"]: f for f in find_dead_code(0.0, False, 1000)}
    child = by_name["_child"]
    assert child["kind"] == "transitively-dead"
    assert "all 1 caller(s) are themselves dead" in child["evidence"]
    assert child["nearest_root_distance"] == 1
    # mid-chain entries: one more dead hop between the chain head and _child.
    assert "nearest_root_distance" not in by_name["_orphan"]


def test_rta_dead_carries_class_evidence(tmp_path):
    (tmp_path / "qt_app.py").write_text(EXTERNAL_SRC, encoding="utf-8")
    analyze(str(tmp_path))
    findings = [
        f for f in find_dead_code(0.0, False, 1000)
        if f["entity_id"].endswith("Derived.eventFilter")
    ]
    assert findings, "Derived.eventFilter should be rta-dead"
    ev = findings[0]["evidence"]
    assert "live only through virtual dispatch" in ev
    assert any("never instantiated" in e for e in ev)


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


# ── §2.4 — framework packs (table-driven, opt-in by detection) ──────────


def test_pytest_pack_covers_nested_test_trees(tmp_path):
    (tmp_path / "tests" / "visual").mkdir(parents=True)
    (tmp_path / "app.py").write_text("def run():\n    return 1\n", encoding="utf-8")
    (tmp_path / "tests" / "conftest.py").write_text(
        "import pytest\n\n\ndef pytest_configure(config):\n    pass\n", encoding="utf-8"
    )
    (tmp_path / "tests" / "visual" / "corner_harness.py").write_text(
        "def build_harness():\n    return 1\n", encoding="utf-8"
    )
    (tmp_path / "tests" / "visual" / "test_corner.py").write_text(
        "import corner_harness\n\n\ndef test_corner():\n"
        "    assert corner_harness.build_harness()\n",
        encoding="utf-8",
    )
    analyze(str(tmp_path))

    def _kinds():
        out = {}
        for f in find_dead_code(0.0, True, 1000):
            # file arrives as `.\tests\visual\corner_harness.py`-style
            out[(f["file"].replace("\\", "/").lstrip("./"),
                 f["entity_id"].rsplit("::", 1)[-1])] = f["kind"]
        return out

    kinds = _kinds()
    # A helper nested in a test tree is test code, not production: its caller
    # is a test, so liveness is test-only even though the caller is live.
    assert kinds.get(("tests/visual/corner_harness.py", "build_harness")) == "test-only"
    # A conftest hook is a test root the runner discovers by name.
    assert kinds.get(("tests/conftest.py", "pytest_configure")) == "test-only"
    # A public helper in app.py still counts as production surface.
    assert ("app.py", "run") not in kinds


def test_qt_slot_pack(tmp_path):
    (tmp_path / "w.py").write_text(
        "from PySide6.QtWidgets import QWidget\n"
        "from PySide6.QtCore import Slot\n\n\n"
        "class W(QWidget):\n"
        "    @Slot()\n"
        "    def reload(self):\n"
        "        return 1\n\n"
        "    def _plain_dead(self):\n"
        "        return 1\n",
        encoding="utf-8",
    )
    analyze(str(tmp_path))
    found = {f["entity_id"].rsplit("::", 1)[-1] for f in find_dead_code(0.0, False, 1000)}
    assert "W.reload" not in found
    # Public methods of a framework subclass are dispatch candidates (§2.3);
    # a PRIVATE one is never dispatched and stays a finding.
    assert "W._plain_dead" in found


def test_pyproject_entry_points_are_roots(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[project.scripts]\nmycli = \"myapp.cli:main\"\n", encoding="utf-8"
    )
    (tmp_path / "myapp").mkdir()
    (tmp_path / "myapp" / "cli.py").write_text(
        "def main():\n    return 1\n\n\ndef _aux():\n    return 2\n", encoding="utf-8"
    )
    analyze(str(tmp_path))
    found = {f["entity_id"].rsplit("::", 1)[-1] for f in find_dead_code(0.0, False, 1000)}
    assert "main" not in found
    assert "_aux" in found


def test_all_names_are_public_api(tmp_path):
    (tmp_path / "mymod.py").write_text(
        '__all__ = ["api"]\n\n\ndef api():\n    return 1\n\n\ndef _secret():\n    return 2\n',
        encoding="utf-8",
    )
    (tmp_path / "user.py").write_text("from mymod import api\n", encoding="utf-8")
    # The `__all__` pack reads star exports, which the Python analyze wrapper
    # applies; the bare Rust analyze leaves them unset.
    from coderadar import analyze as analyze_py

    analyze_py(str(tmp_path))
    found = {f["entity_id"].rsplit("::", 1)[-1] for f in find_dead_code(0.0, False, 1000)}
    # `user.py` imports mymod, so step 4 (never-imported modules) does not
    # protect it; the `__all__` pack does.
    assert "api" not in found
    assert "_secret" in found
