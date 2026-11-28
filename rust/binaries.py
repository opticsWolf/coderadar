"""Locate the Rust binary the harnesses exercise.

The artifact is `coderadar` (the product name). Builds from before the rename
are still on disk as `coderadar-rs`, so both spellings are accepted — but the
new name wins, and every harness uses this helper instead of hard-coding one.
"""

from __future__ import annotations

import os
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NAMES = ("coderadar", "coderadar-rs")


def _names(base: Path) -> list[Path]:
    suffix = ".exe" if os.name == "nt" else ""
    return [base / f"{name}{suffix}" for name in NAMES]


def debug_binary() -> Path:
    """The debug build, preferring the product name."""
    candidates = _names(ROOT / "target" / "debug")
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return candidates[0]


def release_binary() -> Path:
    """The release build, preferring the product name."""
    candidates = _names(ROOT / "target" / "release")
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return candidates[0]


def existing(preferred: Path | None = None) -> Path | None:
    """First of `preferred`, debug, release that exists."""
    for candidate in (preferred, debug_binary(), release_binary()):
        if candidate is not None and candidate.exists():
            return Path(candidate)
    return None
