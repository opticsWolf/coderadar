"""Where is the project?

The MCP server is handed no reliable answer to that question. `rootUri` and
`workspaceFolders` are LSP concepts and do not exist in MCP — the installed
SDK's `InitializeRequestParams` carries only `meta`, `protocol_version`,
`capabilities` and `client_info`, and pydantic drops anything else before the
server could read it. What MCP does have is server-to-client `roots/list`,
which is a real signal but only reachable from inside a request, not at
startup.

So the ladder is:

1. the client's declared roots (`roots/list`), when a caller has them;
2. the `--path` flag, when the operator gave one;
3. the process cwd.

Each candidate is canonicalised with `Path.resolve()` and then walked *up*
looking for a project marker — `.coderadar/` or `.coderadar.toml`. A marker
found above a candidate wins over the candidate itself: an agent invoked in
`src/deep/nested/` is still working on the project whose marker sits five
levels up.

The rungs above cwd exist because cwd is frequently wrong: MCP clients launch
servers from wherever they happen to be, which is often the client's own
install directory.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Optional
from urllib.parse import unquote, urlparse

#: Names that mark a directory as a CodeRadar project root. `.coderadar/` is
#: created by `coderadar init`; `.coderadar.toml` is the config file, which a
#: user may commit without committing the store.
MARKERS = (".coderadar", ".coderadar.toml")

#: Ladder rung names, in the order they are tried.
CLIENT_ROOT = "client root"
PATH_FLAG = "--path"
CWD = "cwd"
#: Source name for a root named by a tool call's ``project_path`` argument.
PROJECT_PATH = "project_path"


@dataclass(frozen=True)
class ResolvedRoot:
    """A project root, and the story of how it was chosen."""

    path: Path
    #: Which rung of the ladder produced it: one of the constants above.
    source: str
    #: The marker that confirmed it, or None if nothing confirmed it and this
    #: is a bare guess. Callers should say so out loud when it is None.
    marker: Optional[Path] = None

    @property
    def confirmed(self) -> bool:
        return self.marker is not None


def uri_to_path(uri: str) -> Optional[Path]:
    """Convert a `file://` URI from `roots/list` into a local path.

    MCP roots are URIs, and the spec currently only defines `file://`. Returns
    None for anything else rather than guessing — a caller that cannot be
    understood should fall through to the next rung, not be misread.
    """
    if not uri:
        return None
    if not uri.startswith("file:"):
        # A bare path is not spec-legal, but hosts do send them; accept it.
        return Path(uri) if os.path.isabs(uri) else None

    parsed = urlparse(uri)
    path = unquote(parsed.path)
    if parsed.netloc and parsed.netloc.lower() not in ("", "localhost"):
        # UNC: file://server/share/...
        return Path(f"//{parsed.netloc}{path}")
    # file:///C:/x → /C:/x on Windows; strip the leading slash off drive paths.
    if os.name == "nt" and len(path) > 2 and path[0] == "/" and path[2] == ":":
        path = path[1:]
    return Path(path) if path else None


def _home() -> Optional[Path]:
    try:
        return Path.home().resolve()
    except (OSError, RuntimeError):
        return None


def _can_be_a_root(directory: Path, home: Optional[Path]) -> bool:
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


def find_marker(start: Path) -> Optional[Path]:
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


def _canonical(path: Path) -> Optional[Path]:
    """Resolve symlinks and `..`, or None if the path is not a usable dir."""
    try:
        resolved = path.expanduser().resolve()
    except (OSError, RuntimeError):
        return None
    if not resolved.is_dir():
        # A file candidate means the project it lives in, not the file.
        resolved = resolved.parent
        if not resolved.is_dir():
            return None
    return resolved


def resolve_project_root(
    client_roots: Optional[Iterable[str]] = None,
    path_flag: Optional[str] = None,
    cwd: Optional[str] = None,
) -> ResolvedRoot:
    """Pick a project root by walking the ladder described in this module.

    Two passes, and the order matters. The first pass takes the first
    candidate whose walk-up finds a marker — a confirmed root on a lower rung
    beats an unconfirmed one on a higher rung, because a marker is evidence
    and a rung is only a preference. The second pass runs only if nothing was
    confirmed anywhere, and returns the highest-priority candidate that at
    least exists, with `marker=None` so the caller knows it is a guess.

    Never raises: the last fallback is cwd, unresolved if need be.
    """
    raw_cwd = Path(cwd) if cwd is not None else Path(os.getcwd())

    candidates: list[tuple[Path, str]] = []
    for uri in client_roots or ():
        as_path = uri_to_path(uri)
        if as_path is not None:
            candidates.append((as_path, CLIENT_ROOT))
    if path_flag is not None:
        candidates.append((Path(path_flag), PATH_FLAG))
    candidates.append((raw_cwd, CWD))

    canonical: list[tuple[Path, str]] = []
    for raw, source in candidates:
        resolved = _canonical(raw)
        if resolved is not None:
            canonical.append((resolved, source))

    for resolved, source in canonical:
        marker = find_marker(resolved)
        if marker is not None:
            return ResolvedRoot(path=marker.parent, source=source, marker=marker)

    if canonical:
        resolved, source = canonical[0]
        return ResolvedRoot(path=resolved, source=source, marker=None)
    return ResolvedRoot(path=raw_cwd, source=CWD, marker=None)


def resolve_from_client_roots(uris: Iterable[str]) -> Optional[ResolvedRoot]:
    """Resolve *only* over the client's declared roots.

    The lazy retry cannot re-run the whole ladder: cwd is always its last
    rung, and by the time the retry runs the process has already chdir'd onto
    whatever startup picked — so re-running the ladder would rediscover that
    same directory and call it a client-driven answer. This considers the
    client's roots and nothing else, and returns None when none of them are
    usable, which means "keep what we have".
    """
    for uri in uris:
        as_path = uri_to_path(uri)
        if as_path is None:
            continue
        resolved = _canonical(as_path)
        if resolved is None:
            continue
        marker = find_marker(resolved)
        if marker is not None:
            return ResolvedRoot(path=marker.parent, source=CLIENT_ROOT, marker=marker)
    # No marker anywhere under the client's roots; the first usable one is
    # still a better guess than a cwd nobody chose.
    for uri in uris:
        as_path = uri_to_path(uri)
        if as_path is None:
            continue
        resolved = _canonical(as_path)
        if resolved is not None:
            return ResolvedRoot(path=resolved, source=CLIENT_ROOT, marker=None)
    return None


def resolve_selector(project_path: str) -> Optional[ResolvedRoot]:
    """Resolve a tool call's ``project_path`` argument the way agents mean it.

    Agents rarely pass a project *root*: they pass a source file they can see
    in an editor, a subdirectory they explored earlier, or a path with a
    trailing component that only looks redundant. All of them mean "the
    project this thing belongs to", which is exactly what the walk-up finds —
    the same rule the startup ladder applies to its own candidates. A bare
    directory with no marker anywhere above it is still answered, but with
    ``marker=None`` so the caller says out loud that nothing confirmed it.

    Returns None only when no part of the path names a usable directory —
    the same rule ``_canonical`` applies for the ladder: a nonexistent name
    under a real directory is read as a file of that directory, but a path
    whose every component is missing is a caller error worth naming, not a
    project to guess about.
    """
    resolved = _canonical(Path(project_path))
    if resolved is None:
        return None
    marker = find_marker(resolved)
    if marker is not None:
        return ResolvedRoot(path=marker.parent, source=PROJECT_PATH, marker=marker)
    return ResolvedRoot(path=resolved, source=PROJECT_PATH, marker=None)


def adopt_project_root(resolved: ResolvedRoot) -> Path:
    """Make the resolved root the process cwd, and report the old one.

    Every read helper in the server resolves graph paths against the process
    cwd — `_read_source` opens them, `_get_stale_files` stats them,
    `_canonical_file_path` relativises against them — and entity ids are
    prefixed with whatever path `analyze()` walked. Serving `--path
    /other/project` while sitting in a different cwd therefore produced ids
    under one prefix and lookups under another, and the agent saw an empty
    project.

    Moving the process is the cheap half of the fix and correct for a
    single-project server: it makes `analyze('.')` and every cwd-relative
    helper right by construction. The expensive half — root-relative entity
    ids in the core — stays available if multi-project serving ever lands.
    """
    previous = Path(os.getcwd())
    if resolved.path != previous:
        os.chdir(resolved.path)
    return previous


def describe(resolved: ResolvedRoot) -> str:
    """One line for the startup log, saying how sure we are."""
    if resolved.confirmed:
        return f"Project root {resolved.path} (from {resolved.source}, marker {resolved.marker.name})"
    return (
        f"Project root {resolved.path} (from {resolved.source}, no .coderadar marker found — "
        f"run `coderadar init` there if this is the wrong project)"
    )


async def client_roots(session: object) -> list[str]:
    """Ask the connected client where its workspace is (`roots/list`).

    This is the top rung of the ladder, and it can only be climbed from
    inside a request: MCP has no `initialize` hook, and a server-to-client
    request awaited during `initialize` deadlocks — the dispatcher does not
    read further inbound messages until the handshake returns. So the roots
    dance happens lazily, on the first tool call.

    Returns an empty list for a client that declared no roots capability, or
    that answers with anything unusable. Never raises: a client that will not
    answer is a reason to keep the root we have, not to fail the tool call.
    """
    try:
        from mcp.types import ClientCapabilities, RootsCapability
    except ImportError:
        return []

    check = getattr(session, "check_client_capability", None) or getattr(
        session, "check_capability", None)
    try:
        if check is not None and not check(ClientCapabilities(roots=RootsCapability())):
            return []
        result = await session.list_roots()
    except Exception:  # noqa: BLE001 — an unhelpful client is not an error
        return []

    uris: list[str] = []
    for root in getattr(result, "roots", None) or ():
        uri = getattr(root, "uri", None)
        if uri:
            uris.append(str(uri))
    return uris
