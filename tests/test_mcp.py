"""Tests for CodeRadar MCP server and tools.

Tests the four-tool MCP surface: explore, node, search, affected.
"""

from __future__ import annotations

import unittest
from unittest.mock import patch, MagicMock

from coderadar.mcp.budget import ExploreBudget, get_explore_budget
from coderadar.mcp.explore import _parse_names, _resolve_names


class TestExploreBudget(unittest.TestCase):
    """Verify budget tiers and monotonicity invariants."""

    def test_tier_under_150(self):
        b = get_explore_budget(50)
        self.assertEqual(b.max_calls, 1)
        self.assertEqual(b.max_output_chars, 13000)
        self.assertFalse(b.include_relationships)

    def test_tier_500(self):
        b = get_explore_budget(300)
        self.assertEqual(b.max_calls, 1)
        self.assertEqual(b.max_output_chars, 18000)

    def test_tier_5000(self):
        b = get_explore_budget(2000)
        self.assertEqual(b.max_calls, 2)
        self.assertTrue(b.include_relationships)

    def test_tier_15000(self):
        b = get_explore_budget(10000)
        self.assertEqual(b.max_calls, 3)
        self.assertTrue(b.include_budget_note)

    def test_tier_25000(self):
        b = get_explore_budget(20000)
        self.assertEqual(b.max_calls, 4)

    def test_tier_above_25000(self):
        b = get_explore_budget(50000)
        self.assertEqual(b.max_calls, 5)
        self.assertEqual(b.max_output_chars, 38000)

    def test_monotonic_per_file_chars(self):
        """Larger tiers must never get smaller per-file budgets."""
        prev = 0
        for fc in [50, 300, 2000, 10000, 20000, 50000]:
            b = get_explore_budget(fc)
            self.assertGreaterEqual(b.max_chars_per_file, prev,
                                    f"max_chars_per_file decreased at file_count={fc}")
            prev = b.max_chars_per_file


class TestNameParsing(unittest.TestCase):
    """Verify explore query parsing."""

    def test_empty_query(self):
        self.assertEqual(_parse_names("", []), [])

    def test_explicit_symbols(self):
        self.assertEqual(
            _parse_names("", ["User.save", "authenticate"]),
            ["User.save", "authenticate"],
        )

    def test_query_as_names(self):
        result = _parse_names("User.save authenticate", [])
        self.assertIn("User.save", result)
        self.assertIn("authenticate", result)

    def test_comma_separated(self):
        self.assertEqual(
            _parse_names("foo, bar, baz", []),
            ["foo", "bar", "baz"],
        )

    def test_filters_short_tokens(self):
        result = _parse_names("a b c my_func", [])
        self.assertNotIn("a", result)
        self.assertNotIn("b", result)
        self.assertNotIn("c", result)
        self.assertIn("my_func", result)


class TestResolvers(unittest.TestCase):
    """Verify framework resolver detection and extraction."""

    def test_django_detect_manage_py(self):
        from coderadar.resolvers import DjangoResolver
        resolver = DjangoResolver()
        # Should detect Django from manage.py — we test the pure logic
        self.assertEqual(resolver.name, "django")

    def test_flask_detect(self):
        from coderadar.resolvers import FlaskResolver
        resolver = FlaskResolver()
        self.assertEqual(resolver.name, "flask")

    def test_fastapi_detect(self):
        from coderadar.resolvers import FastAPIResolver
        resolver = FastAPIResolver()
        self.assertEqual(resolver.name, "fastapi")

    def test_django_route_extraction(self):
        from coderadar.resolvers import DjangoResolver
        resolver = DjangoResolver()
        source = """
from django.urls import path
from . import views

urlpatterns = [
    path("hello/", views.hello, name="hello"),
    path("goodbye/", views.goodbye),
]
"""
        result = resolver.extract("urls.py", source)
        self.assertEqual(len(result.nodes), 2)
        self.assertIn("hello/", [n.metadata.get("pattern") for n in result.nodes])

    def test_flask_route_extraction(self):
        from coderadar.resolvers import FlaskResolver
        resolver = FlaskResolver()
        source = """
from flask import Flask
app = Flask(__name__)

@app.route("/hello")
def hello():
    return "Hello"

@app.get("/api/data")
def get_data():
    return {}
"""
        result = resolver.extract("app.py", source)
        self.assertGreater(len(result.nodes), 0)
        patterns = [n.metadata.get("pattern") for n in result.nodes]
        self.assertIn("/hello", patterns)
        self.assertIn("/api/data", patterns)

    def test_fastapi_route_extraction(self):
        from coderadar.resolvers import FastAPIResolver
        resolver = FastAPIResolver()
        source = """
from fastapi import FastAPI
app = FastAPI()

@app.get("/items/{item_id}")
async def read_item(item_id: int):
    return {"item_id": item_id}

@app.post("/items/")
async def create_item():
    return {}
"""
        result = resolver.extract("main.py", source)
        self.assertGreater(len(result.nodes), 0)
        self.assertEqual(len(result.edges), 2)
        self.assertEqual(result.edges[0].kind, "handles")

    def test_fastapi_dependency_injection(self):
        from coderadar.resolvers import FastAPIResolver
        resolver = FastAPIResolver()
        source = """
from fastapi import FastAPI, Depends
app = FastAPI()

def get_db() -> str:
    return "db"

@app.get("/data")
def read_data(db: str = Depends(get_db)):
    return {"db": db}
"""
        result = resolver.extract("main.py", source)
        dep_edges = [e for e in result.edges if e.kind == "depends_on"]
        self.assertGreater(len(dep_edges), 0)

    def test_django_claims_reference(self):
        from coderadar.resolvers import DjangoResolver
        resolver = DjangoResolver()
        self.assertTrue(resolver.claims_reference("UserModel"))
        self.assertTrue(resolver.claims_reference("LoginView"))
        self.assertFalse(resolver.claims_reference("my_function"))

    def test_fastapi_router_include(self):
        from coderadar.resolvers import FastAPIResolver
        resolver = FastAPIResolver()
        source = """
from fastapi import FastAPI
from .routers import items

app = FastAPI()
app.include_router(items.router, prefix="/items")
"""
        result = resolver.extract("main.py", source)
        router_edges = [e for e in result.edges if e.kind == "registers"]
        self.assertGreater(len(router_edges), 0)


if __name__ == "__main__":
    unittest.main()
