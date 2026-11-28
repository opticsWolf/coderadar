"""Where is the project?

The ladder lives in Rust (`core_indexer::mcp_runtime::resolve_root`); this
module only names the rungs, shapes the result and keeps the one piece that
needs the Python MCP SDK: asking the connected client for `roots/list`.

Ladder order (first confirmed marker wins; otherwise the highest-priority
existing directory, unconfirmed):

1. the client's declared roots (`roots/list`);
2. the `--path` flag;
3. the project recorded for this launch directory;
4. the process cwd.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from .. import (
    roots_adopt_project_root as _adopt,
    roots_describe as _describe,
    roots_resolve_from_client_roots as _resolve_from_roots,
    roots_resolve_project_root as _resolve_ladder,
    roots_resolve_selector as _resolve_selector,
    roots_uri_to_path as _uri_to_path,
)
from ..markers import find_marker

#: Ladder rung names, in the order they are tried.
CLIENT_ROOT = "client root"
PATH_FLAG = "--path"
PREVIOUS_SESSION = "previous session"
CWD = "cwd"
PROJECT_PATH = "project_path"


@dataclass(frozen=True)
class ResolvedRoot:
    """A project root, and the story of how it was chosen."""

    path: Path
    source: str
    marker: Path | None = None

    @property
    def confirmed(self) -> bool:
        return self.marker is not None


def uri_to_path(uri: str) -> Path | None:
    """Convert a `file://` URI from `roots/list` into a local path."""
    raw = _uri_to_path(uri)
    return Path(raw) if raw is not None else None


def _marker_for(path: Path) -> Path | None:
    """The marker file/dir inside a confirmed root, or None."""
    return find_marker(path)


def _resolved(report: dict | None) -> ResolvedRoot | None:
    if report is None:
        return None
    path = Path(report["path"])
    return ResolvedRoot(
        path=path,
        source=report["source"],
        marker=_marker_for(path) if report.get("confirmed") else None,
    )


def resolve_project_root(
    client_roots: Iterable[str] | None = None,
    path_flag: str | None = None,
    cwd: str | None = None,
    launch_cwd: Path | None = None,
) -> ResolvedRoot:
    """Pick a project root by walking the ladder described in this module."""
    report = _resolve_ladder(
        list(client_roots or ()),
        path_flag,
        cwd,
        str(launch_cwd) if launch_cwd is not None else None,
    )
    resolved = _resolved(report)
    assert resolved is not None
    return resolved


def resolve_from_client_roots(uris: Iterable[str]) -> ResolvedRoot | None:
    """Resolve *only* over the client's declared roots (lazy retry)."""
    return _resolved(_resolve_from_roots(list(uris)))


def resolve_selector(project_path: str) -> ResolvedRoot | None:
    """Resolve a tool call's ``project_path`` argument (file or subdirectory)."""
    return _resolved(_resolve_selector(project_path))


def adopt_project_root(resolved: ResolvedRoot) -> Path:
    """Make the resolved root the process cwd, and report the old one."""
    return Path(_adopt(str(resolved.path)))


def describe(resolved: ResolvedRoot) -> str:
    """One line for the startup log, saying how sure we are."""
    return _describe(
        str(resolved.path),
        resolved.source,
        str(resolved.marker) if resolved.marker is not None else None,
    )


async def client_roots(session: object) -> list[str]:
    """Ask the connected client where its workspace is (`roots/list`).

    This is the top rung and can only be climbed from inside a request. The
    SDK objects stay here by necessity; every decision about the answer is
    Rust's. Never raises: an unhelpful client is a reason to keep the root we
    have, not to fail the tool call.
    """
    try:
        from mcp.types import ClientCapabilities, RootsCapability
    except ImportError:
        return []

    check = getattr(session, "check_client_capability", None) or getattr(
        session, "check_capability", None
    )
    try:
        if check is not None and not check(ClientCapabilities(roots=RootsCapability())):
            return []
        result = await session.list_roots()
    except Exception:  # noqa: BLE001 - an unhelpful client is not an error
        return []

    uris: list[str] = []
    for root in getattr(result, "roots", None) or ():
        uri = getattr(root, "uri", None)
        if uri:
            uris.append(str(uri))
    return uris


__all__ = [
    "CLIENT_ROOT",
    "CWD",
    "PATH_FLAG",
    "PREVIOUS_SESSION",
    "PROJECT_PATH",
    "ResolvedRoot",
    "adopt_project_root",
    "client_roots",
    "describe",
    "resolve_from_client_roots",
    "resolve_project_root",
    "resolve_selector",
    "uri_to_path",
]
