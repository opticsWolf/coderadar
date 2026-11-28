"""Focused cases for the Rust framework-resolver port (unit 17).

Each test writes a real mini-project and calls the shared framework hook,
so `rust-anything record --from-tests` captures the resolver's observable
route/handler counts without reaching into resolver class internals.
"""

from __future__ import annotations

from pathlib import Path

from tests._rust_bridge import refresh_framework_file


def _run(tmp_path: Path, files: dict[str, str], rel: str) -> dict:
    for path, text in files.items():
        target = tmp_path / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    return refresh_framework_file(str(tmp_path), rel, files.get(rel))


def test_flask_route_decorators(tmp_path):
    report = _run(
        tmp_path,
        {
            "app.py": """\
from flask import Flask
app = Flask(__name__)

@app.route('/')
def index():
    return 'Hello'

@app.route('/users/<int:id>', methods=['GET', 'POST'])
def user_detail(id):
    return f'User {id}'
""",
        },
        "app.py",
    )
    assert report["routes"] >= 2


def test_flask_method_decorators(tmp_path):
    report = _run(
        tmp_path,
        {
            "app.py": """\
from flask import Flask
app = Flask(__name__)

@app.get('/api/items')
def get_items():
    return []

@app.post('/api/items')
def create_item():
    return {}, 201
""",
        },
        "app.py",
    )
    assert report["routes"] >= 2


def test_flask_blueprint_and_restful(tmp_path):
    report = _run(
        tmp_path,
        {
            "app.py": """\
from flask import Flask, Blueprint
from flask_restful import Api, Resource
app = Flask(__name__)
bp = Blueprint('api', __name__)
app.register_blueprint(bp)
api = Api(app)
class HelloWorld(Resource):
    def get(self):
        return {'hello': 'world'}
api.add_resource(HelloWorld, '/', '/hello')
""",
        },
        "app.py",
    )
    assert report["handlers"] >= 1


def test_django_path_and_router(tmp_path):
    report = _run(
        tmp_path,
        {
            "manage.py": "# Django project\n",
            "urls.py": """\
from django.urls import path, re_path
from rest_framework.routers import DefaultRouter
from .views import index, user_detail, UserViewSet

router = DefaultRouter()
router.register(r'users', UserViewSet)

urlpatterns = [
    path('home/', index, name='index'),
    path('users/<int:id>/', user_detail, name='user_detail'),
    re_path(r'^articles/(?P<year>[0-9]{4})/$', index),
]
""",
        },
        "urls.py",
    )
    assert report["routes"] >= 3


def test_fastapi_routes(tmp_path):
    report = _run(
        tmp_path,
        {
            "main.py": """\
from fastapi import FastAPI, Depends
app = FastAPI()

def get_db():
    return None

@app.get('/items')
def list_items(db=Depends(get_db)):
    return []

@app.post('/items')
def create_item(item: dict):
    return item
""",
        },
        "main.py",
    )
    assert report["routes"] >= 2


def test_express_routes(tmp_path):
    report = _run(
        tmp_path,
        {
            "package.json": '{"dependencies": {"express": "^4.18.0"}}\n',
            "routes.js": """\
const app = require('express')();
app.get('/users', usersController.list);
app.post('/users', usersController.create);
app.put('/users/:id', usersController.update);
app.delete('/users/:id', usersController.remove);
""",
        },
        "routes.js",
    )
    assert report["routes"] >= 4
