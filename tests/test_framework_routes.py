"""Framework route persistence end-to-end (plan §1.3, DR-10).

The done clause: `resolve \"/users/:id\"` → `affected` reaches the handler
in a fresh session from the store. "Fresh session" here is
`coderadar.load` over the store file — the in-memory graph is replaced
wholesale, so only ledger-persisted route nodes + edges can answer.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import coderadar
from coderadar import ops

try:
    from coderadar._core import analyze as _analyze  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

APP = '''\
from flask import Flask
app = Flask(__name__)

@app.route("/users/<id>")
def get_user(id):
    """Return one user."""
    return {"id": id}

@app.route("/health")
def health():
    return {"ok": True}
'''

PLAIN = '''\
def combine(a, b):
    """Combine two things."""
    return (a, b)
'''


@pytest.fixture
def flask_project(tmp_path, monkeypatch):
    root = tmp_path / "shop"
    root.mkdir()
    (root / ".coderadar").mkdir()
    (root / "app.py").write_text(APP, encoding="utf-8")
    monkeypatch.chdir(root)
    coderadar.analyze(".", create_store=True)
    return root


@pytest.fixture
def plain_project(tmp_path, monkeypatch):
    root = tmp_path / "plain"
    root.mkdir()
    (root / ".coderadar").mkdir()
    (root / "util.py").write_text(PLAIN, encoding="utf-8")
    monkeypatch.chdir(root)
    coderadar.analyze(".", create_store=True)
    return root


def _fresh_load(root: Path):
    """Replace the graph wholesale from the store (a fresh session)."""
    return coderadar.load(str(root / ".coderadar" / "store" / "coderadar.db"), str(root))


class TestDoneClause:
    def test_resolve_route_finds_handler_from_store(self, flask_project):
        _fresh_load(flask_project)
        # The done-clause spelling (`:id`) finds the Flask spelling (`<id>`).
        result = ops.resolve("/users/:id")
        assert result["mode"] == "route"
        ids = [r["id"] for r in result["results"]]
        assert "app.py::get_user" in ids
        handler = next(r for r in result["results"] if r["id"] == "app.py::get_user")
        assert handler["resolved_by"] == "route-resolution"
        assert handler["route"]["kind"] == "route"

    def test_affected_handler_contains_route_upstream(self, flask_project):
        _fresh_load(flask_project)
        tree = ops.affected("app.py::get_user")
        depth1 = [e["id"] for e in tree["depths"].get(1, [])]
        assert "app.py::flask:route:/users/<id>" in depth1

    def test_route_is_a_first_class_entity(self, flask_project):
        _fresh_load(flask_project)
        route_id = "app.py::flask:route:/users/<id>"
        node = ops.node(route_id)
        assert node["kind"] == "route"
        assert node["handler"] == "app.py::get_user"
        # `affected` on the route itself answers (empty upstream), not errors.
        tree = ops.affected(route_id)
        assert tree["entity"]["id"] == route_id

    def test_mcp_resolve_names_handler_in_fresh_session(self, flask_project):
        from coderadar.mcp.server import _resolve_ref

        _fresh_load(flask_project)
        text = _resolve_ref(None, "/users/:id", 5)
        assert "app.py::get_user" in text


class TestStatsAndLifecycle:
    def test_reindex_stats_count_routes(self, flask_project):
        report = ops.reindex()
        assert report["stats"]["routes"] == 2
        assert report["stats"]["route_edges"] == 2

    def test_update_file_adds_a_route(self, flask_project):
        _fresh_load(flask_project)
        app = flask_project / "app.py"
        app.write_text(
            APP + '\n@app.route("/orders/<id>")\ndef get_order(id):\n    return {}\n',
            encoding="utf-8",
        )
        coderadar.CodeGraph().update_file("app.py")
        assert ops.node("app.py::flask:route:/orders/<id>")["kind"] == "route"
        assert any(r["id"] == "app.py::get_order"
                   for r in ops.resolve("/orders/:id")["results"])
        # ... and it survives another fresh load.
        _fresh_load(flask_project)
        assert ops.node("app.py::flask:route:/orders/<id>")["kind"] == "route"

    def test_update_file_removes_a_route(self, flask_project):
        _fresh_load(flask_project)
        app = flask_project / "app.py"
        app.write_text(
            'from flask import Flask\napp = Flask(__name__)\n\n'
            '@app.route("/users/<id>")\ndef get_user(id):\n    return {}\n',
            encoding="utf-8",
        )
        coderadar.CodeGraph().update_file("app.py")
        # The removed route's concept retired: resolve finds nothing for it.
        with pytest.raises(ops.NotFound):
            ops.node("app.py::flask:route:/health")
        assert ops.resolve("/health")["results"] == []
        # The surviving route is untouched.
        assert any(r["id"] == "app.py::get_user"
                   for r in ops.resolve("/users/:id")["results"])
        _fresh_load(flask_project)
        assert ops.resolve("/health")["results"] == []

    def test_drop_path_retires_routes(self, flask_project):
        _fresh_load(flask_project)
        (flask_project / "app.py").unlink()
        report = coderadar.CodeGraph().update_file("app.py")
        assert report.removed is True
        # The emptied graph answers NoIndex; the route entity itself is gone.
        assert ops.find_entity("app.py::flask:route:/users/<id>") is None
        assert coderadar.CodeGraph().stats()["routes"] == 0
        _fresh_load(flask_project)
        assert ops.find_entity("app.py::flask:route:/users/<id>") is None

    def test_repeated_analyze_is_stable(self, flask_project):
        before = sorted(r["id"] for r in ops.resolve("/users/:id")["results"])
        coderadar.analyze(".", create_store=True)
        after = sorted(r["id"] for r in ops.resolve("/users/:id")["results"])
        assert before == after == ["app.py::get_user"]
        assert ops.reindex()["stats"]["routes"] == 2


class TestHonesty:
    def test_no_framework_project_is_unaffected(self, plain_project):
        assert ops.reindex()["stats"]["routes"] == 0
        assert ops.resolve("/users/:id")["results"] == []

    def test_unknown_path_resolves_empty(self, flask_project):
        _fresh_load(flask_project)
        assert ops.resolve("/no/such/route")["results"] == []

    def test_pattern_spellings(self):
        from coderadar.resolvers.resolution import route_patterns_match

        assert route_patterns_match("/users/:id", "/users/<id>")
        assert route_patterns_match("/users/{id}", "/users/<int:id>")
        assert not route_patterns_match("/users/:id", "/orders/:id")
        assert not route_patterns_match("/users/:id", "/users/:id/posts")
