"""Query language end-to-end — v0.10 precision plan §3.1–§3.5.

Covers what the Rust unit tests cannot: the query runs against a real index
built by `analyze()`, rows carry identity, the documented field list is the
one the parser accepts, and the documented examples in
`docs/query-language.md` actually execute.
"""

from __future__ import annotations

import re
from pathlib import Path

import coderadar
import pytest

pytestmark = pytest.mark.usefixtures("indexed_repo")

DOCS_PATH = Path(__file__).resolve().parent.parent / "docs" / "query-language.md"

APP_PY = '''\
"""Small app used by the query-language tests."""
import os
from collections import OrderedDict

VERSION = "1.2.3"


class Base:
    def handle(self) -> None:
        pass


class Widget(Base):
    """A widget."""

    count: int = 0

    def handle(self) -> None:
        self.helper()

    def helper(self) -> None:
        pass

    @property
    def size(self) -> int:
        return 1

    async def refresh(self) -> None:
        pass


def build() -> Widget:
    return Widget()


def unused_helper() -> int:
    return 1
'''

BIG_PY = "def big() -> int:\n" + "".join(f"    x{i} = {i}\n" for i in range(60)) + "    return 0\n"

TESTS_PY = """\
from app import Widget


def test_build():
    assert Widget().size == 1
"""


@pytest.fixture(scope="module")
def indexed_repo(tmp_path_factory):
    """One indexed repo shared by the module: `analyze` is the expensive part."""
    root = tmp_path_factory.mktemp("query_language")
    (root / "app.py").write_text(APP_PY, encoding="utf-8")
    (root / "big.py").write_text(BIG_PY, encoding="utf-8")
    tests_dir = root / "tests"
    tests_dir.mkdir()
    (tests_dir / "test_app.py").write_text(TESTS_PY, encoding="utf-8")
    coderadar.analyze(str(root))
    return root


@pytest.fixture
def graph():
    return coderadar.CodeGraph()


def ids(rows):
    return {r["id"] for r in rows}


# ── §3.3 Row identity ──────────────────────────────────────────────────────

IDENTITY = ("id", "file_path", "kind", "parent_id")


@pytest.mark.parametrize(
    "entity",
    ["modules", "classes", "functions", "methods", "constants", "entities", "imports", "calls", "fields"],
)
def test_every_kind_carries_identity_fields(graph, entity):
    rows = list(graph.query(f"{entity} limit 5"))
    assert rows, f"fixture should have at least one {entity} row"
    for row in rows:
        missing = [key for key in IDENTITY if key not in row]
        assert not missing, f"{entity} row lacks {missing}: {row}"


def test_identity_survives_a_narrow_select(graph):
    rows = list(graph.query("functions select name where name == 'build'"))
    assert rows and set(rows[0]) == {"name", *IDENTITY}, rows


@pytest.mark.parametrize(
    "predicate",
    [
        "file_path == 'app.py'",
        "file_path contains 'app'",
        "id contains 'app.py::' and kind == 'function'",
        "parent_id != null",
    ],
)
def test_identity_fields_are_filterable(graph, predicate):
    """Identity fields are row data, not decoration: a predicate on them must
    match (they used to be attached after the WHERE probe ran, so
    `where file_path == …` silently returned nothing)."""
    rows = list(graph.query(f"functions where {predicate}"))
    assert rows, predicate
    for row in rows:
        assert "app" in row["file_path"], row


def test_identity_fields_are_groupable_and_orderable(graph):
    rows = list(
        graph.query("functions select file_path, count(*) as n group by file_path")
    )
    assert rows
    assert {r["file_path"] for r in rows} >= {"app.py"}
    ordered = list(graph.query("functions order by file_path desc"))
    paths = [r["file_path"] for r in ordered]
    assert paths == sorted(paths, reverse=True), paths


def test_identity_kind_is_the_row_kind(graph):
    """`kind` on a row is the identity kind — the same label `methods` and
    `functions` select on, not a second private enum."""
    kinds = {r["kind"] for r in graph.query("functions")}
    assert kinds <= {"function", "method", "static", "classmethod", "property"}, kinds
    method_kinds = {r["kind"] for r in graph.query("methods")}
    assert method_kinds, "the fixture has methods"
    assert method_kinds <= kinds, (method_kinds, kinds)
    assert "function" not in method_kinds or method_kinds == {"function"}


def test_row_id_can_be_fed_back_into_the_api(graph):
    """The point of §3.3: a query hit is usable as an entity id."""
    rows = list(graph.query("functions where name == 'build'"))
    entity_id = rows[0]["id"]
    assert rows[0]["file_path"].endswith("app.py")
    assert graph.callers(entity_id) is not None
    callees = graph.callees(entity_id)
    assert any("Widget" in c["name"] for c in callees), callees


# ── §3.1 Keyword-prefixed fields ───────────────────────────────────────────

def test_keyword_prefixed_field_parses_and_runs(graph):
    # `inherits_from` starts with the keyword `in` — it used to be a parse error.
    rows = list(graph.query("classes where inherits_from contains 'Base'"))
    assert [r["name"] for r in rows] == ["Widget"], rows


def test_starts_with_and_ends_with(graph):
    assert [r["name"] for r in graph.query("functions where name starts_with 'build'")] == ["build"]
    assert "app.py" in next(iter(graph.query("modules where path ends_with 'app.py'")), {}).get("path", "")
    assert list(graph.query("functions where name ends_with '_helper'"))


# ── §3.4 Coverage: methods, constants, entities, new fields ───────────────

def test_methods_are_only_functions_on_a_class(graph):
    rows = list(graph.query("methods"))
    assert rows
    assert all(r["parent_class"] for r in rows), rows
    assert all(r["kind"] != "function" for r in rows), rows
    # A method row's parent_id is the class it lives on.
    assert all(r["parent_id"] for r in rows), rows


def test_methods_are_a_subset_of_functions(graph):
    """`methods` is a view of `functions`; `functions` still lists methods."""
    method_ids = ids(graph.query("methods"))
    function_ids = ids(graph.query("functions"))
    assert method_ids and method_ids < function_ids, (method_ids, function_ids)
    assert any(r["kind"] == "function" for r in graph.query("functions"))


def test_constants_entity(graph):
    rows = list(graph.query("constants where name == 'VERSION'"))
    assert [r["value"] for r in rows] == ['"1.2.3"'], rows


def test_entities_unions_name_bearing_kinds(graph):
    kinds = {r["kind"] for r in graph.query("entities")}
    assert {"module", "class", "function", "constant"} <= kinds, kinds


def test_function_fields(graph):
    row = next(iter(graph.query("functions where name == 'refresh'")))
    assert row["kind"] == "method"
    assert row["is_async"] is True
    assert row["parent_class"].endswith("Widget")
    assert row["is_override"] is False

    handle = next(iter(graph.query("methods where name == 'handle' and parent_class contains 'Widget'")))
    assert handle["is_override"] is True, handle

    prop = next(iter(graph.query("methods where kind == 'property'")))
    assert prop["name"] == "size"


def test_class_fields(graph):
    widget = next(iter(graph.query("classes where name == 'Widget'")))
    assert "Base" in widget["bases"]
    assert widget["is_abstract"] is False
    assert widget["docstring"] == "A widget."
    assert widget["inherits_from"], "the MRO should name the in-repo base"
    assert any("Base" in entry for entry in widget["inherits_from"]), widget["inherits_from"]


def test_complexity_is_available(graph):
    big = next(iter(graph.query("functions where name == 'big'")))
    assert big["complexity"] >= 1
    assert big["line_count"] > 50


# ── §3.2 Unknown fields are errors ─────────────────────────────────────────

def test_unknown_field_raises_with_the_available_list(graph):
    with pytest.raises(ValueError) as excinfo:
        list(graph.query("functions where bogus_field == 1"))
    message = str(excinfo.value)
    assert "unknown field 'bogus_field'" in message
    assert "available:" in message


def test_typo_in_select_is_not_an_empty_result(graph):
    with pytest.raises(ValueError, match="unknown field 'filepath'"):
        list(graph.query("functions select filepath"))


def test_dotted_paths_are_rejected_not_null(graph):
    # `module.name` used to parse and evaluate to null on functions.
    with pytest.raises(ValueError, match="module.name"):
        list(graph.query("functions where module.name == 'app'"))


# ── §3.5 Errors ────────────────────────────────────────────────────────────

def test_parse_errors_are_human_readable(graph):
    with pytest.raises(ValueError) as excinfo:
        list(graph.query("functions where name"))
    message = str(excinfo.value)
    assert "expected a comparison operator" in message
    assert "column" in message
    assert "ParsingError" not in message


# ── Aggregates ─────────────────────────────────────────────────────────────

def test_count_star_is_computed(graph):
    row = next(iter(graph.query("functions select count(*) as n")))
    total = next(iter(graph.query("functions limit 1000")), None)
    assert row["n"] >= 1
    assert total is not None
    assert row["n"] == len(list(graph.query("functions limit 1000")))


def test_group_by_returns_one_row_per_group(graph):
    rows = list(graph.query("methods select kind, count(*) as n group by kind"))
    assert rows
    assert all(r["n"] >= 1 for r in rows), rows
    assert len(rows) == len({r["kind"] for r in rows}), rows
    # Ordering by an aggregate alias is legal.
    ordered = list(graph.query("methods select kind, count(*) as n group by kind order by n desc"))
    assert [r["n"] for r in ordered] == sorted((r["n"] for r in ordered), reverse=True)


def test_aggregate_rows_are_marked_as_groups(graph):
    row = next(iter(graph.query("functions select count(*) as n")))
    assert row["id"] is None
    assert row["kind"] == "group"


# ── Docs drift ─────────────────────────────────────────────────────────────

def documented_fields() -> dict[str, list[str]]:
    """Parse the generated reference into {entity: [field, ...]}."""
    text = DOCS_PATH.read_text(encoding="utf-8")
    sections: dict[str, list[str]] = {}
    current = None
    for line in text.splitlines():
        if line.startswith("## `") and line.endswith("`"):
            current = line.strip("`# ").strip("`")
            sections[current] = []
        elif current and line.startswith("| `"):
            sections[current].append(line.split("|")[1].strip().strip("`"))
    return sections


def documented_examples() -> list[str]:
    """The fenced example block from the reference."""
    text = DOCS_PATH.read_text(encoding="utf-8")
    block = re.search(r"## Examples\n\n```\n(.*?)```", text, re.DOTALL)
    assert block, "docs/query-language.md has no Examples block"
    return [line for line in block.group(1).splitlines() if line.strip()]


def test_docs_cover_every_entity_kind(graph):
    documented = documented_fields()
    for entity in ["modules", "classes", "functions", "methods", "constants", "entities", "imports", "calls", "fields"]:
        assert entity in documented, f"{entity} missing from docs/query-language.md"
        assert documented[entity], f"{entity} has no documented fields"


# Fields that are absent on some rows by design: an unannotated function has
# no return_type, and an import resolved to a whole module has no
# resolved_target. For these, being *accepted* is the whole check.
CONDITIONAL_FIELDS = {"return_type", "resolved_target"}


def test_every_documented_field_is_selectable(graph):
    """A field in the reference that the executor does not emit is a doc bug."""
    for entity, fields in documented_fields().items():
        for field in fields:
            rows = list(graph.query(f"{entity} select {field} limit 500"))
            assert rows, f"{entity} has no rows to check `{field}` against"
            if field in CONDITIONAL_FIELDS:
                continue
            # `entities` is the union of several kinds, so a field may not
            # exist on every row — but it must exist on the rows that have it.
            assert any(field in row for row in rows), (
                f"`{entity} select {field}` never produced the field"
            )


@pytest.mark.parametrize("example", documented_examples())
def test_documented_examples_execute(graph, example):
    list(graph.query(example))
