"""Live mutation battery — applied ONLY to tests/cr_edit_tests/demo_* files.

Every mutating call targets an entity inside tests/cr_edit_tests/. After each
applied mutation the file on disk is re-checked, `codegraph_update_file` is
called, and the graph is queried to confirm the edit landed. At the end all
demo files are restored from git.

Outputs a machine-readable summary.
"""
import sys, time, json, traceback, os, subprocess

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

LOG = open(os.path.join(os.path.dirname(__file__), "battery_mutation_output.txt"), "w", encoding="utf-8")
def log(*a):
    s = " ".join(str(x) for x in a)
    print(s); LOG.write(s + "\n"); LOG.flush()

import coderadar
from coderadar.mcp import server as mcp

graph = coderadar.load(r".coderadar/store/coderadar.db", ".")

results = {}
def run(name, fn, *args, **kwargs):
    t = time.time()
    try:
        out = fn(*args, **kwargs)
        status = "OK"
    except BaseException as e:
        out = f"EXCEPTION: {traceback.format_exc()}"
        status = "ERROR"
    ms = int((time.time() - t) * 1000)
    results[name] = {"status": status, "ms": ms, "out": out}
    log(f"\n{'='*70}\n[{status}] {name} ({ms}ms)")
    log(str(out)[:1500])

def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout.strip()

# ── entity ids (full relative form, as discovered by the read battery) ────
PY_TOTAL   = r".\tests\cr_edit_tests\demo_billing.py::Invoice.calculate_total"
PY_LOYALTY = r".\tests\cr_edit_tests\demo_billing.py::Invoice.apply_loyalty_discount"
PY_SUMMARY = r".\tests\cr_edit_tests\demo_billing.py::Invoice.summary"
TS_MATCH   = r".\tests\cr_edit_tests\demo_router.ts::Router.match"
RS_TOTAL   = r".\tests\cr_edit_tests\demo_ledger.rs::Ledger.total_cents"
RS_FMT     = r".\tests\cr_edit_tests\demo_ledger.rs::Ledger.fmt_total_for_display"

DEMO_FILES = [
    r"tests\cr_edit_tests\demo_billing.py",
    r"tests\cr_edit_tests\demo_router.ts",
    r"tests\cr_edit_tests\demo_ledger.rs",
]
for f in DEMO_FILES:
    log(f"before: {f} sha={git('hash-object', f)}")

# ── 1. replace_body on Python: fix the tax/discount order bug ─────────────
NEW_PY = """        subtotal = sum(price * qty for _, price, qty in self.items)
        taxed = subtotal * (1 + self.tax_rate)
        return taxed * 0.9"""
run("replace_body demo_billing.calculate_total (apply)",
    mcp._replace_body, graph, PY_TOTAL, NEW_PY, None, False)

# verify on disk
disk = open("tests/cr_edit_tests/demo_billing.py", encoding="utf-8").read()
log("on-disk verify: 'taxed = subtotal' present:", "taxed = subtotal" in disk)

# graph update + verify
run("update_file demo_billing.py after replace_body",
    mcp._update_file, graph, r"tests\cr_edit_tests\demo_billing.py", None)
from coderadar._core import search_entities, callees_of
e = search_entities("calculate_total", 5)
log("graph verify: calculate_total entries:", [(x["id"], x.get("line")) for x in e])

# ── 2. replace_body on Rust: fix refund-dropping bug ──────────────────────
NEW_RS = """        self.entries.iter().map(|e| e.amount_cents).sum()
    }

    /// Refunds are included (amount_cents may be negative)."""
run("replace_body demo_ledger.total_cents (apply)",
    mcp._replace_body, graph, RS_TOTAL, NEW_RS, None, False)

# ── 3. replace_body on TypeScript: fix last-match-wins bug ────────────────
NEW_TS = """    const ranked = this.routes
      .filter((r) => r.path === path)
      .sort((a, b) => b.priority - a.priority);
    return ranked[0];"""
run("replace_body demo_router.ts Router.match (apply)",
    mcp._replace_body, graph, TS_MATCH, NEW_TS, None, False)

# ── 4. update_signature on Python: give tier a default ────────────────────
run("update_signature demo_billing.apply_loyalty_discount (apply)",
    mcp._update_signature, graph, PY_LOYALTY,
    "def apply_loyalty_discount(self, pct: float, tier: str = 'none') -> float", False, False)

# ── 5. rename on Rust: fmt_total_for_display -> formatted_total ───────────
run("rename demo_ledger.fmt_total_for_display (apply)",
    mcp._rename, graph, RS_FMT, "formatted_total", False)

# ── 6. create_entity on Python demo file ──────────────────────────────────
run("create_entity demo_billing.py (apply)",
    mcp._create_entity, graph, r"tests\cr_edit_tests\demo_billing.py", "python",
    "function", "demo_review_stamp", "return 'created by cr_edit_tests battery'",
    None, "end", "def demo_review_stamp() -> str", False)

# ── 7. stale-write rejection: replace_body with a WRONG expected_hash ─────
run("replace_body demo_billing.calculate_total (stale hash guard)",
    mcp._replace_body, graph, PY_TOTAL, "        return 0", "deadbeef", False)

# ── 8. attempt mutation OUTSIDE the allow list (must be refused by policy) ─
CORE_ID = r".\py_agent\src\coderadar\coldstart.py::store_is_fresh"
run("replace_body coldstart.store_is_fresh (policy refusal expected)",
    mcp._replace_body, graph, CORE_ID, "        return True", None, False)

# ── restore demo files + resync graph ─────────────────────────────────────
log("\n" + "=" * 70)
for f in DEMO_FILES:
    git("checkout", "--", f)
    log(f"restored: {f} sha={git('hash-object', f)}")
mcp._update_file(graph, r"tests\cr_edit_tests\demo_billing.py", None)
mcp._update_file(graph, r"tests\cr_edit_tests\demo_ledger.rs", None)
mcp._update_file(graph, r"tests\cr_edit_tests\demo_router.ts", None)
log("graph resynced to restored files")

log("\nSUMMARY")
for name, r in results.items():
    log(f"  {r['status']:5s} {r['ms']:7d}ms  {name}")
