"""End-to-end test of `coderadar mcp serve` over stdio using the mcp client.

Exercises the real protocol surface: initialize, tools/list, tools/call on a
sample of read-only and mutation tools. Records protocol-level issues.
"""
import asyncio, json, sys, time, os

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

SERVER = ["..", "..", ".venv", "Scripts", "coderadar.exe"]
SERVER = [r"D:\User\Documents\Python\CodeRadar\.venv\Scripts\coderadar.exe", "mcp", "serve"]

async def main():
    t0 = time.time()
    params = StdioServerParameters(command=SERVER[0], args=SERVER[1:], cwd=r"D:\User\Documents\Python\CodeRadar")
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            init = await session.initialize()
            print(f"[init] server={init.server_info.name} v{init.server_info.version}")
            tools = await session.list_tools()
            names = [t.name for t in tools.tools]
            print(f"[tools/list] {len(names)} tools in {time.time()-t0:.1f}s: {names}")
            issues = []
            for tool_name, args in [
                ("codegraph_explore", {"query": "mutation policy"}),
                ("codegraph_search", {"query": "clone detection", "top_k": 3}),
                ("codegraph_get_smells", {"strictness": "high"}),
                ("codegraph_query", {"query": "classes where name contains 'Mutation'"}),
                ("coderadar_rename", {"entity_id": r".\tests\cr_edit_tests\demo_billing.py::print_invoice",
                                       "new_name": "print_invoice_renamed", "dry_run": True}),
                ("coderadar_create_entity", {"file_path": r"tests\cr_edit_tests\demo_billing.py",
                                              "language": "python", "kind": "function",
                                              "name": "proto_probe", "body": "return 1", "dry_run": True}),
            ]:
                t = time.time()
                try:
                    res = await session.call_tool(tool_name, args)
                    text = res.content[0].text if res.content else ""
                    ms = int((time.time()-t)*1000)
                    head = text[:300].replace("\n", " | ")
                    print(f"[call {tool_name}] {ms}ms -> {head}")
                    if "Error" in text[:200] or "not found" in text[:200].lower():
                        issues.append((tool_name, head))
                except Exception as e:
                    ms = int((time.time()-t)*1000)
                    print(f"[call {tool_name}] {ms}ms EXCEPTION {type(e).__name__}: {str(e)[:200]}")
                    issues.append((tool_name, f"{type(e).__name__}: {e}"))
            print(f"\n[issues] {issues if issues else 'none'}")

asyncio.run(main())
