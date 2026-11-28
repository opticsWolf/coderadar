"""Index exclusion rules, evaluated by the engine."""

from pathlib import Path

from . import is_excluded as _is_excluded
from . import project_files
from . import stats as _stats

_ROOT = Path.cwd()


def set_indexed_root(root):
    """Point the helpers at a project root."""
    global _ROOT
    _ROOT = Path(root)


def is_excluded(path, is_dir=False, extra=None):
    """Whether `path` is excluded under the current root."""
    return _is_excluded(str(_ROOT), str(path), is_dir, extra or [])


def iter_project_files(root=None, extra=None):
    """Yield every indexable file under `root`, slash-separated."""
    base = Path(root) if root is not None else _ROOT
    for relative in project_files(str(base), extra or []):
        yield base / relative


def resolve_entity_path(path):
    """Resolve a possibly root-relative entity path for disk reads."""
    candidate = Path(path)
    if candidate.is_absolute():
        return candidate
    try:
        indexed = _stats().get("indexed_root")
    except Exception:  # noqa: BLE001 - best-effort root probe; None means "unknown"
        indexed = None
    if indexed:
        return Path(indexed) / candidate
    return _ROOT / candidate


__all__ = [
    "is_excluded", "iter_project_files", "project_files",
    "resolve_entity_path", "set_indexed_root",
]
