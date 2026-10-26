"""Agent skill packs bundled with CodeRadar.

The `SKILL.md` files under this package are the single source of truth. They
live inside `python-source` so maturin copies them into the wheel: the drift
tests read them through `importlib.resources`, and an installed release reads
the same files `coderadar skills install` copies to disk.

Bundling is transport, not activation — agent harnesses discover skills as
directories on disk (`~/.claude/skills/<name>/SKILL.md`,
`~/.config/opencode/skills/<name>/SKILL.md`, project-local equivalents), not
inside site-packages. That is what `install()` is for.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Iterator
from dataclasses import dataclass
from importlib.resources import files
from pathlib import Path

#: Every skill shipped in the wheel, sorted for stable CLI output.
SKILL_NAMES = ("coderadar-cli", "coderadar-mcp")

_FRONTMATTER = re.compile(r"\A---\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|\Z)", re.DOTALL)
_FIELD = re.compile(r"^([A-Za-z_][\w-]*):[ \t]*(.*)$")


class SkillNotFound(KeyError):
    """Raised when a name does not identify a bundled skill."""


@dataclass(frozen=True)
class InstallResult:
    """One skill copy attempt."""

    path: Path
    status: str  # "installed", "updated" or "skipped"


def skill_names() -> tuple[str, ...]:
    """Names of the skill packs this distribution ships."""
    return SKILL_NAMES


def _root():
    return files(__name__)


def _selected(names: Iterable[str] | None) -> tuple[str, ...]:
    if not names:
        return SKILL_NAMES
    chosen: list[str] = []
    for name in names:
        if name not in SKILL_NAMES:
            raise SkillNotFound(
                f"unknown skill {name!r}; available: {', '.join(SKILL_NAMES)}"
            )
        if name not in chosen:
            chosen.append(name)
    return tuple(chosen)


def skill_text(name: str) -> str:
    """Return a bundled `SKILL.md` verbatim."""
    if name not in SKILL_NAMES:
        raise SkillNotFound(
            f"unknown skill {name!r}; available: {', '.join(SKILL_NAMES)}"
        )
    return (_root() / name / "SKILL.md").read_text(encoding="utf-8")


def _fields(text: str) -> dict[str, str]:
    match = _FRONTMATTER.match(text)
    if not match:
        return {}
    fields: dict[str, str] = {}
    for line in match.group(1).splitlines():
        if not line or line[0].isspace():
            continue
        field = _FIELD.match(line)
        if not field:
            continue
        value = field.group(2).strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        fields[field.group(1)] = value
    return fields


def skill_description(name: str) -> str:
    """Frontmatter trigger description for one skill.

    The description is what a harness reads before loading the full file, so
    a missing one is a packaging bug rather than an empty string.
    """
    fields = _fields(skill_text(name))
    description = fields.get("description", "").strip()
    if not description:
        raise ValueError(f"{name}/SKILL.md has no frontmatter description")
    declared = fields.get("name")
    if declared is not None and declared != name:
        raise ValueError(f"{name}/SKILL.md frontmatter name does not match its directory")
    return description


def iter_skills() -> Iterator[tuple[str, str]]:
    """Yield `(name, description)` for every bundled skill."""
    for name in SKILL_NAMES:
        yield name, skill_description(name)


def default_destinations(*, project: bool = False) -> tuple[Path, ...]:
    """Harness skill directories used when the caller names none.

    Personal installs go to Claude Code and OpenCode discovery paths on every
    OS; `project=True` targets the same harnesses inside the current
    directory. Anything else (Cursor, pi, a custom harness) takes `--dest`.
    """
    if project:
        root = Path.cwd()
        return (root / ".claude" / "skills", root / ".opencode" / "skills")
    home = Path.home()
    return (home / ".claude" / "skills", home / ".config" / "opencode" / "skills")


def _copy_tree(source, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        target = destination / entry.name
        if entry.is_dir():
            _copy_tree(entry, target)
        else:
            target.write_bytes(entry.read_bytes())


def install(
    destination: str | Path,
    *,
    names: Iterable[str] | None = None,
    force: bool = False,
) -> list[InstallResult]:
    """Copy bundled skills into `destination`.

    Existing skill directories are left alone unless `force` is set, so a
    re-run never clobbers local edits by accident.
    """
    dest = Path(destination).expanduser()
    results: list[InstallResult] = []
    for name in _selected(names):
        target = dest / name
        marker = target / "SKILL.md"
        existed = marker.is_file()
        if existed and not force:
            results.append(InstallResult(target, "skipped"))
            continue
        _copy_tree(_root() / name, target)
        results.append(InstallResult(target, "updated" if existed else "installed"))
    return results


__all__ = [
    "SKILL_NAMES",
    "InstallResult",
    "SkillNotFound",
    "default_destinations",
    "install",
    "iter_skills",
    "skill_description",
    "skill_names",
    "skill_text",
]
