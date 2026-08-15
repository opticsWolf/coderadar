# Refactoring Plan: split `core_indexer/src/graph.rs` into a `src/graph/` module tree

Status: **proposed, pending approval**
Date: 2026-07-09
Baseline: commit `1fcafb4` (v0.6.6), rustc/cargo 1.97.1, `cargo check --lib` green with 50 pre-existing warnings.

---

## 1. Goal and invariants

Split `core_indexer/src/graph.rs` (4,770 lines, ~101 tests + 6 helpers in one module) into
`core_indexer/src/graph/` as a module directory, following the tree in §3.

Hard invariants — any violation aborts the step:

1. **No behavior change.** Code is moved verbatim (line-range slicing, never retyped). No logic,
   formatting inside bodies, or doc-comment edits.
2. **No public API change.** Everything currently reachable via `crate::graph::…` from outside the
   module stays reachable at the same path with the same visibility (verified surface in §2.2).
3. **Visibility only widens where mechanically required** (§5), from `private` to `pub(super)`,
   never beyond. `pub`, `pub(crate)`, and `pub` field semantics are untouched.
4. **Gate per step:** `cargo check --lib` (no errors; warning count must not grow beyond the
   delta documented in §5). Test phase gates with `cargo test -p core_indexer`.

## 2. Verified current state

### 2.1 Line map of `graph.rs` (1-indexed, verified by grep)

| Lines | Item | Visibility |
|---|---|---|
| 1–21 | Header comments + crate-level `use` block | — |
| 22–28 | `normalize_path_str` | `fn` (private) |
| 31–43 | `ImportNode`, `ImportGraph` | `pub struct` |
| 45–167 | `impl ImportGraph` (7 `pub fn`) | — |
| 169–185 | `CallNode`, `CallEdge`, `CallGraph` | `pub struct` |
| 187–281 | `impl CallGraph` (3 `pub fn`) | — |
| 284–438 | `GraphConfig` + Default, `ResolutionConfig`, `StackGraphConfig`, `ImportGraphConfig`, `SignatureConfig`, `MemoryConfig`, `MutationConfig`, `QueryConfig`, `GitConfig` | `pub struct` |
| 439–511 | `find_module_by_dotted_name` | `pub(crate) fn` |
| 513–537 | `find_symbol_in_module` | `fn` (private) |
| 537–562 | `CodeGraph` struct (pub fields) | `pub struct` |
| 564–608 | `c3_merge` | `fn` (private) |
| 609–2774 | `impl CodeGraph` (37 methods) | see §2.3 |
| 2776 | `// ── Tests` comment | — |
| 2778–4770 | `#[cfg(test)] mod tests` (101 tests, 6 helpers) | — |

Method map inside `impl CodeGraph` (fn start lines): new 610 · snapshot 642 · commit_projection
647 · get_function 654 · get_class 659 · get_module 664 · callers_of 669 · callees_of 678 ·
with_store 689 · has_store 695 · compute_all_mro 700 · compute_c3_mro 714 · resolve_base_by_name
770 · base_candidates 784 · import_aware_base 824 · resolve_class_hierarchy 862 · resolve_imports
920 · populate_class_methods 980 · resolve_overrides 1011 · neighbors_of 1083 · traverse_bfs
1120 · count_unresolved_targets 1157 · resolve_all_calls 1193 · resolve_one_function 1205 ·
resolve_calls_scoped 1307 · persist_entities 1544 · persist_edges 1559 · register_synthetic_edge
1665 · set_embedding 1696 · clear_embeddings_for_file 1725 · set_module_star_exports 1769 ·
ts_language 1786 · index_file 1795 · index_file_accumulate 1809 · synthesize_module_unit 1829 ·
extract_only 1850 · build_fragment 1891 · index_file_inner 2052 · remove_file_entities 2087 ·
apply_diff_update 2231 · update_file 2469 · insert_extracted 2554.

### 2.2 External API surface (every `crate::graph::…` reference outside the module, via grep)

| Caller | Uses |
|---|---|
| `lib.rs` | `GraphConfig`, `CodeGraph` |
| `mutation/mod.rs` | `MutationConfig`, `SignatureConfig`, `GitConfig`, `CodeGraph::traverse_bfs`, `CodeGraph::count_unresolved_targets`, `CodeGraph::ts_language` |
| `storage/mod.rs` | `ImportGraph`, `ImportNode`, `CodeGraph::extract_only` |
| `storage/projection_ops.rs` | `CodeGraph::build_fragment`, `ImportGraph::build_import_edges` |
| `extract/mod.rs` | `CodeGraph`, `ImportGraph` |
| `resolve/stack_graph.rs` | `CallGraph`, `CallNode`, `CallEdge` |
| test module | `super::find_module_by_dotted_name`, `crate::graph::find_module_by_dotted_name`, `crate::graph::CodeGraph::ts_language` |

→ `mod.rs` must re-export: `CodeGraph`, `ImportGraph`, `ImportNode`, `CallGraph`, `CallNode`,
`CallEdge`, `GraphConfig` + the other 8 config structs, and `find_module_by_dotted_name` (so the
`crate::graph::` path used by tests keeps working).

### 2.3 Method visibilities (verified)

- `pub(crate)`: `neighbors_of`, `traverse_bfs`, `count_unresolved_targets`, `synthesize_module_unit`
- `pub`: all the rest (`extract_only`, `build_fragment`, `ts_language`, persist*, resolve*,
  update_file, index_file*, …)
- **private**: `compute_c3_mro`, `resolve_base_by_name`, `base_candidates`, `import_aware_base`,
  `resolve_one_function`, `resolve_calls_scoped`, `index_file_inner`, `insert_extracted`
- Free fns private: `normalize_path_str`, `find_symbol_in_module`, `c3_merge`;
  `find_module_by_dotted_name` is `pub(crate)`.

## 3. Target module tree

```
core_indexer/src/graph/
├── mod.rs              # CodeGraph struct, anchor impl, re-exports
├── import_graph.rs     # ImportNode, ImportGraph
├── call_graph.rs       # CallNode, CallEdge, CallGraph
├── config.rs           # GraphConfig + sub-configs (pure data)
├── module_resolution.rs# normalize_path_str, find_module_by_dotted_name, find_symbol_in_module
├── mro.rs              # c3_merge, compute_all_mro, compute_c3_mro, resolve_base_by_name,
│                       # base_candidates, import_aware_base
├── inheritance.rs      # resolve_class_hierarchy, resolve_imports, populate_class_methods, resolve_overrides
├── traversal.rs        # neighbors_of, traverse_bfs, count_unresolved_targets
├── resolve_calls.rs    # resolve_all_calls, resolve_one_function, resolve_calls_scoped
├── persistence.rs      # persist_entities, persist_edges, register_synthetic_edge
├── embeddings.rs       # set_embedding, clear_embeddings_for_file, set_module_star_exports
├── indexing.rs         # ts_language, index_file, index_file_accumulate, synthesize_module_unit,
│                       # extract_only, build_fragment, index_file_inner
├── projection_ops.rs   # remove_file_entities, apply_diff_update, update_file, insert_extracted
└── tests/
    ├── mod.rs          # `use super::*;`, 6 shared helpers, submodule decls
    ├── import_graph_tests.rs
    ├── call_graph_tests.rs
    ├── mro_tests.rs
    ├── inheritance_tests.rs
    ├── traversal_tests.rs
    ├── indexing_tests.rs
    ├── embedding_tests.rs
    ├── persistence_tests.rs
    ├── projection_tests.rs   # NEW (not in original plan — see §11)
    └── query_compile_tests.rs
```

### `mod.rs` (residual content)

```rust
// (original header comment 1–4 kept)
use std::sync::Arc;
use lru::LruCache;                       // used by CodeGraph::new (resolution_cache: LruCache::new(256))
use parking_lot::RwLock;
use crate::resolve::cache::ResolutionCache;
use crate::resolve::stack_graph::StackGraphResolver;
use crate::types::*;

pub mod import_graph; pub mod call_graph; pub mod config; pub mod module_resolution;
pub mod mro; pub mod inheritance; pub mod traversal; pub mod resolve_calls;
pub mod persistence; pub mod embeddings; pub mod indexing; pub mod projection_ops;

pub use import_graph::{ImportGraph, ImportNode};
pub use call_graph::{CallGraph, CallNode, CallEdge};
pub use config::*;
pub(crate) use module_resolution::find_module_by_dotted_name;  // keeps crate::graph::… path (tests)
pub(crate) use module_resolution::find_symbol_in_module;       // preserves current pub surface
// (find_module_by_dotted_name stays pub(crate) in its home module; see §5 note)

pub struct CodeGraph { …verbatim… }   // lines ~537–562, incl. pub fields

impl CodeGraph {
    pub fn new(…) { …verbatim… }            // 610–641
    pub fn snapshot(…) { …verbatim… }        // 642–646
    pub fn commit_projection(…) { …verbatim… } // 647–653
}

// ── Tests ──
#[cfg(test)]
mod tests;
```

## 4. Per-file specs (content, imports, notes)

All bodies are moved **verbatim**. Only the leading `use` block and (for `mod.rs`) the
re-exports are new text. Doc comments always travel with the item below them.

| File | Content (anchor items) | `use` block to add |
|---|---|---|
| `import_graph.rs` | `ImportNode`, `ImportGraph`, `impl ImportGraph` (31–167) | `std::collections::BTreeSet`; `std::path::PathBuf`; `dashmap::DashMap`; `petgraph::stable_graph::{NodeIndex, StableDiGraph}`; `crate::types::*` |
| `call_graph.rs` | `CallNode`, `CallEdge`, `CallGraph`, `impl CallGraph` (169–281) | `std::collections::{BTreeSet, HashMap}`; `dashmap::DashMap`; `petgraph::stable_graph::{NodeIndex, StableDiGraph}`; `crate::types::*` |
| `config.rs` | all 9 config structs + Defaults (284–438) | — (std prelude only) |
| `module_resolution.rs` | `normalize_path_str` (24), `find_module_by_dotted_name` (439), `find_symbol_in_module` (513) | `crate::types::*` |
| `mro.rs` | `c3_merge` (564) + `impl CodeGraph { compute_all_mro, compute_c3_mro, resolve_base_by_name, base_candidates, import_aware_base }` (700–860) | `crate::types::*`; `super::CodeGraph` |
| `inheritance.rs` | `impl CodeGraph { resolve_class_hierarchy, resolve_imports, populate_class_methods, resolve_overrides }` (862–1082) | `crate::types::*`; `super::CodeGraph`; `super::module_resolution::find_module_by_dotted_name` |
| `traversal.rs` | `impl CodeGraph { neighbors_of, traverse_bfs, count_unresolved_targets }` (1083–1192) | `crate::types::*`; `super::CodeGraph` |
| `resolve_calls.rs` | `impl CodeGraph { resolve_all_calls, resolve_one_function, resolve_calls_scoped }` (1193–1543) | `crate::types::*`; `super::CodeGraph`; `super::module_resolution::{find_module_by_dotted_name, find_symbol_in_module}` (inline `use` inside fns at 1308–1309 move verbatim) |
| `persistence.rs` | `impl CodeGraph { persist_entities, persist_edges, register_synthetic_edge }` (1544–1695) | `crate::types::*`; `super::CodeGraph` (`EdgeAssertion`/`crate::storage` are already fully-qualified inline; add `macrame::graph::EdgeAssertion` if not) |
| `embeddings.rs` | `impl CodeGraph { set_embedding, clear_embeddings_for_file, set_module_star_exports }` (1696–1785) | `crate::types::*`; `super::CodeGraph` |
| `indexing.rs` | `impl CodeGraph { ts_language, index_file, index_file_accumulate, synthesize_module_unit, extract_only, build_fragment, index_file_inner }` (1786–2086) | `crate::types::*`; `super::CodeGraph` (`tree_sitter`, `crate::extract`, `crate::storage` referenced fully-qualified inline or with one-line `use tree_sitter_language_pack::get_language;` inside `ts_language`) |
| `projection_ops.rs` | `impl CodeGraph { remove_file_entities, apply_diff_update, update_file, insert_extracted }` (2087–2773) | `crate::types::*`; `super::CodeGraph`; `super::module_resolution::normalize_path_str` |

Notes:

- Multiple `impl CodeGraph` blocks across files are legal Rust (same crate). No orphan rules
  involved.
- `resolve_calls_scoped` keeps its internal `std::thread::scope` + `std::sync::Mutex` verbatim;
  those are already fully qualified, so no new imports are needed.
- `find_module_by_dotted_name` stays `pub(crate)` in `module_resolution.rs` — that visibility
  already covers all sibling files and the tests; no change needed beyond the re-export in
  `mod.rs` for the `crate::graph::…` path.

## 5. Visibility deltas (minimal set, mechanically derived from call-site grep)

Only private items called from another file after the split must widen, and only to `pub(super)`
(visible to the `graph` module and all descendants — exactly the old same-module scope):

| Item | New visibility | Reason (call site) |
|---|---|---|
| `base_candidates` | `pub(super)` | called by `resolve_class_hierarchy` (inheritance.rs, line 882) |
| `resolve_calls_scoped` | `pub(super)` | called by `update_file` (projection_ops.rs, line 2526) |
| `insert_extracted` | `pub(super)` | called by `index_file_inner` (indexing.rs, line 2079) |
| `normalize_path_str` | `pub(super)` | called by projection_ops.rs (2095, 2240–2266, 2476) |
| `find_symbol_in_module` | `pub(super)` | called by `resolve_one_function` (resolve_calls.rs, line 1266) |

Verified **not** needed (call sites stay in-file or items already `pub`/`pub(crate)`):
`compute_c3_mro`, `resolve_base_by_name`, `import_aware_base`, `c3_merge`, `resolve_one_function`,
`index_file_inner` (in-file only); `ts_language`, `neighbors_of`, `traverse_bfs`,
`count_unresolved_targets` (already `pub`/`pub(crate)`); all `ImportGraph`/`CallGraph` methods
(already `pub`).

Verified by reading the test bodies: **no test calls a private item directly** — tests use only
`pub`/`pub(crate)` methods plus the helpers in `tests/mod.rs`. So no test-driven visibility bumps.

Expected warning delta: 0 (no new unused imports if §4 use-blocks are applied as specified; the
pre-existing unused `LruCache`/`EdgeAssertion`-style warnings, if any, are simply relocated).

## 6. Test split map (101 tests + 6 helpers)

Helpers stay in `tests/mod.rs` (shared; each test submodule gets them via `use super::*;`, the
same transitive-glob pattern the current single `tests` module already uses):
`index_source`, `make_call_node`, `make_call_edge`, `snapshot_from`, `fn_id_of`,
`graph_with_temp_store` (the last uses `tempfile` — dev-dep, stays valid in `cfg(test)`).

| Test file | Tests (name — current line) |
|---|---|
| `import_graph_tests.rs` (8) | add_and_find 2783, remove_file 2792, transitive 2802, multi_hop 2814, star_exports 2835, extension_agnostic 2877, nonexistent 2908, alias_aware 3354 |
| `call_graph_tests.rs` (5) | find_callers 2937, chain 2946, cycle_safe 2959, codegraph_snapshot 2968, callers_of_empty 2976 |
| `mro_tests.rs` (4) | c3_single 3162, c3_multiple 3176, mro_method_resolution 3191, c3_diamond 3624 |
| `inheritance_tests.rs` (8) | hierarchy 3216, imports_populates 3244, language_family 3279, ambiguous_base 3303, ts_typeonly 3327, import_aware 3400, overrides 3426, builtin_bases 4282 |
| `traversal_tests.rs` (11) | count_unresolved 3367, traverse_calls_down 3483, up 3503, cycle 3517, diamond 3529, depth_zero 3542, empty_kinds 3554, imports_up 3564, extends_down 3581, overrides_up 3594, inherits_alias 3611 |
| `indexing_tests.rs` (46) | kotlin(2) 2984/2995, typescript(2) 3017/3030, member_expr_base 3044, go(2) 3083/3094, java(2) 3109/3122, cpp(2) 3136/3147, ruby(2) 3650/3661, php(2) 3672/3681, csharp(2) 3692/3701, go_method_receiver 3712, swift(2) 4096/4106, scala 4118, lua(2) 4129/4138, elixir 4148, zig(2) 4160/4170, r 4180, fn_ref(5) 4192/4212/4229/4246/4264, literal_receiver 4307, grammar_kind(6) 4331–4381, fn_ref_cross_file 4440, rust_struct 3921, rust_method 3943, class_methods_27 3963, import_parsing(2) 3829/3851, cross_file(2) 3868/3896, module_children 4465, param_annotations 4492, return_type_filtered 4519 |
| `embedding_tests.rs` (5) | embedding_field 3727, cosine 3743, set_stores 3752, search_similar 3795, empty_vector 3818 |
| `persistence_tests.rs` (11) | persist_no_store(2) 4042/4052, persist_with_index(2) 4062/4074, synthetic(2) 4393/4419, temporal(4) 4541/4564/4583/4652, persists_imports_extends 4612 |
| `projection_tests.rs` (2) | update_file_adds 3986, update_file_removes 4014 |
| `query_compile_tests.rs` (1) | all_queries_compile 4682 (self-contained: its own inline `use crate::extract::tagger;` moves with it) |

Per-file imports: `use super::*;` only, except where a test file references items that are not
re-exported — the compiler gate catches those (expected: none, since tests use fully-qualified
`crate::…` paths for storage/extract/cosine per the grep in §2.2).

## 7. Mechanical slicing method (no manual copy)

1. **`git mv core_indexer/src/graph.rs core_indexer/src/graph/mod.rs`** (step 1, gate).
2. A Python slicer (`.harness/slice.py`, loop-safe, indent/anchor based — **no brace state
   machine**, which is what hung the first attempt):
   - item spans are anchored on start patterns (top-level `^(pub…)?(fn|struct|impl)`, method
     `^    (pub…)?fn `) and extended upward over contiguous `///`, `//`, `#[…]` lines;
   - end of a span = the line before the next anchored start in the same block (verified against
     the `^    }$` / `^}$` closer);
   - writes each target file = header comment + §4 use-block + concatenated spans (verbatim
     lines), and rewrites `mod.rs` per §3.
3. **Preservation invariant (strong check):** every non-blank, non-added line of the original
   file appears in exactly one new file, in original order within its file. The slicer asserts
   this (line multiset per file + concatenation order equals the original ranges) before writing.
   Added lines are only: per-file header comment, use-blocks, `mod`/`pub use` declarations in
   `mod.rs`, and blank separators.
4. Compile gate after every file move; fix import/visibility nits mechanically (compiler-driven).

## 8. Execution order and gates

| Step | Action | Gate |
|---|---|---|
| 0 | Baseline: record `cargo check --lib` warning list (done: 50 warnings, green) | — |
| 1 | `git mv graph.rs graph/mod.rs` | `cargo check --lib` |
| 2 | Extract `config.rs` (pure data, zero deps — lowest risk) | check |
| 3 | Extract `import_graph.rs` | check |
| 4 | Extract `call_graph.rs` | check |
| 5 | Extract `module_resolution.rs` (+ `pub(super)` on `normalize_path_str`, `find_symbol_in_module`) | check |
| 6 | Extract `mro.rs` (incl. `c3_merge`; `base_candidates` → `pub(super)`) | check |
| 7 | Extract `inheritance.rs` | check |
| 8 | Extract `traversal.rs` | check |
| 9 | Extract `resolve_calls.rs` (`resolve_calls_scoped` → `pub(super)`) | check |
| 10 | Extract `persistence.rs` | check |
| 11 | Extract `embeddings.rs` | check |
| 12 | Extract `indexing.rs` | check |
| 13 | Extract `projection_ops.rs` (`insert_extracted` → `pub(super)`); `mod.rs` now anchor-only | check |
| 14 | Move whole test module to `tests/mod.rs` verbatim | `cargo test -p core_indexer --no-run` |
| 15 | Peel test files one at a time: query_compile → import_graph → call_graph → mro → traversal → inheritance → embedding → persistence → projection → indexing | `cargo test --no-run` each; full `cargo test -p core_indexer` after last |
| 16 | Final: full `cargo test -p core_indexer`, `cargo check` at workspace root (pyo3 crate), diff warnings vs baseline, delete `.harness/` | all green |

Commits: one per step (13 source steps + test steps), so any regression bisects to a single
commit. Final squash is the user's choice.

## 9. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Transcription error | Verbatim line slicing + preservation invariant (§7.3); nothing is retyped by hand |
| Borrow-checker surprises from splitting `impl` blocks | None expected: bodies unchanged, multiple impl blocks per type are legal; the `thread::scope` closure in `resolve_calls_scoped` moves intact |
| Missed cross-module reference | Call-site grep done (§5); compiler gate per step catches the rest |
| `pub` fields accessed across files | Already `pub` on `CodeGraph`; no change |
| Test-helper visibility across submodules | Transitive `use super::*;` glob chain (same pattern as today's tests module) |
| Warning creep | Warning-count gate vs baseline; §4 use-blocks are minimal per file |
| Slicer bug (like the hung brace-matcher) | Anchor+indent based, hard-bounded loops, dry-run prints all spans for review **before** any write |

## 10. Rollback

Every step is an independent commit on the current `main` → rollback = `git reset --hard` to the
last green commit (or rebase-drop the bad commit). No external consumers are affected because the
public API is invariant (§1.2), so no consumer-side rollback is ever needed.

## 11. Deviations from the original plan (flagged for approval)

1. **`tests/projection_tests.rs` added** — `test_update_file_adds_entities` /
   `test_update_file_removes_entities` need a home; they don't fit "per-language indexing".
2. `find_module_by_dotted_name` stays `pub(crate)` in `module_resolution.rs` (no need to
   downgrade/upgrade) plus a `pub(crate) use` re-export in `mod.rs` to preserve the
   `crate::graph::…` path used by tests.
3. `set_module_star_exports` stays in `embeddings.rs` exactly as the original plan assigned it
   (thematically it's module-related, but pub + no cross-file calls → zero risk either way).
4. Test-peel gate is compile-only per file with one full test run at the end of step 15 (full
   suite per peel would be ~10× the wall time for no additional signal on a mechanical move).
