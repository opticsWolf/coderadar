"""Run `rust-anything bench` with the Python name-collision shim enabled.

Usage: python rust/bench.py [--min-ms 50] [--json]
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SUPPORT = ROOT / "rust" / "bench_support"

env = os.environ.copy()
pythonpath = [str(SUPPORT)]
if env.get("PYTHONPATH"):
    pythonpath.append(env["PYTHONPATH"])
env["PYTHONPATH"] = os.pathsep.join(pythonpath)

command = ["rust-anything", "bench", "rust/coderadar", *sys.argv[1:]]
raise SystemExit(subprocess.call(command, cwd=ROOT, env=env))
