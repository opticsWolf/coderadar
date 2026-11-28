"""Skill packs bundled with the binary."""

from pathlib import Path

from . import skill_description, skill_names, skill_text


class SkillNotFound(LookupError):
    """A skill name outside `skill_names()`."""


def iter_skills():
    """Yield `(name, description, text)` for every bundled skill."""
    for name in skill_names():
        yield name, skill_description(name) or "", skill_text(name) or ""


def default_destinations(project: bool = False) -> list[Path]:
    """Where `install` writes without an explicit destination."""
    if project:
        return [Path(".claude/skills"), Path(".opencode/skills")]
    home = Path.home()
    return [home / ".claude/skills", home / ".config/opencode/skills"]


def install(destinations=None, names=None, project=False, force=False):
    """Copy bundled skills into discovery directories.

    Returns `(installed, skipped)` path lists.
    """
    targets = [Path(item) for item in (destinations or default_destinations(project))]
    selected = names or skill_names()
    installed, skipped = [], []
    for target in targets:
        for name in selected:
            text = skill_text(name)
            if text is None:
                raise SkillNotFound(name)
            directory = target / name
            marker = directory / "SKILL.md"
            if marker.is_file() and not force:
                skipped.append(directory)
                continue
            directory.mkdir(parents=True, exist_ok=True)
            marker.write_text(text, encoding="utf-8")
            installed.append(directory)
    return installed, skipped


__all__ = [
    "SkillNotFound", "default_destinations", "install", "iter_skills",
    "skill_description", "skill_names", "skill_text",
]
