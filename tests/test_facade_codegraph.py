"""The Python facade's CodeGraph handle delegates to Rust, not to Python ops."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "rust" / "coderadar" / "python"


def test_codegraph_facade_runs_in_rust(tmp_path: Path) -> None:
    project = tmp_path / "demo"
    project.mkdir()
    (project / "app.py").write_text("def alpha():\n    return 1\n", encoding="utf-8")

    script = """
import json

import coderadar
assert hasattr(coderadar, "analyze") and hasattr(coderadar, "CodeGraph")
g = coderadar.analyze(".", True, None)
assert isinstance(g, coderadar.CodeGraph)
assert g.stats_result.get("files_indexed", 0) >= 1
assert isinstance(g.query("functions"), list)
note = g.annotate("app.py::module", "- [x] facade test")
assert note.get("id")
rows = g.annotations_on("app.py::module")["versions"]
assert any("facade test" in str(row) for row in rows)
print(json.dumps({"stats": sorted(g.stats_result), "rows": len(rows)}))
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
    report = json.loads(result.stdout.strip().splitlines()[-1])
    assert "files_indexed" in report["stats"]
    assert report["rows"] >= 1
