"""Dogfood review battery: run ALL 22 CodeRadar MCP tools against CodeRadar itself.

Mutation tools (coderadar_replace_body / update_signature / rename /
create_entity) are applied with dry_run=True for entities across the repo,
and with dry_run=False ONLY against tests/cr_edit_tests/demo_* files, which
are restored via git checkout afterwards.

Writes a machine-readable summary to stdout at the end.
"""
import os
import sys
import time
import traceback

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:  # noqa: BLE001, S110 - best-effort console setup for Windows pipes
    pass

LOG = open(  # noqa: SIM115 - module-global run log, lives until process exit
    os.path.join(os.path.dirname(__file__), "battery_output.txt"), "w", encoding="utf-8"
)
def log(*a):
    s = " ".join(str(x) for x in a)
    print(s); LOG.write(s + "\n"); LOG.flush()

import coderadar
from coderadar.mcp import server as mcp

log("=" * 70)
log("LOADING GRAPH (cold start)")
t0 = time.time()
graph = coderadar.load(r".coderadar/store/coderadar.db", ".")
log(f"loaded in {time.time()-t0:.2f}s")

results = {}
def run(name, fn, *args, **kwargs):
    t = time.time()
    try:
        out = fn(*args, **kwargs)
        status = "OK"
    except BaseException:  # noqa: BLE001 - PanicException is BaseException, not Exception
        out = f"EXCEPTION: {traceback.format_exc()}"
        status = "ERROR"
    ms = int((time.time() - t) * 1000)
    results[name] = {"status": status, "ms": ms, "out": out}
    log(f"\n{'='*70}\n[{status}] {name} ({ms}ms)")
    log(str(out)[:1000])

# ── discover entity ids ───────────────────────────────────────────────────
from coderadar._core import search_entities


def first(q, kind=None):
    r = search_entities(q, 3, kind)
    return r[0] if r else None

demo_py  = first("calculate_total")
demo_ts  = first("dispatch", "method")
demo_rs  = first("total_cents")
core_fn  = first("_ensure_graph")
any_file = (demo_py or {}).get("file_path", r".\tests\cr_edit_tests\demo_billing.py")
demo_py_id  = (demo_py or {}).get("id", "")
demo_ts_id  = (demo_ts or {}).get("id", "")
demo_rs_id  = (demo_rs or {}).get("id", "")
core_id     = (core_fn or {}).get("id", "")
log(f"demo_py_id={demo_py_id!r}\ndemo_ts_id={demo_ts_id!r}\ndemo_rs_id={demo_rs_id!r}\ncore_id={core_id!r}")

# ── 1-15: read-only tools ─────────────────────────────────────────────────
run("codegraph_explore",        mcp._explore, graph, "cold start store load", [], "both", 4)
run("codegraph_node",           mcp._node_detail, graph, core_id, True)
run("codegraph_search",         mcp._search, graph, "mutation plan", None, 5)
run("codegraph_affected",       mcp._affected, graph, core_id, 3)
run("coderadar_resolve",        mcp._resolve_ref, graph, "/users/:id", 5)
run("codegraph_query",          mcp._query_graph, graph, "functions where name contains 'parse'")
run("codegraph_search_similar", mcp._search_similar, graph, "incremental file update", 5)
run("codegraph_module_children",mcp._module_children, graph, r".\py_agent\src\coderadar::module")
run("codegraph_as_of",          mcp._as_of, graph, "2025-01-15T10:00:00Z", "", [])
run("codegraph_traverse",       mcp._traverse, graph, core_id, "both", ["calls"], 2)
run("codegraph_get_smells",     mcp._get_smells, graph, None, None, "normal")
run("codegraph_dead_code",      mcp._dead_code, graph, 0.6, False, 100)
run("codegraph_find_clones",    mcp._find_clones, graph, 10, 0.8, 100)
run("codegraph_find_scaffolding", mcp._find_scaffolding, False, 100)
run("codegraph_update_file",    mcp._update_file, graph, any_file, None)

# ── 16: mutation tools — DRY RUN on core repo entity (must refuse or plan) ─
run("coderadar_replace_body[core,dry]",    mcp._replace_body, graph, core_id, "pass", None, True)
run("coderadar_update_signature[core,dry]",mcp._update_signature, graph, core_id, "def _ensure_graph(path: str)", False, True)
run("coderadar_rename[core,dry]",          mcp._rename, graph, core_id, "_ensure_graph_renamed", True)

# ── 17: create_entity — DRY RUN on demo file ──────────────────────────────
run("coderadar_create_entity[demo,dry]", mcp._create_entity, graph,
    any_file, "python", "function", "demo_review_helper", "return 42", None, "end", None, True)

# ── 18: compute embeddings (slow, optional) ───────────────────────────────
run("codegraph_compute_embeddings", mcp._compute_embeddings, graph)

# ── summary ───────────────────────────────────────────────────────────────
log("\n" + "=" * 70)
log("SUMMARY")
for name, r in results.items():
    log(f"  {r['status']:5s} {r['ms']:7d}ms  {name}")
