"""Plan unit M4: the Rust MCP server speaks valid MCP."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def test_mcp_conformance() -> None:
    script = ROOT / "rust" / "mcp_conformance.py"
    sys.path.insert(0, str(ROOT / "rust"))
    from binaries import debug_binary

    if not debug_binary().exists():
        import pytest

        pytest.skip("the debug binary is not built")
    completed = subprocess.run(
        [sys.executable, str(script), "--json"],
        capture_output=True,
        text=True,
        timeout=180,
    )
    report = json.loads(completed.stdout)
    assert report["ok"], report["problems"]
    assert report["tools"] >= 49
    assert report["resources"] >= 3
