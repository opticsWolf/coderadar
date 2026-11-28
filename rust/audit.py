#!/usr/bin/env python3
"""One command that runs every completion gate (unit A2).

The gates are the committed harnesses, not a reimplementation of them:

- `fmt` / `clippy` / `cargo test` — Rust hygiene and unit coverage.
- `gate` — `rust-anything gate rust/coderadar` (behavior + size).
- `cli parity` — 42 JSON payloads against the Python reference.
- `mcp` — protocol conformance (49 tools, 3 resources).
- `surface` — Python import surface against the recorded baseline.
- `embedding` — the E2 drift contract (loads the ONNX model).
- `smoke` — release binary end to end without Python.

Usage::

    python rust/audit.py                # everything
    python rust/audit.py --quick        # skip the model + release smoke
    python rust/audit.py --json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PY = sys.executable


@dataclass
class Check:
    name: str
    argv: list[str]
    summary: str = ""
    ok: bool = False
    seconds: float = 0.0
    output: str = field(default="")


def checks(quick: bool) -> list[Check]:
    rows = [
        Check("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
        Check("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        Check("tests", ["cargo", "test", "--workspace"]),
        Check("gate", ["rust-anything", "gate", "rust/coderadar"]),
        Check("cli parity", [PY, "rust/cli_parity.py"]),
        Check("mcp", [PY, "rust/mcp_conformance.py"]),
        Check("surface", [PY, "rust/import_surface.py", "--check"]),
    ]
    if not quick:
        rows.append(
            Check(
                "embedding",
                ["cargo", "test", "-p", "core_indexer", "embedding_parity", "--", "--ignored"],
            )
        )
        rows.append(Check("smoke", [PY, "rust/smoke.py"]))
    return rows


def summarize(name: str, stdout: str) -> str:
    """The one line worth printing for a check."""
    lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    if name == "gate":
        behavior = [line for line in lines if "behavior" in line]
        return behavior[-1] if behavior else (lines[-1] if lines else "")
    if name in {"tests"}:
        passed = sum(int(part.split()[0]) for part in re_find(r"(\d+) passed", stdout))
        failed = sum(int(part.split()[0]) for part in re_find(r"(\d+) failed", stdout))
        return f"{passed} passed, {failed} failed"
    if name == "clippy":
        return "clean"
    if name == "embedding":
        results = [line for line in lines if line.startswith("test result:")]
        passed = [line for line in results if "ok." in line and " 0 passed" not in line]
        return passed[0] if passed else (results[-1] if results else "")
    return lines[-1] if lines else ""


def re_find(pattern: str, text: str) -> list[str]:
    import re

    return [match.group(1) for match in re.finditer(pattern, text)]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--quick", action="store_true", help="skip the model test and release smoke")
    parser.add_argument("--json", action="store_true")
    options = parser.parse_args(argv)

    rows = checks(options.quick)
    for row in rows:
        started = time.monotonic()
        proc = subprocess.run(
            row.argv,
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        )
        row.seconds = time.monotonic() - started
        row.ok = proc.returncode == 0
        row.output = (proc.stdout + proc.stderr)[-4000:]
        row.summary = summarize(row.name, proc.stdout + proc.stderr)
        if not options.json:
            state = "pass" if row.ok else "FAIL"
            print(f"{state}  {row.name:<12} {row.seconds:6.1f}s  {row.summary[:100]}", flush=True)

    if options.json:
        print(
            json.dumps(
                [
                    {"name": row.name, "ok": row.ok, "seconds": round(row.seconds, 1), "summary": row.summary}
                    for row in rows
                ],
                indent=1,
            )
        )

    failed = [row for row in rows if not row.ok]
    if failed:
        if not options.json:
            for row in failed:
                print(f"\n--- {row.name} ---\n{row.output[-1500:]}")
            print(f"audit FAILED: {len(failed)} of {len(rows)} checks")
        return 1
    if not options.json:
        print(f"\naudit ok: {len(rows)} checks, {sum(row.seconds for row in rows):.0f}s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
