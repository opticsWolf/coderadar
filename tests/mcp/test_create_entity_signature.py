"""v0.8 P2-2: create_entity signature override (guide: docs/v0.8-p2-agent-ux-guide.md).

`create_entity` used to render `pub fn {name}() {` for Rust — no parameters,
no return type, hardwired `pub` — so the tool could not express a real
signature (Süvea session report 1, item 5). `signature` now carries the
complete header, written verbatim; the renderer only adds the language's
body delimiter.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from coderadar.mcp import lazy, server as server_mod, startup
from coderadar.mcp.server import _render_entity_code

try:
    from coderadar._core import analyze as _analyze
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


@pytest.fixture(autouse=True)
def _clean_handles(tmp_path):
    lazy.configure(None)
    startup.configure(None)
    previous = Path(os.getcwd())
    yield
    os.chdir(previous)
    lazy.configure(None)
    startup.configure(None)


def test_rust_signature_written_verbatim():
    code = _render_entity_code(
        "rust", "function", "sync_status_text",
        "    Ok(())\n", None,
        "fn sync_status_text(store: &Store) -> Result<(), String>",
    )
    assert code == (
        "fn sync_status_text(store: &Store) -> Result<(), String> {\n"
        "    Ok(())\n"
        "}\n"
    )


def test_python_signature_gets_its_colon_and_indent():
    code = _render_entity_code(
        "python", "function", "save",
        "return name", None,
        "def save(self, name: str) -> None",
    )
    assert code == "def save(self, name: str) -> None:\n    return name\n"


def test_python_signature_with_colon_not_doubled():
    code = _render_entity_code(
        "python", "function", "save",
        "return 1", None,
        "def save(self) -> int:",
    )
    assert code == "def save(self) -> int:\n    return 1\n"


def test_python_signature_empty_body_gets_pass():
    code = _render_entity_code(
        "python", "function", "stub", "", None, "def stub(self) -> None")
    assert "pass" in code
    assert code.startswith("def stub(self) -> None:\n")


def test_rust_without_signature_keeps_legacy_rendering():
    code = _render_entity_code("rust", "function", "f", "x", None)
    assert code == "pub fn f() {\nx\n}\n"


def test_decorators_still_prepend_with_signature():
    code = _render_entity_code(
        "python", "method", "hook", "return 1", ["@app.route('/x')"],
        "def hook(self, req) -> int",
    )
    assert code.startswith("@app.route('/x')\ndef hook(self, req) -> int:\n")


@pytest.fixture
def project(tmp_path):
    (tmp_path / "m.py").write_text("def existing():\n    return 1\n",
                                   encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


def test_create_entity_dry_run_shows_the_exact_header(project):
    import coderadar
    from coderadar.mcp.server import _create_entity
    graph = coderadar.CodeGraph()
    out = _create_entity(
        graph, "m.py", "python", "function", "save",
        "return name", None, "end",
        "def save(self, name: str) -> None:", True,
    )
    assert "DRY RUN" in out
    assert "def save(self, name: str) -> None:" in out
    # And it must not be the legacy parameter-less header.
    assert "def save():" not in out


def test_signature_on_a_class_kind_is_noted_and_ignored(project):
    import coderadar
    from coderadar.mcp.server import _create_entity
    graph = coderadar.CodeGraph()
    out = _create_entity(
        graph, "m.py", "python", "class", "Widget",
        "pass", None, "end", "fn should_not_appear() -> i32", True,
    )
    assert "ignored for kind 'class'" in out
    assert "fn should_not_appear" not in out.split("### Diff Preview")[-1]
    assert "class Widget:" in out
