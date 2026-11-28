"""Last-project-per-directory state, owned by the engine."""

from pathlib import Path

from . import last_project_for as _last
from . import project_state_file, record_project as _record


def record_project(launch_cwd, root):
    """Remember the project chosen from `launch_cwd`."""
    _record(str(launch_cwd), str(root))


def last_project_for(launch_cwd):
    """The project remembered for `launch_cwd`, if any."""
    found = _last(str(launch_cwd))
    return None if found is None else Path(found)


def state_file():
    """The JSON file that stores those choices."""
    found = project_state_file()
    return None if found is None else Path(found)


__all__ = ["last_project_for", "record_project", "state_file"]
