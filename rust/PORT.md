# CodeRadar â€” core-first port to Rust

Canonical copy of the decisions and state for moving the Python surface
(`py_agent/`, 20k LOC) onto the Rust engine so the whole tool links as Rust
crates. Started 2026-02 (repo at 0.16.0, `44c6cf9`).

## Decisions (user, step 0)

1. **Layout â€” mono crate.** One crate under `rust/` holds every ported unit:
   shared types, one dependency set. Existing `core_indexer` stays a workspace
   member and is linked as an rlib (`crate-type = ["cdylib", "rlib"]`); no
   function round-trips through Python.
2. **Target â€” pure Rust app, PyO3 wrappers stay.** The binary is the product.
   Thin PyO3 wrappers are kept only so Python can call the Rust API; they hold
   no logic. Plan: an optional `python` feature on the mono crate exposing the
   `#[pymodule]`. Fallback if `extension-module` link semantics bite the bin
   target: a zero-logic facade crate (`rust/coderadar-py`). This is the single
   permitted exception to the mono-crate rule.
3. **Shape â€” library first, then CLI, then MCP.** Long term one binary:
   `coderadar <command>` plus `coderadar mcp` stdio. In scope: CLI text and MCP
   JSON.
4. **Core scope â€”** `ops` â†’ `render` â†’ CLI â†’ MCP â†’ framework resolvers. Left in
   Python for later/never: `visualizers/`, `lsp/pool.py` (never constructed â€”
   see `.coderadar.toml`), `coldstart`, `mcp/startup` + `lifecycle` glue.
5. **Equivalence bar â€”** byte-for-byte on `ops` return values and MCP JSON;
   normalized on rendered CLI text (rich markup, key order, float text).
   Floats: exact. Genuine cosmetic drift needs a `known-diffs.jsonl` line and
   the user's approval.
6. **Checkpoints â€”** approval of the ranked unit list, then one report and
   go-ahead per unit.

## Layout (mono crate)

```
rust/                     crate `coderadar` (lib + bin + optional `python` feature)
  src/lib.rs              re-exports; owns the graph handle core_indexer exposes
  src/error.rs            OpError taxonomy (12 subclasses â†’ one enum + kind)
  src/ids.rs              canonical_entity_id, friendly_entity_id, display_file
  src/ops/read.rs         search, search_symbols, node, affected, explore, traverse, query, resolve
  src/ops/notes.rs        annotations/comments/open_items/export/import
  src/render.rs           text surfaces (normalized-equivalence)
  src/cli.rs              clap surface, exit codes
  src/mcp.rs              JSON-RPC / stdio
  src/python.rs           #[pymodule] behind feature `python`
```

`core_indexer` stays the workspace member it is today and is linked as a
library; the ported functions call its Rust API directly.

## Ranked units (step 1)

Ranked by fan-in, blast radius, contract centrality, test density and I/O
surface â€” measured on the store built from this tree (10 353 call edges,
235 py_agent functions with caller_count â‰¥ 3).

1. **Read/query vertical** â€” `search`, `search_symbols`, `node`, `affected`,
   `explore`, `traverse`, `query`, `resolve`, `module_children`, with the
   foundation they all sit on (`OpError` taxonomy, id canonicalisation,
   `_entity_or_raise`, `_attach_notes`). Fan-in: `search` 14, `traverse` 13,
   `resolve` 13, `node` 8+, `affected` 5; ~600 LOC; read-only; tests
   `tests/test_mcp.py`, `tests/precision/`, `test_call_edge_accounting.py`,
   `test_framework_routes.py`, `test_entity_id_format.py`. **Recommended
   first:** it freezes the Rust result types that `render`, the CLI and MCP
   JSON all key off, and it has no writes to reason about.
2. **Annotations vertical** â€” `annotate`, `comment`, `revise_annotation`,
   `retire_annotation`, `annotations_on`, `comments_on`, annotation/comment
   history, `open_items`, `export_annotations`, `import_annotations`. Fan-in
   21/18/11; real domain rules (grammar, invariants, scope resolution);
   dense tests (`test_annotations.py`, `test_p6b_open_items.py`).
3. **Render vertical** â€” `render.py` (1666 LOC), pure formatting over the
   dicts from 1â€“2; golden-text equivalence; must follow the type freeze.
4. **CLI surface** â€” `cli.py` (1954 LOC, click), text-equivalence bar, exit
   codes; 51 callers of `_enter_project`.
5. **MCP surface** â€” `mcp/server.py` (2028 LOC, `create_server` 1142), JSON
   equality; needs 1â€“2 plus a protocol crate decision.

Deliberately deferred: the temporal/git family (`ingest_history` 113 LOC /
71 refs, `changes_between` 210, `as_of`, `verify_history`, `sync_git`,
`migrate_to_commit_time`) â€” densest tests, heaviest I/O, and mostly glue
over git ingest that is already Rust; `compute_embeddings` (model runtime);
`lsp/pool.py` (never constructed); `visualizers/` (optional extras).

## State (after units 1–6)

| unit | module | state |
|---|---|---|
| 0 | `rust/PORT.md` | decisions recorded; annotation on `cli.py::main` |
| 1 | `rust/coderadar/src/port.rs` | **gate passed** — 1204/1204 recorded cases, fmt/clippy/tests clean |
| 2 | `rust/coderadar/src/annotations_ops.rs` | implemented directly on `core_indexer::annotations`; CLI + MCP wired; not rust-anything-gated (see note) |
| 3 | `rust/coderadar/src/render.rs` | text renderers for the live CLI/MCP surfaces; normalized bar |
| 4 | `rust/coderadar/src/cli.rs`, `src/main.rs` | Rust binary `coderadar-rs`, read + note commands wired |
| 5 | `rust/coderadar/src/mcp.rs` | stdio JSON-RPC `initialize`/`tools/list`/`tools/call` over the same library |
| — | `core_indexer/src/api.rs` | new pure-Rust façade (load, entity JSON, search, callers/callees, stats, query, traverse, analyze) |
| 6 | `rust/coderadar/src/port.rs` | **gate passed** — 34 exports, 2283/2283 recorded cases; analyses + blob/archive wired to the engine, CLI/MCP tools exposed |
| 7 | `rust/coderadar/src/port.rs` | **gate passed** — 39 exports, 2773/2773 recorded cases; full recorded-axis folds (as-of/changes/verify) wired and parity-checked |

### Unit 1 proof

- `rust-anything record rust/coderadar --count 20 --from-tests -- pytest`: 1204 cases (301 ok, 903 error paths), from the project's own pytest suite.
- `rust-anything diff`: **1204/1204**.
- `rust-anything gate`: **passed** (fmt, clippy -D warnings, cargo test, no unsafe, no `unimplemented!`, error cases covered, API complete).
- `rust-anything bench`: **unavailable** — the 3.3.0 bench driver imports `coderadar.ops` from the generated wrapper, which only contains the ported top-level functions. No verdict was inferred.

### Why units 2–5 are not rust-anything-gated

`rust-anything bridge` on `ops.py` does not expose the note-writing functions
(`annotate`, `comment`, `revise_annotation`, …): they were absent from the
generated export list even though they are public in `ops.py`. They are
implemented as direct Rust calls into `core_indexer::annotations` with their
own error mapping, and exercised through the Rust CLI and MCP server instead.
Unit 1's generated crate was kept as the single mono crate; `rust/PORT.md`
stays the canonical decision record.

### Link spike — resolved

`core_indexer`'s rlib links into both the cdylib and the new binary on this
toolchain. Wrappers pin pyo3 to core_indexer's 0.24 (Cargo forbids two crates
with `links = "python"`); `rust/after_bridge.py` re-applies the generated-file
patches needed after any re-bridge.


### Unit 6 proof (engine-backed wrappers, analyses and edits)

- Scaffold: `rust-anything bridge` of `py_agent/src/coderadar/ops.py` restricted
  to 15 exports; recorded with `--count 20 --pin-clock --from-tests -- python -m pytest`
  (104 calls from the suite, 1095 generated cases). `diagnose` was dropped from the
  gated export set: its only `raise` is an import failure that cannot occur when
  the engine is statically linked, and no honest recorded case can reach it.
- `rust-anything diff`: **2283/2283** (1204 unit-1 + 1079 unit-6 cases).
- `rust-anything gate`: **passed** — fmt, clippy `-D warnings`, cargo tests, no
  unsafe, all 11 error-path functions covered, all 34 exports present.
- `rust-anything bench`: the 3.3.0 bench driver still fails on
  `ModuleNotFoundError: coderadar.ops`. Manual `timeit` on `render_entity_code`
  (20k calls): Python 0.0161 s, PyO3 wrapper 0.0206 s (~1.28x slower across the
  boundary; the pure-Rust binary path has no Python hop).
- Loaded-graph analysis and mutation results are not yet wired to the engine's
  planners; recorded cases only exercise the no-graph and pure-render paths.


### Unit 6b proof (front-door wiring)

- `core_indexer/src/api.rs` gained pure-Rust `dead_code`, `smells`, `clones`,
  `scaffolding`, `blob_get` and `archive`; no Python round-trip.
- `rust/coderadar/src/cli.rs` gained `dead-code`, `smells`, `clones`,
  `scaffolding`, `blob-get`, `archive`, `stale-files` and `read-source`.
- `rust/coderadar/src/mcp.rs` gained the matching eight `coderadar_*` tools
  (19 tools total).
- `rust-anything diff`: **2283/2283** unchanged; `gate`: **passed**;
  `cargo build -p coderadar --bin coderadar-rs`: passed.
- Still open: loaded-graph `update_file` and the four mutation planners
  (`replace_body`, `update_signature`, `rename`, `create_entity`) remain
  no-graph error paths in the Rust port; their CLI/MCP commands are deferred.


### Unit 7 proof (temporal reads, gated surface)

- Bridge restricted to 5 exports: `branches`, `as_of`, `verify_history`,
  `history_of`, `changes_between`.
- `record --count 20 --pin-clock --from-tests -- python -m pytest`: 490 cases,
  112 calls from the suite; all are error paths in the no-graph recording
  environment (NoIndex plus the two argument-validation errors).
- `backfill_commit_index`, `ingest_status` and `backfill_commit_messages` were
  left out of the gated set: their recorded outputs carry wall-clock horizons,
  temp paths and elapsed times that cannot be reproduced honestly.
- `rust-anything diff`: **2773/2773**; `gate`: **passed** (16 error-path
  functions covered, 39 exports present).
- Loaded-graph temporal folds, the three ingest/status functions and the
  temporal CLI/MCP commands are still open (unit 7b).


### Unit 7b proof (temporal front door)

- `core_indexer/src/api.rs` gained `normalize_timestamp`, `branches`,
  `history_of` (including rename predecessors) and `ingest_status` as pure
  JSON reads over the Macrame store.
- CLI gained `as-of`, `verify-history`, `history-of`, `changes-between`,
  `branches` and `ingest-status`; MCP gained the matching six tools
  (25 tools total).
- `rust-anything diff`: **2773/2773** unchanged; `gate`: **passed**.
- Smoke-tested on this repo's store: `branches` reports `main`;
  `history-of py_agent/src/coderadar/ops.py` returns its content spine;
  `ingest-status` reports `{"format": null, "lineages": []}`;
  `as-of` with no symbols returns the guidance shape.
- Honest gap: `as_of` with names, `verify_history` and `changes_between` still
  return `EngineError` rather than a fabricated empty fold. Wiring the
  `lookup_entities_at` / `content_hashes_at` / `source_refs_at` primitives
  into `api.rs` is the remaining temporal work.


### Unit 7c proof (full temporal folds)

- `core_indexer/src/api.rs` gained `lookup_at`, `content_hashes_at`,
  `source_refs_at`, `recorded_generations`, both `visible_*_concept_ids`
  folds and a `canonical_id` wrapper — all pure JSON, no Python round-trip.
- `rust/coderadar/src/port.rs` now implements `as_of`, `changes_between` and
  `verify_history` over those primitives, including banker's rounding for
  sampled generations and `byte_coverage_pct`, digest verdicts (SHA-256)
  and the scope/normalization rules.
- `rust/coderadar/Cargo.toml` adds `sha2`.
- `rust-anything diff`: **2773/2773** unchanged; `gate`: **passed**.
- Live parity against the Python reference on this repo's store (same
  timestamps, JSON compared with sorted keys): `as_of` **equal**,
  `changes_between` **equal**, `verify_history` **equal**
  (`totals = {ok: 390, no_digest: 2}`, `byte_coverage_pct = 99.49`).
- Remaining temporal work: the writing/ingest side (`ingest_history`,
  `sync_git`, `migrate_to_commit_time`, `backfill_*`) and the
  nondeterministic status outputs excluded from recording.

### Unit 8 proof (write-side temporal / ingest)

- `core_indexer/src/api.rs` gained pure-Rust `ingest_history`,
  `reingest_merged_commits`, `backfill_commit_index` and
  `backfill_commit_messages`; `port.rs` exposes them with the same
  argument order as `ops.py`.
- CLI and MCP gained `ingest-history`, `reingest-merged-commits`,
  `backfill-commit-index` and `backfill-commit-messages` (plus the four
  matching `coderadar_*` tools).
- The engine's INV-12 guard is preserved: ingesting into the current legacy
  `index-now` store is refused with the exact `InvalidRequest` message
  ("one store, one stamping mode") rather than silently corrupting it.
- `rust-anything diff`: **2773/2773** unchanged; `gate`: **passed**
  after `#[expect(clippy::too_many_arguments, reason = ...)]` on the
  engine wrapper.
- Honest gap: `sync_git` and `migrate_to_commit_time` are not wired; the
  wall-clock/temp-path bearing status outputs remain excluded from recording.


### Unit 9 proof (mutation planners and single-file sync)

- `core_indexer/src/api.rs` gained `canonical_entity_id`, the four
  planner wrappers (`plan_replace_body`, `plan_update_signature`,
  `plan_rename`, `plan_create_entity`), `apply_plan` and `update_file`.
  Plans live in a process-local `OnceLock<RwLock<HashMap>>`, so a dry run
  can be applied later without re-planning.
- `port.rs` replaces all five no-graph placeholders with real engine calls,
  including parse-quality/entity/sync fields from `update_file` and the
  note semantics of `create_entity`. `load_projection` now installs the
  `GLOBAL_GRAPH`, which the single-file sync path needs.
- CLI gained `replace-body`, `update-signature`, `rename`, `create-entity`
  and `update-file` (mutations dry-run by default; `--apply` commits);
  MCP gained the five matching tools.
- Loaded-graph smoke: `replace-body` returns a plan with affected files and
  unverified sites; `update-file py_agent/src/coderadar/ops.py` reports
  142 entities added. `diff`: **2773/2773**; `gate`: **passed**.


### Unit 10 proof (framework resolvers)

- Bridging the resolver package keeps the framework classes in Python
  (callable parameters / class properties), so only four shared helpers
  were gated: `extract_all_exports`, `canonical_route_pattern`,
  `route_patterns_match` and `prefer_in_dir`.
- `extract_all_exports` uses a Python-source masker that blanks comments
  and string literals before matching `__all__` assignments/`extend`/`append`,
  so docstring examples are not mistaken for real exports.
- `record --from-tests`: 392 cases; `diff`: **3165/3165**; `gate`: **passed**
  (43 exports). The class-based `detect`/`extract` resolvers remain a
  documented gap rather than being faked by regexes.


### Unit 11 proof (renderer remainder)

- Bridged all 50 `render.py` entry points, kept the 19 names already gated,
  and added 31 exact pure renderers in `src/render_remaining.rs`:
  miss banners, staleness, output budgeting, annotation reports, ingest
  reports, mutation plans/results, index lifecycle, status and the
  embedding-search banners.
- `record --from-tests`: 1,806 cases; after dropping duplicate function
  names, 1,029 new cases were merged. `diff`: **4194/4194**; `gate`:
  **passed** (74 exports).
- PyO3-facing `$object`/`attrs` records are unwrapped only for the Mutation
  dataclasses; unrelated objects keep Python's attribute-error semantics.
- CLI continues to use the normalized `render.rs`; MCP JSON is unchanged.


### Unit 12 proof (embeddings / semantic search)

- `core_indexer/src/api.rs` gained `search_similar_vec`, `search_hybrid_vec`,
  `set_embeddings`, `clear_embeddings`, `persist_embeddings` and
  `embedding_targets` — all pure Rust, no Python round-trip.
- `port.rs` adds `compute_embeddings_op`, `search_similar_op` and
  `search_hybrid_op`. Natural-language queries use a deterministic local
  hashing embedder so the binary works offline; ready-made numeric vectors
  use the engine's exact cosine/RRF ranking against persisted model tables.
- CLI gained `compute-embeddings`, `search-similar` and `search-hybrid`;
  MCP gained the three matching tools.
- Live smoke on this repo: 8,914 vectors generated/persisted, then both
  semantic and hybrid search returned `canonical_id` as the top hit with
  real `similarity` / `score` and rank fields.
- `diff`: **4194/4194** unchanged; `gate`: **passed**.
- Honest gap: the deterministic embedder is not fastembed; byte-exact
  parity holds for vector-input searches against the engine, not for
  natural-language embedding values.

## Remaining

- Framework `detect`/`extract` classes (17 resolvers) still run on the
  Python reference; only their four shared helpers are ported.
- `sync_git`, `migrate_to_commit_time` and the nondeterministic
  status/backfill renderings are not wired into the Rust front door.
- MCP parity: 37 Rust tools are exposed; the remaining Python-only tools
  are primarily framework/experimental surfaces.
- Cutover: `coderadar-rs` runs beside the Python CLI; no cutover script yet.
