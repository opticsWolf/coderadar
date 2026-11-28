#!/usr/bin/env python
"""MCP conformance check for the Rust server (plan unit M4).

Spawns `coderadar-rs mcp serve`, walks the handshake, validates the protocol
shape a client SDK enforces, and exits non-zero on the first violation.

Usage:
    python rust/mcp_conformance.py [--bin PATH] [--json]
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SHARED_TOOLS = ROOT / "rust" / "golden" / "mcp_tools.json"
PROTOCOLS = {"2025-06-18", "2025-03-26", "2024-11-05"}


class Client:
    def __init__(self, binary: Path, cwd: Path) -> None:
        env = {
            **os.environ,
            "CODERADAR_MCP_PATH": str(cwd),
            "CODERADAR_PARENT_WATCHDOG": "0",
            "CODERADAR_ROOTS_TIMEOUT_MS": "200",
            "CODERADAR_HANDSHAKE_TIMEOUT": "30",
        }
        self.proc = subprocess.Popen(
            [str(binary), "mcp", "serve"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            cwd=cwd,
            env=env,
        )
        self.next_id = 1

    def send(self, method: str, params: dict | None = None) -> int:
        request_id = self.next_id
        self.next_id += 1
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()
        return request_id

    def reply(self, request_id: int) -> dict:
        assert self.proc.stdout is not None
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError(f"server closed before replying to {request_id}")
            message = json.loads(line)
            if message.get("id") == request_id:
                return message
            # Server-to-client requests (roots/list) are answered by the
            # server itself; anything else out of band is ignored.

    def call(self, method: str, params: dict | None = None) -> dict:
        return self.reply(self.send(method, params))

    def close(self) -> None:
        self.proc.kill()
        self.proc.wait(timeout=10)


def fail(problems: list[str], message: str) -> None:
    problems.append(message)


def check_schema(problems: list[str], tool: dict) -> None:
    name = tool.get("name")
    if not isinstance(tool.get("description"), str):
        fail(problems, f"{name}: description is not a string")
    schema = tool.get("inputSchema")
    if not isinstance(schema, dict):
        fail(problems, f"{name}: inputSchema is not an object")
        return
    if schema.get("type") != "object":
        fail(problems, f"{name}: inputSchema.type is not 'object'")
    properties = schema.get("properties")
    if properties is not None and not isinstance(properties, dict):
        fail(problems, f"{name}: inputSchema.properties is not an object")
        return
    for required in schema.get("required", []):
        if required not in (properties or {}):
            fail(problems, f"{name}: required `{required}` is not a property")


def run(binary: Path, cwd: Path) -> dict:
    problems: list[str] = []
    client = Client(binary, cwd)
    try:
        init = client.call(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "conformance", "version": "1"},
            },
        )
        if "error" in init:
            fail(problems, f"initialize failed: {init['error']}")
        result = init.get("result", {})
        if result.get("protocolVersion") not in PROTOCOLS:
            fail(problems, f"initialize: bad protocolVersion {result.get('protocolVersion')!r}")
        info = result.get("serverInfo", {})
        if info.get("name") != "CodeRadar":
            fail(problems, f"initialize: serverInfo.name is {info.get('name')!r}")
        if not isinstance(info.get("version"), str):
            fail(problems, "initialize: serverInfo.version is not a string")
        capabilities = result.get("capabilities", {})
        for expected in ("tools", "resources"):
            if expected not in capabilities:
                fail(problems, f"initialize: capabilities.{expected} missing")
        if not isinstance(result.get("instructions"), str):
            fail(problems, "initialize: instructions missing")

        listed = client.call("tools/list")
        tools = listed.get("result", {}).get("tools", [])
        if "error" in listed:
            fail(problems, f"tools/list failed: {listed['error']}")
        if not tools:
            fail(problems, "tools/list returned no tools")
        names = []
        for tool in tools:
            if not isinstance(tool, dict):
                fail(problems, f"tools/list entry is {type(tool).__name__}, not an object")
                continue
            names.append(tool.get("name"))
            check_schema(problems, tool)
        if len(names) != len(set(names)):
            fail(problems, "tools/list has duplicate names")
        golden = json.loads(SHARED_TOOLS.read_text(encoding="utf-8"))["tools"]
        missing = sorted({tool["name"] for tool in golden} - set(names))
        if missing:
            fail(problems, f"tools/list is missing shared tools: {missing}")

        resources = client.call("resources/list")
        rows = resources.get("result", {}).get("resources", [])
        if len(rows) < 3:
            fail(problems, f"resources/list returned {len(rows)} resources")
        for row in rows:
            uri = row.get("uri")
            read = client.call("resources/read", {"uri": uri})
            contents = read.get("result", {}).get("contents", [])
            if not contents or not contents[0].get("text"):
                fail(problems, f"resources/read {uri!r} returned no text")

        unknown = client.call("tools/call", {"name": "coderadar_not_a_tool", "arguments": {}})
        if "error" in unknown:
            fail(problems, "unknown tool returned a JSON-RPC error instead of isError")
        elif unknown.get("result", {}).get("isError") is not True:
            fail(problems, "unknown tool did not set isError")

        ping = client.call("ping")
        if "error" in ping:
            fail(problems, f"ping failed: {ping['error']}")
    finally:
        client.close()
    return {
        "ok": not problems,
        "problems": problems,
        "tools": len(names),
        "resources": len(rows) if "rows" in locals() else 0,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", default=str(ROOT / "target" / "debug" / "coderadar-rs.exe"))
    parser.add_argument("--cwd", default=str(ROOT))
    parser.add_argument("--json", action="store_true")
    options = parser.parse_args(argv)
    report = run(Path(options.bin), Path(options.cwd))
    if options.json:
        print(json.dumps(report, indent=1))
    else:
        state = "ok" if report["ok"] else "FAILED"
        print(f"MCP conformance {state}: {report['tools']} tools, {report['resources']} resources")
        for problem in report["problems"]:
            print(f"  - {problem}")
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
