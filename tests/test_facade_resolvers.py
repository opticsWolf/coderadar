"""The facade's framework resolver classes are thin Rust delegates."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "rust" / "coderadar" / "python"


def test_framework_resolver_classes_delegate_to_rust(tmp_path: Path) -> None:
    project = tmp_path / "shop"
    project.mkdir()
    (project / "app.py").write_text(
        "from flask import Flask\n"
        "app = Flask(__name__)\n\n"
        "@app.route('/users/<id>')\n"
        "def get_user(id):\n"
        "    return id\n",
        encoding="utf-8",
    )

    script = """
import json
import coderadar
from coderadar.resolvers import (
    ALL_RESOLVERS, DjangoResolver, FlaskResolver, RustFrameworkResolver,
)
from coderadar.resolvers import resolution

assert len(ALL_RESOLVERS) == 13
assert [r().name for r in ALL_RESOLVERS] == [
    'django', 'flask', 'fastapi', 'go', 'actix', 'express', 'springboot',
    'laravel', 'aspnet', 'rails', 'nestjs', 'vue-router', 'react-router',
]
assert FlaskResolver.extract is RustFrameworkResolver.extract
assert FlaskResolver.detect is RustFrameworkResolver.detect

flask = FlaskResolver()
assert flask.detect('.') is True
assert DjangoResolver().detect('.') is False
assert flask.claims_reference('UserBlueprint') is True
assert flask.claims_reference('plain_name') is False

source = open('app.py', encoding='utf-8').read()
extraction = flask.extract('app.py', source)
assert [(n.name, n.kind) for n in extraction.nodes] == [('/users/<id>', 'route')]
assert extraction.edges and extraction.edges[0].kind == 'handles'
assert extraction.edges[0].target_id == 'app.py::get_user'

candidates = [{'id': 'm', 'name': 'User', 'file_path': 'app/models.py'}]
assert DjangoResolver().resolve('UserModel', candidates)['confidence'] == 0.85
assert resolution.resolve_reference('UserModel', lambda n, l: candidates)[0]['resolved_by'] == 'django'
assert resolution.canonical_route_pattern('/users/:id') == ('users', '*')
assert resolution.route_patterns_match('/users/:id', '/users/<id>') is True
assert resolution.prefer_in_dir(candidates, 'User', 'models')['confidence'] == 0.85

print(json.dumps({'classes': len(ALL_RESOLVERS), 'nodes': len(extraction.nodes)}))
"""
    env = os.environ.copy()
    env["PYTHONPATH"] = str(FACADE)
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {"classes": 13, "nodes": 1}
