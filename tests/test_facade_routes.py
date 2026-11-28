"""File-scoped framework route refresh through the Rust-backed facade."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "rust" / "coderadar" / "python"

APP = '''\
from flask import Flask
app = Flask(__name__)

@app.route("/users/<id>")
def get_user(id):
    return {"id": id}

@app.route("/health")
def health():
    return {"ok": True}
'''


def test_update_file_refreshes_routes_in_the_live_graph(tmp_path: Path) -> None:
    project = tmp_path / "shop"
    project.mkdir()
    (project / ".coderadar").mkdir()
    (project / "app.py").write_text(APP, encoding="utf-8")

    script = """
import json
from pathlib import Path

import coderadar

coderadar.analyze(".", True, None)
coderadar.load(".coderadar/store/coderadar.db", ".")

def ids(path):
    return [row["id"] for row in coderadar.resolve(path)["results"]]

assert ids("/health") == ["app.py::health"], ids("/health")

src = Path("app.py").read_text() + (
    '\\n@app.route("/orders/<id>")\\ndef get_order(id):\\n    return {}\\n'
)
graph = coderadar.CodeGraph(".coderadar/store/coderadar.db")
report = graph.update_file("app.py", src)
assert report["route_warning"] is None, report
assert coderadar.node("app.py::flask:route:/orders/<id>")["kind"] == "route"
assert ids("/orders/:id") == ["app.py::get_order"], ids("/orders/:id")

# The new route is in the store, not only in the refresh report.
coderadar.load(".coderadar/store/coderadar.db", ".")
assert ids("/orders/:id") == ["app.py::get_order"], ids("/orders/:id")

src = (
    'from flask import Flask\\napp = Flask(__name__)\\n\\n'
    '@app.route("/users/<id>")\\ndef get_user(id):\\n    return {"id": id}\\n'
)
graph.update_file("app.py", src)
assert ids("/health") == [], ids("/health")
print(json.dumps({"ok": True}))
"""
    env = os.environ.copy()
    env["PYTHONPATH"] = str(FACADE)
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout.strip().splitlines()[-1]) == {"ok": True}
