# `traverse_smell` Branch — Status, Findings, & Planning

Branch: `traverse_smell` (off main @ `4f40d1f`). Goal: generalized Rust
graph-traversal binding + native code-smell engine, per the consolidated plan.

This file is the **living checkpoint**: what shipped, what is deliberately
deferred, and every known gap/limitation. It complements
`docs/traversal-matrix.md` (the pre-flight artifact that motivated doing the
resolve back-fill ahead of schedule).

> **Naming note.** Two numbering schemes exist in this branch's history:
> (a) the consolidated plan's **Phases 1–4**, and (b) the matrix's **Steps
> A–F**. They map as follows (implementation order differs from both):
>
> | Matrix step | Plan phase | Deliverable | Commit | Status |
> |---|---|---|---|---|
> | D | — (unplanned prerequisite) | Resolve back-fill | `445b0c4` | ✅ |
> | B + C + E(part) | Phase 1 | Rust `traverse` binding (4 kinds) + tests | `4024bc0` | ✅ |
> | A | Phase 3 | Honesty pass (snapshot I/O raises) | `f498c66` | ✅ |
> | — (ceiling 1) | Phase 2 | TS/JS extends/implements base capture | `5bdba61` | ✅ |
> | — (ceiling 2) | Phase 2 | Module concepts → IMPORTS persistence | `c0bc99e` | ✅ |
> | E(part) | — | `as_of` temporal traversal | — | ⛔ deferred (Phase 3B) |
> | F | Phase 4 | Native Rust smell engine | — | ⛔ not started |

---

## 0. At a glance

| Deliverable | Status | Commit |
|---|---|---|
| Phase D — resolve back-fill (subclasses/importers/overrides) | ✅ done | `445b0c4` |
| Phase 1 — generalized Rust `traverse` binding; delete Python BFS | ✅ done | `4024bc0` |
| Phase 3A — honesty pass (`export`/`load_snapshot` raise loudly) | ✅ done | `f498c66` |
| Phase 2 caveat 1 — TS/JS `extends`/`implements` base capture | ✅ done | `5bdba61` |
| Phase 2 caveat 2 — Module concepts → IMPORTS persistence | ✅ done | `c0bc99e` |
| `as_of` temporal traversal (matrix step E, 2nd half) | ⛔ deferred | — |
| Snapshot I/O + cold-start (`load`/`export` real impl) | ⛔ deferred (Phase 3B) | — |
| Phase 4 — native Rust smell engine | ⛔ not started | — |

**Working tree: clean** (all of the above committed on `traverse_smell`,
5 commits ahead of main @ `4f40d1f`).
**Test budget: 195 Rust + 351 Python = 546, 0 failures.**

---

## 1. What is IMPLEMENTED and verified

### 1.1 Phase D — resolve back-fill (`445b0c4`)
The three reverse indexes on `ProjectedGraph` (`importers`, `subclasses`,
`overridden_by`) were declared but **never populated** — `build_fragment`
initialised them empty and nothing ever inserted. So
`codegraph_traverse(['imports'/'inherits'/'overrides'])` returned empty
*not* because of the v0.6.4 edge-filter bug, but because there was no data
behind those kinds (matrix §0).

Added:
- `resolve_class_hierarchy` — fills `Class.resolved_bases` + inverts into
  `subclasses`. New shared `resolve_base_by_name` (same-module first, then
  global-unique fallback).
- `resolve_imports` — sets `Import.resolution` (module-level) + builds
  `importers` via existing `find_module_by_dotted_name`.
- `resolve_overrides` — name-based override detection over `Class.mro` →
  `overridden_by` (reverse) + new `overrides_base` forward field.
- `compute_all_mro` now routes base resolution through
  `resolve_base_by_name`, so **cross-file inheritance enters the MRO**
  (was same-module-only — this was why `overrides` was stuck at 0).
- `persist_edges` asserts EXTENDS + OVERRIDES to Macrame (IMPORTS came later,
  see §1.5).
- New read-only `index_edge_stats()` pyfunction for observability.
- Wired into **both** `analyze()` and `update_file`.

Tests: +4 Rust (3 back-fill unit + 1 persistence D.5).
Real-world (codegraph-main, 605 mod / 1176 cls / 4236 fn / 2234 imp):
imports 1968/2234 resolved, subclasses 6, overrides 1 (numbers *before* the
§1.4 ceiling fix).

### 1.2 Phase 1 — generalized Rust `traverse` binding (`4024bc0`)
- `#[pyfunction] traverse(start_id, max_depth, edge_kinds, direction, as_of=None)`
  with `py.allow_threads` for the BFS, GIL re-acquired only to materialise
  dicts. Single `with_graph` lock, single BFS — replaces the pure-Python
  fallback (~100 FFI hops/100-node set).
- All 4 edge kinds from the start (D's back-fill backs them):
  `calls`, `imports`, `extends`, `overrides`. `inherits`→`extends` alias.
- Direction: `in`/`upstream`, `out`/`downstream`, `both`.
- Result dicts tagged `depth` + `edge_type` (matches `server._traverse`).
- Honesty guards: `as_of` → `PyNotImplementedError`; unknown direction →
  `PyValueError`.
- Forward index added: `imports_by_importer` (symmetric to `importers`),
  populated in `resolve_imports`.
- Pure-Rust core extracted as `CodeGraph::traverse_bfs` + `neighbors_of`
  (`pub(crate)`) so graph.rs unit tests exercise the BFS on a local
  snapshot **without GLOBAL_GRAPH** — the lib.rs pyfunction is a thin wrapper.
- Python (`query/executor.py`): **deleted** `_projected_traverse` BFS +
  `_neighbors` fallback. Routes to Rust, raises loud on ImportError.

Tests: +10 Rust (calls down/up, cycle, diamond one-entry, max_depth=0,
empty kinds, imports upstream, extends downstream, overrides upstream,
inherits-alias). Real-world: MCP path returns **422 reachable** depth-3
from a real fn.

### 1.3 Phase 3A — honesty pass (`f498c66`)
`export_snapshot`/`load_snapshot` returned `Ok(())` unconditionally →
`coderadar export` silently produced no file; `coderadar.load(db_path)`
silently returned an empty graph (worse — silent partial data).

Both Rust pyfunctions now raise `PyNotImplementedError` with a link to
`docs/traversal-matrix.md §3`. Python `load(db_path)` raises
`NotImplementedError` too. CLI `export`/`load` catch and print cleanly.
No behaviour change for any working path; cold-start from Macrame is
Phase 3B (still deferred, §7.2).

### 1.4 Phase 2 caveat 1 — TS/JS `extends`/`implements` base capture (`5bdba61`)

**Root cause (discovered by dumping the TS AST):** TS/JS
`class Sub extends X.Y {}` parses as:
```
(class_declaration name: (type_identifier)
  (class_heritage (extends_clause value: (member_expression object:(identifier) property:(property_identifier))))
  body: (class_body))
```
The base is **not** under a `superclasses`/`superclass`/`bases` field on
`class_declaration` (those field names are Python/Java). TS uses
`class_heritage → extends_clause value:`, and `implements_clause` (comma-
separated children). The original filter also looked only at *direct
children* of the field, so even `extends Animal` (simple type_identifier,
where `extends_clause value:` is the identifier itself) was dropped.

**The fix** (`extract_base_classes`):
- Descendant DFS over the class node; pull base nodes from (a) the
  `superclasses`/`superclass`/`bases` fields (Python/Java), and (b)
  `extends_clause value:` + `implements_clause` children (TS/JS).
- Base-node kinds widened to `member_expression | qualified_type |
  scoped_type_id | qualified_identifier | nested_type_identifier |
  generic_type`; qualified bases stringified via `dotted_name_of`
  (leaf identifiers joined with `.`).
- De-duplicated by tree-sitter node id; builtin leaf check retained.

**Test:** `test_member_expression_base_is_stringified_not_dropped` asserts
`extends X.Y` → base `X.Y`, `extends E` → `E`, `implements G.H, J` → `G.H`+`J`.

**Real-world lift on codegraph-main** (before → after):
- subclasses 6 → **28**, subclass_keys 5 → **11**
- overrides 1 → **7**, overridden_by_keys 1 → **2**

(remaining ceiling: external bases like `extends React.Component` correctly
stay unresolved — resolving external→local would be wrong, see §7.3.)

### 1.5 Phase 2 caveat 2 — Module concepts → IMPORTS persistence (`c0bc99e`)

**Root cause:** `persist_edges` guarded IMPORTS out because modules were
never persisted as Macrame concepts — the extractor emitted no
`ExtractedUnit::Module` (only Class/Function/Import/Constant/TypeAlias),
so `build_concept` never produced a `kind=module` concept and module→module
IMPORTS edges failed Macrame's FK constraint.

**The fix:**
- Added `synthesize_module_unit(file_path, language)` (graph.rs) that
  builds an `ExtractedUnit::Module` with id `"{}::module"` (matching
  build_fragment's synthetic module id).
- Prepend it in `extract_only` (analyze path), `index_file_inner`
  (index_file/accumulate path), and `update_file` — so `build_concept`
  persists a module concept on every path.
- `update_file`: moved `persist_entities` **before** `persist_edges` (was
  after) — edges assert FK references against concept ids, so concepts
  must commit first.
- Removed the `projection.modules.contains_key(...)` IMPORTS guard in
  `persist_edges` (kept the `external::` guards).
- Strengthened `test_persist_edges_emits_imports_and_extends` to assert
  `persisted == calls + imports + extends + overrides` (exact, not just
  `> calls`).

**Verification:** 195 Rust + 351 Python = 546, 0 failures. Real-world
`analyze` on codegraph-main completes cleanly (605 files, 7646 entities,
no FK error).

---

## 2. Key findings worth keeping

1. **The three reverse indexes were silently dead until Phase D.** This
   is the single most important discovery of the branch. Any future
   "why does imports/extends/overrides traversal return 0?" question is
   answered by: were the resolve passes run? (matrix §0).

2. **`compute_all_mro` was same-module-only**, blocking cross-file MRO →
   `overrides` was 0 forever. Phase D routed it through
   `resolve_base_by_name`. Genuine correctness improvement, shipped in
   `445b0c4`, covered by `test_resolve_overrides_…`.

3. **`overrides_base` needed a forward index** (not just reverse
   `overridden_by`) for downstream traversal. Added to `ProjectedGraph`
   in Phase D, used by the binding in Phase 1. `extends` downstream needed
   no forward index — derived directly from `Class.resolved_bases`.

4. **`graph.rs` tests can't use the lib.rs `traverse` pyfunction** (it
   reads GLOBAL_GRAPH). So the BFS core was extracted to
   `CodeGraph::traverse_bfs` as a `pub(crate)` associated fn — the pattern
   to reuse for any future pyfunction needing unit tests in graph.rs.

5. **The plan's PyO3 sketch was outdated**: it used `graph: &ProjectedGraph`
   as an arg and `py.detach`. Real surface: `with_graph` + `py.allow_threads`
   (PyO3 0.24). The plan's `EdgeKind` enum doesn't exist — kinds are string
   consts (`storage.rs:40`) + `u8` indices (`buffers.rs:71`).

6. **TS/JS `extends` extraction was entirely broken, not just
   member-expression qualified bases.** The base lives under
   `class_heritage → extends_clause value:`, which the field-name-based
   lookup never found — so even simple `class D extends E` was dropped.
   §1.4 fixed both at once.

---

## 3. Phase 4 — native Rust smell engine (NOT started)

This is the **only remaining phase of the original plan**, and the single
largest gap on this branch. Nothing exists yet:

- **No `smells/` Rust module.** The planned structure is absent:
  - 4.1 metric pass (cyclomatic, `nesting_depth`, `param_count`, WMC, CBO;
    LCOM4/ATFD already deferred to 4.5) — **not computed anywhere**.
  - 4.2 `smells/rule.rs` — `SmellRule` trait, `EvalContext`, `Finding`,
    `Scope`, `Severity` — **not written**.
  - 4.3 concrete rules (struct-based thresholds) + `SmellEngine`
    (`Vec<Box<dyn SmellRule>>` loop) — **not written**.
  - 4.4 `SmellRegistry` PyClass + `@mcp.tool("get_smells")` in server.py —
    **not written**.
- **MCP tool surface is still 17 tools, not 18.** There is no
  `get_smells` tool (verified: `server.py` has 17 `@mcp.tool` registrations,
  none named `get_smells`).
- **No metrics on the `Function`/`Class` structs.** Existing derivable
  metrics are limited to `line_count` (`exit_line - line`), `param_count`
  (`parameters.len()`), `caller_count`/`callee_count` (reverse indexes),
  `method_count` (scan by `parent_class`) — all via Pest queries, not a
  native metrics pass.

**Design guidance (already decided in planning):** independent of
traversal, no blockers — can start immediately. It is the natural next
commit after this doc lands.

---

## 4. (Removed) — previously "in progress / not started" sections

Both Phase-2 caveats (formerly "in progress, 1 test failing" and
"not started, medium risk") are now **done and committed** — see §1.4 and
§1.5. The only not-started work is Phase 4 (§3) and the deferred items (§7).

---

## 5. Immediate next actions (in order)

1. **Phase 4 smell engine** — start with 4.1 (metrics) since rules (§4.3)
   depend on it. Land as one commit per sub-step.
2. (Optional, parallel) **`as_of` temporal traversal** — route
   `CodeGraphStore::traverse` (storage.rs:144, exists but unexposed) through
   the binding once persisted IMPORTS/EXTENDS/OVERRIDES edges are confirmed
   in the ledger (§7.1).
3. (Optional) **Coverage deepening** — relative-path-aware base resolution,
   same-package-before-global-unique precedence, `@/...` alias handling in
   `find_module_by_dotted_name` (§7.3).
4. Sync the matrix's "not persisted" columns — IMPORTS/EXTENDS/OVERRIDES are
   now persisted (§1.4/§1.5); only `as_of` reads remain deferred.

---

## 6. Test budget

- Baseline before this branch: 180 Rust + 351 Python = 531.
- After Phase D + 1 + 3A: 194 Rust + 351 Python = **545**.
- After Phase 2 caveat 1: 195 Rust + 351 Python = **546**.
- After Phase 2 caveat 2: 195 Rust + 351 Python = **546, 0 failures**
  (same count; D.5 assertion strengthened from `>` to exact `==`).

---

## 7. DEFERRED, MISSING, AND KNOWN GAPS (honest inventory)

This is the "is anything left hanging?" section. Everything below is
**deliberately out of scope for this branch's committed work**, but is a
real limitation a reader should not mistake for "done".

### 7.1 Temporal traversal — `as_of` (matrix step E, 2nd half) ⛔
- `traverse(as_of=<ts>)` raises `PyNotImplementedError` (lib.rs:956). Only
  current-state traversal works.
- `CodeGraphStore::traverse` (storage.rs:144, `TraversalBuilder` +
  `load_subgraph_with(..., "now", …)`) **exists in Macrame but is not
  exposed to Python** — the binding never calls it.
- `codegraph_as_of` MCP tool does point-in-time *reconstruction* (materialised
  state at a timestamp), **not** point-in-time *traversal*.
- **Impact:** persisted IMPORTS/EXTENDS/OVERRIDES/CALLS edges (§1.4/§1.5) are
  write-only today. They're in the ledger but nothing reads them back except
  a full re-`analyze()`.

### 7.2 Snapshot I/O + cold-start (Phase 3B) ⛔
- `export_snapshot(path)` / `load_snapshot(path)` → `PyNotImplementedError`.
- `coderadar.load(db_path)` → `NotImplementedError` (Python layer).
- **Impact:** every session must re-`analyze()` from source. There is no
  load-from-Macrame-ledger path, no incremental resume. The ledger exists and
  is written (concepts + edges), but cold-start is unimplemented.

### 7.3 Resolve heuristic ceilings (correct-but-incomplete coverage)
These are honest limits of the base-name + import resolution heuristics, not
wiring bugs. They cap `subclasses`/`overrides`/`importers` coverage:

- **`resolve_base_by_name`** returns `None` on *ambiguity* — if two or more
  classes share a name across the project, neither resolves. No
  relative-path-aware or same-package-before-global precedence.
- **External bases stay unresolved by design** — `extends React.Component`,
  `Error`, `EventEmitter` are not local classes; resolving external→local
  would be wrong. This is the dominant ceiling on codegraph-main (subclasses
  28, overrides 7, with the remainder mostly external/ambiguous inheritance).
- **`find_module_by_dotted_name`** matches by file-path suffix; it is weak
  for `@/...` aliases and bare package names (correctly `Unresolved` for
  those). No tsconfig/pyproject alias awareness.

### 7.4 Cross-cutting roadmap deferrals (unchanged, out of this branch)
- **SQLite PRAGMAs (#6)** — Macrame's `configure()` is private; tuning
  (journal_mode, cache_size) blocked.
- **Parse-error early rejection (#3)** — false positives on real Kotlin code,
  so not enabled by default.
- **String arena / `Arc<String>` interning (#4)** — large refactor, post-v1.
- **Columnar / flat-buffer extraction (#11)** — high risk, post-v1.
- **Plugin API, Stack Graphs, Distributed snapshots** — post-v1.
- **Call-site cascade for `update_signature`** — argument spans are not
  indexed, so call-site arg rewrites surface as `unverified_sites` instead
  of being auto-edited.

### 7.5 Structural invariants (documented, not bugs)
- **`class.methods: Vec<EntityId>` is always `vec![]`** — never populated by
  `build_fragment`; method lookup scans `projection.functions` by
  `parent_class` (the `resolve_one_function` pattern). A reader must not
  expect `class.methods` to be meaningful.
- **`Module` is a synthetic in-memory entity** — `build_fragment`/`insert_extracted`
  build it at the end (id `"{}::module"`). It is *now* also persisted as a
  concept (§1.5), but its member lists (`classes`/`functions`/`imports`/…)
  are populated only in-memory; the persisted concept's `content` metadata
  carries name/path/language only.
- **Modules are the only concepts with no `Module` unit from the extractor**
  — `synthesize_module_unit` fills that gap at the callers, not inside
  `extract_single_pass` (which is node-driven and has no `Language`).

### 7.6 Net assessment
The branch's *traversal* goal is met for **current-state, in-memory**
traversal across all 4 edge kinds, with the data actually populated behind
them. What remains is:
1. **Phase 4 smell engine** — zero code, the headline "smell" half of the
   branch name (start now, §3).
2. **Temporal reads** (`as_of` traversal + cold-start) — persistence is
   write-only until Phase 3B.
3. **Heuristic depth** — resolution coverage beyond same-module/global-unique
   and path-suffix import matching (optional deepening).
