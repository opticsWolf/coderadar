"""Project marker discovery."""

from . import find_marker as _find_marker

MARKERS = (".coderadar", ".coderadar.toml")


def find_marker(path):
    """The nearest directory at or above `path` that carries a marker."""
    found = _find_marker(str(path))
    return None if found is None else __import__("pathlib").Path(found)


__all__ = ["MARKERS", "find_marker"]
