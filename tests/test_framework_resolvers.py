"""Tests for Framework Resolvers (§28)

Validates that Django, Flask, and FastAPI resolvers correctly
extract route nodes, handler edges, and framework-specific patterns
from Python source code.
"""

from pathlib import Path
from coderadar.resolvers.django import DjangoResolver
from coderadar.resolvers.flask import FlaskResolver
from coderadar.resolvers.fastapi import FastAPIResolver


class TestDjangoResolver:
    """Django URL routing and admin registration extraction."""

    def test_detects_manage_py(self, tmp_path):
        resolver = DjangoResolver()
        (tmp_path / "manage.py").write_text("# Django project")
        assert resolver.detect(tmp_path)

    def test_no_detect_without_manage_py(self, tmp_path):
        resolver = DjangoResolver()
        assert not resolver.detect(tmp_path)

    def test_extracts_path_routes(self):
        resolver = DjangoResolver()
        source = """\
from django.urls import path
from . import views

urlpatterns = [
    path('home/', views.index, name='index'),
    path('users/<int:id>/', views.user_detail, name='user_detail'),
]
"""
        result = resolver.extract("urls.py", source)
        assert len(result.nodes) >= 2
        pattern_names = [n.metadata.get("pattern") for n in result.nodes]
        assert "" in pattern_names or "/" in str(pattern_names)
        assert len(result.edges) >= 2

    def test_extracts_re_path_routes(self):
        resolver = DjangoResolver()
        source = """\
from django.urls import re_path
from . import views

urlpatterns = [
    re_path(r'^articles/(?P<year>[0-9]{4})/$', views.year_archive),
]
"""
        result = resolver.extract("urls.py", source)
        assert len(result.nodes) >= 1
        assert len(result.edges) >= 1

    def test_extracts_admin_register(self):
        resolver = DjangoResolver()
        source = """\
from django.contrib import admin
from .models import Book

admin.site.register(Book)
"""
        result = resolver.extract("admin.py", source)
        assert len(result.edges) >= 1
        edge = result.edges[0]
        assert edge.kind == "registers"

    def test_claims_model_view_form_suffixes(self):
        resolver = DjangoResolver()
        assert resolver.claims_reference("myapp.models.BookModel")
        assert resolver.claims_reference("myapp.views.BookView")
        assert resolver.claims_reference("myapp.forms.BookForm")
        assert not resolver.claims_reference("myapp.utils.helper")


class TestFlaskResolver:
    """Flask route decorator and blueprint extraction."""

    def test_detects_flask_import(self, tmp_path):
        resolver = FlaskResolver()
        (tmp_path / "app.py").write_text("from flask import Flask\napp = Flask(__name__)")
        assert resolver.detect(tmp_path)

    def test_no_detect_without_flask(self, tmp_path):
        resolver = FlaskResolver()
        (tmp_path / "main.py").write_text("print('hello')")
        assert not resolver.detect(tmp_path)

    def test_extracts_route_decorators(self):
        resolver = FlaskResolver()
        source = """\
from flask import Flask
app = Flask(__name__)

@app.route('/')
def index():
    return 'Hello'

@app.route('/users/<int:id>', methods=['GET', 'POST'])
def user_detail(id):
    return f'User {id}'
"""
        result = resolver.extract("app.py", source)
        assert len(result.nodes) >= 2
        assert len(result.edges) >= 2
        patterns = [n.metadata.get("pattern") for n in result.nodes]
        assert "/" in patterns or any("users" in (p or "") for p in patterns)

    def test_extracts_method_decorators(self):
        resolver = FlaskResolver()
        source = """\
from flask import Flask
app = Flask(__name__)

@app.get('/api/items')
def get_items():
    return []

@app.post('/api/items')
def create_item():
    return {}, 201
"""
        result = resolver.extract("app.py", source)
        assert len(result.nodes) >= 2
        for n in result.nodes:
            methods = n.metadata.get("methods", [])
            assert methods  # should have at least one HTTP method

    def test_extracts_blueprint_registration(self):
        resolver = FlaskResolver()
        source = """\
from flask import Flask, Blueprint
bp = Blueprint('api', __name__)
app = Flask(__name__)
app.register_blueprint(bp)
"""
        result = resolver.extract("app.py", source)
        # Blueprint registration creates edges, not nodes
        assert len(result.edges) >= 1

    def test_claims_flask_references(self):
        resolver = FlaskResolver()
        # Flask claims_reference checks for blueprint, route, flask, current_app, g
        assert resolver.claims_reference("myapp.blueprint")
        assert resolver.claims_reference("flask.current_app")
        # "g" keyword is over-broad but matches flask.g pattern
        assert resolver.claims_reference("flask.g")


class TestFastAPIResolver:
    """FastAPI route decorator, dependency injection, and router extraction."""

    def test_detects_fastapi_import(self, tmp_path):
        resolver = FastAPIResolver()
        (tmp_path / "main.py").write_text(
            "from fastapi import FastAPI\napp = FastAPI()")
        assert resolver.detect(tmp_path)

    def test_no_detect_without_fastapi(self, tmp_path):
        resolver = FastAPIResolver()
        assert not resolver.detect(tmp_path)

    def test_extracts_route_decorators(self):
        resolver = FastAPIResolver()
        source = """\
from fastapi import FastAPI
app = FastAPI()

@app.get("/items")
def list_items():
    return []

@app.post("/items")
def create_item():
    return {}
"""
        result = resolver.extract("main.py", source)
        assert len(result.nodes) >= 2
        assert len(result.edges) >= 2
        for e in result.edges:
            assert e.kind == "handles"

    def test_extracts_async_routes(self):
        resolver = FastAPIResolver()
        source = """\
from fastapi import FastAPI
app = FastAPI()

@app.get("/async-items")
async def list_items():
    return []
"""
        result = resolver.extract("main.py", source)
        assert len(result.nodes) >= 1
        assert len(result.edges) >= 1

    def test_extracts_dependencies(self):
        resolver = FastAPIResolver()
        source = """\
from fastapi import FastAPI, Depends
app = FastAPI()

def get_db():
    return {'db': 'connection'}

@app.get("/users")
def list_users(db=Depends(get_db)):
    return db
"""
        result = resolver.extract("main.py", source)
        # Should have route edges + dependency edges
        dep_edges = [e for e in result.edges if e.kind == "depends_on"]
        assert len(dep_edges) >= 1

    def test_extracts_router_include(self):
        resolver = FastAPIResolver()
        source = """\
from fastapi import FastAPI, APIRouter
app = FastAPI()
router = APIRouter()
app.include_router(router)
"""
        result = resolver.extract("main.py", source)
        router_edges = [e for e in result.edges if e.kind == "registers"]
        assert len(router_edges) >= 1

    def test_claims_fastapi_references(self):
        resolver = FastAPIResolver()
        assert resolver.claims_reference("fastapi.FastAPI")
        assert resolver.claims_reference("fastapi.Depends")
        assert not resolver.claims_reference("django.urls.path")

    # ── v3.6: Flask-RESTful and DRF patterns ───────────────────────

    def test_flask_add_resource(self):
        resolver = FlaskResolver()
        source = """\
from flask import Flask
from flask_restful import Api, Resource
app = Flask(__name__)
api = Api(app)
class HelloWorld(Resource):
    def get(self):
        return {'hello': 'world'}
api.add_resource(HelloWorld, '/', '/hello')
"""
        result = resolver.extract("app.py", source)
        # add_resource creates handler edges
        resource_edges = [e for e in result.edges
                          if e.metadata.get("resource")]
        assert len(resource_edges) >= 2  # two paths: / and /hello

    def test_django_drf_router_register(self):
        resolver = DjangoResolver()
        source = """\
from rest_framework.routers import DefaultRouter
from .views import UserViewSet, PostViewSet
router = DefaultRouter()
router.register(r'users', UserViewSet)
router.register(r'posts', PostViewSet, basename='post')
"""
        result = resolver.extract("urls.py", source)
        router_edges = [e for e in result.edges
                        if e.metadata.get("viewset")]
        assert len(router_edges) >= 2

    def test_django_as_view_stripping(self):
        resolver = DjangoResolver()
        source = """\
from django.urls import path
from . import views

urlpatterns = [
    path('users/', views.UserView.as_view(), name='user_list'),
    path('users/<int:pk>/', views.UserView.as_view(), name='user_detail'),
]
"""
        result = resolver.extract("urls.py", source)
        assert len(result.edges) >= 2
        for edge in result.edges:
            target = edge.target_id
            # .as_view should be stripped
            assert ".as_view" not in target, f"Handler should not contain .as_view: {target}"
            assert "UserView" in target, f"Handler should contain UserView: {target}"


# ── v3.6: __all__ Export Detection (F.4) ─────────────────────────

class TestAllExports:
    """Test __all__ export extraction from Python source."""

    def test_literal_list(self):
        from coderadar.resolvers.exports import extract_all_exports
        source = '__all__ = ["foo", "bar", "baz"]'
        assert extract_all_exports(source) == ["foo", "bar", "baz"]

    def test_augmented_assign(self):
        from coderadar.resolvers.exports import extract_all_exports
        source = '__all__ = ["foo"]\n__all__ += ["bar"]'
        result = extract_all_exports(source)
        assert result is not None
        assert "foo" in result
        assert "bar" in result

    def test_extend_method(self):
        from coderadar.resolvers.exports import extract_all_exports
        source = '__all__ = ["foo"]\n__all__.extend(["bar", "baz"])'
        result = extract_all_exports(source)
        assert result is not None
        assert "foo" in result
        assert "bar" in result
        assert "baz" in result

    def test_append_method(self):
        from coderadar.resolvers.exports import extract_all_exports
        source = '__all__ = ["foo"]\n__all__.append("bar")'
        result = extract_all_exports(source)
        assert result is not None
        assert "foo" in result
        assert "bar" in result

    def test_no_all_returns_none(self):
        from coderadar.resolvers.exports import extract_all_exports
        source = 'x = ["not_all"]'
        assert extract_all_exports(source) is None

    def test_non_string_literals_ignored(self):
        from coderadar.resolvers.exports import extract_all_exports
        source = '__all__ = ["valid", 42, x]'
        result = extract_all_exports(source)
        assert result == ["valid"]

    def test_deduplication(self):
        from coderadar.resolvers.exports import extract_all_exports
        source = '__all__ = ["foo", "bar"]\n__all__ += ["foo"]\n__all__.extend(["bar"])'
        result = extract_all_exports(source)
        assert result == ["foo", "bar"]


class TestGoResolver:
    """v0.5: Go framework resolver — Gin, net/http route extraction."""

    def test_detects_go_mod(self, tmp_path):
        from coderadar.resolvers.go import GoResolver
        resolver = GoResolver()
        (tmp_path / "go.mod").write_text("module example\n")
        assert resolver.detect(tmp_path)

    def test_no_detect_without_go_files(self, tmp_path):
        from coderadar.resolvers.go import GoResolver
        resolver = GoResolver()
        assert not resolver.detect(tmp_path)

    def test_extracts_gin_routes(self):
        from coderadar.resolvers.go import GoResolver
        resolver = GoResolver()
        fixture = Path(__file__).parent / "fixtures" / "go" / "gin_routes.go"
        source = fixture.read_text()
        extraction = resolver.extract(str(fixture), source)

        routes = [n for n in extraction.nodes if n.kind == "route"]
        assert len(routes) == 6, f"Expected 6 Gin routes, got {len(routes)}"

        route_paths = {n.metadata["path"] for n in routes}
        assert "/users" in route_paths
        assert "/users/:id" in route_paths
        assert "/items" in route_paths

        handler_edges = [e for e in extraction.edges if e.kind == "handles"]
        assert len(handler_edges) == 6
        handler_names = {e.target_id for e in handler_edges}
        assert "listUsers" in handler_names
        assert "createUser" in handler_names

    def test_extracts_nethttp_routes(self):
        from coderadar.resolvers.go import GoResolver
        resolver = GoResolver()
        fixture = Path(__file__).parent / "fixtures" / "go" / "nethttp_routes.go"
        source = fixture.read_text()
        extraction = resolver.extract(str(fixture), source)

        routes = [n for n in extraction.nodes if n.kind == "route"]
        assert len(routes) == 3, f"Expected 3 net/http routes, got {len(routes)}"

        methods = {n.metadata["method"] for n in routes}
        assert "GET" in methods
        assert "POST" in methods
        assert "ANY" in methods

    def test_claims_reference_patterns(self):
        from coderadar.resolvers.go import GoResolver
        resolver = GoResolver()
        assert resolver.claims_reference("userHandler")
        assert resolver.claims_reference("HandleRequest")
        assert resolver.claims_reference("UserService")
        assert resolver.claims_reference("UserRepository")
        assert resolver.claims_reference("AuthMiddleware")
        assert not resolver.claims_reference("calculate")

    def test_resolve_looks_up_by_name(self):
        from coderadar.resolvers.go import GoResolver
        resolver = GoResolver()
        result = resolver.resolve("noSuchHandler", None)
        assert result is None

