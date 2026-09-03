"""Incremental cold start: load the store, update what changed.

A warm project already has a Macrame ledger — re-parsing every file to get
the same answer is the thing P2-4 removes. `build_graph` is the single
implementation of "give me a current graph for this root, the cheap way":

* the store exists and loads → `load` it (milliseconds: the ledger *is* the
  index) and `update_file` each file newer than the store — mtime-driven,
  same skip rules as the CLI's staleness check;
* no store, or the store will not load (corrupt, foreign, or v1 — a full
  analyze is also the v1 → v2 upgrade path) → `analyze`.

The staleness primitives (`INDEXABLE_EXTS`, `STALENESS_SKIP_DIRS`,
`store_db_path`, `store_is_fresh`) moved here from `cli.py`; the CLI keeps
its own load/analyze ordering (fresh → load, else full analyze) but now
shares the rules instead of owning them.

`create_store` defaults to False deliberately: only `coderadar init` plants
the `.coderadar/` marker, and a background index that created it would make
a wrong root guess self-confirming (see `analyze`'s docstring).
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Optional

__all__ = [
    "INDEXABLE_EXTS",
    "STALENESS_SKIP_DIRS",
    "store_db_path",
    "store_is_fresh",
    "stale_source_files",
    "build_graph",
]

#: Extensions the Rust indexer will parse (Language::from_extension minus
#: the languages without a tree-sitter grammar). The staleness check must
#: cover at least what analyze walks; a superset is safe, because an extra
#: file can only make the store look stale, never fresh.
INDEXABLE_EXTS = frozenset({
    "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "rs", "java",
    "c", "h", "cpp", "cc", "cxx", "hpp", "hxx", "rb", "php", "cs", "kt",
    "kts", "swift", "scala", "sc", "lua", "ex", "exs", "zig", "zon", "r",
    "sh", "bash", "zsh", "dart", "proto", "sql", "hcl", "tf", "cmake",
    "graphql", "gql", "erl", "hrl", "hs", "lhs", "nix", "groovy", "gvy",
})
STALENESS_SKIP_DIRS = frozenset(
    {"__pycache__", "node_modules", "target", "dist", "build"})


def store_db_path(project_root: Path) -> Optional[Path]:
    """Where the Macrame store file lives for this project.

    Mirrors `store_path_for` (core_indexer/src/lib.rs): an absolute
    `[database] path` is used as-is, a relative one is root-relative, and
    the default is `.coderadar/store/coderadar.db`.
    """
    from .config import load_config
    try:
        configured = Path(load_config(project_root).database.path)
    except Exception:
        configured = Path(".coderadar/store/coderadar.db")
    if configured.is_absolute():
        return configured
    return project_root / configured


def store_is_fresh(project_root: Path, db_path: Path,
                   grace_s: float = 2.0) -> bool:
    """Cheap staleness heuristic (no content hashing).

    Fresh = the store file exists and no indexable source file under the
    root has an mtime more than `grace_s` newer than the store's. The grace
    absorbs the seconds between the last file the analyze walked and the
    ledger commit that recorded it.
    """
    try:
        db_mtime = db_path.stat().st_mtime
    except OSError:
        return False
    for dirpath, dirnames, filenames in os.walk(project_root):
        dirnames[:] = [
            d for d in dirnames
            if not d.startswith(".") and d not in STALENESS_SKIP_DIRS
        ]
        for name in filenames:
            ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
            if ext not in INDEXABLE_EXTS and name.lower() != "dockerfile":
                continue
            try:
                if os.path.getmtime(os.path.join(dirpath, name)) > db_mtime + grace_s:
                    return False
            except OSError:
                continue
    return True


def stale_source_files(project_root: Path, db_path: Path,
                       grace_s: float = 2.0) -> list[Path]:
    """The indexable files `store_is_fresh` would flag — as a list.

    Same walk, same extension set, same grace: files whose mtime is more
    than `grace_s` newer than the store's.
    """
    try:
        db_mtime = db_path.stat().st_mtime
    except OSError:
        return []
    stale: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(project_root):
        dirnames[:] = [
            d for d in dirnames
            if not d.startswith(".") and d not in STALENESS_SKIP_DIRS
        ]
        for name in filenames:
            ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
            if ext not in INDEXABLE_EXTS and name.lower() != "dockerfile":
                continue
            full = Path(dirpath) / name
            try:
                if os.path.getmtime(full) > db_mtime + grace_s:
                    stale.append(full)
            except OSError:
                continue
    return stale


def build_graph(root: "str | Path" = ".", create_store: bool = False):
    """A current CodeGraph for `root`, the cheap way to get one.

    See the module docstring for the load-vs-analyze contract. The build
    runs from the resolved root: entity ids are prefixed with the path the
    walk takes, and `update_file` reads from disk relative to the cwd, so
    `root` is walked as the directory it names whatever the caller's cwd is.
    The caller's cwd is restored before returning, including on failure.

    Raises whatever `analyze` raises when the analyze path is taken; a
    store that will not load falls through to analyze rather than failing.
    """
    import coderadar

    # `update_file` is a CodeGraph method that acts on the process's global
    # graph (the core keeps one), so any instance is a handle, not a state.
    graph = coderadar.CodeGraph()

    root_str = str(root)
    resolved = Path(root).expanduser().resolve()
    previous = Path(os.getcwd())
    os.chdir(resolved)
    try:
        db = store_db_path(resolved)
        if db is not None and db.is_file():
            try:
                coderadar.load(str(db), root_str)
            except Exception:
                # Corrupt, foreign, or v1 store. The full analyze also
                # performs the v1 -> v2 upgrade; do not report the load
                # error — analyze succeeding is the answer.
                coderadar.analyze(root_str, create_store=create_store)
                return graph
            if not store_is_fresh(resolved, db):
                for stale in stale_source_files(resolved, db):
                    try:
                        rel = stale.relative_to(resolved).as_posix()
                    except ValueError:
                        continue
                    try:
                        graph.update_file(rel)
                    except Exception:
                        # One unreadable or unparseable file must not sink
                        # the whole cold start: everything else is current
                        # and the next update_file can retry this one.
                        continue
            return graph
        coderadar.analyze(root_str, create_store=create_store)
        return graph
    finally:
        os.chdir(previous)
