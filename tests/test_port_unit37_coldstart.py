"""Focused recording test for the cold-start fast path."""

from __future__ import annotations

import os
import time

import pytest

pytest.importorskip(
    "coderadar._core",
    reason="the Python reference needs the built extension",
)

import coderadar
from coderadar import ops


def test_coldstart_reindex_updates_a_new_file(tmp_path):
    root = tmp_path / "project"
    (root / "src").mkdir(parents=True)
    (root / "src" / "a.py").write_text("def alpha():\n    return 1\n", encoding="utf8")
    coderadar.analyze(str(root), create_store=True)

    (root / "src" / "b.py").write_text("def beta():\n    return 2\n", encoding="utf8")
    old = time.time() - 30
    os.utime(root / ".coderadar" / "store" / "coderadar.db", (old, old))
    report = ops.reindex(root=str(root))

    assert report["stats"]["modules"] >= 2
    assert any(row["name"] == "beta" for row in ops.query("functions where name == 'beta'"))
