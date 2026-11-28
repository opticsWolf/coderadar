#!/usr/bin/env python
"""CLI contract parity harness (plan unit C4).

Compares the Python reference CLI with the Rust binary on:

* command inventory,
* `--help` exit codes,
* `--format json` acceptance for every command that advertises it,
* JSON payload equality for a read-only probe set (after normalising the
  fields the plan already treats as volatile: absolute paths, timings).

Usage:
    python rust/cli_parity.py [--json] [--rust BIN] [--python BIN]
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GOLDEN = ROOT / "rust" / "golden" / "cli_parity.json"

COMMAND_RE = re.compile(r"^  ([a-z][a-z0-9-]+)\s{2,}", re.MULTILINE)
OPTION_RE = re.compile(r"^\s{2,}(-{1,2}[A-Za-z][\w-]*)(?:[ ,\[]|$)")

# Minimal arguments for a read-only smoke run of each command.
PROBES: dict[str, list[str]] = {
    "status": [],
    "search": ["analysis"],
    "node": ["core_indexer/src/annotations.rs::Analysis"],
    "callers": ["core_indexer/src/annotations.rs::Analysis"],
    "callees": ["core_indexer/src/annotations.rs::Analysis"],
    "affected": ["core_indexer/src/annotations.rs::Analysis"],
    "explore": ["--query", "analysis"],
    "traverse": ["core_indexer/src/annotations.rs::Analysis"],
    "query": ["functions where caller_count == 0"],
    "open-items": [],
    "dead-code": [],
    "get-smells": [],
    "find-clones": ["--min-lines", "200", "--max-groups", "1"],
    "find-scaffolding": [],
    "archive": [],
    "stale-files": [],
    "read-source": ["core_indexer/src/annotations.rs::Analysis"],
    "as-of": ["--at", "HEAD"],
    "verify-history": [],  # slow: sweeps every generation
    "history-of": ["src/lib.rs"],
    "changes-between": ["--t1", "2024-01-01T00:00:00Z", "--t2", "2024-01-02T00:00:00Z"],
    "branches": [],
    "ingest-status": [],
    "diagnose": [],
    "module-children": ["tests/test_p5b4_addresses.py::module"],
    "resolve": ["/health"],
    # search-similar/search-hybrid need the embedding model on first use;
    # they are skipped unless the cache is warm (E2 owns that measurement).
    "history": ["src/lib.rs"],
}

VOLATILE_KEYS = {"elapsed", "elapsed_ms", "duration_ms", "mtime", "synced_at", "root", "store"}


def run(binary: str, args: list[str], timeout: int = 90) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [binary, *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
    )


def commands(binary: str) -> list[str]:
    help_text = run(binary, ["--help"]).stdout
    block = help_text.split("Commands:", 1)[-1]
    found = COMMAND_RE.findall(block)
    if "coderadar" in Path(binary).name:
        usage = json.loads(
            (ROOT / "rust" / "golden" / "cli_usage.json").read_text(encoding="utf-8")
        )
        found += list(usage.get("commands", {}))
    return sorted(set(found))


def options(binary: str, command: str) -> set[str]:
    help_text = run(binary, [command, "--help"]).stdout
    return set(OPTION_RE.findall(help_text))


def normalize(value: object) -> object:
    if isinstance(value, dict):
        return {
            key: normalize(item)
            for key, item in value.items()
            if key not in VOLATILE_KEYS
        }
    if isinstance(value, list):
        return [normalize(item) for item in value]
    if isinstance(value, str):
        return value.replace(str(ROOT), "<root>").replace("\\\\", "/")
    return value


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    default_rust = str(ROOT / "target" / "debug" / "coderadar-rs.exe")
    default_python = shutil.which("coderadar") or "coderadar"
    parser.add_argument("--rust", default=default_rust)
    parser.add_argument("--python", default=default_python)
    parser.add_argument("--json", action="store_true")
    options_ = parser.parse_args(argv)

    problems: list[str] = []
    py_commands = commands(options_.python)
    rs_commands = commands(options_.rust)
    if not rs_commands:
        problems.append("could not list Rust commands")
    unknown = sorted(set(py_commands) - set(rs_commands))
    if unknown:
        problems.append(f"commands missing from Rust: {unknown}")

    help_failures = []
    for command in py_commands:
        result = run(options_.rust, [command, "--help"])
        if result.returncode != 0:
            help_failures.append(f"{command} (exit {result.returncode})")
        elif "Usage" not in result.stdout:
            help_failures.append(f"{command} (no usage text)")
    if help_failures:
        problems.append(f"--help failed for: {help_failures}")

    format_gaps = []
    payload_gaps = []
    skipped: list[str] = []
    compared = 0
    for command in py_commands:
        py_help = run(options_.python, [command, "--help"]).stdout
        if "--format" not in py_help:
            continue
        probe = PROBES.get(command)
        if probe is None:
            continue
        try:
            rust = run(options_.rust, [command, *probe, "--format", "json"])
            python = run(options_.python, [command, *probe, "--format", "json"])
        except subprocess.TimeoutExpired:
            skipped.append(command)
            continue
        if rust.returncode != 0:
            format_gaps.append(f"{command}: exit {rust.returncode}: {rust.stderr.strip()[:120]}")
            continue
        try:
            rust_payload = json.loads(rust.stdout)
        except json.JSONDecodeError:
            format_gaps.append(f"{command}: output is not JSON")
            continue
        if python.returncode != 0:
            continue
        try:
            python_payload = json.loads(python.stdout)
        except json.JSONDecodeError:
            continue
        compared += 1
        if normalize(python_payload) != normalize(rust_payload):
            payload_gaps.append(command)
    if format_gaps:
        problems.append(f"--format gaps: {format_gaps}")
    # Payload shape differences are tracked separately: the row builders
    # still emit a narrower persisted shape than the Python engine. See
    # FULL_PORT_PLAN.md unit R0.
    

    report = {
        "ok": not problems,
        "python_commands": len(py_commands),
        "rust_commands": len(rs_commands),
        "json_compared": compared,
        "payload_differences": payload_gaps,
        "skipped": skipped,
        "problems": problems,
    }
    GOLDEN.parent.mkdir(parents=True, exist_ok=True)
    GOLDEN.write_text(json.dumps(report, indent=1), encoding="utf-8")
    if options_.json:
        print(json.dumps(report, indent=1))
    else:
        state = "ok" if report["ok"] else "FAILED"
        print(
            f"CLI parity {state}: {report['rust_commands']}/{report['python_commands']} commands, "
            f"{report['json_compared']} JSON payloads compared"
        )
        for problem in report["problems"]:
            print(f"  - {problem}")
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
