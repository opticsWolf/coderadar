#!/usr/bin/env python3
"""Clean-machine smoke test for the release binary (unit R3).

Copies the golden fixture into a fresh directory, then exercises the surfaces
an install has to get right — `--version`, `init`, `status --format json`,
`query`, and an MCP `tools/list` handshake — using only the system `PATH`, so a
missing Python runtime or a stray developer environment cannot mask a problem.
Finally it checks that the artifact references no Python DLL.

Usage::

    python rust/smoke.py                       # target/release/coderadar
    python rust/smoke.py --bin /path/to/binary --keep
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
FIXTURE = ROOT / "rust" / "golden" / "parity_fixture"

sys.path.insert(0, str(Path(__file__).resolve().parent))
from binaries import release_binary  # noqa: E402


def system_path() -> str:
    """A PATH with no developer tooling beyond the OS itself."""
    if os.name == "nt":
        return r"C:\Windows\system32;C:\Windows"
    return "/usr/bin:/bin"


def run(binary: Path, cwd: Path, argv: list[str], timeout: int = 300) -> subprocess.CompletedProcess[str]:
    env = {
        "PATH": system_path(),
        "TEMP": os.environ.get("TEMP", str(cwd)),
        "TMP": os.environ.get("TMP", str(cwd)),
        "HOME": str(cwd),
        "USERPROFILE": str(cwd),
        "CODERADAR_DETERMINISTIC_EMBEDDINGS": "1",
        "CODERADAR_PARENT_WATCHDOG": "0",
    }
    return subprocess.run(
        [str(binary), *argv],
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        timeout=timeout,
        check=False,
    )


def dll_references(binary: Path) -> list[str]:
    data = binary.read_bytes()
    return sorted(
        {match.group(0).decode("ascii").lower() for match in re.finditer(rb"[A-Za-z0-9_.-]{2,60}\.dll", data)}
    )


def mcp_handshake(binary: Path, cwd: Path) -> tuple[bool, str]:
    """Initialize and list tools over stdio, without the Python SDK."""
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "1"},
        }},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    ]
    payload = "\n".join(json.dumps(request) for request in requests) + "\n"
    env = {
        "PATH": system_path(),
        "HOME": str(cwd),
        "USERPROFILE": str(cwd),
        "CODERADAR_PARENT_WATCHDOG": "0",
    }
    proc = subprocess.run(
        [str(binary), "mcp", "serve"],
        cwd=cwd,
        input=payload,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        timeout=120,
        check=False,
    )
    tools: list[str] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("id") == 2:
            tools = [tool["name"] for tool in message.get("result", {}).get("tools", [])]
    if not tools:
        return False, f"no tools/list response (stderr: {proc.stderr.strip()[:200]})"
    return True, f"{len(tools)} tools"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", default=None, help="binary under test (default: release build)")
    parser.add_argument("--keep", action="store_true", help="keep the temporary project")
    options = parser.parse_args(argv)

    binary = Path(options.bin) if options.bin else release_binary()
    if not binary.exists():
        print(f"FAILED: {binary} is not built (cargo build --release -p coderadar)")
        return 1

    work = Path(tempfile.mkdtemp(prefix="coderadar-smoke-"))
    project = work / "project"
    shutil.copytree(FIXTURE, project, ignore=shutil.ignore_patterns(".coderadar"))

    problems: list[str] = []

    def check(name: str, ok: bool, detail: str = "") -> None:
        suffix = f" - {detail}" if detail else ""
        print(f"{'pass' if ok else 'FAIL'}  {name}{suffix}")
        if not ok:
            problems.append(f"{name}: {detail}")

    version = run(binary, work, ["--version"])
    check("--version", version.returncode == 0 and version.stdout.startswith("coderadar "), version.stdout.strip() or version.stderr.strip()[:120])

    init = run(binary, project, ["init"])
    check("init", init.returncode == 0 and (project / ".coderadar" / "store").exists(), init.stderr.strip()[:160])

    status = run(binary, project, ["status", "--format", "json"])
    loaded = False
    try:
        loaded = json.loads(status.stdout).get("loaded") is True
    except json.JSONDecodeError:
        loaded = False
    check("status --format json", status.returncode == 0 and loaded, status.stderr.strip()[:160])

    query = run(binary, project, ["query", "functions", "--format", "json"])
    rows: object = []
    try:
        rows = json.loads(query.stdout)
    except json.JSONDecodeError:
        rows = []
    check("query functions", query.returncode == 0 and isinstance(rows, list) and bool(rows), f"{len(rows) if isinstance(rows, list) else 0} rows")

    ok, detail = mcp_handshake(binary, project)
    check("mcp tools/list", ok, detail)

    references = dll_references(binary)
    python_refs = [name for name in references if "python" in name]
    check("no Python DLL", not python_refs, ", ".join(python_refs) if python_refs else f"{len(references)} system DLL strings")

    if not options.keep:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print(f"kept {work}")

    if problems:
        print(f"\nSmoke FAILED: {len(problems)} problem(s)")
        return 1
    print("\nSmoke ok: release binary runs a project end to end without Python")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
