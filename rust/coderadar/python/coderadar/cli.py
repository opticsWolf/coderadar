"""Run the Rust binary with the Python CLI's argument vector."""

import os
import subprocess
import sys
from pathlib import Path


def binary():
    """Locate the Rust binary for this install."""
    override = os.environ.get("CODERADAR_BIN")
    if override:
        return override
    for name in ("coderadar", "coderadar-rs"):
        for directory in os.environ.get("PATH", "").split(os.pathsep):
            candidate = Path(directory) / name
            if candidate.is_file():
                return str(candidate)
            if candidate.with_suffix(".exe").is_file():
                return str(candidate.with_suffix(".exe"))
    sibling = Path(sys.executable).parent / "coderadar"
    return str(sibling)


def main(argv=None):
    """Delegate one CLI invocation to the Rust binary."""
    args = list(sys.argv[1:] if argv is None else argv)
    return subprocess.call([binary(), *args])


__all__ = ["binary", "main"]
