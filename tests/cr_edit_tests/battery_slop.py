"""AI-slop / scaffold-detection option matrix — closes the coverage gap in
the dogfood review (find_scaffolding was only ever run with defaults).

Creates slop demo files at runtime (never committed with secret-shaped
literals), indexes them, then exercises the FULL option surface:

  find_scaffolding: include_secrets=False/True, max_findings
  get_smells:       strictness strict/normal/loose, rule_id filter, entity_id scope
  dead_code:        min_confidence sweep, include_test_reachable
  find_clones:      min_lines/min_similarity variants (F1 crash check)

Read-only tools only. Demo files are deleted again at the end and the graph
re-synced.
"""
import sys, os, time, traceback

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

HERE = os.path.dirname(os.path.abspath(__file__))
LOG = open(os.path.join(HERE, "battery_slop_output.txt"), "w", encoding="utf-8")
def log(*a):
    s = " ".join(str(x) for x in a)
    print(s); LOG.write(s + "\n"); LOG.flush()

import coderadar
from coderadar.mcp import server as mcp

SLOP_PY = os.path.join(HERE, "demo_slop.py").replace("\\", "/")
TEMP_PY = os.path.join(HERE, "temp_slop_notes.py").replace("\\", "/")
OLD_PY  = os.path.join(HERE, "old_slop_backup.py").replace("\\", "/")

# Runtime-generated fake secrets (no secret-shaped literals in this file).
openai_like   = "sk-" + "x" * 32
github_like   = "ghp_" + "y" * 36
aws_like      = "AKIA" + "Z" * 16
slack_like    = "xoxb-" + "z" * 24

SLOP_SOURCE = f'''# AI slop demo — placeholder bodies, markers, fake secrets.
# TODO: implement me
# FIXME: this is WIP for Step 1 of Phase 2
# HACK: for now, in a real production system this would be cached

import os


def stub_one():
    pass


def stub_two():
    ...


def stub_unicode():
    …


def stub_raise():
    raise NotImplementedError


def slop_with_secret():
    # fake credentials, generated at runtime by the battery
    api_key = "{openai_like}"
    github_token = "{github_like}"
    aws = "{aws_like}"
    slack = "{slack_like}"
    return api_key, github_token, aws, slack


class SlopClass:
    def method_with_todo(self):
        # TODO: implement this later
        return 0  # type: ignore
'''

TEMP_SOURCE = "# temp_* filename detector fodder\n# WIP: phase 3 of the plan\nx = 1\n"
OLD_SOURCE = "# old_* filename detector fodder\nold_var = 2  # noqa\n"

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
    log(f"\n{'='*70}\n[{status}] {name} ({ms}ms)")
    log(str(out)[:1800])

# ── create + index slop files ──────────────────────────────────────────────
open(SLOP_PY, "w", encoding="utf-8").write(SLOP_SOURCE)
open(TEMP_PY, "w", encoding="utf-8").write(TEMP_SOURCE)
open(OLD_PY, "w", encoding="utf-8").write(OLD_SOURCE)

graph = coderadar.load(r".coderadar/store/coderadar.db", ".")
for f in (SLOP_PY, TEMP_PY, OLD_PY):
    mcp._update_file(graph, f.replace("/", "\\"), None)
log("slop files indexed")

# ── find_scaffolding option matrix ─────────────────────────────────────────
run("scaffolding (defaults)", mcp._find_scaffolding, False, 100)
run("scaffolding include_secrets=True", mcp._find_scaffolding, True, 100)
run("scaffolding max_findings=3", mcp._find_scaffolding, False, 3)
# scoped re-check: how many slop findings come from the demo folder itself?
out = mcp._find_scaffolding(True, 10000)
demo_lines = [l for l in out.splitlines() if "cr_edit_tests" in l]
log(f"\nscaffolding(secrets) findings targeting cr_edit_tests/: {len(demo_lines)}")
log("\n".join(demo_lines[:10]))

# ── get_smells option matrix (scoped to the slop file) ─────────────────────
SLOP_ENTITY = r".\tests\cr_edit_tests\demo_slop.py::slop_with_secret"
for strictness in ("strict", "normal", "loose"):
    run(f"get_smells strictness={strictness} (slop file)",
        mcp._get_smells, graph, SLOP_ENTITY, None, strictness)
run("get_smells rule_id=long-method (whole repo)",
    mcp._get_smells, graph, None, "long-method", "normal")
run("get_smells rule_id=deep-nesting entity scope",
    mcp._get_smells, graph, SLOP_ENTITY, "deep-nesting", "strict")

# ── dead_code option matrix ────────────────────────────────────────────────
for conf in (0.0, 0.3, 0.6, 0.9):
    run(f"dead_code min_confidence={conf}", mcp._dead_code, graph, conf, False, 15)
run("dead_code include_test_reachable=True (0.6)",
    mcp._dead_code, graph, 0.6, True, 15)

# ── find_clones option variants (F1 crash check at other params) ──────────
run("find_clones min_lines=5 sim=0.6", mcp._find_clones, graph, 5, 0.6, 20)
run("find_clones min_lines=30 sim=0.95", mcp._find_clones, graph, 30, 0.95, 20)

# ── cleanup ────────────────────────────────────────────────────────────────
for f in (SLOP_PY, TEMP_PY, OLD_PY):
    os.remove(f)
    mcp._update_file(graph, f.replace("/", "\\"), None)
log("\ncleanup: slop demo files removed, graph re-synced")
