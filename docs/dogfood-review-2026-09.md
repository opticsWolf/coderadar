# CodeRadar Dogfood Review — v0.8.0

**Date:** 2026-09-10 · **Project exercised on itself:** `D:/User/Documents/Python/CodeRadar`
(207 files: Python + Rust + fixture TS/JS/Java/PHP/Ruby/C#/Go)
**Method:** every CLI command and every MCP tool driven against the repo, including
live mutations applied **only** to throwaway demo files (`tests/cr_edit_tests/`).
Harnesses kept under `tests/cr_edit_tests/` (`battery_readonly.py`,
`battery_mutation.py`, `mcp_stdio_probe.py`) so each finding below is reproducible.

---

## 0. Executive summary

The engine core is in good shape — warm-graph queries answer in 1–6 ms, the
cold-start data path is correct, stale-write rejection and rollback-on-error
work, and Python `replace_body` now applies cleanly (the v0.7 indent-corruption
bug from `CODERADAR_BUGS_QUIRKS.md` is fixed). But the review found **four P0
defects**, two of which are safety/safety-adjacent in the mutation engine — the
product's headline feature — and one that silently taxes **every CLI command
and every cold start by ~17 s**, nullifying both of the release's flagship
performance claims (554 ms analyze, millisecond cold start).

| # | Severity | Area | One-line summary |
|---|----------|------|------------------|
| F1 | **P0 crash** | Clones | `codegraph_find_clones` panics in Rust and the stdio MCP server **hangs forever** — session dead, no error returned |
| F2 | **P0 safety** | Mutation | Policy allow-list is substring-matched — a mutation to `py_agent/src/…` (outside the allow list) was **applied to a production file** live |
| F3 | **P0 functional** | Mutation | `replace_body` is broken for every brace-delimited language (TS/Rust confirmed) — splice eats `{…}`, always rolls back |
| F4 | **P0 functional** | Mutation | `update_signature` writes the new signature at the wrong span, mangling the body and deleting the next method's `def` line |
| F5 | **P1 perf** | Indexing | Star-export pass rglobs `.venv` (8,638 files) on **every** analyze *and* every cold load: 17.6 s instead of 0.36 s / 0.15 s (48–113×) |
| F6 | **P1 data** | Storage | v1 store poisoning: no working upgrade path; `init --force` doesn't clear it; 180 MB store that only manual deletion fixes |
| F7 | **P1 precision** | Dead code | FFI-called Rust functions (`MutationEngine.apply`…) flagged dead at 0.90 confidence; 2,197 smell findings on own repo, most unactionable |
| F8 | **P1 UX** | Query | `codegraph_query` / CLI `callers`/`callees` render file paths as `?` — results cannot be located or acted on |
| F9 | P1 UX | MCP | First tool call blocks ~17 s with no progress/warming signal |
| F10 | P1 UX | Mutation | Raw Rust debug structs leak into LLM-facing errors (`Mutation failed: StaleIndex { … }`) |
| F11 | P2 | Packaging | `--version` lies: hardcoded `0.7.20` vs pyproject `0.8.0`; `_core.pyd` older than Rust source, no freshness guard |
| F12 | P2 | CLI | Bare-name lookups fail silently (prior BUGS_QUIRKS #5 still unresolved); `traverse` table unreadable; `git-diff` docstring mismatch; shell quirks |

---

## 1. Environment findings (found before any tool ran)

### E1 — Version skew everywhere (F11)

- `pyproject.toml`: `version = "0.8.0"`; README says v0.8.0.
- `py_agent/src/coderadar/__init__.py:20`: `__version__ = "0.7.20"` — the CLI's
  `--version`, the MCP server's `serverInfo.version` (verified over stdio) and
  PyPI metadata (editable install) all report **0.7.20**.
- `py_agent/src/coderadar/_core.pyd` mtime is older than `core_indexer/src/lib.rs`
  — the compiled Rust extension may lag the source, and nothing warns.

There is no single source of truth for the version and no build-freshness
check. Any bug report, benchmark, or support interaction that quotes
`coderadar --version` is unreliable today.

### E2 — The store I inherited was poison (F6)

`.coderadar/store/coderadar.db` was 163–180 MB (a 207-file repo needs ~21 MB)
and **permanently** failed cold start:

```
Store load failed (the Macrame store contains 3857 concept(s) without
meta_version: 2 (concept-JSON v1). Re-run `coderadar analyze` on the project
to upgrade the store; the snapshot will not load.)
```

Two confirmed mechanism details:

1. **The documented upgrade path is a no-op.** The error says "re-run
   `coderadar analyze`" and `cli.py`'s fallback comment says the full analyze
   "also upgrades a v1 store" — but `analyze` writes *new* v2 concepts
   alongside the v1 ones and never retires them (3857 → still 3857 after
   `analyze`; 2699 after `init --force`). The loader hard-fails if **any**
   v1 concept remains, so cold start never succeeds.
2. **Entity-ID form changed** between v1 and v2 writers (absolute
   `D:\…\graph.rs::import@6` vs relative `.\py_agent\…::import@15`), so the
   new analyze cannot match old IDs even in principle — they accumulate as
   orphans forever.

`coderadar init --force` does **not** delete the store either; it only
overwrites the config and re-analyzes into the same poisoned ledger. The only
working fix is manual deletion (`rm .coderadar/store/coderadar.db`), which
took the store from 180 MB → **21 MB** and made cold start work. Nothing in
the tool helps the user discover or execute this.

**Additional config observation:** `init --force` overwrites a hand-tuned
`.coderadar.toml` (this repo's config carries `truncated_dimension`,
`max_body_tokens`, `max_repair_attempts`, custom allow/deny lists — none of
which are in the generated default) with no backup.

---

## 2. P0 findings

### F1 — `codegraph_find_clones` panics and wedges the MCP server

Reproduced three times, including once over the real stdio protocol:

```
thread '<unnamed>' panicked at core_indexer\src\clones\mod.rs:238:17:
index out of bounds: the len is 1058 but the index is 1058
```

- Panic site: the LSH pool build loop `lsh.insert(slot as u32, fps[*i].sig.clone())`
  — `pool` holds an index one past the end of `fps` (off-by-one: index == len).
- Python layer: `_find_clones` catches `(ValueError, TypeError)` and
  `Exception` — but PyO3 raises `PanicException`, which derives from
  `BaseException`, so **no handler catches it**.
- Real consequence over stdio: the panic kills the Tokio worker that owns the
  request, the response never completes, and the client session **hangs with
  no timeout** (verified: 180 s wait, no answer, server unusable).

An agent that calls clone detection on a Rust-containing project freezes its
entire MCP connection. This is the single worst defect found.

### F2 — Mutation policy allow-list doesn't actually restrict (safety)

`core_indexer/src/mutation/mod.rs:318`:

```rust
fn path_matches(rel: &str, pattern: &str) -> bool {
    ...
    rel.starts_with(fragment) || rel.contains(&format!("/{}", fragment))
}
```

With the project's own config `allow = ["src/", "lib/", "tests/", "scripts/"]`:

- `rel = "py_agent/src/coderadar/coldstart.py"` **contains `/src/`** → allowed.
- Verified live: `coderadar_replace_body(dry_run=False)` on
  `…coldstart.py::store_is_fresh` returned **`Status: Applied`** and wrote the
  file (restored from git afterwards).
- Every CodeRadar source file lives under `py_agent/src/` or
  `core_indexer/src/` — for this layout the allow list protects nothing.
  Vendored code, `node_modules/<pkg>/src/`, `.venv/Lib/site-packages/**/src/`
  would all be writable too. The deny list still works (it is checked first),
  but the allow list — the mechanism that was supposed to make mutations
  fail-safe — is effectively decorative.

Note the gate fires only in `apply()` (mod.rs:1242), not in
`plan_body_replacement()`: a dry-run happily renders a full diff preview for a
path `apply` would refuse (plan/apply mismatch; wasted agent round-trip and a
misleading preview).

### F3 — `replace_body` produces invalid syntax for all brace-delimited languages

Dry-run plan for `demo_ledger.rs::Ledger.total_cents` (my replacement body was
plain, valid Rust):

```diff
-    pub fn total_cents(&self) -> i64 {
-        self.entries.iter().map(|e| e.amount_cents).sum()
-    }
+    pub fn total_cents(&self) -> i64         self.entries.iter().map(|e| e.amount_cents).sum()
```

The splice deletes the method's `{ … }` and glues the body directly onto the
signature line. Guaranteed syntax error → post-write parse check catches it →
**every** such mutation rolls back. Identical diff shape confirmed for
TypeScript (`demo_router.ts::Router.match`). The same operation on Python
applies cleanly, so the splice contract (`normalize_body_for_splice`,
`indent.rs`) has no brace-language handling: `replace_body` — the flagship
LLM-refactoring primitive — works only on Python today.

The safety net deserves credit: the write was caught, the backup noted, the
file untouched. But the tool is unusable for its purpose on ~40 of 41
languages.

### F4 — `update_signature` splices at the wrong span

Dry-run plan for `demo_billing.py::Invoice.apply_loyalty_discount`
(new signature passed verbatim per the tool's documented contract):

```diff
         if tier == "silver":
-            pct = pct + 2.0
-        return pct
-
-    def summary(self) -> str:
+            pct = pc(self, pct: float, tier: str = 'none') -> float:
```

The new signature is written **into the middle of the previous method's body**
(`pct = pct + 2.0` becomes `pct = pc(self, …) -> float:`), and the following
method's `def summary` line is deleted. The span math is off by an entire
body + docstring length. Parse check catches it → rollback. Net effect:
`update_signature` never succeeds on this shape; a caller-following cascade
can't even be tested because the primary edit always fails.

Related oddity from the same run: the def-site itself was reported under
"call sites could not be verified/rewritten" as a *textual occurrence* —
the signature-update path appears to treat the def line as a call site.

---

## 3. P1 findings

### F5 — The star-export pass taxes every CLI command and every cold start (~17 s)

Profiled cold load (cProfile, 41.3 s under profiler):

| Component | Time |
|---|---|
| `_apply_star_exports` → `extract_all_exports` | **37.8 s (92%)** |
| of which `ast.parse` of 8,746 files | 7.6 s |
| of which `ast.walk` over ~9.3M nodes | 21 s |

Mechanism (`py_agent/src/coderadar/__init__.py:757`):

```python
for py_file in root_path.rglob("*.py"):
```

No exclude globs, no gitignore, no skip-dir set — so every `analyze()` and
every `load()` re-parses **all 8,638 `.venv` files** with the pure-Python AST
to find `__all__` in the project's own 13 modules. Measured with the pass
bypassed:

| Operation | With pass | Without pass | Ratio |
|---|---|---|---|
| `analyze('.')` | 17.6 s | **0.36 s** | 48× |
| cold `load()` | 16.95 s | **0.15 s** | 113× |

This single regression invalidates both headline numbers in the README
("CodeRadar self: 554 ms"; "cold start <50 ms / milliseconds") for any project
with a virtualenv — i.e. every Python project. The tree-sitter engine itself
**does** meet the published benchmarks; the Python wrapper around it does not.

### F6 — Cold start works correctly once the store is clean (credit, with caveats)

After rebuilding the store manually: cold load restores 207 files / 2,336
functions with correct stats, call edges (`callees_of` spot-checked against a
fresh analyze — identical results), and resolved calls from `resolved_calls`
concepts. The data path is sound. The only thing between the current UX and
the design doc's "milliseconds" promise is F5. (`coderadar stats` cold start:
17.1 s today, ~0.3 s potential.)

### F7 — Dead-code/smells precision: cross-language blindness and noise

- `MutationEngine.apply` (~202 lines, the FFI entry of the mutation engine),
  `plan_signature_update`, `insert_extracted`, `build_fragment`, and five
  resolver `extract` methods are all flagged **unreachable, High (0.90)** —
  they are called via PyO3 FFI, which the reachability graph cannot see.
  A user following the tool's own pipeline ("verify with `affected` before
  removing") gets the same false answer from `affected`.
- `codegraph_get_smells` on the repo returns **2,197 findings**, rendered as
  entity names without file paths (`dead-code — new: 'new' is unreachable`),
  so nothing can be located or triaged. Python `pub fn`s and `__init__`
  exports are systematically blind spots. The prior precision report
  (`docs/rta-lite-precision-report.md`) measured Python-only corpora; Rust FFI
  is a new, worse case.

### F8 — Query/callers/callees outputs have no file information

- `codegraph_query` over MCP: `1. test_parse_with_and_chain (?) — ?`
  for all 73 results — name and line only.
- CLI `callers`/`callees`: `.\tests\…::build_demo_invoice (?:51)` — file
  column renders as `?`.

An LLM consuming these outputs cannot read the referenced code without a
second round-trip. The underlying Rust results do carry `file_path` (the
`callees_of` dicts in §5 show it) — the formatters drop it.

### F9 — First MCP call blocks ~17 s with no feedback

stdio probe: `[call codegraph_explore] 16794ms` — the server logs "Indexing …
in the background…" then the first tool call waits for the full index. The
stale-file banner mechanism exists (`_get_stale_files`) but there is no
"warming up" state for the cold-index case. On a big repo this looks like a
hang to the client. (Improvement over the prior "returns empty" bug #8, but
still agent-hostile.)

### F10 — Raw Rust error structs in LLM-facing output

`coderadar_rename` (dry-run, entity since re-edited):

```
Mutation failed: StaleIndex { file: ".\\tests\\…", expected: "print_invoice", span: ByteSpan { start: 2007, end: 2020 } }
```

Leaked internals, no guidance. The stale-body case by contrast produces a good
message ("content changed since planning (expected deadbeef, found c87e420f)")
— the translation layer exists but covers one error variant.

---

## 4. P2 findings

- **Shell**: `status` command prints nothing; `>>` prompt echoes twice;
  the shell re-indexes instead of cold-loading ("No graph for this directory
  - indexing..." on a warm store).
- **`traverse` CLI**: 21 unnamed Rich columns ellipsize into an unreadable
  wall ("…" everywhere); unusable as a terminal table.
- **Windows mojibake**: CLI does not reconfigure stdout to UTF-8 (the repo's
  own `run_mcp_suite.py` does, with a comment explaining why) — em-dashes and
  checkmarks render as `�` in several tables (`traverse`, mutation output).
- **`diagnose` truncation collides entity identities**: long entity IDs are
  cut at column width mid-UTF-8-sequence (`…projection_ops.rs::Code�`), and
  two *different* entities truncate to the identical visible string in the
  same table (two `CodeGraph` methods, distinct unresolved-target counts,
  indistinguishable rows). Truncation must preserve whole code points, use a
  real ellipsis, and keep the disambiguating tail (`…::CodeGraph.insert`),
  not the head.
- **`git-diff`**: docstring says "Show files changed between two commits" but
  the positional arg is `[REPO]`; passing `HEAD~3` errors with "Path
  'HEAD~3' does not exist". OIDs go in `--old/--new` flags.
- **`codegraph_module_children`** on the natural directory-module id
  (`.\py_agent\src\coderadar::module`) → "Module not found"; no documented
  id form works for it.
- **Mixed path forms in one output**: `codegraph_search` result block shows
  ID `tests/fixtures/cold_start/app/models.py::Derived.clone` next to File
  `.\tests\fixtures\cold_start\app\models.py` — two canonical forms, one
  block, downstream consumers must guess.
- **MCP schema gaps**: `get_smells.strictness` is a free string; the engine
  validates it late ("unknown strictness 'high'") where an enum in the tool
  schema would prevent the error entirely. `codegraph_search.kind` likewise.
- **CLI bare names fail silently** (prior BUGS_QUIRKS #5, still open):
  `coderadar callees print_invoice` → "No callees from print_invoice" with no
  hint that the full `.\path::name` id is required (verified: full id works,
  and the stale-rename path leaks that it *did* resolve the entity internally).
- **Framework extraction on self**: `analyze` reports 795 routes / 1,318
  handler edges for a repo with zero HTTP frameworks — they come from
  `tests/fixtures/**`. Harmless but misleading in the headline stats; the
  fixtures could be excluded from framework extraction.
- **Demo-file restore**: `git checkout -- <file>` cannot restore untracked
  files; the `.coderadar-bak` backups are the real recovery path (they exist
  and work — worth documenting as the mutation-undo story).

---

## 5. What works well (verified)

- **Warm graph query speed**: `explore` 6 ms, `node` 0 ms, `search` 1 ms,
  `affected` 1 ms, `traverse` 4 ms, `get_smells` 14 ms, `dead_code` 2 ms.
- **Cold-start correctness**: full stats, call edges, and resolved-calls
  restoration all match a fresh analyze (spot-checked with `callees_of`).
- **Stale-write rejection**: wrong `expected_hash` → `RejectedStale` with an
  actionable message.
- **Rollback + backup**: invalid splice (F3/F4) never reached disk; backups
  created and cleaned up correctly.
- **Python `replace_body`**: clean apply, correct indentation (BUGS_QUIRKS #1
  fixed), graph updated in 13 ms, `update_file` re-sync verified.
- **`rename` on Rust**: applied in 36 ms, definition rewritten.
- **`create_entity`**: correct top-level indent, signature written verbatim.
- **`diagnose`**: finds real unresolved references (1,241 targets at 35% of
  call edges — see F7 for why that number matters).
- **`scaffold` scan**: 491 ms, no false alarms on own repo.
- **`compute_embeddings`**: 13.9 s one-time for 2,336 functions; post-embed
  `search_similar` answers in 548 ms.
- **Incremental `update`**: 19.3 ms parse quality clean (the 16.9 s wall time
  is entirely F5's bootstrap tax).
- **MCP surface**: all 22 tools listed with correct annotations; protocol
  errors (unknown strictness) are clean text, not stack traces.

---

## 6. Improvement plan

### P0 — ship-stoppers (target: one week)

1. **Bound the LSH pool off-by-one** (`clones/mod.rs:238`): build `pool`
   over a materialized filtered `Vec<(usize, &Fingerprint)>` and index with
   bounds-checked access; add `#[cfg(debug_assertions)]` invariant checks.
   Then **wrap every PyO3-exposed analysis entry in `catch_unwind`** so a
   panic becomes a tool error, and fix the stdio hang (response must complete
   or fail — investigate why the Tokio request task dies without replying;
   candidate: panic during the call kills the connection's read half). Add a
   regression test that runs `find_clones` on this repo's fixtures
   (`tests/rust/clones/`) in CI.
2. **Anchor `path_matches`** (mutation/mod.rs:318): a leading-directory
   pattern (`src/`) must match `rel.starts_with("src/")` only — delete the
   `contains("/src/")` fallback for those; interior fragments keep the
   `"/{fragment}"` form (`/migrations/`). Add unit tests:
   `py_agent/src/x.py` ∉ allow=`src/`; `.venv/Lib/site-packages/foo/src/x.py`
   ∉ allow; `src/x.py` ∈. Re-run `tests/cr_edit_tests/battery_mutation.py`.
   Also surface the policy check in **dry-run** (plan phase) so forbidden
   targets are refused before a diff preview is shown.
3. **Fix the brace-language splice** (`edit.rs` / `indent.rs`): body_span for
   brace languages must exclude the braces, and the splice must re-emit
   `{ … }` around the re-indented body. Acceptance test: apply
   `battery_mutation.py`'s TS and Rust cases — status `Applied`, on-disk file
   re-parses clean, `update_file` reflects the change.
4. **Fix `update_signature` span math**: the replacement span is computed from
   the *pre-edit* concept's span without rebasing to the file (off by body +
   docstring length). Add a regression test with two consecutive methods
   (the `demo_billing.py` case is exactly that fixture).

### P1 — promises restored (target: two weeks)

5. **Kill the star-export tax** (`__init__.py:_apply_star_exports`): respect
   config `exclude` globs and a skip-dir set (`.venv`, `node_modules`,
   `target`, `dist`, `build` — reuse `STALENESS_SKIP_DIRS`), or better,
   persist star exports in the ledger so cold load doesn't re-run the pass at
   all. Acceptance: `coderadar stats` on this repo < 2 s wall; cold load
   < 1 s. This one change restores both README benchmark claims.
6. **v1 store repair**: on full analyze, retire v1 concepts (their absolute-
   path ids can be rewritten to relative ids mechanically); on `init --force`,
   rebuild the store from scratch instead of reusing the ledger; add
   `coderadar store repair` that reports counts and offers deletion. The
   error message must stop recommending a path that doesn't work.
7. **First-class path / folder / subfolder exclusion** (synthesizes F5, the
   §4 fixture-routes noise, and F6): today `exclude` in `[project]` is
   honored by the Rust walker only — the star-export pass, framework
   extraction, the watcher, and the staleness check each keep private skip
   rules, so a folder can be excluded from one pass and indexed by another
   (this review: 795 framework routes from `tests/fixtures/**`; 8,638
   `.venv` files re-parsed by star exports). Make exclusion one shared
   matcher, one config surface, enforced by every pass:
   - **Matcher semantics** — gitignore-syntax globs *anchored* per the F2
     lesson: leading-directory patterns (`src/`) match root-relative
     prefixes only; `name/` excludes that folder at any depth; `**/name/**`
     recurses; `/*.<ext>` matches by extension. One shared
     `fn path_excluded(rel) -> bool` used by walker, star exports,
     framework extraction, watcher events, and staleness — not five
     private skip lists.
   - **Config** — `[project] exclude` stays the source of truth;
     `[project] roots` interplay documented (roots narrow the walk, exclude
     subtracts from what roots allowed).
   - **CLI** — `coderadar exclude list|add <pattern>|remove <pattern>`
     edits `.coderadar.toml`; `--exclude <pattern>` one-shot flag on
     `analyze`/`rebuild` for ad-hoc narrowing without touching config.
   - **Store retraction** — when a path becomes excluded, the next analyze
     retires its concepts (same retire mechanism the v1 repair in item 6
     needs); until retracted, excluded-but-indexed entities keep answering
     queries with a staleness-style banner, never silently.
   - **Watch** — event filter shares the matcher so edits inside excluded
     folders never trigger updates.
   - **Defaults shipped and visible** — `.venv/`, `node_modules/`,
     `target/`, `dist/`, `build/`, `__pycache__/`, `.git/`, `.coderadar/`,
     `.pytest_cache/` as built-in baseline on top of user config, and
     `coderadar stats` prints the effective exclude list instead of
     leaving the skips implicit.
   - Acceptance: add `tests/cr_edit_tests/` via `coderadar exclude add`,
     re-analyze — its entities vanish from search/query/stats on the next
     cold load; the star-export pass and framework extraction never touch
     an excluded folder (verify with the cProfile method from F5).
8. **Dead-code cross-language awareness**: treat PyO3-exposed functions
   (everything reachable from the `#[pyfunction]`/`#[pymethods]` bridge and
   the `__init__.py` facade) as entry points; cap confidence at Medium for
   any entity whose reachability depends on cross-language calls; suppress
   dead-code for Rust `pub fn` outside `#[allow(dead_code)]` context until
   export analysis lands. Add file path + `codegraph_node` id to every
   smell finding; group by file; cap output or add top_k.
9. **Query/callers/callees output completeness**: include `id` and
   `file_path` in `_query_graph`, CLI `callers`/`callees`, and `traverse`
   rows (the data is in the Rust dicts already). Acceptance: zero `?` fields.
10. **MCP first-call UX**: while the background index runs, tool calls should
   return immediately with a "warming up, N files remaining, retry" message
   (matching the stale-banner pattern), or block with progress lines on
   stderr. Never silent.
11. **Mutation error translation**: map `StaleIndex`/`RejectedPolicy`/etc.
    to the same friendly prose the stale-body path already has.

### P2 — hygiene (target: next release)

12. **Single version source**: `__version__` reads `importlib.metadata` with
    a pyproject-derived fallback; CI asserts the three agree. Add a build
    freshness check (fail tests if `lib.rs` newer than `_core.pyd`).
13. **CLI polish**: bare-name fallback via `search_entities` with
    disambiguation; `traverse` limited columns + `--format json`;
    UTF-8 stdout reconfigure in `cli.py` entry; `git-diff` docstring fix;
    shell `status` implemented + cold-load reuse.
14. **Schema enums** for `strictness` and `kind`; document the entity-ID
    grammar (`.<relative-path>::<Qualified.name>`) in the MCP tool
    descriptions and README (BUGS_QUIRKS #5 closure).
15. **Path-form normalization**: one canonical form (project-relative, `.\`
    prefix on Windows) across IDs, files, and diff previews.
16. **Document the backup/undo story**: `.coderadar-bak` files, `git checkout`
    caveat for untracked files, `post_verify`/rollback behavior.

---

## 7. Reproduction index

| Finding | Repro |
|---|---|
| F1 | `.venv/Scripts/python tests/cr_edit_tests/battery_readonly.py` (see `battery_output.txt`), or `tests/cr_edit_tests/mcp_stdio_probe.py` extended with `find_clones` |
| F2 | `battery_mutation.py` §8 ("policy refusal expected") — returned `Applied` for `py_agent/src/...` |
| F3 | `graph.plan_body_replacement(demo_ledger.rs::Ledger.total_cents, "        self.entries...")` — diff preview shows brace-less splice |
| F4 | `graph.plan_signature_update(demo_billing.py::Invoice.apply_loyalty_discount, "def apply_loyalty_discount(self, pct: float, tier: str = 'none') -> float")` |
| F5 | cProfile of `coderadar.load()`; timings table in §3/F5 |
| F6/F8–F10, §4 | `battery_output.txt`, `battery_mutation_output.txt`, stdio probe output |
