"""Project marker discovery: the marker itself, not its directory.

Python's `find_marker` returns `.../.coderadar` or `.../.coderadar.toml` so a
caller can both report it and take its parent as the root. The engine's
walk-up returns the directory; this adapter maps back to the marker file.
"""

from __future__ import annotations

from pathlib import Path

from . import find_marker as _find_directory

MARKERS = (".coderadar", ".coderadar.toml")


def find_marker(path: str | Path) -> Path | None:
    """Walk up from `path` looking for a project marker."""
    directory = _find_directory(str(path))
    if directory is None:
        return None
    base = Path(directory)
    for name in MARKERS:
        candidate = base / name
        if candidate.exists():
            return candidate
    return base


__all__ = ["MARKERS", "find_marker"]
