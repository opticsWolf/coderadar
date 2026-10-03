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
        from coderadar._core import search_entities
        from coderadar.mcp.server import _create_entity, _reindex

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

    def _with_module_level_call(
        self, project: Path, call: str = 'shout("x")',
    ) -> None:
        app = project / "app.py"
        app.write_text(_app(project) + f"\n\n{call}\n", encoding="utf-8")
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

    def test_signature_update_lists_both_sides(self, project):
        # The spec's Python integration, literally: plan_signature_update on
        # a fixture with direct callers and an unresolvable-context call
        # lists both — the structural edits and the flagged textual site.
        from coderadar.mcp.server import _update_signature

        self._with_module_level_call(project, 'greet("x")')
        out = _update_signature(
            coderadar.CodeGraph(), _entity_id("greet"),
            "def greet(name, punctuation):", False, True)

        assert "Mutation failed" not in out, out
        # Structural side: the plan carries the rewritten definition and
        # the resolved callers as a diff.
        assert "### Diff Preview" in out, out
        assert "def greet(name, punctuation):" in out, out
        # Unresolvable side: the module-level call (line 14) is flagged,
        # not silently dropped.
        assert 'greet("x")' in out, out
        assert "Textual occurrence" in out, out
        assert ":14`" in out, out
        # Dry run changed nothing.
        assert "def greet(name):" in _app(project)

HIERARCHY = '''class Base:
    def save(self, value):
        return value

    def run(self):
        return self.save(1)


class Child(Base):
    def save(self, value):
        return value + 1


def exercise():
    child = Child()
    return child.save(2)
'''


@pytest.fixture
def hierarchy(tmp_path):
    """A two-level override family with a `self.` call site."""
    (tmp_path / "app.py").write_text(HIERARCHY, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


def _method_id(qualified: str) -> str:
    """`Base.save` → its entity id (search_entities matches on the simple name)."""
    from coderadar._core import search_entities

    name = qualified.rsplit(".", 1)[-1]
    for hit in search_entities(name, 50):
        if hit.get("id", "").endswith(f"::{qualified}"):
            return hit["id"]
    raise AssertionError(f"{qualified} is not in the index")


class TestMethodCallRename:
    """§4.1 — a *resolved* attribute call is a real edit, not a review note."""

    def test_self_method_call_is_rewritten(self, hierarchy):
        from coderadar.mcp.server import _rename

        out = _rename(coderadar.CodeGraph(), _method_id("Base.save"), "store", False)

        after = _app(hierarchy)
        assert "Mutation failed" not in out, out
        assert "def store(self, value):" in after
        # The receiver `self.` was resolved, so the plan rewrites the name
        # in place instead of asking for manual review.
        assert "return self.store(1)" in after, after
        assert "child.store(2)" in after, after

    def test_a_resolved_method_call_is_not_an_unverified_site(self, hierarchy):
        graph = coderadar.CodeGraph()
        plan = graph.plan_rename(_method_id("Base.save"), "store", dry_run=True)

        snippets = [site["snippet"] for site in plan.unverified_sites]
        assert not any("self.save" in snippet for snippet in snippets), snippets

        # The edit targets exactly the name inside `self.save(1)`.
        source = (hierarchy / "app.py").read_bytes()
        name_offset = source.index(b"self.save(1)") + len(b"self.")
        target = next(
            (e for e in plan.edits if e.span_start == name_offset), None
        )
        assert target is not None, plan.edits
        assert target.replacement == "store"

        # Plan 5.4: the same span in the form a reviewer reads — the byte
        # offset alone required re-reading the file and counting newlines.
        line_no = source[:name_offset].count(b"\n") + 1
        col = name_offset - (source.rfind(b"\n", 0, name_offset) + 1)
        assert target.line == line_no, (target, line_no)
        assert target.col == col, (target, col)
        assert target.end_line == line_no
        assert target.end_col == col + len("save")
        assert "save" in source[target.span_start:target.span_end].decode()

    def test_renaming_a_base_renames_the_override_family(self, hierarchy):
        from coderadar.mcp.server import _rename

        out = _rename(coderadar.CodeGraph(), _method_id("Base.save"), "store", True)

        assert "Warnings" in out, out
        assert "overridden" in out, out
        assert "Child.save" in out, out

        _rename(coderadar.CodeGraph(), _method_id("Base.save"), "store", False)
        after = _app(hierarchy)
        assert after.count("def store(self, value):") == 2, after
        assert "def save(" not in after, after

    def test_renaming_an_override_warns_that_the_base_is_untouched(self, hierarchy):
        from coderadar.mcp.server import _rename

        out = _rename(coderadar.CodeGraph(), _method_id("Child.save"), "store", True)

        assert "overrides" in out, out
        assert "NOT renamed" in out, out

        _rename(coderadar.CodeGraph(), _method_id("Child.save"), "store", False)
        after = _app(hierarchy)
        assert "def store(self, value):" in after
        assert "def save(self, value):" in after, "the base method must survive"


FIXTURE_APP = """class Manager:
    def save_state(self):
        return 1
"""

FIXTURE_CONFTEST = """import pytest

from app import Manager


@pytest.fixture
def make_manager():
    return Manager()
"""

FIXTURE_TEST = """def test_save(make_manager):
    make_manager.save_state()
"""


@pytest.fixture
def fixture_project(tmp_path):
    """A pytest fixture-typed receiver — the weakest evidence there is."""
    (tmp_path / "app.py").write_text(FIXTURE_APP, encoding="utf-8")
    tests_dir = tmp_path / "tests"
    tests_dir.mkdir()
    (tests_dir / "conftest.py").write_text(FIXTURE_CONFTEST, encoding="utf-8")
    (tests_dir / "test_weak.py").write_text(FIXTURE_TEST, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


class TestWeakEvidenceCalls:
    """§4.1 — fixture-inferred receivers go to review, not into the file."""

    def test_a_fixture_typed_call_is_reported_for_review(self, fixture_project):
        graph = coderadar.CodeGraph()
        plan = graph.plan_rename(_method_id("Manager.save_state"), "store", dry_run=True)

        sites = {site["snippet"]: site["reason"] for site in plan.unverified_sites}
        assert "make_manager.save_state" in sites, sites
        assert "fixture" in sites["make_manager.save_state"], sites

        # The definition is still renamed — only the guessed call waits.
        assert any(edit.replacement == "store" for edit in plan.edits), plan.edits

    def test_applying_leaves_the_fixture_call_untouched(self, fixture_project):
        from coderadar.mcp.server import _rename

        out = _rename(
            coderadar.CodeGraph(), _method_id("Manager.save_state"), "store", False
        )

        assert "Mutation failed" not in out, out
        app = (fixture_project / "app.py").read_text(encoding="utf-8")
        test_file = (fixture_project / "tests" / "test_weak.py").read_text(encoding="utf-8")
        assert "def store(self):" in app
        assert "make_manager.save_state()" in test_file, "a guess must not be applied"
