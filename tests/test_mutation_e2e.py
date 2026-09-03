"""Plan → apply → reindex → read the file back.

The plan's first cross-cutting test-debt item: `mutation::tests` exercises
`apply()` on synthetic plans, and nothing drove a mutation all the way
through the Python/MCP layer to assert on the bytes that ended up on disk.
Four write-path bugs lived in exactly that gap — spans computed against a
stale index, a class rename that was unreachable, parameters dropped on
signature update, and a params_span that pointed at the wrong bytes.

These tests go through the MCP backends the agent actually calls, on real
files in a tmp_path, and assert on file contents afterwards. They also
assert the half that is easy to forget: that a dry run changes nothing.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

try:
    import coderadar
    from coderadar._core import analyze as _analyze
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


SOURCE = '''\
def greet(name):
    return "hello " + name


def shout(name):
    return greet(name).upper()


class Greeter:
    def hello(self, name):
        return greet(name)
'''


@pytest.fixture
def project(tmp_path):
    """A tiny indexed project, with the cwd on it.

    The graph prefixes entity ids with the path it walked, and every read
    helper resolves against the cwd, so the two have to agree — which is the
    same reason the MCP server chdirs onto its resolved root.
    """
    (tmp_path / "app.py").write_text(SOURCE, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


def _entity_id(name: str) -> str:
    from coderadar._core import search_entities

    for hit in search_entities(name, 50):
        if hit.get("name") == name:
            return hit["id"]
    raise AssertionError(f"{name} is not in the index")


def _app(project: Path) -> str:
    return (project / "app.py").read_text(encoding="utf-8")


class TestReplaceBody:
    def test_a_dry_run_writes_nothing_and_shows_a_diff(self, project):
        from coderadar.mcp.server import _replace_body

        before = _app(project)
        out = _replace_body(
            coderadar.CodeGraph(), _entity_id("greet"),
            'return "HELLO " + name', None, True)

        assert _app(project) == before, "a dry run touched the file"
        # The preview is a real unified diff, not a positional line pairing.
        assert "--- a/" in out and "+++ b/" in out
        assert "dry_run=False" in out

    def test_applying_it_rewrites_the_body_and_leaves_the_rest(self, project):
        from coderadar.mcp.server import _replace_body

        out = _replace_body(
            coderadar.CodeGraph(), _entity_id("greet"),
            'return "HELLO " + name', None, False)

        after = _app(project)
        assert "Mutation failed" not in out, out
        assert '"HELLO " + name' in after
        assert "def shout(name):" in after, "an unrelated function was damaged"
        assert "class Greeter:" in after


class TestUpdateSignature:
    def test_the_new_signature_lands_and_keeps_its_parameters(self, project):
        from coderadar.mcp.server import _update_signature

        out = _update_signature(
            coderadar.CodeGraph(), _entity_id("greet"),
            "def greet(name, punctuation):", False, False)

        after = _app(project)
        assert "Mutation failed" not in out, out
        assert "def greet(name, punctuation):" in after
        # The parameters used to be dropped on the way through the span math.
        assert "def greet():" not in after


class TestRename:
    def test_a_function_rename_reaches_its_call_sites(self, project):
        from coderadar.mcp.server import _rename

        out = _rename(coderadar.CodeGraph(), _entity_id("greet"), "salute", False)

        after = _app(project)
        assert "Mutation failed" not in out, out
        assert "def salute(name):" in after
        assert "return salute(name).upper()" in after, "the call site was missed"
        assert "def greet(" not in after

    def test_a_class_rename_is_reachable(self, project):
        # Class rename used to be routed nowhere and silently do nothing.
        from coderadar.mcp.server import _rename

        out = _rename(coderadar.CodeGraph(), _entity_id("Greeter"), "Welcomer", False)

        after = _app(project)
        assert "Mutation failed" not in out, out
        assert "class Welcomer:" in after
        assert "class Greeter:" not in after


class TestTheGraphFollowsTheFile:
    def test_reindex_sees_the_renamed_entity(self, project):
        from coderadar._core import search_entities
        from coderadar.mcp.server import _reindex, _rename

        _rename(coderadar.CodeGraph(), _entity_id("greet"), "salute", False)
        _reindex(coderadar.CodeGraph())

        names = {h.get("name") for h in search_entities("salute", 50)}
        assert "salute" in names
        assert _entity_id("salute")

    def test_update_file_sees_a_replaced_body(self, project):
        from coderadar.mcp.server import _replace_body, _update_file

        _replace_body(
            coderadar.CodeGraph(), _entity_id("greet"),
            'return "HELLO " + name', None, False)
        out = _update_file(coderadar.CodeGraph(), "app.py", None)

        assert "not available" not in out, out
        # The file on disk and the graph now agree, which is the whole point
        # of the mutation pipeline over a plain edit.
        assert '"HELLO " + name' in _app(project)


class TestCreateEntity:
    def test_a_new_function_is_appended_and_indexed(self, project):
        from coderadar.mcp.server import _create_entity, _reindex
        from coderadar._core import search_entities

        out = _create_entity(
            coderadar.CodeGraph(), "app.py", "python", "function",
            "farewell", 'return "bye " + name', None, "end",
            signature=None, dry_run=False)

        after = _app(project)
        assert "Mutation failed" not in out, out
        assert "def farewell" in after
        # Everything that was there before is still there.
        assert "def greet(name):" in after
        assert "class Greeter:" in after

        _reindex(coderadar.CodeGraph())
        assert "farewell" in {h.get("name") for h in search_entities("farewell", 50)}

    def test_a_dry_run_creates_nothing(self, project):
        from coderadar.mcp.server import _create_entity

        before = _app(project)
        _create_entity(
            coderadar.CodeGraph(), "app.py", "python", "function",
            "farewell", 'return "bye"', None, "end",
            signature=None, dry_run=True)

        assert _app(project) == before


class TestTextualCallSiteBackstop:
    """P2-5: a call the cascade cannot resolve (here: module-level, no
    enclosing function) must surface as an unverified textual site, not
    break silently after the rename."""

    def _with_module_level_call(self, project: Path) -> None:
        app = project / "app.py"
        app.write_text(_app(project) + "\n\nshout(\"x\")\n", encoding="utf-8")
        _analyze(".")

    def test_dry_run_reports_the_unresolved_call(self, project):
        from coderadar.mcp.server import _rename

        self._with_module_level_call(project)
        out = _rename(coderadar.CodeGraph(), _entity_id("shout"), "shout2", True)

        # The module-level call is reported, with its textual reason.
        assert 'shout("x")' in out, out
        assert "Textual occurrence" in out, out
        # The definition line (which also matches `shout(`) is covered and
        # stays out of the unverified list.
        assert "def shout" not in out.split("Textual occurrence")[1]
        # Dry run changed nothing.
        assert 'shout("x")' in _app(project)
        assert "def shout(name):" in _app(project)

    def test_apply_leaves_the_unresolved_call_for_the_agent(self, project):
        from coderadar.mcp.server import _rename

        self._with_module_level_call(project)
        out = _rename(coderadar.CodeGraph(), _entity_id("shout"), "shout2", False)

        after = _app(project)
        assert "Mutation failed" not in out, out
        assert "def shout2(name):" in after, "definition renamed"
        assert 'shout("x")' in after, (
            "the unresolvable call site is left for manual review, not "
            "guessed at or deleted"
        )
        assert "Textual occurrence" in out, out
