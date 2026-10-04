"""Project markers: is this directory (or one above it) a CodeRadar project?

Shared by the MCP root ladder (`coderadar.mcp.roots`) and the CLI's
walk-up; kept free of the MCP SDK so the CLI starts fast.
"""

from __future__ import annotations

from pathlib import Path

#: Names that mark a directory as a CodeRadar project root. `.coderadar/` is
#: created by `coderadar init`; `.coderadar.toml` is the config file, which a
#: user may commit without committing the store.
MARKERS = (".coderadar", ".coderadar.toml")


def _home() -> Path | None:
    try:
        return Path.home().resolve()
    except (OSError, RuntimeError):
        return None


def _can_be_a_root(directory: Path, home: Path | None) -> bool:
    """Is this directory low enough in the tree to be somebody's project?

    Projects live *inside* the home directory, never at it and never above
    it. The walk-up has to stop somewhere, and this is the honest boundary:
    a stray `~/.coderadar` — which CodeRadar itself may have left there, and
    which is a user-level directory rather than a project — would otherwise
    be found from anywhere under the home tree and adopted as the root of
    every project the user has.
    """
    if home is None:
        return True  # no boundary to enforce; the walk still ends at the root
    if directory == home:
        return False
    # An ancestor of home — C:/Users, /home, the filesystem root — is never
    # one either, so the walk is done once it climbs past home.
    return directory not in home.parents


def find_marker(start: Path) -> Path | None:
    """Walk up from `start` looking for a project marker.

    Returns the marker itself (`.../.coderadar` or `.../.coderadar.toml`), so
    the caller can both report it and take its parent as the root. Returns
    None if the walk reaches its boundary — the home directory, or the
    filesystem root — without finding one.
    """
    current = start if start.is_dir() else start.parent
    home = _home()
    for directory in (current, *current.parents):
        if not _can_be_a_root(directory, home):
            break
        for name in MARKERS:
            candidate = directory / name
            if candidate.exists():
                return candidate
    return None
