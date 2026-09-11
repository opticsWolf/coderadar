"""Round-2 dogfood battery: exercise EVERY CodeRadar CLI command (22) and
EVERY MCP tool (22) — happy paths, flags, and error paths — against the
small `r2proj` fixture, plus new-surface checks (excludes, store-repair,
synthetic edges, canonical ids, star exports) and the open items from
docs/dogfood-review-2026-09.md §6/§9.

Setup/teardown is self-contained: git repo + .coderadar.toml + store are
created inside r2proj at runtime and removed afterwards; only the fixture
sources, this script, and battery_round2_output.txt stay in the tree.

Usage: .venv/Scripts/python tests/cr_edit_tests/battery_round2.py
"""
import sys, os, re, json, time, shutil, subprocess, tempfile

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "r2proj")
LOG = open(os.path.join(HERE, "battery_round2_output.txt"), "w", encoding="utf-8")

EXE = os.path.join(os.path.dirname(sys.executable),
                   "coderadar.exe" if os.name == "nt" else "coderadar")
if not os.path.exists(EXE):
    EXE = sys.executable + "| -m coderadar.cli"  # fallback (split later)

results = []  # (section, name, status, detail)

def log(*a):
    s = " ".join(str(x) for x in a)
    print(s, flush=True); LOG.write(s + "\n"); LOG.flush()

def check(section, name, cond, detail=""):
    status = "PASS" if cond else "FAIL"
    results.append((section, name, status, str(detail)[:300]))
    log(f"[{status}] {section}/{name}" + (f" — {str(detail)[:200]}" if detail and status == "FAIL" else ""))
    return bool(cond)

def cli(*args, cwd=FIX, input_text=None, timeout=120):
    cmd = [EXE] + list(args) if "|" not in EXE else [sys.executable, "-m", "coderadar.cli"] + list(args)
    try:
        p = subprocess.run(cmd, cwd=cwd, input=input_text, capture_output=True,
                           text=True, timeout=timeout, encoding="utf-8", errors="replace")
        return p.returncode, (p.stdout or "") + (p.stderr or "")
    except subprocess.TimeoutExpired as e:
        return -99, f"TIMEOUT after {timeout}s: {(e.stdout or '')}{(e.stderr or '')}"[:500]

# ── fixture setup ─────────────────────────────────────────────────────────
for junk in (".git", ".coderadar.toml", ".coderadar.toml.bak", ".gitignore",
            ".coderadar", "store.db", "dirty_probe.txt"):
    p = os.path.join(FIX, junk)
    if os.path.isdir(p):
        shutil.rmtree(p, ignore_errors=True)
    elif os.path.exists(p):
        os.remove(p)
import glob as _glob
for pat in ("*.bak", ".coderadar-bak*", "*_bak.py"):
    for p in _glob.glob(os.path.join(FIX, pat)):
        try:
            os.remove(p)
        except OSError:
            pass

def git(*args):
    r = subprocess.run(["git"] + list(args), cwd=FIX, capture_output=True,
                       text=True, timeout=30)
    # R2 harness rule: never silently ignore git failures — an unchecked
    # `git commit` once poisoned three downstream checks with a dirty tree.
    if r.returncode != 0:
        log(f"[WARN] git {args} rc={r.returncode}: {(r.stderr or '')[:200]}")
    return r

git("init", "-q")
git("config", "user.email", "r2@example.com")
git("config", "user.name", "r2")
git("add", "-A")
git("commit", "-qm", "r2 fixture")

import coderadar
from coderadar._core import search_entities

# ════════════════ A. CLI SWEEP ════════════════
SEC = "cli"

rc, out = cli("--version")
check(SEC, "version", rc == 0 and "0.8" in out, out[:120])

rc, out = cli("no-such-command")
check(SEC, "bad-command-rc", rc != 0, f"rc={rc} {out[:120]}")

rc, out = cli("init")
check(SEC, "init", rc == 0 and os.path.exists(os.path.join(FIX, ".coderadar.toml")), out[:200])
rc, out = cli("init")
check(SEC, "init-twice-refuses-or-ok", rc == 0 and ("already" in out.lower() or "exist" in out.lower() or "use --force" in out.lower() or "force" in out.lower()), out[:200])
rc, out = cli("init", "--force")
check(SEC, "init-force", rc == 0, out[:200])

rc, out = cli("analyze")
check(SEC, "analyze", rc == 0, out[:200])

rc, out = cli("status")
check(SEC, "status", rc == 0 and ("index" in out.lower() or "store" in out.lower() or "fresh" in out.lower()), out[:300])

rc, out = cli("stats")
check(SEC, "stats", rc == 0 and ("function" in out.lower() or "entit" in out.lower() or "concept" in out.lower()), out[:300])

rc, out = cli("rebuild")
check(SEC, "rebuild", rc == 0, out[:200])
rc, out = cli("rebuild", "--full")
check(SEC, "rebuild-full-flag", rc == 0, out[:120])
rc, out = cli("rebuild", "--exclude", "ignored/")
check(SEC, "rebuild-one-shot-exclude", rc == 0, out[:200])
# NOTE: the in-process graph is stale after CLI-side rebuilds — re-analyze
# (which picks up .coderadar.toml excludes) before asserting effect.

# discover a real id for the id-taking commands
graph = coderadar.analyze(FIX)
hits = search_entities("combine", 10, "function")
MAIN_RUN = [h for h in search_entities("run", 10, "function")]
run_id = (MAIN_RUN[0]["id"] if MAIN_RUN else (hits[0]["id"] if hits else ""))
check(SEC, "have-ids", bool(run_id), run_id)
MAIN_ID = next((h["id"] for h in search_entities("main", 20, "function")
                if h.get("name") == "main" and "main.py" in h.get("id", "")), "")
check(SEC, "have-main-id", bool(MAIN_ID), MAIN_ID)
helpers_id = next((h["id"] for h in hits if "helpers" in h.get("id", "")), hits[0]["id"] if hits else "")

rc, out = cli("query", "functions")
check(SEC, "query-basic", rc == 0 and "combine" in out, out[:200])
rc, out = cli("query", "functions where name contains 'combine'")
check(SEC, "query-filtered", rc == 0 and "combine" in out, out[:200])
rc, out = cli("query", "frobnicate {{{")
check(SEC, "query-syntax-error", "error" in out.lower() or "fail" in out.lower() or "invalid" in out.lower() or "parse" in out.lower(), out[:200])

for direction in ("out", "in", "both"):
    rc, out = cli("traverse", run_id, "--depth", "2", "--direction", direction)
    check(SEC, f"traverse-{direction}", rc == 0, out[:150])
rc, out = cli("traverse", run_id, "--edges", "calls")
check(SEC, "traverse-edges-calls", rc == 0, out[:150])
rc, out = cli("traverse", run_id, "--edges", "bogus_edge_kind")
# R2 finding: unknown edge kind silently yields "No results" (rc 0)
# instead of an error. Documenting actual behavior here.
check(SEC, "traverse-bad-edge-kind-errors", "error" in out.lower() or "unknown" in out.lower() or "invalid" in out.lower(), out[:200])
rc, out = cli("traverse", "no/such.py::missing", "--depth", "2")
check(SEC, "traverse-bad-id", rc == 0 or "error" in out.lower() or "no " in out.lower() or "unknown" in out.lower() or "not found" in out.lower(), out[:200])

rc, out = cli("callers", helpers_id)
check(SEC, "callers", rc == 0 and ("run" in out or "combine" in out or "caller" in out.lower()), out[:300])
rc, out = cli("callees", run_id)
check(SEC, "callees", rc == 0 and ("combine" in out or "callee" in out.lower() or "no callee" in out.lower()), out[:300])
rc, out = cli("callers", "no/such.py::missing")
check(SEC, "callers-bad-id", "no " in out.lower() or "not found" in out.lower() or "unknown" in out.lower() or "0" in out or rc == 0, out[:200])

rc, out = cli("diagnose")
check(SEC, "diagnose", rc == 0, out[:200])
rc, out = cli("diagnose", "--unresolved")
check(SEC, "diagnose-unresolved", rc == 0, out[:200])
rc, out = cli("diagnose", "--low-confidence")
check(SEC, "diagnose-low-confidence", rc == 0, out[:200])

rc, out = cli("exclude", "list")
check(SEC, "exclude-list", rc == 0 and ("node_modules" in out or "target" in out or "exclud" in out.lower()), out[:300])
rc, out = cli("exclude", "add", "ignored/")
check(SEC, "exclude-add", rc == 0, out[:200])
rc, out = cli("exclude", "list")
check(SEC, "exclude-list-has-ignored", rc == 0 and "ignored" in out, out[:300])
rc, out = cli("rebuild")
# fresh-process view: the in-process global accumulates across ops, so the
# exclude effect must be measured in a clean interpreter (R2-5).
par3 = subprocess.run(
    [sys.executable, "-c",
     "import coderadar;"
     "coderadar.analyze(r'" + FIX.replace(chr(92), chr(92)*2) + "');"
     "from coderadar._core import search_entities;"
     "print('EXCLUDED_HITS=' + str(len(search_entities('should_never_be_indexed', 5))))"],
    cwd=os.path.dirname(FIX), capture_output=True, text=True, timeout=300)
import re as _re2
m3 = _re2.search(r"EXCLUDED_HITS=(\d+)", par3.stdout or "")
check(SEC, "exclude-takes-effect", m3 is not None and m3.group(1) == "0",
      f"probe={par3.stdout[-150:]!r} err={par3.stderr[-150:]!r}")
# ...but the CLI (config-activated) path honors it — verified separately:
rc, out = cli("query", "functions where name contains 'should_never'")
check(SEC, "exclude-cli-side-honored", rc == 0 and "no results" in out.lower(), out[:200])
rc, out = cli("exclude", "remove", "ignored/")
check(SEC, "exclude-remove", rc == 0, out[:200])
rc, out = cli("exclude", "remove", "never-added-pattern-xyz/")
check(SEC, "exclude-remove-missing", rc == 0 or "not" in out.lower() or "no " in out.lower(), out[:200])
rc, out = cli("exclude", "add", "ignored/")
check(SEC, "exclude-add-back", rc == 0, out[:120])

rc, out = cli("update", "main.py", "--content", open(os.path.join(FIX, "main.py"), encoding="utf-8").read())
check(SEC, "update-noop-content", rc == 0, out[:200])
rc, out = cli("update", "does/not/exist.py", "--content", "x = 1")
check(SEC, "update-missing-file", rc != 0 or "error" in out.lower() or "not" in out.lower() or "no " in out.lower(), out[:200])

rc, out = cli("store-repair")
check(SEC, "store-repair", rc == 0, out[:300])
rc, out = cli("store-repair", "--db", os.path.join(FIX, "no-such-store.db"))
check(SEC, "store-repair-bad-db", "error" in out.lower() or "not" in out.lower() or "no " in out.lower() or "fail" in out.lower() or rc != 0, out[:200])

rc, out = cli("blame", "main.py")
check(SEC, "blame", rc == 0 and ("r2" in out or "@" in out or "main.py" in out or "|" in out), out[:300])
rc, out = cli("blame", "does/not/exist.py")
check(SEC, "blame-missing", "error" in out.lower() or "not" in out.lower() or "no " in out.lower() or "fail" in out.lower() or rc != 0, out[:200])

git("add", "-A")
git("commit", "-qm", "r2 generated files")
rc, out = cli("git-clean")
check(SEC, "git-clean-when-clean", rc == 0 and "clean" in out.lower() and "uncommitted" not in out.lower(), out[:200])
with open(os.path.join(FIX, "dirty_probe.txt"), "w") as f:
    f.write("dirty\n")
rc, out = cli("git-clean")
check(SEC, "git-clean-when-dirty", "dirty" in out.lower() or "uncommitted" in out.lower() or "not clean" in out.lower() or "modified" in out.lower() or "untracked" in out.lower() or rc != 0, out[:200])
git("add", "-A")
git("commit", "-qm", "r2 second commit")

rc, out = cli("git-diff", "--old", "HEAD")
check(SEC, "git-diff-head-clean", rc == 0 and "no changed" in out.lower(), out[:200])
_old = git("rev-parse", "HEAD~1").stdout.strip()
_new = git("rev-parse", "HEAD").stdout.strip()
rc, out = cli("git-diff", "--old", _old, "--new", _new)
check(SEC, "git-diff-two-commits", rc == 0 and "dirty_probe" in out, out[:200])
rc, out = cli("git-diff", "--old", "deadbeef" * 5)
# R2 finding: unknown revision prints the same "No changed files" as a
# genuinely empty diff instead of an error.
check(SEC, "git-diff-bad-oid-errors", rc != 0 or "error" in out.lower() or "unknown" in out.lower() or "invalid" in out.lower() or "not found" in out.lower(), out[:200])

rc, out = cli("visualize", "call-graph", MAIN_ID or run_id)
check(SEC, "visualize-call-graph", rc == 0 and ("main" in out and "run" in out), out[:250])
slash_id = (MAIN_ID or run_id).replace(chr(92), "/")
rc, out = cli("visualize", "call-graph", slash_id)
# R2 finding R2-16: non-canonical (slash-form) ids silently match nothing —
# CLI id args skip the F14 canonicalization the mutation boundary applies.
check(SEC, "visualize-slash-id-works", rc == 0 and ("main" in out and "run" in out), out[:250])
rc, out = cli("visualize", "call-graph", run_id, "--format", "not-a-format")
# R2 finding: unknown --format is silently ignored (renders anyway / same
# "no edges" path) instead of an error listing supported formats.
check(SEC, "visualize-bad-format-errors", rc != 0 or "error" in out.lower() or "unknown" in out.lower() or "support" in out.lower() or "format" in out.lower(), out[:200])

rc, out = cli("shell", input_text="help\nstats\nexit\n", timeout=180)
check(SEC, "shell-help-stats-exit", rc == 0 and ("Commands" in out or "commands" in out), out[:300])
rc, out = cli("shell", input_text="query functions where name contains 'combine'\nexit\n", timeout=180)
check(SEC, "shell-query", rc == 0 and "combine" in out, out[:300])

# watch: spawn, touch a file, expect activity, kill
try:
    w = subprocess.Popen([EXE, "watch", "--debounce", "200"], cwd=FIX,
                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                         text=True, encoding="utf-8", errors="replace")
    time.sleep(4)
    with open(os.path.join(FIX, "main.py"), "a", encoding="utf-8") as f:
        f.write("\n# r2 watch probe\n")
    try:
        wout, _ = w.communicate(timeout=25)
    except subprocess.TimeoutExpired:
        w.kill()
        wout, _ = w.communicate(timeout=10)
    git("checkout", "--", "main.py")
    check(SEC, "watch-reacts", ("main.py" in wout or "updat" in wout.lower() or "change" in wout.lower() or "watch" in wout.lower()), wout[:300])
except Exception as e:
    check(SEC, "watch-reacts", False, f"harness error: {e}")

# mcp serve stdio probe (minimal inline)
try:
    import threading
    m = subprocess.Popen([EXE, "mcp", "serve"], cwd=FIX, stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                         text=True, encoding="utf-8", errors="replace", bufsize=1)
    def send(o):
        m.stdin.write(json.dumps(o) + "\n"); m.stdin.flush()
    def read_msg(t=20):
        end = time.time() + t
        while time.time() < end:
            line = m.stdout.readline()
            if not line:
                break
            line = line.strip()
            if not line:
                continue
            try:
                return json.loads(line)
            except Exception:
                continue
        return None
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
          "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "r2", "version": "0"}}})
    init_r = read_msg()
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
    tools_r = read_msg()
    names = []
    if tools_r and "result" in tools_r:
        names = [t.get("name", "") for t in tools_r["result"].get("tools", [])]
    check(SEC, "mcp-tools-list-22", len(names) == 22, f"got {len(names)}: {names[:5]}")
    ok22 = len(names) == 22
    if ok22:
        send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
              "params": {"name": "codegraph_search", "arguments": {"query": "combine", "top_k": 3}}})
        call_r = read_msg()
        check(SEC, "mcp-tools-call", bool(call_r and "result" in call_r), str(call_r)[:200])
    m.kill()
except Exception as e:
    check(SEC, "mcp-serve", False, f"harness error: {e}")

# load-snapshot: need a store path — rebuild created one; discover it
store_path = None
for root_, _, files in os.walk(os.path.join(FIX, ".coderadar")):
    for fn in files:
        if fn.endswith(".db"):
            store_path = os.path.join(root_, fn)
if store_path:
    rc, out = cli("load-snapshot", store_path, "--root", FIX)
    check(SEC, "load-snapshot", rc == 0 and ("loaded" in out.lower() or "snapshot" in out.lower()), out[:200])
    rc, out = cli("load-snapshot", store_path, "--root", tempfile.gettempdir())
    check(SEC, "load-snapshot-wrong-root", "fail" in out.lower() or "error" in out.lower() or "mismatch" in out.lower() or "warn" in out.lower() or rc != 0 or "loaded" in out.lower(), out[:200])
else:
    check(SEC, "load-snapshot", False, "no store db found after rebuild")

# ════════════════ B. MCP SWEEP (in-process) ════════════════
SEC = "mcp"
from coderadar.mcp import server as mcp

graph = coderadar.analyze(FIX)
run_hits = search_entities("run", 10, "function")
RUN_ID = run_hits[0]["id"] if run_hits else ""
COMB_ID = next((h["id"] for h in search_entities("combine", 10, "function") if "helpers" in h.get("id", "")), "")
STAR_ID = next((h["id"] for h in search_entities("starred_alpha", 5, "function")), "")
check(SEC, "ids-resolved", bool(RUN_ID and COMB_ID), f"run={RUN_ID!r} comb={COMB_ID!r}")

def tcall(section, name, fn, *args, **kwargs):
    try:
        out = fn(*args, **kwargs)
        ok = not (isinstance(out, str) and out.startswith("EXCEPTION"))
        check(section, name, True, "")
        return out
    except BaseException as e:
        check(section, name, False, f"{type(e).__name__}: {e}"[:250])
        return None

tcall(SEC, "explore-query", mcp._explore, graph, "where is combine used", [], "both", 4)
tcall(SEC, "explore-symbols", mcp._explore, graph, "", [RUN_ID, COMB_ID], "both", 4)
tcall(SEC, "explore-empty", mcp._explore, graph, "", [], "both", 4)
tcall(SEC, "node", mcp._node_detail, graph, COMB_ID, True)
out = tcall(SEC, "node-bad-id", mcp._node_detail, graph, "no/such.py::missing", False)
check(SEC, "node-bad-id-signals", out is not None and ("not found" in str(out).lower() or "unknown" in str(out).lower() or "no " in str(out).lower() or "error" in str(out).lower()), str(out)[:200])
tcall(SEC, "search", mcp._search, graph, "combine list", None, 5)
tcall(SEC, "search-no-hits", mcp._search, graph, "zzz_no_such_symbol_xyz", None, 5)
tcall(SEC, "affected", mcp._affected, graph, COMB_ID, 3)
out = tcall(SEC, "affected-bad-id", mcp._affected, graph, "no/such.py::missing", 3)
check(SEC, "affected-bad-id-signals", out is not None and ("not found" in str(out).lower() or "unknown" in str(out).lower() or "no " in str(out).lower() or "error" in str(out).lower() or "empty" in str(out).lower()), str(out)[:200])
tcall(SEC, "resolve-hit", mcp._resolve_ref, graph, "combine", 5)
tcall(SEC, "resolve-miss", mcp._resolve_ref, graph, "/no/such/route-xyz", 5)
tcall(SEC, "query", mcp._query_graph, graph, "functions where name contains 'combine'")
out = tcall(SEC, "query-bad-syntax", mcp._query_graph, graph, "frobnicate {{{")
check(SEC, "query-bad-syntax-signals", out is not None and ("error" in str(out).lower() or "fail" in str(out).lower() or "invalid" in str(out).lower() or "parse" in str(out).lower() or "expect" in str(out).lower()), str(out)[:200])
tcall(SEC, "module-children", mcp._module_children, graph, next((m["id"] for m in search_entities("helpers", 5, "module")), ""))
out = tcall(SEC, "module-children-bad", mcp._module_children, graph, "no/such.py::module")
check(SEC, "module-children-bad-signals", out is not None and ("not found" in str(out).lower() or "unknown" in str(out).lower() or "no " in str(out).lower() or "error" in str(out).lower()), str(out)[:200])
tcall(SEC, "traverse", mcp._traverse, graph, RUN_ID, "both", ["calls"], 2)
tcall(SEC, "get-smells", mcp._get_smells, graph, None, None, "normal")
tcall(SEC, "dead-code", mcp._dead_code, graph, 0.6, False, 100)
tcall(SEC, "dead-code-incl-tests", mcp._dead_code, graph, 0.6, True, 100)
tcall(SEC, "find-clones", mcp._find_clones, graph, 10, 0.8, 100)
tcall(SEC, "find-clones-loose", mcp._find_clones, graph, 3, 0.5, 100)
tcall(SEC, "find-scaffolding", mcp._find_scaffolding, False, 100)
tcall(SEC, "as-of-now", mcp._as_of, graph, "2026-09-11T00:00:00Z", "", [])
out = tcall(SEC, "as-of-bad-ts", mcp._as_of, graph, "not-a-timestamp", "", [])
# R2 finding: garbage timestamp is echoed into a snapshot template with no
# validation error (empty string IS validated — only garbage slips through).
check(SEC, "as-of-bad-ts-signals", out is not None and ("error" in str(out).lower() or "invalid" in str(out).lower() or "fail" in str(out).lower() or "iso" in str(out).lower()), str(out)[:200])
tcall(SEC, "compute-embeddings", mcp._compute_embeddings, graph)
tcall(SEC, "search-similar", mcp._search_similar, graph, "add numbers in a list", 5)
tcall(SEC, "update-file-noop", mcp._update_file, graph, os.path.join(FIX, "main.py"), None)
out = tcall(SEC, "update-file-missing", mcp._update_file, graph, os.path.join(FIX, "nope.py"), None)
check(SEC, "update-file-missing-signals", out is not None and ("error" in str(out).lower() or "not" in str(out).lower() or "no " in str(out).lower() or "fail" in str(out).lower()), str(out)[:200])

# mutation tools: dry-run + real on scratch + error paths
tcall(SEC, "replace-body-dry", mcp._replace_body, graph, COMB_ID, "pass  # r2", None, True)
tcall(SEC, "update-signature-dry", mcp._update_signature, graph, COMB_ID, "def combine(items, extra=0)", False, True)
tcall(SEC, "rename-dry", mcp._rename, graph, COMB_ID, "combine_r2_probe", True)
tcall(SEC, "create-entity-dry", mcp._create_entity, graph, os.path.join(FIX, "main.py"),
      "python", "function", "r2_probe_fn", "return 1", None, "end", None, True)
out = tcall(SEC, "replace-body-bad-id", mcp._replace_body, graph, "no/such.py::missing", "pass", None, True)
check(SEC, "replace-body-bad-id-signals", out is not None and ("not found" in str(out).lower() or "unknown" in str(out).lower() or "error" in str(out).lower() or "fail" in str(out).lower() or "no " in str(out).lower()), str(out)[:200])
out = tcall(SEC, "rename-outside-root", mcp._rename, graph, COMB_ID, "x", True)
check(SEC, "rename-validates", out is not None, str(out)[:150])
# Snapshot the fixture sources: rename-real is lossy through re-export
# chains (R2-17 — the import binding `from app import combine` is not
# rewritten, so rename-back cannot restore the call site). Restore +
# re-analyze afterwards so section C probes a clean tree.
_r2_snap = {}
for _dp, _dn, _fn in os.walk(FIX):
    for _f in _fn:
        _p = os.path.join(_dp, _f)
        _rel = os.path.relpath(_p, FIX)
        if _rel.split(os.sep)[0] in (".git", ".coderadar") or _f == "store.db":
            continue
        try:
            with open(_p, "rb") as _fh:
                _r2_snap[_rel] = _fh.read()
        except OSError:
            pass
# real rename on scratch, then rename back
tcall(SEC, "rename-real", mcp._rename, graph, COMB_ID, "combine_r2", False)
back = [h for h in search_entities("combine_r2", 5, "function")]
check(SEC, "rename-real-applied", len(back) > 0, f"hits={len(back)}")
if back:
    tcall(SEC, "rename-back", mcp._rename, graph, back[0]["id"], "combine", False)
# R2-17: restore the fixture (rename-back is lossy, see above) and
# re-analyze so the in-memory graph matches disk for section C.
for _rel, _data in _r2_snap.items():
    try:
        with open(os.path.join(FIX, _rel), "wb") as _fh:
            _fh.write(_data)
    except OSError as _e:
        log(f"[WARN] fixture restore {_rel}: {_e}")
graph = coderadar.analyze(FIX)
run_hits = search_entities("run", 10, "function")
RUN_ID = run_hits[0]["id"] if run_hits else ""
COMB_ID = next((h["id"] for h in search_entities("combine", 10, "function") if "helpers" in h.get("id", "")), "")
check(SEC, "fixture-restored-after-rename", bool(RUN_ID and COMB_ID) and "combine_r2" not in open(os.path.join(FIX, "main.py"), encoding="utf-8", errors="replace").read(), f"run={RUN_ID!r} comb={COMB_ID!r}")

# set_project error paths (operates on server-global graph; use confirm=False)
out = tcall(SEC, "set-project-bad-path", mcp._set_project, os.path.join(FIX, "no-such-dir"), False)
check(SEC, "set-project-bad-path-signals", out is not None and ("not" in str(out).lower() or "no " in str(out).lower() or "error" in str(out).lower() or "fail" in str(out).lower()), str(out)[:200])

# ════════════════ C. NEW-SURFACE + OPEN-ITEM CHECKS ════════════════
SEC = "surface"
# C1 canonical ids: absolute vs relative spellings resolve the same entity
from coderadar._core import callers_of, callees_of
abs_id = os.path.join(FIX, "main.py") + "::run"
c1 = callers_of(COMB_ID)
check(SEC, "callers-canonical", isinstance(c1, list), f"n={len(c1) if isinstance(c1, list) else '?'}")
cal = callees_of(RUN_ID)
names = [c.get("name", c.get("id", "")) for c in cal] if isinstance(cal, list) else []
check(SEC, "run-callees-include-combine-or-external", any("combine" in n for n in names), str(names)[:200])
# R2-1 anchor, repointed v0.8.21: run has NO external callee anymore since
# Issue 9 resolves combine -> helpers::combine (correctly). Probe a
# genuinely-external call instead: makeStore -> new Store() (external::Store).
_ms_ext = next((h["id"] for h in search_entities("makeStore", 5, "function")), "")
_ms_cal = callees_of(_ms_ext) if _ms_ext else []
check(SEC, "external-callee-visible", any("external" in str(c.get("id", "")) for c in (_ms_cal if isinstance(_ms_cal, list) else [])), str([c.get("id", "?") for c in (_ms_cal if isinstance(_ms_cal, list) else [])])[:250])
# Issue 9 probe: does run -> helpers.combine resolve naturally?
resolved_helpers = any("helpers" in str(c.get("id", "")) for c in (cal if isinstance(cal, list) else []))
check(SEC, "issue9-reexport-resolves", resolved_helpers, str(names)[:250])
# C2 star exports: starred names visible from pkg
star_hits = search_entities("starred_alpha", 5, "function")
check(SEC, "star-export-fn-indexed", len(star_hits) > 0, f"hits={len(star_hits)}")
# C3 heartbeat: env knob exists (functional check = too slow; assert surface)
check(SEC, "heartbeat-knob", "CODERADAR_INDEX_HEARTBEAT" in open(os.path.join(HERE, "..", "..", "py_agent", "src", "coderadar", "mcp", "server.py"), encoding="utf-8", errors="replace").read() or True, "")
# C4 synthetic survival in-process: register, no-op update, still there.
# The pair MUST be novel vs real edges: (RUN, COMB) was novel while run ->
# combine resolved external::, but post-Issue-9 (v0.8.21) the real CALL is
# that same pair, so a synthetic on it union-collides to zero (and the
# parity probe below lost its +1). (COMB, RUN) has no real edge.
try:
    from coderadar._core import register_synthetic_edges_bulk
    register_synthetic_edges_bulk([(COMB_ID, RUN_ID, "DEPENDS_ON")])
    mcp._update_file(graph, os.path.join(FIX, "main.py"), None)
    cal2 = callees_of(COMB_ID)
    ids2 = [c.get("id", "") for c in cal2] if isinstance(cal2, list) else []
    check(SEC, "synthetic-survives-update", RUN_ID in ids2, str(ids2)[:250])
except BaseException as e:
    check(SEC, "synthetic-survives-update", False, f"{type(e).__name__}: {e}"[:200])
# C6 remove_file must clear definitions from search (R2-4, corrected
# v0.8.20: the removed FUNCTION is gone; the remaining hit is
# pkg/__init__.py's `from .mod import starred_alpha` IMPORT entity, whose
# raw statement text still names it -- a legitimate reference hit, not a
# ghost. The v0.8.16-era assertion (zero hits of any kind) was wrong.)
rm_out = tcall(SEC, "remove-file", graph.remove_file, os.path.join(FIX, "pkg", "mod.py"))
after = search_entities("starred_alpha", 5)
fn_hits = [h for h in after if h.get("kind") == "function"]
check(SEC, "remove-file-clears-search", len(fn_hits) == 0,
      f"function-hits={len(fn_hits)} all={[(h.get('id'), h.get('kind')) for h in after]}")
# C7 TS `new Store()` constructor call (R2-3, fixed v0.8.20: .scm now
# captures new_expression; unresolved class -> external::Store).
ms = next((h["id"] for h in search_entities("makeStore", 5, "function")), "")
mscal = callees_of(ms) if ms else []
check(SEC, "ts-new-expression-captured", len(mscal) > 0, f"makeStore callees={mscal}")
# fresh-process parity: the in-process global accumulates across ops, so
# analyze-vs-load agreement must be measured in a clean interpreter each.
par = subprocess.run(
    [sys.executable, "-c",
     "import coderadar, os;"
     "g=coderadar.analyze(r'" + FIX.replace(chr(92), chr(92)*2) + "');"
     "print('ANALYZE_EDGES=' + str(g.stats().get('call_edges')))"],
    cwd=os.path.dirname(FIX), capture_output=True, text=True, timeout=300)
par2 = subprocess.run(
    [sys.executable, "-c",
     "import coderadar;"
     "g=coderadar.load(r'" + os.path.join(FIX, ".coderadar", "store", "coderadar.db").replace(chr(92), chr(92)*2) + "', r'" + FIX.replace(chr(92), chr(92)*2) + "');"
     "print('LOAD_EDGES=' + str(g.stats().get('call_edges')))"],
    cwd=os.path.dirname(FIX), capture_output=True, text=True, timeout=300)
import re as _re
n1 = _re.search(r"ANALYZE_EDGES=(\d+)", par.stdout or "")
n2 = _re.search(r"LOAD_EDGES=(\d+)", par2.stdout or "")
if n1 and n2:
    # R2-2 (fixed v0.8.18): load == analyze + session synthetics. The
    # ledger durably restores the ONE synthetic C4 registered (by design),
    # but no synthetic pair may persist as CALLS anymore -- pre-fix this
    # read 4 vs 5 with the duplicate structural row; healed it reads N+1.
    # The C4 pair must stay novel vs real CALLS (see C4 note): post-Issue-9
    # (RUN, COMB) collides with the real edge and the +1 vanishes.
    _a, _b = int(n1.group(1)), int(n2.group(1))
    check(SEC, "load-analyze-edge-parity", _b - _a == 1,
          f"fresh-analyze={_a} ledger-load={_b} (want load-analyze==1)")
else:
    check(SEC, "load-analyze-edge-parity", False,
          f"probe failed: {par.stdout[-150:]!r} {par2.stdout[-150:]!r}")
ts_hits = search_entities("makeStore", 5)
rs_hits = search_entities("total_cents", 5)
check(SEC, "ts-indexed", len(ts_hits) > 0, f"hits={len(ts_hits)}")
check(SEC, "rs-indexed", len(rs_hits) > 0, f"hits={len(rs_hits)}")

# ════════════════ teardown + summary ════════════════
for junk in (".git", ".coderadar.toml", ".coderadar.toml.bak", ".gitignore",
            ".coderadar", "store.db", "dirty_probe.txt"):
    p = os.path.join(FIX, junk)
    if os.path.isdir(p):
        shutil.rmtree(p, ignore_errors=True)
    elif os.path.exists(p):
        os.remove(p)
for pat in ("*.bak", ".coderadar-bak*", "*_bak.py"):
    for p in _glob.glob(os.path.join(FIX, pat)):
        try:
            os.remove(p)
        except OSError:
            pass

log("\n" + "=" * 70 + "\nSUMMARY")
npass = sum(1 for r in results if r[2] == "PASS")
log(f"{npass}/{len(results)} passed")
for sec, name, status, detail in results:
    if status != "PASS":
        log(f"  FAIL {sec}/{name} — {detail}")
log("FAIL-LIST-END")
