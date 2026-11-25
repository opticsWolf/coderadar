#!/usr/bin/env python3
"""Cutover helper: build, verify and optionally shim the Rust CLI.

The Python package is never modified. ``--verify`` compares read-only
surfaces of the Python CLI and ``coderadar-rs``; ``--install DIR`` writes a
small launcher named ``coderadar`` in ``DIR`` so callers on PATH pick the
Rust binary up without changing source files.

Usage::

    python rust/cutover.py --build --verify
    python rust/cutover.py --install ~/.local/bin
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUST_BIN = ROOT / "target" / "debug" / ("coderadar-rs.exe" if os.name == "nt" else "coderadar-rs")
PY_CLI = os.environ.get("CODERADAR_PY_CLI", "coderadar")

VERIFY_COMMANDS: tuple[tuple[str, ...], ...] = (
    ("status", "--format", "json"),
    ("search", "canonical", "--format", "json"),
    ("dead-code", "--format", "json"),
)


def run(argv: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )


def build() -> int:
    print("building coderadar-rs ...", flush=True)
    result = run(["cargo", "build", "-p", "coderadar", "--bin", "coderadar-rs"])
    sys.stdout.write(result.stdout[-2000:])
    sys.stderr.write(result.stderr[-2000:])
    return result.returncode


def json_from(text: str) -> object | None:
    """Find the last JSON value in CLI output (cold-start lines are ignored)."""
    decoder = json.JSONDecoder()
    found: object | None = None
    for index, char in enumerate(text):
        if char not in "{[":
            continue
        try:
            value, _ = decoder.raw_decode(text[index:])
            found = value
        except json.JSONDecodeError:
            continue
    return found


def normalize(args: tuple[str, ...], value: object | None) -> object | None:
    """Make volatile/mutable fields explicit instead of pretending byte parity."""
    if args[0] == "status" and isinstance(value, dict):
        # Counts and stamps are live graph state, not the CLI contract.
        return sorted(value)
    return value


def verify() -> int:
    if not RUST_BIN.is_file():
        print(f"missing {RUST_BIN} — run with --build first", file=sys.stderr)
        return 2
    failures = 0
    for args in VERIFY_COMMANDS:
        py = run([PY_CLI, *args])
        rs = run([str(RUST_BIN), *args])
        py_json = normalize(args, json_from(py.stdout))
        rs_json = normalize(args, json_from(rs.stdout))
        same = py.returncode == rs.returncode and py_json == rs_json
        label = " ".join(args)
        if same:
            print(f"pass  {label}")
        else:
            failures += 1
            print(f"FAIL  {label}")
            print(f"  python rc={py.returncode} json={str(py_json)[:200]}")
            print(f"  rust   rc={rs.returncode} json={str(rs_json)[:200]}")
    print(f"{len(VERIFY_COMMANDS) - failures}/{len(VERIFY_COMMANDS)} verified")
    return 1 if failures else 0


def install(target: Path) -> int:
    if not RUST_BIN.is_file():
        print(f"missing {RUST_BIN} — run with --build first", file=sys.stderr)
        return 2
    target.mkdir(parents=True, exist_ok=True)
    if os.name == "nt":
        shim = target / "coderadar.cmd"
        shim.write_text(
            f'@echo off\r\n"{RUST_BIN}" %*\r\n', encoding="utf-8", newline=""
        )
    else:
        shim = target / "coderadar"
        shim.write_text(
            f'#!/bin/sh\nexec "{RUST_BIN}" "$@"\n', encoding="utf-8", newline=""
        )
        shim.chmod(0o755)
    print(f"installed {shim}")
    print("put that directory on PATH; no Python files were changed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", action="store_true", help="build the Rust binary")
    parser.add_argument("--verify", action="store_true", help="compare read-only CLI surfaces")
    parser.add_argument(
        "--install",
        type=Path,
        metavar="DIR",
        help="write a coderadar launcher in DIR (does not edit the Python project)",
    )
    args = parser.parse_args()
    if not (args.build or args.verify or args.install):
        parser.error("pass --build, --verify and/or --install DIR")
    if args.build and build() != 0:
        return 1
    code = 0
    if args.verify:
        code = verify()
    if args.install:
        code = install(args.install) or code
    return code


if __name__ == "__main__":
    raise SystemExit(main())
