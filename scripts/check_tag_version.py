#!/usr/bin/env python3
"""Fail when the release tag and `pyproject.toml` disagree.

`release.yml` publishes whatever `pyproject.toml` says, so a tag cut against
the wrong commit would put wheels on PyPI under a version the source never
claims — and PyPI is one-way. The workflow passes the tag it was triggered
by (`GITHUB_REF_NAME`) or, for a manual run, the `version` input; a local
run passes the tag as an argument:

    python scripts/check_tag_version.py v0.15.0

With nothing to compare (no argument, no CI environment) the check reports
what it found and passes — it gates publication, not development.
"""

from __future__ import annotations

import os
import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
TAG_SHAPE = re.compile(r"^v\d+\.\d+\.\d+$", re.IGNORECASE)


def package_version() -> str:
    """The version `pyproject.toml` declares, without needing tomllib >= 3.11."""
    text = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = re.search(r'^\s*version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if match is None:
        raise SystemExit("pyproject.toml has no [project] version")
    return match.group(1)


def named_tag(argv: list[str]) -> str | None:
    """The tag this run is about: argv first, then CI's own spelling."""
    if len(argv) > 1:
        return argv[1]
    return os.environ.get("INPUT_VERSION") or os.environ.get("GITHUB_REF_NAME") or None


def main(argv: list[str]) -> int:
    version = package_version()
    tag = named_tag(argv)
    if tag is None:
        print(f"package version {version}; no tag to compare (local run)")
        return 0
    if not TAG_SHAPE.match(tag):
        print(f"not a release tag: {tag!r} (expected vMAJOR.MINOR.PATCH)")
        return 1
    expected = f"v{version}"
    if tag != expected:
        print(f"tag {tag} does not match pyproject.toml version {version} ({expected})")
        return 1
    print(f"tag {tag} matches pyproject.toml version {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
