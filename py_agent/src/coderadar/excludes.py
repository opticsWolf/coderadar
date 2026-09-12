"""Shared file-exclusion surface (item 7: first-class exclusion).

One matcher, one config surface, every pass. The Rust index walk owns the
authoritative matcher (gitignore grammar over ``[project] exclude`` +
one-shot extras + the built-in baseline); this module is the Python-side
front door to it:

- :func:`is_excluded` — predicate for framework resolvers and ad-hoc
  filtering. Backed by the ``is_path_excluded`` Rust binding (the SAME
  matcher the walk and store retraction use), with a stdlib fallback when
  the compiled extension is unavailable.
- :func:`iter_project_files` — ``os.walk`` with excluded dirs pruned in
  place (the F5 lesson: never descend), for star exports and resolvers.
- :data:`FALLBACK_BASELINE` — last-resort copy of the default excludes,
  used only when the extension is missing. The Rust ``DEFAULT_EXCLUDES``
  is the source of truth; keep in sync.

Layering (documented, not accidental): the index walk honors config +
one-shot ``analyze(exclude=[…])`` + baseline. Query-time Python passes
honor config + baseline — one-shot extras are an index-time narrowing.
"""

from __future__ import annotations

import fnmatch
import os
from collections.abc import Iterable, Iterator, Sequence
from pathlib import Path

# Last-resort copy of Rust DEFAULT_EXCLUDES (lib.rs). Used ONLY when the
# compiled extension is unavailable; otherwise default_excludes() is read
# live so the two can never silently diverge.
FALLBACK_BASELINE = (
    ".venv/",
    "venv/",
    "node_modules/",
    "site-packages/",
    "target/",
    "dist/",
    "build/",
    "__pycache__/",
    ".mypy_cache/",
    ".tox/",
    ".git/",
    ".hg/",
    ".svn/",
    ".coderadar/",
    ".pytest_cache/",
)


def _rust_excluded(rel_posix: str) -> bool | None:
    """Ask the real matcher. None when the extension is unavailable."""
    try:
        from coderadar._core import is_path_excluded as _rust_fn
    except ImportError:
        return None
    try:
        return bool(_rust_fn(rel_posix))
    except Exception:  # noqa: BLE001 - matcher hiccup means "fall back to Python"
        return None


def _fallback_excluded(rel_posix: str, extra: Sequence[str] = ()) -> bool:
    """Stdlib approximation: dir-name membership + anchored prefixes + globs.

    Mirrors the float semantics of bare ``name/`` (any depth) and the
    anchoring of interior-slash patterns. Close enough for a backend that
    only runs when the real index is absent too.
    """
    parts = rel_posix.split("/")
    for pat in list(FALLBACK_BASELINE) + list(extra):
        p = pat.strip()
        if not p:
            continue
        if "*" in p or "?" in p or "[" in p:
            if fnmatch.fnmatch(rel_posix, p.lstrip("/")) or fnmatch.fnmatch(
                parts[-1], p.lstrip("/")
            ):
                return True
            continue
        if "/" in p.strip("/"):
            # Interior slash → root-anchored prefix.
            if rel_posix == p.strip("/") or rel_posix.startswith(p.strip("/") + "/"):
                return True
            continue
        # Bare name/ → any depth.
        if p.strip("/") in parts:
            return True
    return False


def is_excluded(path: str | Path, root: str | Path) -> bool:
    """Whether ``path`` is excluded from project passes relative to ``root``.

    Uses the shared Rust matcher (config + baseline). Paths escaping the
    root (``..``) are passed through absolutely — only unanchored patterns
    can match those, which is the documented behavior.
    """
    rel = os.path.relpath(os.fspath(path), os.fspath(root))
    # relpath never yields an empty string; guard anyway.
    if not rel or rel == ".":
        return False
    # Forward slashes: the matcher grammar is slash-oriented on every OS.
    rel_posix = rel.replace(os.sep, "/")
    decided = _rust_excluded(rel_posix)
    if decided is not None:
        return decided
    return _fallback_excluded(rel_posix)


def iter_project_files(    root: str | Path,
    *,
    suffixes: Sequence[str] = (),
    names: Iterable[str] = (),
) -> Iterator[Path]:
    """Yield files under ``root``, pruning excluded directories in place.

    ``suffixes`` filters by filename ending (``(\".py\",)``); ``names``
    filters by exact filename (``(\"package.json\",)``). Excluded dirs are
    pruned before descent (F5: 17.6 s → 0.16 s), so neither this walk nor
    any consumer ever pays for ``.venv`` et al.
    """
    root_path = Path(root)
    wanted_names = set(names)
    for dirpath, dirnames, filenames in os.walk(root_path):
        here = Path(dirpath)
        dirnames[:] = [d for d in dirnames if not is_excluded(here / d, root_path)]
        for fn in filenames:
            if suffixes and not fn.endswith(tuple(suffixes)):
                continue
            if wanted_names and fn not in wanted_names:
                continue
            candidate = here / fn
            if is_excluded(candidate, root_path):
                continue
            yield candidate


# ── Indexed-root tracking (F14: readers) ────────────────────────────────
# Entity ids are canonical root-relative form (`.\x.py`). Any Python code
# that opens such a path must resolve it against the indexed root — the
# process CWD is only right by accident. `analyze()`/`load()` record the
# root here; readers resolve through `resolve_entity_path`.
_INDEXED_ROOT: Path | None = None


def set_indexed_root(root: str | Path | None) -> None:
    """Record the project root the current graph was indexed from."""
    global _INDEXED_ROOT
    _INDEXED_ROOT = Path(os.path.abspath(os.fspath(root))) if root else None


def resolve_entity_path(path: str | Path) -> Path:
    """Resolve a possibly root-relative entity path for disk reads."""
    p = Path(os.fspath(path))
    if p.is_absolute():
        return p
    # F14: the Rust core knows the indexed root in every flow (analyze and
    # load record it, even through the raw binding); the Python record is
    # the fallback for extension-less flows.
    try:
        from coderadar._core import indexed_root_py as _rust_root
        if _rust_root():
            return Path(_rust_root()) / p
    except Exception:  # noqa: BLE001, S110 - best-effort root probe, None means "unknown"
        pass
    if _INDEXED_ROOT is not None:
        return _INDEXED_ROOT / p
    return p
