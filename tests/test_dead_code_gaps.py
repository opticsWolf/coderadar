"""Dead-code gaps found dogfooding on a Qt app: module-scope uses, properties
and template methods, and Python bare-name scoping."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, find_dead_code
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

pytestmark = pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")

FILES = {
    "main.py": '''
from tables import run_tables
from widgets import build
from model import Model


def main():
    run_tables()
    build()
    Model().apply_fix()
''',
    # Module-level code runs at import: tables, class bodies, registrations.
    "tables.py": '''
from somewhere import qInstallMessageHandler, AfterValidator


def _push():
    return 1


def _combo():
    return 2


def _handler(*args):
    return None


def _validate(v):
    return v


def _make():
    return 3


def _class_level():
    return 4


def _really_dead():
    return 5


_BUILDERS = {"push": _push, "combo": _combo}
_DEFAULT = _make()
qInstallMessageHandler(_handler)
Checked = AfterValidator(_validate)


class Holder:
    value = _class_level()


def run_tables():
    return _BUILDERS, _DEFAULT, Holder, Checked
''',
    # Properties are read; a mixin's self.m() reaches subclass definitions;
    # a mixin's public override serves the external base it is combined with.
    "widgets.py": '''
from qt import QWidget


class Tab:
    @property
    def _floatable(self):
        return True

    @property
    def _never_read(self):
        return False


class Behaviour:
    def start(self):
        self._install_filter()

    def closeEvent(self, event):
        return event


class Window(Behaviour, QWidget):
    def _install_filter(self):
        return 1


def build():
    w = Window()
    w.start()
    return Tab()._floatable
''',
    "audit.py": '''
def apply_fix(model):
    return model
''',
    # `apply_fix()` inside `Model.apply_fix` is the imported function.
    "model.py": '''
from audit import apply_fix


class Model:
    def apply_fix(self):
        return apply_fix(self)
''',
}


@pytest.fixture(scope="module")
def dead(tmp_path_factory):
    root = tmp_path_factory.mktemp("gaps")
    for name, text in FILES.items():
        (root / name).write_text(text, encoding="utf-8")
    analyze(str(root))
    return {f["entity_id"] for f in find_dead_code(0.0, False, 1000)}


def _ids(dead, suffix):
    return {i for i in dead if i.endswith(suffix)}


@pytest.mark.parametrize(
    "name", ["_push", "_combo", "_handler", "_validate", "_make", "_class_level"]
)
def test_module_scope_uses_are_live(dead, name):
    assert not _ids(dead, f"tables.py::{name}")


def test_unused_module_function_is_still_dead(dead):
    assert _ids(dead, "tables.py::_really_dead")


def test_read_property_is_live_and_unread_one_is_dead(dead):
    assert not _ids(dead, "Tab._floatable")
    assert _ids(dead, "Tab._never_read")


def test_template_method_reaches_subclass(dead):
    assert not _ids(dead, "Window._install_filter")


def test_mixin_override_of_external_base_is_live(dead):
    assert not _ids(dead, "Behaviour.closeEvent")


def test_bare_call_in_method_resolves_to_import_not_itself(dead):
    assert not _ids(dead, "audit.py::apply_fix")
