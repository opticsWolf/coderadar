"""Phase 5 — API ergonomics & identifiers (v0.10 precision plan §5.2–§5.6).

* §5.2 dotted qualified names (`pkg.mod.Class.method`) resolve, and a miss
  comes back with candidates instead of a bare "not found".
* §5.3 every emitted path is root-relative with forward slashes — no Windows
  `\\\\?\\` prefix, no absolute walker paths.
* §5.6 enum-shaped arguments reject unknown values loudly.
"""

from __future__ import annotations

import os
from pathlib import Path

import coderadar
import pytest

try:
    from coderadar._core import analyze as _analyze

    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

PKG_INIT = ""
PKG_MODEL = """\
VERSION = "1.0"


class Widget:
    def render(self):
        return self.helper()

    def helper(self):
        return 1


def build() -> Widget:
    return Widget()
"""


@pytest.fixture
def project(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "__init__.py").write_text(PKG_INIT, encoding="utf-8")
    (pkg / "model.py").write_text(PKG_MODEL, encoding="utf-8")
    (tmp_path / "main.py").write_text(
        "from pkg.model import Widget\n\n\ndef main():\n    return Widget().render()\n",
        encoding="utf-8",
    )
    previous = Path(os.getcwd())
    os.chdir(tmp_path)
    try:
        _analyze(".")
        yield tmp_path
    finally:
        os.chdir(previous)


# ── §5.2 Dotted qualified names ────────────────────────────────────────────

class TestQualifiedNames:
    def test_dotted_name_resolves_to_the_entity(self, project):
        from coderadar._core import lookup_entity

        entity = lookup_entity("pkg.model.Widget.render")
        assert entity is not None, "the traceback spelling must resolve"
        assert entity["name"] == "render"
        assert entity["id"].endswith("::Widget.render")

    def test_dotted_class_name_resolves(self, project):
        from coderadar._core import lookup_entity

        entity = lookup_entity("pkg.model.Widget")
        assert entity is not None
        assert entity["kind"] == "class"

    def test_dotted_function_name_resolves(self, project):
        from coderadar._core import lookup_entity

        entity = lookup_entity("pkg.model.build")
        assert entity is not None and entity["name"] == "build"

    def test_dotted_module_name_resolves_to_the_module(self, project):
        from coderadar._core import lookup_entity

        entity = lookup_entity("pkg.model")
        assert entity is not None and entity["kind"] == "module"

    def test_mixed_module_colon_symbol_form_resolves(self, project):
        from coderadar._core import lookup_entity

        entity = lookup_entity("pkg.model::Widget.render")
        assert entity is not None and entity["name"] == "render"

    def test_graph_find_accepts_the_dotted_form(self, project):
        found = coderadar.CodeGraph().find("pkg.model.Widget.render")
        assert found is not None and found["name"] == "render"

    def test_the_resolved_id_is_the_real_one(self, project):
        graph = coderadar.CodeGraph()
        dotted = graph.find("pkg.model.Widget.render")
        exact = graph.find(dotted["id"])
        assert dotted == exact

    def test_a_miss_offers_candidates(self, project):
        from coderadar.mcp.server import _not_found_message

        message = _not_found_message(coderadar.CodeGraph(), "pkg.model.Widget.rendr")
        assert "not found" in message
        assert "render" in message, message

    def test_a_name_that_exists_nowhere_says_so(self, project):
        from coderadar.mcp.server import _not_found_message

        message = _not_found_message(coderadar.CodeGraph(), "pkg.model.Nothing.zzz")
        assert "not found" in message
        assert "codegraph_search" in message


# ── §5.3 One path spelling ─────────────────────────────────────────────────

class TestPathSpelling:
    def test_emitted_ids_are_portable(self, project):
        """Plan 5.1: one id spelling on every platform — no backslashes, no
        `./` prefix, whatever the OS that wrote the store."""
        from coderadar._core import search_entities

        for kind in ("module", "class", "function"):
            for hit in search_entities("", 50, kind):
                eid = hit["id"]
                assert "\\" not in eid, eid
                assert not eid.startswith("./"), eid
                assert eid.split("::")[0] == hit.get("file_path") or kind == "class"

    def test_query_rows_and_callers_agree_on_the_spelling(self, project):
        graph = coderadar.CodeGraph()
        rows = list(graph.query("functions"))
        assert rows
        for row in rows:
            assert "\\" not in row["id"] and "\\" not in row["file_path"], row
            assert not row["id"].startswith("./"), row
        for caller in graph.callers_of("pkg/model.py::Widget.render"):
            assert "\\" not in caller["id"], caller

    def test_graph_stats_root_has_no_verbatim_prefix(self, project):
        from coderadar._core import graph_stats

        root = graph_stats()["indexed_root"]
        assert root, "the indexed root must be reported"
        # The root is a comparison key (the MCP layer compares it against the
        # cwd), so it keeps native separators — but never the Windows `\\?\`
        # verbatim prefix, which is what leaked into reports.
        assert "\\\\?\\" not in root, root
        assert Path(root).exists(), root

    def test_scaffolding_findings_are_root_relative(self, project):
        from coderadar._core import find_scaffolding

        (project / "pkg" / "model.py").write_text(
            PKG_MODEL + "\n# TODO: finish this\n", encoding="utf-8"
        )
        _analyze(".")
        findings = find_scaffolding(False, 100)
        real = [f for f in findings if f["kind"] != "scan-stats"]
        assert real, findings
        for finding in real:
            assert "\\\\?\\" not in finding["file"], finding
            assert "\\" not in finding["file"], finding
            assert not finding["file"].startswith("/"), finding
            assert not (len(finding["file"]) > 1 and finding["file"][1] == ":"), finding

    def test_a_secret_finding_uses_the_same_spelling(self, project):
        from coderadar._core import find_scaffolding

        # Deliberately short and obviously fake: it matches the scanner's
        # `sk_live_[A-Za-z0-9]{16,}` shape without looking like a real key
        # (GitHub push protection flags realistic-looking ones).
        secret = 'API_KEY = "sk_live_0123456789abcdef"\n'
        (project / "pkg" / "secrets.py").write_text(secret, encoding="utf-8")
        _analyze(".")
        findings = find_scaffolding(True, 100)
        secrets = [f for f in findings if f["kind"] == "secret"]
        assert secrets, findings
        assert secrets[0]["file"] == "pkg/secrets.py", secrets[0]


# ── §5.6 Enum arguments reject unknown values ──────────────────────────────

class TestEnumArguments:
    def test_unknown_search_kind_raises(self, project):
        from coderadar._core import search_entities

        with pytest.raises(ValueError, match="unknown kind"):
            search_entities("widget", 5, "klass")

    def test_unknown_edge_kind_raises(self, project):
        from coderadar._core import traverse

        with pytest.raises(ValueError, match="[Uu]nknown edge kind"):
            traverse("pkg/model.py::Widget.render", 2, ["calls", "nonsense"], "out", None)

    def test_unknown_direction_raises(self, project):
        from coderadar._core import traverse

        with pytest.raises(ValueError, match="direction"):
            traverse("pkg/model.py::Widget.render", 2, [], "sideways", None)
