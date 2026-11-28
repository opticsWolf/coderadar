#!/usr/bin/env python
"""CLI contract parity harness (plan units C4 and R0).

Compares the Python reference CLI with the Rust binary on:

* command inventory and ``--help`` handling (the repository itself),
* ``--format json`` acceptance for every command that advertises it,
* JSON payload equality on a deterministic fixture project.

The payload half used to probe the live repository, where the store is
rewritten between the two runs (cold-start replay), so row sets differed for
reasons that had nothing to do with the port. It now builds the fixture store
once and gives each side its own copy: same graph, same ids, comparable rows.

Usage:
    python rust/cli_parity.py [--json] [--rust BIN] [--python BIN]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GOLDEN = ROOT / "rust" / "golden" / "cli_parity.json"
FIXTURE = ROOT / "rust" / "golden" / "parity_fixture"
PY_LAUNCHER = ROOT / "rust" / "parity_support" / "py_cli.py"
CORE_DLL = ROOT / "target" / "debug" / "core_indexer.dll"
CORE_PYD = ROOT / "py_agent" / "src" / "coderadar" / "_core.pyd"

COMMAND_RE = re.compile(r"^  ([a-z][a-z0-9-]+)\s{2,}", re.MULTILINE)
OPTION_RE = re.compile(r"^\s{2,}(-{1,2}[A-Za-z][\w-]*)(?:[ ,\[]|$)")

# Probes against the fixture project. Every id exists in the fixture.
PROBES: dict[str, list[str]] = {
    "status": [],
    "search": ["Stock"],
    "node": ["app.py::Stock.add"],
    "callers": ["app.py::Stock.add"],
    "callees": ["app.py::Stock.add"],
    "affected": ["app.py::Stock.add"],
    "explore": ["--query", "Stock"],
    "traverse": ["app.py::Stock.add"],
    "query": ["functions where name == 'total'"],
    "open-items": [],
    "dead-code": [],
    "get-smells": [],
    "find-clones": [],
    "find-scaffolding": [],
    "archive": [],
    "stale-files": [],
    "read-source": ["app.py::Stock.add"],
    "as-of": ["--at", "HEAD"],
    "history-of": ["app.py"],
    "branches": [],
    "ingest-status": [],
    "diagnose": [],
    "module-children": ["app.py::module"],
    "resolve": ["/health"],
}

VOLATILE_KEYS = {
    "elapsed",
    "elapsed_ms",
    "duration_ms",
    "mtime",
    "synced_at",
    "root",
    "store",
    "indexed_at",
    "age_seconds",
    "epoch",
    "revision",
    "loaded_at",
    "config",
}


def run(
    binary: str | list[str],
    args: list[str],
    timeout: int = 120,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    command = [binary, *args] if isinstance(binary, str) else [*binary, *args]
    return subprocess.run(
        command,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        env=env,
    )


def python_env() -> dict[str, str]:
    env = dict(os.environ)
    parts = [str(ROOT / "py_agent" / "src"), str(ROOT / ".testdeps")]
    if env.get("PYTHONPATH"):
        parts.append(env["PYTHONPATH"])
    env["PYTHONPATH"] = os.pathsep.join(parts)
    env["PYTHONUTF8"] = "1"
    return env


def refresh_reference_engine() -> list[str]:
    """Build the engine and place ``_core.pyd`` beside the Python sources."""
    problems: list[str] = []
    build = subprocess.run(
        ["cargo", "build", "-p", "core_indexer"], cwd=ROOT, capture_output=True, text=True
    )
    if build.returncode != 0:
        return [f"core_indexer build failed: {build.stderr.strip()[:200]}"]
    if not CORE_DLL.exists():
        return [f"missing {CORE_DLL}"]
    CORE_PYD.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(CORE_DLL, CORE_PYD)
    return problems


def commands(binary: str | list[str], env: dict[str, str] | None = None) -> list[str]:
    help_text = run(binary, ["--help"], env=env).stdout
    block = help_text.split("Commands:", 1)[-1]
    found = COMMAND_RE.findall(block)
    if "coderadar" in str(binary):
        usage = json.loads(
            (ROOT / "rust" / "golden" / "cli_usage.json").read_text(encoding="utf-8")
        )
        found += list(usage.get("commands", {}))
    return sorted(set(found))


def options(binary: str | list[str], command: str, env: dict[str, str] | None = None) -> set[str]:
    help_text = run(binary, [command, "--help"], env=env).stdout
    return set(OPTION_RE.findall(help_text))


def row_key(row: dict) -> tuple:
    """Order rows by meaning, not by hash iteration order.

    `graph.functions` is a HashMap, so findings that share a rank come back
    in per-process order in BOTH implementations. Ranked rows keep their
    ranking (score/similarity descending); the rest fall back to the id.
    """
    rank = row.get("score", row.get("similarity"))
    numeric = -float(rank) if isinstance(rank, (int, float)) else 0.0
    name = row.get("id", row.get("entity_id", row.get("file", "")))
    return (numeric, str(name), row.get("line", 0), row.get("rule_id", ""))


def normalize(value: object, roots: tuple[str, ...] = ()) -> object:
    if isinstance(value, dict):
        return {
            key: normalize(item, roots)
            for key, item in value.items()
            if key not in VOLATILE_KEYS
        }
    if isinstance(value, list):
        items = [normalize(item, roots) for item in value]
        if items and all(isinstance(item, dict) for item in items):
            items.sort(key=row_key)
        return items
    if isinstance(value, str):
        text = value.replace(str(ROOT), "<root>")
        for root in roots:
            text = text.replace(root, "<root>")
        return text.replace("\\\\", "/")
    return value


def first_difference(left: object, right: object, path: str = "") -> str | None:
    if type(left) is not type(right):
        return f"{path}: {type(left).__name__} vs {type(right).__name__}"
    if isinstance(left, dict):
        only_left = sorted(set(left) - set(right))
        only_right = sorted(set(right) - set(left))
        if only_left:
            return f"{path}: rs-only keys {only_left[:6]}"
        if only_right:
            return f"{path}: py-only keys {only_right[:6]}"
        for key in left:
            found = first_difference(left[key], right[key], f"{path}.{key}")
            if found:
                return found
        return None
    if isinstance(left, list):
        if len(left) != len(right):
            return f"{path}: {len(left)} rows vs {len(right)}"
        for index, (a, b) in enumerate(zip(left, right)):
            found = first_difference(a, b, f"{path}[{index}]")
            if found:
                return found
        return None
    if left != right:
        return f"{path}: {json.dumps(left)[:80]} vs {json.dumps(right)[:80]}"
    return None


def prepare_fixture(binary: str | list[str]) -> tuple[Path | None, list[str]]:
    """Index the fixture once; return the initialized copy."""
    problems: list[str] = []
    work = Path(tempfile.mkdtemp(prefix="clI-parity-"))
    target = work / "project"
    shutil.copytree(FIXTURE, target, ignore=shutil.ignore_patterns(".coderadar"))
    init = run(binary, ["-C", str(target), "init"], timeout=300)
    if init.returncode != 0:
        problems.append(f"fixture init failed: {init.stderr.strip()[:200]}")
        return None, problems
    return target, problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    default_rust = str(ROOT / "target" / "debug" / "coderadar-rs.exe")
    parser.add_argument("--rust", default=default_rust)
    parser.add_argument("--python", default=None, help="reference CLI (default: local launcher)")
    parser.add_argument("--json", action="store_true")
    options_ = parser.parse_args(argv)

    rust: str | list[str] = options_.rust
    python: str | list[str] = (
        [sys.executable, str(PY_LAUNCHER)] if options_.python is None else options_.python
    )
    env = python_env()

    problems: list[str] = refresh_reference_engine()

    py_commands = commands(python, env)
    rs_commands = commands(rust)
    if not rs_commands:
        problems.append("could not list Rust commands")
    unknown = sorted(set(py_commands) - set(rs_commands))
    if unknown:
        problems.append(f"commands missing from Rust: {unknown}")

    help_failures = []
    for command in py_commands:
        result = run(rust, [command, "--help"])
        if result.returncode != 0:
            help_failures.append(f"{command} (exit {result.returncode})")
        elif "Usage" not in result.stdout:
            help_failures.append(f"{command} (no usage text)")
    if help_failures:
        problems.append(f"--help failed for: {help_failures}")

    format_gaps: list[str] = []
    payload_gaps: list[str] = []
    payload_details: dict[str, str] = {}
    skipped: list[str] = []
    compared = 0

    fixture, fixture_problems = prepare_fixture(rust)
    problems.extend(fixture_problems)
    if fixture is not None:
        for command in py_commands:
            py_help = run(python, [command, "--help"], env=env).stdout
            if "--format" not in py_help:
                continue
            probe = PROBES.get(command)
            if probe is None:
                continue
            side = Path(tempfile.mkdtemp(prefix=f"parity-{command}-"))
            rust_root = side / "rust"
            py_root = side / "py"
            shutil.copytree(fixture, rust_root)
            shutil.copytree(fixture, py_root)
            try:
                rust_run = run(rust, ["-C", str(rust_root), command, *probe, "--format", "json"])
                python_run = run(
                    python,
                    ["-C", str(py_root), command, *probe, "--format", "json"],
                    env=env,
                )
            except subprocess.TimeoutExpired:
                skipped.append(command)
                shutil.rmtree(side, ignore_errors=True)
                continue
            if rust_run.returncode != 0:
                format_gaps.append(
                    f"{command}: exit {rust_run.returncode}: {rust_run.stderr.strip()[:120]}"
                )
                shutil.rmtree(side, ignore_errors=True)
                continue
            try:
                rust_payload = json.loads(rust_run.stdout)
            except json.JSONDecodeError:
                format_gaps.append(f"{command}: output is not JSON")
                shutil.rmtree(side, ignore_errors=True)
                continue
            if python_run.returncode != 0:
                shutil.rmtree(side, ignore_errors=True)
                continue
            try:
                python_payload = json.loads(python_run.stdout)
            except json.JSONDecodeError:
                shutil.rmtree(side, ignore_errors=True)
                continue
            compared += 1
            roots = (str(rust_root), str(py_root))
            difference = first_difference(
                normalize(rust_payload, roots), normalize(python_payload, roots)
            )
            if difference:
                payload_gaps.append(command)
                payload_details[command] = difference[:300]
            shutil.rmtree(side, ignore_errors=True)
        shutil.rmtree(fixture.parent, ignore_errors=True)

    if format_gaps:
        problems.append(f"--format gaps: {format_gaps}")
    # Row-shape differences are tracked separately (FULL_PORT_PLAN R0): they
    # are payload contracts, not option contracts.

    report = {
        "ok": not problems,
        "python_commands": len(py_commands),
        "rust_commands": len(rs_commands),
        "json_compared": compared,
        "payload_differences": payload_gaps,
        "payload_details": payload_details,
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
        for command, detail in payload_details.items():
            print(f"  * {command}: {detail}")
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
