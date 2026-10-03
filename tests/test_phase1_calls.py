"""v0.10 Phase 1 - call-graph recall, and the guards that keep it honest.

1.1  attribute calls with any receiver are extracted, as path segments
1.2  receivers are typed from constructors, annotations and return types
1.3  constructors of in-repo classes resolve to the class / its __init__
1.4  `Class.method()` never produces a made-up entity id
1.5  `mod.f()` resolves inside the imported module
"""

from __future__ import annotations

import os
import textwrap

import pytest

import coderadar

try:
    from coderadar import _core
except ImportError:  # pragma: no cover
    _core = None

pytestmark = pytest.mark.skipif(
    _core is None or not hasattr(_core, "call_sites"), reason="needs the built _core extension")


def _project(tmp_path, monkeypatch, files):
    for name, text in files.items():
        (tmp_path / name).write_bytes(textwrap.dedent(text).encode("utf-8"))
    monkeypatch.chdir(tmp_path)
    return coderadar.analyze(str(tmp_path))


def _sites(func_id):
    return _core.call_sites(func_id)


def _fid(file, qualname):
    return os.path.join(".", file) + "::" + qualname


def _site(func_id, name, line=None):
    hits = [s for s in _sites(func_id) if s["name"] == name and (line is None or s["line"] == line)]
    assert len(hits) == 1, (name, _sites(func_id))
    return hits[0]


def _target(func_id, name):
    s = _site(func_id, name)
    return s["target"].split("::", 1)[-1] if s["status"] in ("function", "constructor") else s["status"]


TRAP = {
    "m.py": '''\
        class Serializer:
            def serialize(self, obj):
                return str(obj)


        class Manager:
            def __init__(self):
                self.ser = Serializer()

            def serialize(self, obj):
                return self.ser.serialize(obj)

            def other(self):
                return self.ser.serialize(1)
    ''',
}


def test_nested_receiver_is_extracted_as_segments(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch, TRAP)
    site = _site(_fid("m.py", "Manager.serialize"), "serialize")
    assert site["path"] == ["self", "ser"]


def test_deeper_receiver_never_binds_to_the_callers_own_method(tmp_path, monkeypatch):
    """`self.ser.serialize()` inside `Manager.serialize` must reach
    `Serializer.serialize`, not recurse into itself."""
    _project(tmp_path, monkeypatch, TRAP)
    for fn in ("Manager.serialize", "Manager.other"):
        assert _target(_fid("m.py", fn), "serialize") == "Serializer.serialize"


def test_untyped_receiver_stays_unresolved_instead_of_guessing(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch, {"m.py": '''\
        class A:
            def run(self, thing):
                return thing.run()
    '''})
    assert _site(_fid("m.py", "A.run"), "run")["status"] == "unresolved"


def test_call_receiver_is_a_marked_segment(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch, {"m.py": '''\
        def make():
            return 1


        def use():
            return make().real
    ''', "n.py": '''\
        def make():
            return 1


        def use():
            return make().bit_length()
    '''})
    site = _site(_fid("n.py", "use"), "bit_length")
    assert site["path"] == ["<call:make>"]


def test_class_method_call_has_no_synthetic_id(tmp_path, monkeypatch):
    graph = _project(tmp_path, monkeypatch, {"m.py": '''\
        class Manager:
            def add(self):
                return 1


        def use():
            return Manager.add(None)


        def use_unknown():
            return Missing.add(None)
    '''})
    assert _target(_fid("m.py", "use"), "add") == "Manager.add"
    unknown = _site(_fid("m.py", "use_unknown"), "add")
    assert unknown["status"] == "external"
    assert [c["id"] for c in graph.callees_of(_fid("m.py", "use"))] == [_fid("m.py", "Manager.add")]
    assert [c["id"] for c in graph.callees_of(_fid("m.py", "use_unknown"))] == ["external::Missing.add"]


def test_in_repo_constructor_resolves_to_init(tmp_path, monkeypatch):
    graph = _project(tmp_path, monkeypatch, {"m.py": '''\
        class Plain:
            pass


        class WithInit:
            def __init__(self):
                pass


        def build():
            Plain()
            WithInit()
    '''})
    assert _site(_fid("m.py", "build"), "Plain")["target"].endswith("::Plain")
    assert _site(_fid("m.py", "build"), "WithInit")["status"] == "constructor"
    # The edge lands on `__init__` when there is one, on the class otherwise.
    assert {c["id"] for c in graph.callees_of(_fid("m.py", "build"))} == \
        {_fid("m.py", "Plain"), _fid("m.py", "WithInit.__init__")}


def test_local_and_annotated_receivers(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch, {"m.py": '''\
        class Box:
            def open(self):
                return 1


        def local():
            b = Box()
            return b.open()


        def annotated(b: "Box"):
            return b.open()


        def optional(b: Box | None):
            return b.open()


        def ambiguous(flag):
            b = Box() if flag else None
            c = Box()
            c = other()
            return c.open()
    '''})
    for fn in ("local", "annotated", "optional"):
        assert _target(_fid("m.py", fn), "open") == "Box.open", fn
    assert _site(_fid("m.py", "ambiguous"), "open")["status"] == "unresolved"


def test_factory_return_annotation_and_return_constructor(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch, {"m.py": '''\
        class Box:
            def open(self):
                return 1


        def annotated() -> Box:
            return load()


        def constructed():
            return Box()


        def use_a():
            return annotated().open()


        def use_c():
            return constructed().open()
    '''})
    for fn in ("use_a", "use_c"):
        assert _target(_fid("m.py", fn), "open") == "Box.open", fn


def test_module_prefix_resolves_inside_the_module(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch, {
        "helpers.py": '''\
            def util():
                return 1


            class Thing:
                pass
        ''',
        "m.py": '''\
            import helpers


            def use():
                helpers.util()
                return helpers.Thing()
        ''',
    })
    assert _target(_fid("m.py", "use"), "util") == "util"
    assert _site(_fid("m.py", "use"), "Thing")["target"].endswith("::Thing")


def test_attribute_type_comes_from_the_defining_module(tmp_path, monkeypatch):
    """`m.ser.serialize()` where `ser` is bound in another module's `__init__`."""
    _project(tmp_path, monkeypatch, {
        "models.py": '''\
            class Serializer:
                def serialize(self):
                    return 1


            class Manager:
                def __init__(self):
                    self.ser = Serializer()
        ''',
        "m.py": '''\
            from models import Manager


            def use(m: Manager):
                return m.ser.serialize()
        ''',
    })
    assert _target(_fid("m.py", "use"), "serialize") == "Serializer.serialize"
