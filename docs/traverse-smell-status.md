# `traverse_smell` Branch — Status, Findings, & Planning

Branch: `traverse_smell` (off main @ `4f40d1f`). Goal: generalized Rust
graph-traversal binding + native code-smell engine, per the consolidated plan.

This file is a **living checkpoint**: what's done, what's broken right now,
what each finding means, and the remaining plan. It complements
`docs/traversal-matrix.md` (the pre-flight artifact that motivated doing
"Step D" — the resolve back-fill — ahead of schedule).

---

## 0. At a glance

| Phase (plan) | Status | Commit |
|---|---|---|
| Phase D — resolve back-fill (subclasses/importers/overrides) | ✅ done | `445b0c4` |
| Phase 1 — generalized Rust `traverse` binding; delete Python BFS | ✅ done | `4024bc0` |
| Phase 3A — honesty pass (`export`/`load_snapshot` raise loudly) | ✅ done | `f498c66` |
| Phase 2 caveat 1 — `member_expression` base stringification | ✅ done (committed below) |
| Phase 2 caveat 2 — Module concepts → unblock IMPORTS persistence | ✅ done (committed below) |
| Phase 4 — native Rust smell engine | ⏳ not started |

Current working tree: **committed** — see §2.1 (Phase 2 caveat 1 landed).
Test budget: 195 Rust + 351 Python = **546, 0 failures**.

---

## 1. What is DONE and verified

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
- `persist_edges` asserts EXTENDS + OVERRIDES to Macrame. IMPORTS **guarded
  out** (Phase 2 caveat 2 — modules aren't Macrame concepts).
- New read-only `index_edge_stats()` pyfunction for observability.
- Wired into **both** `analyze()` and `update_file`.

Tests: +4 Rust (3 back-fill unit + 1 persistence D.5).
Real-world (codegraph-main, 605 mod / 1176 cls / 4236 fn / 2234 imp):
imports 1968/2234 resolved, subclasses 6, overrides 1. Coverage ceilings
are base-name heuristic limitations (matrix §2), not wiring bugs.

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
inherits-alias). **194 Rust + 351 Python = 545, 0 failures.**
Real-world: MCP path returns **422 reachable** depth-3 from a real fn.

### 1.3 Phase 3A — honesty pass (`f498c66`)
`export_snapshot`/`load_snapshot` returned `Ok(())` unconditionally →
`coderadar export` silently produced no file; `coderadar.load(db_path)`
silently returned an empty graph (worse — silent partial data).

Both Rust pyfunctions now raise `PyNotImplementedError` with a link to
`docs/traversal-matrix.md §3`. Python `load(db_path)` raises
`NotImplementedError` too. CLI `export`/`load` catch and print cleanly.
No behaviour change for any working path; cold-start from Macrame is
Phase 3B. 545 tests, 0 failures.

---

## 2. What is IN PROGRESS and currently failing

### 2.1 Phase 2 caveat 1 — `member_expression` base stringification — ✅ DONE

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

(remaining coverage ceiling: external bases like `extends React.Component`
correctly stay unresolved — resolving external→local would be wrong.)

---

## 3. What is NOT started (remaining plan)

### 3.1 Phase 2 caveat 2 — Module concepts → unblock IMPORTS persistence — ✅ DONE

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

### 3.2 Phase 4 — native Rust smell engine
Per the consolidated plan:
- 4.1 Rust metric pass in the tree-sitter extraction (cyclomatic,
  nesting_depth, param_count, WMC, CBO; defer LCOM4/ATFD to 4.5).
- 4.2 `smells/rule.rs` — `SmellRule` trait, `EvalContext`, `Finding`,
  `Scope`, `Severity`.
- 4.3 Concrete rules (struct-based thresholds) + `SmellEngine`
  (`Vec<Box<dyn SmellRule>>` loop).
- 4.4 `SmellRegistry` PyClass + `@mcp.tool("get_smells")` in server.py.

No design blockers identified. Independent of traversal — can parallelize
with Phase 2.

---

## 4. Key findings worth keeping (even if Phase 2 is abandoned)

1. **The three reverse indexes were silently dead until Phase D.** This
   is the single most important discovery of the branch. Any future
   "why does imports/extends/overrides traversal return 0?" question is
   answered by: were the resolve passes run? (matrix §0).

2. **`compute_all_mro` was same-module-only**, blocking cross-file MRO →
   `overrides` was 0 forever. Phase D routed it through
   `resolve_base_by_name`. This is a genuine correctness improvement
   that shipped in `445b0c4` and is covered by `test_resolve_overrides_…`.

3. **`overrides_base` needed a forward index** (not just reverse
   `overridden_by`) for downstream traversal. Added to `ProjectedGraph`
   in Phase D, used by the binding in Phase 1. `extends` downstream needed
   no forward index — derived directly from `Class.resolved_bases`
   (matrix §2 derivation verdict).

4. **`graph.rs` tests can't use the lib.rs `traverse` pyfunction** (it
   reads GLOBAL_GRAPH). So the BFS core was extracted to
   `CodeGraph::traverse_bfs` as a `pub(crate)` associated fn — this is
   the pattern to reuse for any future pyfunction that needs unit testing
   in graph.rs.

5. **The plan's PyO3 sketch was outdated**: it used `graph: &ProjectedGraph`
   as an arg and `py.detach`. Real surface: `with_graph` + `py.allow_threads`
   (PyO3 0.24). The plan's `EdgeKind` enum doesn't exist — kinds are string
   consts (`storage.rs:40`) + `u8` indices (`buffers.rs:71`).

---

## 5. Immediate next actions (in order)

1. **Phase 4 (smell engine)** — the last remaining phase. Independent of
   traversal; can start now.
2. Sync `docs/traversal-matrix.md` §1.5 (PyO3 version note is already
   modernized; §1.4 updated).
3. Consider updating the matrix's "Populated?" / "not persisted" columns
   (§1.4 and the D row) to reflect that IMPORTS/EXTENDS/OVERRIDES are now
   persisted (Phase 2 caveat 2 done).

## 6. Test budget

- Baseline before this branch: 180 Rust + 351 Python = 531.
- After Phase D + 1 + 3A: 194 Rust + 351 Python = **545**.
- After Phase 2 caveat 1: 195 Rust + 351 Python = **546**.
- After Phase 2 caveat 2: 195 Rust + 351 Python = **546, 0 failures**
  (same test count; D.5 assertion strengthened from `>` to exact `==`).