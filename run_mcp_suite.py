"""Run all 17 CodeRadar MCP tools against a target codebase and record results."""
import sys, time, json, traceback

# Force UTF-8 output (Windows console is cp1252 and mangles Unicode)
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

LOG = open("mcp_suite_output.txt", "w", encoding="utf-8")
def log(*a):
    s = " ".join(str(x) for x in a)
    print(s)
    LOG.write(s + "\n")
    LOG.flush()

TARGET = r"D:\User\Documents\Python\codegraph-main"

import coderadar
from coderadar._core import graph_stats, search_entities

# ── 0. Build the graph (what `coderadar mcp serve` does at startup) ──────
log("=" * 70)
log("INDEXING", TARGET)
t0 = time.time()
graph = coderadar.analyze(TARGET)
log(f"indexed in {time.time()-t0:.1f}s")
stats = graph_stats()
log("graph stats:", json.dumps(stats))

from coderadar.mcp import server as mcp

results = {}
def run(name, fn, *args, **kwargs):
    t = time.time()
    try:
        out = fn(*args, **kwargs)
        status = "OK"
    except Exception as e:
        out = f"EXCEPTION: {e}"
        status = "ERROR"
    dt = time.time() - t
    results[name] = {"status": status, "ms": round(dt*1000), "out": out}
    log(f"\n{'='*70}\n[{status}] {name} ({dt*1000:.0f}ms)")
    log(out[:1200])

# ── Discover some entities for the ID-based tools ─────────────────────────
def first_id(q, kind=None):
    r = search_entities(q, 3, kind)
    return r[0] if r else None

idx = first_id("index") or first_id("Index")
fn = first_id("parse") or first_id("Parse")
cls = first_id("Token") or first_id("db")
any_e = idx or fn or cls
any_id = any_e.get("id") if any_e else None
any_file = any_e.get("file_path") if any_e else None
log("\nDiscovered:", json.dumps(any_e, default=str) if any_e else "NONE")

# ── 1. codegraph_explore ──────────────────────────────────────────────────
run("codegraph_explore", mcp._explore, graph, "index parse token", [], "both", 4)

# ── 2. codegraph_node ─────────────────────────────────────────────────────
run("codegraph_node", mcp._node_detail, graph, any_id or "::", True)

# ── 3. codegraph_search ───────────────────────────────────────────────────
run("codegraph_search", mcp._search, graph, "parse", None, 5)

# ── 4. codegraph_affected ─────────────────────────────────────────────────
run("codegraph_affected", mcp._affected, graph, any_id or "::", 3)

# ── 5. coderadar_resolve ──────────────────────────────────────────────────
run("coderadar_resolve", mcp._resolve_ref, graph, "db/index", 5)

# ── 6. codegraph_query ────────────────────────────────────────────────────
run("codegraph_query", mcp._query_graph, graph, "functions where name contains 'parse'")

# ── 7. codegraph_compute_embeddings ───────────────────────────────────────
run("codegraph_compute_embeddings", mcp._compute_embeddings, graph)

# ── 8. codegraph_search_similar ───────────────────────────────────────────
run("codegraph_search_similar", mcp._search_similar, graph, "indexing source files", 5)

# ── 9. codegraph_module_children ──────────────────────────────────────────
module_id = f"{any_file}::module" if any_file else "::module"
run("codegraph_module_children", mcp._module_children, graph, module_id)

# ── 10. codegraph_as_of ───────────────────────────────────────────────────
run("codegraph_as_of", mcp._as_of, graph, "2025-01-15T10:00:00Z", "", [])

# ── 11. codegraph_traverse ────────────────────────────────────────────────
run("codegraph_traverse", mcp._traverse, graph, any_id or "::", "both", ["calls"], 2)

# ── 12. coderadar_replace_body (dry_run) ─────────────────────────────────
run("coderadar_replace_body", mcp._replace_body, graph, any_id or "::", "return null", None, True)

# ── 13. coderadar_update_signature (dry_run) ─────────────────────────────
run("coderadar_update_signature", mcp._update_signature, graph, any_id or "::", "parse(x: number): void", False, True)

# ── 14. coderadar_rename (dry_run) ────────────────────────────────────────
run("coderadar_rename", mcp._rename, graph, any_id or "::", "renamed_parse", True)

# ── 15. coderadar_create_entity (dry_run) ────────────────────────────────
run("coderadar_create_entity", mcp._create_entity, graph,
    any_file or "src/__test__.ts", "typescript", "function", "mcpTestFn", "return 1", None, "end", True)

# ── 16. codegraph_reindex ─────────────────────────────────────────────────
run("codegraph_reindex", mcp._reindex, graph, False)

# ── 17. codegraph_update_file ─────────────────────────────────────────────
run("codegraph_update_file", mcp._update_file, graph, any_file or "src/__test__.ts", None)

# ── Summary ───────────────────────────────────────────────────────────────
log("\n" + "=" * 70)
log("SUMMARY")
for name, r in results.items():
    log(f"  {r['status']:5s}  {r['ms']:7d}ms  {name}")
log(f"\nTotal: {len(results)} tools")
