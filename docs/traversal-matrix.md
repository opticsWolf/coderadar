# Traversal Matrix & Pre-flight Verification

Branch: `traverse_smell` · base `4f40d1f` (main)

This document is the **non-negotiable prerequisite** for the Rust `traverse()`
binding work. It records the actual state of the in-memory reverse indexes,
resolves every `⚠ VERIFY` in the implementation plan against the real code,
and corrects one assumption that turned out to be false.

---

## 0. Headline finding (corrects the plan)

The implementation plan assumed all five reverse indexes
(`callers_by_callee`, `callees_by_caller`, `importers`, `subclasses`,
`overridden_by`) are *populated* and ready for traversal.

**They are not.** Only the two **call** indexes carry data. The other three
are declared on `ProjectedGraph`, initialized to `HashMap::new()` in
`build_fragment` (`core_indexer/src/graph.rs:1369-1373`), merged at
`core_indexer/src/lib.rs:498-502` (moving empty maps into empty maps),
and **never written to anywhere in the crate**.

Verified exhaustively:

```
$ rg --type rust "importers|subclasses|overridden_by" \
  | rg -i "entry|insert|extend|push|get_mut|or_default"
# → only matches are HashMap::new() inits, fragment-merge moves,
#   remove()/clear()/retain() cleanup, and one test literal in resolve/cache.rs.
#   ZERO population sites.
```

The forward sides are equally empty:

- `Class.resolved_bases` is set to `vec![]` in `build_fragment`
  (`graph.rs:~1380`) and **never filled** (no `make_mut`/push anywhere).
- `Import.resolution` is set to `ImportResolution::Unresolved` in
  `build_fragment` and **never overwritten**.
- The only `Arc::make_mut` writes in the crate touch `embedding`
  (`graph.rs:1184-1233`), never `resolved_bases`/`resolution`/reverse indexes.
- `resolve_calls_scoped` (`graph.rs:847`) resolves **call** edges only —
  which is exactly why the two call indexes have data and the others don't.

**Consequence:** `codegraph_traverse` with `edge_kinds=["imports"]` /
`["inherits"]` / `["overrides"]` returns empty today **not** because of the
edge-filter bug fixed in v0.6.4 Bug #2, but because there is no index data
behind those kinds. The v0.6.4 `['imports'] → 0` result was
*correct-by-accident* (empty index), not a working filter.

So Phase 1 is **not** "add a Rust binding on top of existing data." It is,
in priority order:

1. **Honesty pass (3A)** — flag the three dead index fields and the
   `load_snapshot`/`export_snapshot` stubs.
2. **Calls-only Rust `traverse` binding** — real FFI-roundtrip win, ships now
   on the only edge kind that has data.
3. **Resolve backfill** — the actual feature that imports/extends/overrides
   traversal depends on (resolve import targets, invert subclass/override).
   Substantial; this is where edge-kind generality is *earned*.

---

## 1. Verification of the plan's seven "verify before coding" items

### 1.1 `EdgeKind` enum

There is **no Rust `EdgeKind` enum**. Edge kinds are:

- String constants in `core_indexer/src/storage.rs:40-48` (`pub mod edge_type`):
  `CONTAINS`, `CALLS`, `IMPORTS`, `EXTENDS`, `IMPLEMENTS`, `REFERENCES`,
  `DECORATES`, `INSTANTIATES`, `OVERRIDES`.
- `u8` indices in `core_indexer/src/buffers.rs:71` (`edge_kind_index`):
  `contains=0, calls=1, imports=2, extends=3, implements=4, …` (stable, tested
  at `buffers.rs:592-597`).

`persist_edges` (`graph.rs:1112`) hardcodes the string `"CALLS"`. Phase 3B
"extend the `EdgeKind` enum" therefore means *adding string constants / index
entries*, not extending a Rust enum. No enum to change.

### 1.2 `ProjectedGraph` struct

`core_indexer/src/types.rs:997`. Five reverse-index fields (all
`HashMap<EntityId, BTreeSet<EntityId>>`):

```rust
pub importers: ...,        pub callers_by_callee: ...,
pub callees_by_caller: ..., pub subclasses: ...,
pub overridden_by: ...
```

### 1.3 `server.py:_traverse`

`py_agent/src/coderadar/mcp/server.py:1278`. It calls
`graph.traverse(entity_id, depth, edge_kinds, macrame_dir)` →
`MacrameQuery.traverse` (`py_agent/src/coderadar/query/executor.py:62`) →
`from coderadar._core import traverse` → **`ImportError`** →
`_projected_traverse` (pure-Python BFS).
`_neighbors` (`executor.py:102`) returns `[]` for any non-call kind, by
design ("the fallback can only honour call-edge requests").

### 1.4 `assert_edges_bulk` signature (`graph.rs:1112`)

The surrounding fn is `persist_edges(&self, projection: &ProjectedGraph)
-> Result<usize, macrame::DbError>` (`graph.rs:~1397`). It asserts CALLS +
IMPORTS + EXTENDS + OVERRIDES edges (batched at 200, filtering
`external::`/`builtins.` targets). **Updated (Phase 2 caveat 2):** IMPORTS
eges are no longer guarded out — `synthesize_module_unit` now prepends a
Module unit in `extract_only`/`index_file_inner`/`update_file`, so
`build_concept` persists modules as Macrame concepts and the module→module
FK target exists. (The guard previously existed because modules were
in-memory-only; see the old §1.4 note.)

### 1.5 PyO3 version

`core_indexer/Cargo.toml:12`: `pyo3 = { version = "0.24", features =
["extension-module", "abi3-py39"] }`. → Use **`py.allow_threads(...)`**,
**not** `py.detach(...)` (removed in PyO3 0.21+). The plan's API sketch is
outdated and must be modernized. The crate currently uses **neither**
(`allow_threads`/`detach` appear nowhere — all functions hold the GIL
throughout, mirroring `callers_of`/`callees_of`).

### 1.6 Macrame store API

`core_indexer/src/storage.rs::CodeGraphStore`:
- `assert_edge`, `assert_edges_bulk(Vec<EdgeAssertion>)` (`storage.rs:120-139`)
- `traverse(start_id, max_depth, edge_types) -> macrame::Result<Subgraph>`
  wraps `TraversalBuilder::new` + `db.load_subgraph_with(&traversal, "now",
  10_000_000)` (`storage.rs:144-158`) — **temporal via the `"now"` literal**;
  `as_of` would replace that string. Already exists, full edge-type aware,
  but only sees CALLS + synthetic edges (per §1.4).
- `reconstruct(ts) -> MaterializedState` (`storage.rs:160`).

### 1.7 MCP tool registration in `server.py`

`create_server(graph)` (`server.py:64`) instantiates `MCPServer("CodeRadar",
version="0.6.4", instructions=...)` and registers each tool with
`@mcp.tool(description=..., annotations={...})` on a plain, type-hinted
Python function (`server.py:135`, `162`, …, 17 tools through `531`). A new
`get_smells` tool follows the identical decorator pattern.

Pyfunction registration on the Rust side: `core_indexer/src/lib.rs:34-65`
(`m.add_function(wrap_pyfunction!(<fn>, m)?)?`). A new `traverse` must be
added there alongside `callers_of`/`callees_of` (`lib.rs:49-50`).

---

## 2. The Traversal Matrix (verified)

| Edge kind | Out-index (downstream) | In-index (upstream) | Populated? | Macrame-persisted? | TS extractor emits source? |
|-----------|------------------------|----------------------|------------|--------------------|----------------------------|
| `calls`   | `callees_by_caller`    | `callers_by_callee`  | ✅ built in `build_fragment` from resolved/raw calls | ✅ `CALLS` + synthetic | ✅ `function.calls`/`resolved_calls` |
| `imports`  | *(none)* — `Import` has no importer field, would need `imports_by_importer` | `importers` (field) | ❌ **never populated**; `Import.resolution` stuck at `Unresolved` | ❌ not persisted | ⚠ partial — Import entities exist in `proj.imports`, but no resolution/back-index |
| `extends`  | derive from `Class.parent_class` / `resolved_bases` | `subclasses` (field) | ❌ `subclasses` **never populated**; `resolved_bases` stuck at `vec![]` (fwd side empty too); only `parent_class: Option` is set from the AST | ❌ not persisted | ✅ `Class.parent_class` + raw `bases` extracted |
| `overrides`| *(none)* — `Function` has no "overrides base" field | `overridden_by` (field) | ❌ **never populated** | ❌ not persisted | ⚠ not extracted as an edge |

"Populated?" is the column the original plan was missing. It is the gating
fact for everything downstream.

### Downstream-derivation verdict (resolves the "build symmetric indexes vs derive" fork)

- **`extends` downstream**: `Class.parent_class: Option<EntityId>` (and
  `resolved_bases: Vec<EntityId>`, once filled) gives a **single/Vec field
  lookup — no forward index needed**. Cheap to derive.
- **`imports` downstream**: `Import` has **no importer field** and
  `resolution` is `Unresolved`. A forward index `imports_by_importer` is
  required (or an O(E) scan).
- **`overrides` downstream**: `Function` has **no "overrides base" field**.
  A forward index `overrides_base` is required (or an O(E) scan, or MRO
  inference — non-trivial).

Net: even after a resolve backfill, traversal symmetry needs **two new
forward indexes** (`imports_by_importer`, `overrides_base`). `extends`
needs none (derive from the struct field).

---

## 3. Honesty-pass targets (Phase 3A) — blast radius

From the `Ok(())`/silent-empty audit:

| Site | Status | Caller? | Action |
|------|--------|---------|--------|
| `lib.rs:991` `export_snapshot` | `Ok(())` stub | ✅ `cli.py:404` `graph.export_snapshot(path)` → `_core.export_snapshot` (`__init__.py:601-604`) | Loud `PyNotImplementedError` (**breaks `coderadar export` — intended honesty**) |
| `lib.rs:994` `load_snapshot` | `Ok(())` stub | ❌ no Python import (`__init__.py:load` just returns `CodeGraph(db_path)`) | Loud `PyNotImplementedError` (safe) |
| `graph.rs:1369-1373` dead index fields | declared, never built | used by traversal (returns empty) | Document + phase-1 backfill; until then `_neighbors` already honestly returns `[]` for non-call kinds |
| `storage.rs:144` `traverse` temporal | uses `"now"` literal | not exposed to Python | Phase 3B: route `as_of` here only after edges persisted |

Other `Ok(())` hits are legitimate early-returns (empty-input guards in
`storage.rs:100/107`, `register_synthetic_edge` success, watcher, etc.) —
not stubs. No action.

---

## 4. Corrected sequencing for `traverse_smell`

Replaces the plan's Phase-1 premise. Each line is an independently-mergeable
commit/PR.

| Step | Deliverable | Risk | Depends on |
|------|-------------|------|------------|
| **A** | Honesty pass: `export_snapshot`/`load_snapshot` → `PyNotImplementedError`; doc-comment the three dead index fields with a link to this matrix. | low (one CLI behavior change) | — |
| **B** | Calls-only Rust `#[pyfunction] traverse` over `with_graph` snapshot, mirroring `callers_of`/`callees_of`; GIL released via `py.allow_threads` for the BFS, re-acquired for `entity_ref_to_dict`. Delete the Python BFS fallback in `executor.py` (route to Rust, raise loud `ImportError` if missing). | medium | A |
| **C** | Tests: cycle, diamond, self-loop, max_depth=0, empty edge_kinds, upstream/downstream/both, unknown-direction → `PyValueError`, `as_of` (non-calls) → `PyValueError`. Bench: `criterion` for depth-3 on codegraph-main call slice. | low | B |
| **D** | Resolve backfill — the real feature: fill `Import.resolution` + build `importers` (and `imports_by_importer`); fill `resolved_bases` + invert into `subclasses`; detect overrides into `overridden_by` (+ `overrides_base`). Persist IMPORTS/EXTENDS/OVERRIDES in `persist_edges`. | **high** — touches extract+resolve+graph | B |
| **E** | Generalize the Rust `traverse` binding to all 4 edge kinds (now backed by data). Wire `as_of` → `CodeGraphStore::traverse` for persisted kinds. | medium | D |
| **F** | Phase 2 (smell engine) — independent of traversal; can be parallelized. | medium | — |

**Phase 1 of the original plan = steps A+B+C here.** Step D is the work the
plan under-specified; without it, "all 4 edge kinds" is a promise the
in-memory graph cannot keep.

---

## 5. Open decision for the maintainer

Step **D** (resolve backfill) is the difference between a calls-only Rust
traverse (real, shippable, smaller) and the full 4-edge-kind traverse the
plan promises (substantial new resolve machinery). Recommended path:

1. Land **A→C** now: calls-only Rust traverse + honesty pass.
2. Decide D's scope separately — it is a feature, not a binding exercise,
   and competes with other v0.6.x work.

This document exists precisely so that decision is made with the real
matrix in front of it, not the assumed one.