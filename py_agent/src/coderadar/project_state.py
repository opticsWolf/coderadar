"""Where has this launch directory pointed before?

MCP clients launch the server from wherever they happen to be — often the
client's install directory, not the project — and every session re-runs the
path ladder from scratch. The agent that called `codegraph_set_project` in
last session has already answered "which project does this launch directory
mean"; this module remembers that answer, keyed by the *launch* cwd (the
directory the process started in, before any chdir), so the next launch from
the same place can resume where the agent left off.

The record is a convenience, not a source of truth:

* it lives in ``~/.coderadar/mcp/last_projects.json`` — user state, never
  inside the project;
* writes are best-effort and never raise into the tool path;
* reads validate that the recorded directory still exists, so a deleted or
  moved project simply falls through to the rest of the ladder.

`--path` always beats a record (explicit operator intent), and a
`set_project` call — whatever it switches to, including back to the marker
project — rewrites the record, so the last explicit choice wins.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Optional

__all__ = ["record_project", "last_project_for", "state_file"]


def state_file() -> Optional[Path]:
    """Where the record lives, or None if there is no usable home."""
    try:
        return Path.home() / ".coderadar" / "mcp" / "last_projects.json"
    except (OSError, RuntimeError):
        return None


def _key(directory: Path) -> str:
    """Canonical launch-cwd key: resolved, and lower-cased on Windows."""
    try:
        resolved = Path(directory).expanduser().resolve()
    except (OSError, RuntimeError):
        resolved = Path(directory)
    text = str(resolved)
    if os.name == "nt":
        text = text.lower()  # case-insensitive filesystem
    return text


def record_project(launch_cwd: Path, root: Path) -> None:
    """Remember that `launch_cwd` last worked on `root`.

    Best-effort by contract: every failure (no home, permission, disk) is
    swallowed, because the record exists to help the *next* session and must
    never surface into the call that records it.
    """
    try:
        path = state_file()
        if path is None:
            return
        path.parent.mkdir(parents=True, exist_ok=True)
        data: dict = {}
        if path.exists():
            try:
                loaded = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                loaded = None  # corrupt — start fresh; the write repairs it
            if isinstance(loaded, dict):
                data = loaded
        data[_key(launch_cwd)] = str(root)
        path.write_text(
            json.dumps(data, indent=2, sort_keys=True), encoding="utf-8")
    except Exception:  # noqa: BLE001 — best-effort, see module docstring
        pass


def last_project_for(launch_cwd: Path) -> Optional[Path]:
    """The root recorded for this launch directory, validated.

    Returns None when there is no record, the record file is corrupt, or the
    recorded directory no longer exists (stale record). Stale entries are
    left in place: pruning is not this module's job, and a concurrent server
    may be about to write them again.
    """
    try:
        path = state_file()
        if path is None or not path.exists():
            return None
        data = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(data, dict):
            return None
        value = data.get(_key(launch_cwd))
        if not isinstance(value, str) or not value:
            return None
        candidate = Path(value)
        if not candidate.is_dir():
            return None
        return candidate
    except Exception:  # noqa: BLE001 — a broken record reads as nothing
        return None
