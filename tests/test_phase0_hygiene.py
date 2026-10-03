"""v0.10 Phase 0 — hygiene fixes that must not regress.

0.1  every `_core` function carries its own, non-empty docstring
0.2  `clear_embeddings_for_file` takes the GIL token instead of assuming it
0.3  `traverse_unresolved` accepts `edge_kinds=None` like `traverse`
0.4  `update_file` reports what it changed instead of constant placeholders
"""

from __future__ import annotations

import collections
import inspect
import os
import textwrap

import coderadar
import pytest

try:
    from coderadar import _core
except ImportError:  # pragma: no cover
    _core = None

pytestmark = pytest.mark.skipif(_core is None, reason="Rust _core extension not built")


def _public_functions():
    return [(name, fn) for name, fn in vars(_core).items()
            if not name.startswith("_") and inspect.isbuiltin(fn)]


# -- 0.1 docstrings ----------------------------------------------------------

def test_every_core_function_has_a_docstring():
    missing = [n for n, fn in _public_functions() if not (fn.__doc__ or "").strip()]
    assert not missing, f"undocumented _core functions: {missing}"


def test_no_two_core_functions_share_a_docstring():
    """A copy-pasted docstring is how `clear_embeddings_for_file` ended up
    describing `set_module_star_exports`."""
    seen = collections.defaultdict(list)
    for name, fn in _public_functions():
        seen[(fn.__doc__ or "").strip()].append(name)
    dupes = [names for doc, names in seen.items() if doc and len(names) > 1]
    assert not dupes, f"functions with identical docstrings: {dupes}"


@pytest.mark.parametrize("name, needle", [
    ("clear_embeddings_for_file", "embedding"),
    ("set_module_star_exports", "__all__"),
    ("unresolved_targets", "call-target"),
    ("traverse_unresolved", "edge_kinds"),
])
def test_docstring_describes_its_own_function(name, needle):
    assert needle in getattr(_core, name).__doc__


# -- a throwaway project -----------------------------------------------------

def _read(path):
    # Bytes, not read_text(): on Windows text mode turns CRLF into LF, and the
    # signature hash is byte-exact, so an LF buffer would diff against a CRLF file.
    return path.read_bytes().decode("utf-8")


def _write(path, text):
    path.write_bytes(text.encode("utf-8"))


@pytest.fixture
def project(tmp_path, monkeypatch):
    _write(tmp_path / "mod.py", textwrap.dedent('''\
        def helper():
            return 1


        def caller():
            return helper()
    '''))
    monkeypatch.chdir(tmp_path)
    graph = coderadar.analyze(str(tmp_path))
    return graph, tmp_path / "mod.py"


def _fid(name):
    # Canonical ids: root-relative, forward slashes, no dot prefix (plan 5.1).
    return "mod.py::" + name


# -- 0.2 / 0.3 ---------------------------------------------------------------

def test_clear_embeddings_for_file_runs(project):
    result = _core.clear_embeddings_for_file("mod.py")
    assert result == {"ok": True}


def test_traverse_unresolved_defaults_to_every_edge_kind(project):
    start = _fid("caller")
    assert _core.traverse_unresolved(start, 2) == _core.traverse_unresolved(start, 2, [], "out")
    assert _core.traverse_unresolved(start, 2, None, "out") == \
        _core.traverse_unresolved(start, 2, [], "out")


# -- 0.4 update report -------------------------------------------------------

def _update(graph, path, text):
    return graph.update_file(str(path), content=text)


def test_update_with_no_change_reports_no_changes(project):
    graph, path = project
    report = _update(graph, path, _read(path))
    assert report.changed_symbols == []
    assert report.new_unresolved_references == []
    assert report.newly_resolved_references == []


def test_added_function_is_reported(project):
    graph, path = project
    text = _read(path) + "\n\ndef fresh():\n    return helper()\n"
    report = _update(graph, path, text)
    added = [(s.kind, s.operation, s.qualified_name) for s in report.changed_symbols]
    assert ("function", "added", _fid("fresh")) in added
    assert all(op == "added" for _, op, _ in added), added
    assert all(s.file == "mod.py" for s in report.changed_symbols)


def test_removed_function_is_reported(project):
    graph, path = project
    report = _update(graph, path, "def helper():\n    return 1\n")
    removed = [s for s in report.changed_symbols if s.operation == "removed"]
    assert [s.qualified_name for s in removed] == [_fid("caller")]


def test_signature_and_body_changes_are_told_apart(project):
    graph, path = project
    text = _read(path)
    sig = _update(graph, path, text.replace("def helper():", "def helper(x=0):"))
    assert {(s.qualified_name, s.operation) for s in sig.changed_symbols} == \
        {(_fid("helper"), "signature_changed")}
    body = _update(graph, path, text.replace("return 1", "return 2")
                   .replace("def helper():", "def helper(x=0):"))
    assert {(s.qualified_name, s.operation) for s in body.changed_symbols} == \
        {(_fid("helper"), "body_changed")}


def test_a_neighbouring_file_is_not_claimed(project, tmp_path):
    """`mod.py` must not pick up the entities of `mod.pyi`'s siblings."""
    graph, path = project
    _write(tmp_path / "mod.pyi", "def other(): ...\n")
    graph.update_file(str(tmp_path / "mod.pyi"))
    report = _update(graph, path, _read(path))
    assert report.changed_symbols == []


def test_unresolved_targets_are_diffed(project):
    graph, path = project
    text = _read(path)
    added = _update(graph, path, text + "\n\ndef uses_unknown():\n    return mystery_call()\n")
    new = [(r["entity_id"], r["target"]) for r in added.new_unresolved_references]
    assert (_fid("uses_unknown"), "mystery_call") in new
    assert added.newly_resolved_references == []

    gone = _update(graph, path, text)
    resolved = [(r["entity_id"], r["target"]) for r in gone.newly_resolved_references]
    assert (_fid("uses_unknown"), "mystery_call") in resolved


def test_epoch_advances_with_each_update(project):
    graph, path = project
    text = _read(path)
    first = _update(graph, path, text + "\n# one\n")
    second = _update(graph, path, text + "\n# two\n")
    assert first.epoch_after > first.epoch_before
    assert second.epoch_before == first.epoch_after
    assert second.epoch_after > second.epoch_before


def test_failed_update_does_not_claim_an_epoch(project):
    graph, _ = project
    report = graph.update_file(os.path.join("no", "such", "file.xyz"))
    assert report.fully_applied is False
    assert (report.epoch_before, report.epoch_after) == (0, 0)
