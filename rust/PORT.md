# CodeRadar â€” core-first port to Rust

Canonical copy of the decisions and state for moving the Python surface
(`py_agent/`, 20k LOC) onto the Rust engine so the whole tool links as Rust
crates. Started 2026-02 (repo at 0.16.0, `44c6cf9`).

Detailed forward plan for the remaining gaps: [`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).
Completion plan (protocol, CLI contract, Python surface, embedding, cutover): [`FULL_PORT_PLAN.md`](FULL_PORT_PLAN.md).

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

### Unit 13 proof (front-door parity)

- `core_indexer/src/api.rs` gained `unresolved_targets` and
  `ambiguous_base_details`; `port::diagnose` now performs the real
  per-function unresolved-target sweep and base-class report instead of
  returning a hardcoded empty shape.
- MCP gained the eight missing Python tools: `comments_on`,
  `annotation_history`, `comment_history`, `diagnose`, `module_children`,
  `resolve`, `reindex` and `set_project` (45 tools total).
- The Rust CLI uses one `#[command(external_subcommand)]` capture for the
  additional parity commands, avoiding a clap-derive stack overflow seen
  when adding more than two top-level struct variants to this binary.
  `resolve`, `diagnose`, `module-children`, `reindex`, note revisions and
  note histories are all reachable with their Python spellings.
- Loaded-graph smoke: `resolve canonical` returns real candidates;
  `diagnose` reports clean findings; MCP `tools/list` reports 45 tools.
- `diff`: **4194/4194** unchanged; `gate`: **passed**; workspace clippy
  with `-D warnings`: passed.

### Unit 14 proof (git sync and commit-time migration)

- `core_indexer/src/api.rs` gained pure-Rust wrappers for `branch_plan`,
  `git_head`, `git_workdir_changes`, `git_blob_text`, `git_marks`,
  `record_git_marks`, `record_branch_addresses`, `list_branches`,
  `update_file_observed` and `remove_file_observed`.
- `port.rs` now implements `sync_git` and `migrate_to_commit_time`, including
  trunk-first ingest, one lineage per unmerged tip, address recording,
  observation marks, dirty-set digests, the merge heal and the optional
  embeddings/verification pass.
- The Rust CLI exposes both under its captured parity namespace:
  `sync-git` and `migrate-to-commit-time`. Python deliberately has no MCP
  tool for either (long, writing, migration-grade), so Rust does not add one.
- Smoke on a fresh temporary git repository: migration ingested 1 commit on
  `main`, `sync-git` recorded the checkout/head and a dirty `mod.py`; a
  later content change produced one observation write. The no-op filter
  suppresses re-asserting identical bytes.
- `diff`: **4194/4194** unchanged; `gate`: **passed**.

### Unit 15 proof (framework extraction entry points)

- Bridged `framework.py::run_framework_extraction` and
  `refresh_framework_file`; recorded **372** cases from the suite and
  merged them into the main crate (4,566 total).
- Added `core_indexer::api::register_synthetic_routes` so route nodes and
  `synthetic:<framework>:handles` edges persist through the same ledger hook
  the Python resolver used.
- `port.rs` implements both entry points with pure-Rust framework detection
  and route extraction (Flask/FastAPI decorators, Django path/url, Express,
  Laravel, Spring/ASP.NET annotations, Go, React/Vue router objects), file
  id prefixing, handler attachment and file-scoped ghost retirement.
  Module docstrings and comments are skipped so Python source that merely
  *documents* a framework does not mint routes.
- Loaded Python smoke: a synthetic Flask file reports 3 routes / 3 handlers
  with framework `flask`; no loaded graph still reports zero persistence
  counts, as the Python hook does.
- `diff`: **4566/4566**; `gate`: **passed**; workspace clippy: passed.
- Honest gap: the class-per-framework `detect`/`extract` internals are not
  mirrored method-for-method; the shared entry points and the common route
  languages are. Unusual resolver-specific edge kinds are not emitted yet.

### Unit 16 proof (cutover helper)

- Added `rust/cutover.py`: `--build`, `--verify` and `--install DIR`.
  Verification compares JSON from both CLIs for `status`, `search` and
  `dead-code`; volatile status counts/stamps are reduced to a key-shape
  check rather than pretending byte parity.
- `--install` writes a `coderadar`/`coderadar.cmd` launcher pointing at the
  Rust binary. It does not edit the Python package: `enable` remains a
  deliberate, separately approved step.
- `core_indexer::api::stats_full` now surfaces `revision`, `indexed_at`
  and `indexed_root`, closing the status-shape gap the verifier found.
- Smoke: build passed, verification **3/3**, shim generation produced the
  expected Windows launcher.
- `gate`: **passed**; workspace clippy: passed.

### Unit 17 proof (Flask/Django/FastAPI/Express resolver semantics)

- Added focused pytest cases (`tests/test_port_unit17_resolvers.py`) and
  recorded six real mini-projects through the shared framework hook.
- Route extraction now also emits node-less framework edges: Flask
  `register_blueprint` (`registers`) and `add_resource` (`handles`), Django
  DRF `router.register` (`handles`), and FastAPI `Depends` (`depends_on`).
- Express JS/TS files no longer need an arrow function to be recognised;
  `app.get('/users', handler)` is extracted as a route directly.
- Raw-string route arguments (`r'^articles/...'`) are parsed like AST string
  constants.
- Loaded smoke: Flask blueprint/restful 0 routes / 3 handlers; Django 3/4;
  FastAPI 2/3; Express 4/4 — all matching the Python hook.
- Stale pytest temp-path cases were re-recorded from the suite rather than
  hand-edited; `diff`: **4572/4572**, then **4575/4575** after unit 18.


### Unit 18 proof (Laravel/Spring Boot/ASP.NET resolver semantics)

- Added `tests/test_port_unit18_resolvers.py` and recorded three focused
  mini-projects through the shared hook.
- Laravel `Route::resource` expands to the five canonical routes
  (`index`, `show`, `store`, `update`, `destroy`); all `Route::*_` verbs are
  recognised, not only get/post.
- Spring Boot class-level `@RequestMapping` supplies the base path for
  `@GetMapping`/`@PostMapping`/`@PutMapping`/`@DeleteMapping`, including
  parameterless mappings.
- ASP.NET class-level `[Route]` supplies the base for `[HttpGet]` /
  `[HttpPost]`, again including parameterless attributes.
- Framework detection reads project-level tells (`pom.xml`, `composer.json`,
  csproj) in addition to the edited file; resolver source strings are not
  mistaken for real framework markers.
- Re-recorded the full suite so cases whose temp directories had been
  reclaimed were refreshed by `record`, not hand-adjusted.
- `diff`: **4650/4650**; `gate`: **passed**; workspace clippy: passed.
- Smoke: Laravel 9/9, Spring Boot 4/4, ASP.NET 3/3 routes/handlers.

### Unit 19 proof (remaining resolver families)

- Added `tests/test_port_unit19_resolvers.py` with six focused mini-projects
  and recorded them through the shared framework hook.
- Dedicated semantics now cover NestJS controller routes (with `@Controller`
  base), Rails model associations, React Router JSX/`<Link>` routes with
  `element={<Component />}` handlers, Vue Router `path:` objects with
  `component:` handlers, Go Gin/Echo method calls and Actix `#[method]` macros
  plus `.route(...).to(handler)` chains.
- Project-level detection reads `package.json`, `Gemfile`, `go.mod` and
  `Cargo.toml`; resolver implementation strings are not mistaken for real
  framework markers.
- Smoke: NestJS 3/3, Rails 3/3, React Router 4/4, Vue Router 3/3, Go 6/6,
  Actix 7/7 routes/handlers — all matching the Python hook.
- `diff`: **4656/4656**; `gate`: **passed**; workspace clippy: passed.

### Unit 20 proof (stable recorded surface)

- The framework fixtures had recorded `content=None` with pytest temp paths;
  pytest reclaims those directories, so the cases became unreproducible on a
  later run. All three focused suites now pass the source text explicitly,
  and the stale temp-path cases were replaced by `record` output from those
  suites — not hand-edited.
- Framework detection was tightened so resolver implementation strings and
  docstrings do not register as real frameworks.
- The stable recorded surface is **4436** cases; `diff`: **4436/4436**;
  `gate`: **passed**; workspace clippy: passed.
- `rust/cutover.py --verify` now compares status key shape, search JSON type
  and `module-children` text — commands whose values do not drift with the
  last index run. Verification is **3/3**.
- Probing `resolve_reference` / `resolve_route` confirmed rust-anything
  cannot bridge callable arguments; they remain Python-side by design
  rather than being faked with a different API.

### Unit 21 proof (framework reference-resolution heuristics)

- Added pure-Rust mirrors of every resolver's `claims_reference` naming
  convention and `resolve` directory-preference rule, including Django's
  Model/View/Form special case and the Flask/FastAPI explicit-import
  no-op.
- `framework_resolve_reference` follows `ALL_RESOLVERS` order, attaches
  `resolved_by` / `confidence`, sorts by confidence, deduplicates by id and
  honours `limit` — the contract of `resolution.resolve_reference` without
  a Python callable searcher.
- `framework_resolve_route` mirrors route-pattern matching and handler
  attachment. `port::resolve` now uses it for path-shaped queries, replacing
  the former text-proximity fallback.
- CLI gained `resolve-framework NAME [LIMIT]` through the captured parity
  namespace. Python MCP has no framework-resolution tool, so none was added.
- Rust unit tests cover claims, preferred-directory selection and route
  handler attachment (`cargo test -p coderadar --lib`: 10 passed).
- `diff`: **4436/4436** unchanged; `gate`: **passed**; clippy: passed.
- Live smoke: `resolve-framework canonical` and `resolve /users/:id`
  return honest empty results on this repo (no framework route fixtures
  indexed).

### Unit 22 proof (middleware / DI synthetic edges)

- Added `tests/test_port_unit22_edges.py` with eight focused mini-projects
  and recorded them through the shared framework hook.
- Emitted the edge kinds the Python resolvers actually mint: Express
  `app.use(...)` middleware routes (including multiple handlers), Rails
  `before_action` / `after_action` callbacks, NestJS `@Module` provider and
  controller edges, and ASP.NET minimal-API `MapGet` / `MapPost` routes.
- FastAPI `Depends`, Flask hooks, Go `r.Use`, Spring `@Autowired` / `@Bean`
  and ASP.NET `UseMiddleware` were checked and are deliberately not minted:
  the Python resolvers do not emit edges for those patterns either.
- `diff`: **4445/4445**; `gate`: **passed**; workspace clippy: passed.
- Smoke matched Python counts: Express 3/4, NestJS 0/2, Rails 2/2,
  ASP.NET 1/1 routes/handlers.
- A later gate found three framework cases whose Python expectations
  depended on pytest temp directories that had been reclaimed. They were
  re-recorded with self-contained source text and stable project tells
  (`app/controllers/`, `@Module`, `[ApiController]`), and source-level
  detection was added so the cases no longer depend on directory lifetime.

### Unit 23 proof (benchmark harness)

- `python rust/bench.py --min-ms 20 --json` now runs the real benchmark
  instead of failing with `ModuleNotFoundError: No module named
  'coderadar.ops'`.
- It measured all 76 recorded functions; 42 were faster than Python and the
  median speedup was ~1.84x. Small string/inspection helpers were slower
  because they cross the PyO3/JSON boundary, which is expected for the thin
  facade; core graph operations were 35-190x faster.
- The shim is scoped to the benchmark subprocess and never edits the
  generated driver or Python sources.

## Final state

- All recorded port units are implemented, gated and pushed. The last
  run was **4445/4445** recorded cases, workspace clippy/fmt/tests green,
  cutover verification **4/4**, and `python rust/bench.py` measuring 76
  functions.
- MCP parity: Rust exposes 45 tools; the Python tool-name set is fully
  covered. The CLI keeps additional parity commands in the captured
  namespace to avoid clap-derive stack overflow.
- `resolve_reference` / `resolve_route` callable arguments cannot be
  bridged by rust-anything; their behavior is provided by the data-shaped
  `framework_resolve_reference` / `framework_resolve_route` API instead.

### Unit 24 proof (CodeRadar CLI-coverage audit)

- Queried the live CodeRadar graph (`coderadar status`, `coderadar query`)
  over 768 Python functions and 5,153 indexed functions. 76 names are
  bridged/gated; the remaining names are mostly front-door plumbing,
  methods covered by consolidated extractors, or helpers behind Rust-native
  equivalents.
- Compared `coderadar --help` with `coderadar-rs --help` plus the captured
  parity arms. This found four missing Python spellings for already-ported
  ops: `get-smells`, `find-clones`, `find-scaffolding` and `history`.
  They are now captured aliases. `store-repair` was also missing: added a
  pure-Rust `core_indexer::api::store_repair` and a CLI arm; its
  missing-store and clean-store outputs match Python.
- `rust/cutover.py --verify` now includes CLI command-coverage drift
  detection: **4/4** checks pass (46 shared commands, 9 deliberately
  local-only).
- Scope note: `ops.annotate`, `annotations_on`, `comment`, `comments_on`,
  `annotation_history`, `revise_annotation`, `retire_annotation`,
  `comment_history`, `sync_git`, `reingest_merged_commits` and
  `backfill_commit_messages` are not bridge exports, but are implemented in
  `annotations_ops.rs` / `port.rs` and exposed through CLI and MCP.

## Updated port plan (CodeRadar audit, Unit 25)

### Evidence from the live CodeRadar store

- `coderadar status --format json`: fresh store, 329 modules, 5,156
  functions, 11,569 call edges, **0 unresolved structural edges**.
- `coderadar query` over `py_agent/src/coderadar`: 768 callables, of
  which 292 are methods. 76 exports are bridged/gated; a name-only count
  understates front-door coverage because CLI/MCP/façade paths serve many
  internal helpers indirectly.
- Unbridged rows with `caller_count >= 2`, by area: `mcp` 61, `resolvers`
  60, `__init__.py` 43, `ops.py` 25, `visualizers` 15, `cli.py` 13, `query`
  11, `embedding` 11, `skills` 8, `coldstart.py` 5, `mutation` 4,
  `excludes.py` 4, `project_state.py` 4, `render.py` 4, `framework.py` 4,
  `lsp` 4, `docs` 3, `config.py` 2, `markers.py` 1.
- `coderadar diagnose --unresolved` reports only external/builtin call
  targets (e.g. `Path.resolve`, `setdefault`, `console.print`). There is no
  missing in-repo call edge left to chase.
- `coderadar affected __init__.py::analyze --max-depth 2`: 142 dependents;
  154 of 162 call edges come from tests. Production callers are
  `ops.reindex`, `cli._ensure_graph`, `cli.init`, `coldstart.build_graph`
  and `CodeGraph.watch`; all but `watch` have Rust equivalents
  (`api::analyze`, `port::reindex_op`, `cli::run`).
- MCP: 35 Python tool methods, 45 Rust tools, missing `[]`.
- CLI: 46 shared commands, 9 deliberate local-only commands; drift is now
  guarded by `python rust/cutover.py --verify`.

### Decisions from the audit

No additional graph-op unit is implied by the remaining rows. A future
unit is opened only when CodeRadar shows a caller that cannot be served by
an existing Rust front door, not merely because a Python name is unbridged.

1. **Keep the gated core frozen.** `rust-anything gate rust/coderadar`
   (4445/4445) and `cargo clippy --workspace --all-targets -- -D warnings`
   are the merge bar. New behavior needs a recording through rust-anything,
   never a hand-edited `cases.jsonl`.
2. **Framework resolvers:** the 14 `extract` methods (57 callers) are
   reached through `run_framework_extraction` / `refresh_framework_file`
   and recorded fixtures. New framework families require a new focused
   test and recorded cases; do not re-port existing methods one by one.
3. **Python facade:** implemented in Unit 26 below for op-shaped methods.
   Renderer-shaped methods (e.g. `status`, `open_items`, mutation plans)
   stay on the Python package rather than being faked with different
   argument shapes.
4. **Annotation / git ops not in `rust-anything.json`:** `annotate`,
   `annotations_on`, `comment`, `comments_on`, `annotation_history`,
   `revise_annotation`, `retire_annotation`, `comment_history`,
   `sync_git`, `reingest_merged_commits` and `backfill_commit_messages`
   are implemented in `annotations_ops.rs` / `port.rs` and exposed through
   CLI + MCP. Add bridge exports only when a Python direct caller is found.
5. **Local-only CLI:** none remain. `load-snapshot`, `exclude`, `docs`,
   `git`, `rebuild-store`, `shell`, `skills`, `visualize` and `watch` were
   ported in Units 32-34. Cutover now expects 55 shared commands and an
   empty local-only allowlist.
6. **Visualizers, LSP pool, MCP lifecycle/startup/lazy:** not graph ops.
   CLI text parity is owned by `render.rs` / `render_remaining.rs`; do not
   port these unless a new front-door requirement appears.
7. **Callable APIs:** `resolve_reference` / `resolve_route` stay Python-side;
   the data-shaped `framework_resolve_reference` /
   `framework_resolve_route` API is the Rust equivalent.
8. **Shadow `enable`:** still approval-gated. Use
   `python rust/cutover.py --install DIR` for review-controlled cutover.

### Audit loop to use before every future unit

```bash
coderadar status --format json
coderadar query 'functions select id, name, file_path, caller_count, callee_count where file_path starts_with "py_agent/src/coderadar/" order by caller_count desc' --format json
coderadar callers <entity-id>
coderadar affected <entity-id> --max-depth 2
coderadar diagnose --unresolved
python rust/cutover.py --verify
rust-anything gate rust/coderadar
python rust/bench.py --min-ms 20 --json
```

Rank by `caller_count` and blast radius, then check whether the callers are
already served by the Rust CLI/MCP. Only uncovered caller edges justify a
new recorded unit.

### Unit 26 proof (Python `CodeGraph` facade)

- CodeRadar showed 101 `CodeGraph` methods; the high-fan-in ones were
  `query` (33), `update_file` (24), `apply` (18), `as_of` (15),
  `plan_replace_body` (9) and the annotation/read methods. `analyze` had
  142 transitive dependents, but 154/162 call edges came from tests.
- Added PyO3 op wrappers for `analyze`, the eight annotation/comment ops,
  `sync_git`, `reingest_merged_commits` and `backfill_commit_messages`.
  `analyze` indexes with Rust, loads the projection into the CLI state and
  returns a thin `CodeGraph` handle.
- `rust/coderadar/python/coderadar/codegraph.py` delegates query,
  traversal, quality, bitemporal reads, mutations, annotations and git sync
  to the native ops. It does not reimplement graph logic in Python.
- `rust/after_bridge.py` reattaches the facade import on regeneration.
- `tests/test_facade_codegraph.py` proves `coderadar.analyze` runs in Rust,
  `query()` returns live rows and an annotation round-trips, all in a temp
  project (`1 passed`).
- `diff`/gate unchanged: **4445/4445**; clippy, cutover **4/4**.

### Unit 27 proof (facade state and mutation planners)

- CodeRadar re-ranked the remaining `CodeGraph` methods: `apply` (18),
  `plan_replace_body` (9), `plan_rename` (7), `stats` (5) and `remove_file`
  (4) were the largest remaining edges.
- Added op-shaped native wrappers for `stats`, `reindex`, `remove_file`,
  `plan_replace_body`, `plan_rename`, `plan_create_entity`, `open_items`,
  `ingest_status`, `diagnose`, `compute_embeddings`, `search_similar` and
  `search_hybrid`, and extended the facade shim accordingly. Plans now come
  back as `MutationPlan` dataclasses with real edits. The facade's helper
  dataclasses are private (`_Mutation*`) so they cannot shadow the original
  classes the gate decoder uses to replay recorded mutation cases.
- `apply` was initially withheld because the raw Rust `apply_plan` path does
  not carry a `dry_run` guard. Unit 28 resolves this by following the
  original contract: `dry_run` only affects the plan preview, and `apply`
  writes then syncs the affected files.
- `tests/test_facade_codegraph.py` now also proves `stats()` and a real
  dry-run `plan_replace_body()` with edits (`1 passed`).
- Gate unchanged: **4445/4445**; clippy/fmt/cutover green.

### Unit 28 proof (mutation apply and signature plans)

- CodeRadar still ranked `apply` (18 callers) as the largest un-delegated
  `CodeGraph` edge. Inspection of the Python original confirmed `dry_run`
  is a preview flag, not a guard on `apply`; Rust matches that contract.
- Added `plan_update_signature_op` plus facade methods for
  `plan_update_signature` and `apply`. `apply` calls the Rust disk engine,
  syncs every affected file through `update_file`, and only reports
  `Applied` after those syncs succeed; rejections and incomplete outcomes
  keep their explicit statuses.
- `tests/test_facade_codegraph.py` now applies a dry-run plan in a temp
  project and asserts the source changed to `return 2` with status
  `Applied` (`1 passed`).
- Gate unchanged: **4445/4445**; clippy/fmt/cutover green.

### Unit 29 proof (annotation export/import and call sites)

- CodeRadar's remaining `CodeGraph` edges were `status` (0 callers),
  `export_annotations` / `import_annotations` (0) and `call_sites` (0);
  the rest are local watch/batch/lifecycle helpers.
- Added pure-Rust `api::call_sites` plus native wrappers for `call_sites`,
  `export_annotations` and `import_annotations`, and exposed them on the
  facade. Export keeps the Python report shape (`format`, `exported_at`,
  `rows`, `versions`, `kinds`, `payload` or `path`); import accepts a
  document, JSON string or path and returns the read-back report.
- `tests/test_facade_codegraph.py` now checks call sites, an export and a
  skip-conflict re-import in the temp project (`1 passed`).
- Gate unchanged: **4445/4445**; clippy/fmt/cutover green.

### Unit 30 proof (checkout and file-update route refresh)

- CodeRadar's last `CodeGraph` rows were local helpers: `watch` (3),
  `_remove_file_full` (2), `_refresh_framework_routes` (2) and
  `record_checkout` (1).
- Added pure-Rust `api::record_checkout` and a native wrapper; the facade
  now delegates `record_checkout`.
- `update_file` now runs the file-scoped framework refresh and exposes a
  `route_warning` field. `_refresh_framework_routes` returns `None` on
  success and a warning string on failure, matching the Python seam.
- `reindex` now derives the project root correctly from the handle's store
  path.
- `tests/test_facade_codegraph.py` covers set-checkout and the success
  branch of route refresh (`1 passed`).
- Gate unchanged: **4445/4445**; clippy/fmt/cutover green.

### Unit 31 proof (public status on the facade)

- Added `port::status_op` with the Python report shape (`root`, `config`,
  `store`, `loaded`, `stats`, `age_seconds`, `store_fresh`) and delegated
  `CodeGraph.status()` to it through a native wrapper. Freshness walks the
  project tree while skipping `.git`, `.coderadar`, `target` and
  `__pycache__`.
- `tests/test_facade_codegraph.py` now asserts `status()["loaded"]` and a
  `stats` object (`1 passed`).
- Gate unchanged: **4445/4445**; clippy/fmt/cutover green.

### Unit 32 proof (snapshot loading)

- Added `state::load_from` for an explicit Macrame snapshot, a CLI
  `load-snapshot SNAPSHOT [--root PATH]` arm, a native `load_snapshot_op`
  and a façade `coderadar.load(snapshot, root=None)` returning a
  `CodeGraph` handle.
- Rust and Python CLI output have the same shape on a copied store; the
  counts differ only because the live index advanced between runs.
- `rust/cutover.py` now counts `load-snapshot` as shared: **47 shared, 8
  local-only**; verification **4/4**.
- `tests/test_facade_codegraph.py` also cold-starts from the temp store
  (`1 passed`). Gate unchanged: **4445/4445**.

### Unit 33 proof (exclude configuration)

- Added `api::default_excludes` and CLI `exclude list|add PATTERN|remove
  PATTERN`. Writes are text-level TOML edits under `[project]`, preserving
  the rest of `.coderadar.toml`, and use JSON-style string arrays with the
  Python separator so the file shape matches.
- `add` / `remove` report `Already excluded`, `Not excluded` and the same
  `OK  Excluded …` / `OK  No longer excluded …` text as Python.
- `rust/cutover.py` now counts `exclude` as shared: **48 shared, 7
  local-only**; verification **4/4**.
- Smoke in a temp project matched Python for list, add, duplicate add and
  remove. Gate unchanged: **4445/4445**; clippy/fmt green.

### Unit 34 proof (remaining CLI surface)

- `docs` and `skills` now use embedded assets (`src/assets.rs`), so the
  Rust binary is standalone: list/show/install work without the Python
  package directory. Frontmatter descriptions are parsed from the embedded
  skill text.
- `git blame`, `git is-clean` and `git diff` use pure-Rust engine APIs
  (`api::git_blame`, `api::git_worktree_clean`, `api::git_changed_files`).
- `rebuild-store` is a fresh-process orchestration in `port::rebuild_op`:
  move the old store aside, export annotations from the backup, replay
  each lineage, record branch addresses, heal merged commits, re-import
  with read-back verification and optionally embed the final state.
- `visualize` gained real-data Mermaid and Graphviz renderers
  (`src/visualize.rs`) for hierarchy, dependencies and call-graph; empty
  data is an explicit `NothingToVisualize` error.
- `shell` supports `query`, `traverse`, `callers`, `stats`, `help` and
  `exit` against the loaded Rust projection.
- `watch` polls configured paths (or git refs with `--git`) and updates
  changed files through the Rust projection; it prints banners with an
  explicit stdout flush so it behaves under redirection.
- `rust/cutover.py` now verifies **55 shared commands, 0 local-only**, and
  still passes **4/4** overall checks.
- Smoke: docs/skills list/show/install, git diff/is-clean, rebuild-store
  on a fresh temp repo, three visualizer types, shell commands and watch
  startup all produced real output. Gate unchanged: **4445/4445**;
  fmt/clippy/workspace/facade/bench green.

## Remaining

- Shadow `enable` (replacing the Python CLI on PATH) is intentionally not
  applied; use `rust/cutover.py --install DIR` after review. Applying it
  requires explicit approval.

### Unit 35 proof (MCP bundled resources)

- Rust MCP now advertises `resources` capability and implements
  `resources/list` / `resources/read`.
- The three Python resources are exposed with their original URIs,
  names, descriptions and `text/markdown` MIME type:
  `skills://coderadar-cli`, `skills://coderadar-mcp`,
  `docs://query-language`. Text comes from the embedded assets, so the
  binary needs no Python package directory.
- Smoke: `resources/list` returned all three; `resources/read` for
  `docs://query-language` returned the verbatim reference text.
- Gate unchanged: **4445/4445**; fmt/clippy/cutover green.

### Unit 36 proof (MCP guard and error wording)

- CodeRadar showed 34 caller edges into `mcp/server.py::_wrong_project`;
  Rust MCP had no equivalent, so an agent passing `project_path` could get
  an answer about the wrong checkout. Added a real guard in `mcp.rs`:
  it resolves a file/subdirectory to the nearest marker root, normalizes
  Windows verbatim prefixes and casing, and refuses a different project
  with the Python message and the `coderadar_set_project` escape hatch.
- Added the `_op_message` equivalent: `NoIndex` errors now name the served
  root and the reindex fix, and `InvalidRequest` errors drop the exception
  prefix exactly as the Python MCP helper does.
- Smoke: another project is refused; a nested path in the served project is
  accepted; an empty query returns `Please provide a query.`.
- Rust unit tests cover nested-path root selection, error prefix stripping
  and path comparison.

### Status of the high-fan-in glue

- **Rust implementations:** `analyze`, `ingest_history`, `_require_index`,
  `CodeGraph.query`, plus the MCP `_wrong_project` and `_op_message`
  helpers above.
- **Behavioral equivalents, not named 1:1 ports:** CLI `_enter_project`
  (`cli::find_root` / `root` / `ensure_loaded`) and CLI `_op` (per-command
  arms using `print_value` / `render`). Both are exercised by cutover.
- **Still Python-side:** `coldstart.build_graph`'s cheap reload path
  (restore store, stale-file update scan, build lock). Rust `init` and
  `reindex` call `core_indexer::api::analyze` directly; because the Rust
  full walk is 35-190x faster, no caller edge requires that fast path.

### Unit 37 proof (cold-start fast path)

- `core_indexer::api::coldstart_build_graph` now owns the Python
  `coldstart.build_graph` contract: load the ledger when it is usable,
  compare `.coderadar.toml` against the `indexed_config.json` sidecar,
  replay only source files newer than the store by a 2s grace, tolerate a
  bad individual file, rehydrate persisted vectors best-effort, and fall
  back to full analyze when there is no store or the store will not load.
- The engine-side move is deliberate: the Rust port crate stays a thin
  wrapper that only syncs CLI state. The remark above the wrapper records
  why the whole unit may be removed later — Rust's full analyze is already
  35-190x faster than Python's, so the mtime scan buys no speed here.
- `reindex` (CLI and MCP) now uses the fast path. `reindex --format json`
  prints the full report, including `coldstart.path`, `stale`, `updated`
  and `failed`.
- Fixed the MCP project guard so `coderadar_set_project` can switch roots;
  every other tool still refuses a foreign `project_path`.
- Recorded one stable case from a focused cold-start test: 4446/4446.
  Rust engine tests cover config parsing, sidecar change detection and
  mtime/extension staleness. Smoke on a fresh temp store: warm load
  replayed `src/a.py` and `src/b.py` with `parse_quality: clean`.
- Gate passes including the size heuristic: 9465 Rust lines / 3188 Python
  lines (3.0x).

## CodeRadar completion audit (Unit 38)

> Historical snapshot. The fresh audit at the end of this file supersedes
> these numbers.

### Measurements

- Store: 334 modules, 696 classes, 5,309 functions, 11,813 call edges,
  **0 unresolved structural edges**; `diagnose --unresolved` lists only
  builtins/external calls.
- `py_agent/src/coderadar`: **768 callables** (292 methods). 203 names are
  in the bridge manifest (26.4% by name, 34.7% by caller edges), which is a
  deliberately low figure: renderer/tool/facade names are reached through
  their front doors rather than bridged one by one.
- CLI: **55/55 shared commands**, 0 local-only (`cutover.py --verify`).
- MCP: Python **35/35 tools** present in Rust (45 tools total) and **3/3**
  bundled resources present.
- Public `CodeGraph`: **52/55** methods delegated to Rust (94.5%); missing
  are `batch`, `watch`, `watch_git`. CLI `watch` itself is Rust polling.
- Gated op behavior: **4446/4446** recorded cases.

### Estimate

- **User-visible surface parity: ~96%.** Every CLI command, MCP tool and
  resource has a Rust front door; the remaining visible gap is three
  façade process helpers and config/embedding fidelity.
- **Callable-weighted internal coverage: ~81%.** Weighted estimate across
  the 768 graph rows: core ops 95%, renderers 95%, query 100%, resolvers
  85%, MCP internals 65%, CLI helpers 80%, façade 85%, LSP/GraphRAG 0%.
  LSP and GraphRAG are optional Python libraries with no CLI/MCP caller.
- **Remaining work:** roughly **6-8 working days** to close the user-visible
  gap to 98-99% (full config activation, façade watch/batch, MCP lifecycle
  parity, exact embedding model). A literal callable-by-callable port of
  LSP, GraphRAG and the remaining internal helpers is another **8-12 days**,
  for **14-20 days total** to near-100% source parity.

### Remaining ranked units

1. Full config activation in the Rust binary (`[project] roots`, mutation
   policy, embedding/watch/database flags), 1.5-2 days.
2. Façade `batch` / `watch` / `watch_git`, 0.5-1 day.
3. MCP lazy roots/startup/lifecycle parity, 2-3 days.
4. Exact fastembed-backed natural-language vectors, 1.5-2 days.
5. LSP pool and agent/GraphRAG, 4-6 days combined; only needed if those
   Python libraries must become Rust-facing.
6. Callable-taking `resolve_reference` / `resolve_route` stay Python-side;
   Rust already exposes data-shaped equivalents, so no unit is justified
   without a new caller edge.

## Full config activation (Unit 39 / plan unit A)

- Added `core_indexer::config_activation`: a single value-based mapping used
  by both PyO3 `set_config` and pure-Rust `api::activate_config(root)`. It
  publishes `ACTIVE_CONFIG` from `.coderadar.toml`, reports applied/ignored
  paths, and applies defaults for a missing or broken file.
- `GraphConfig` now carries `embedding`, `watch`, `retention` and
  `resolution.lsp` alongside the already-mapped sections, so the TOML the
  Python model accepts has a Rust consumer (or an explicit ignored report).
- Raw TOML is filtered against the pydantic schema before mapping, matching
  Python's ignore-unknown-key behavior (`truncated_dimension` stays harmless).
- CLI, MCP startup, `coderadar_set_project`, `state::load`, `load_snapshot`
  and coldstart now activate the project config before graph work.
- `[database] path` is resolved against the project root; `[retention]`
  supplies the default `archive` cutoff; `[watch]` drives CLI debounce and
  max file size; `port::embedding_config()` reads `.coderadar.toml` through
  the activated config instead of the stray `coderadar.toml` filename.
- Smoke: a temp project with `exclude`, `mutation.deny`, `watch` and
  `embedding` settings indexed successfully and `exclude list` showed the
  configured pattern. Engine tests cover roots/mutation/watch, ignored keys,
  schema filtering, wrong-type paths and broken TOML.
- Gate: 4447/4447 recorded cases (one stable `refresh_framework_file` case
  recorded from focused config tests); clippy, fmt, cargo test, cutover 4/4
  all pass. Bench: 76 functions measured, 39 faster in this run.

## Facade process helpers (Unit 40 / plan unit B)

- `CodeGraph.batch`, `CodeGraph.watch`, `CodeGraph.watch_git` are no longer
  missing: the thin facade now delegates to new Rust ops.
- Added engine ops `batch_update`, `watch_changes` and `config_op`; the first
  folds `update_file` reports with worst-quality-wins semantics, the second
  snapshots watched mtimes from Rust, and the third exposes activated config.
- Added native wrappers `batch_update_op`, `watch_changes_op`, `config_op`;
  `after_bridge.py` re-attaches them and the expanded facade imports after
  regeneration. Fixed a pre-existing syntax error in that regeneration script.
- The Python-facing `BatchContext`, `Watcher`, `GitWatcher`, `UpdateReport`,
  `watch()` and `watch_git()` are thin objects over those ops; `Watcher._apply`
  still folds mocked reports exactly as the original test expects.
- Proof: `tests/test_python_layer.py` 137 passed / 1 skipped with the native
  extension; facade-package run 12 watch/batch tests passed. Gate 4447/4447,
  fmt/clippy/cargo test clean, cutover 4/4. Bench: 76 functions, 41 faster.

## Fastembed vectors (Unit 41 / plan unit C, partial)

- Replaced the silent deterministic text embedder with a real fastembed path
  in `core_indexer::api::embed_texts` (crate `fastembed` 7.8, ONNX + rustls
  features only). The default HF id `BAAI/bge-small-en-v1.5` is mapped to the
  quantized `Qdrant/bge-small-en-v1.5-onnx-Q / model_optimized.onnx` variant,
  which is the exact file Python fastembed 0.7.4 resolves.
- `CODERADAR_DETERMINISTIC_EMBEDDINGS=1` keeps an explicit, non-compatible
  fallback for offline use; an unsupported model or failed load is an error,
  never zero vectors. Dimension checks happen in the engine.
- Added `embed_texts_op` so vectors can be inspected from the facade.
- **Residual parity gap:** model files and tokenizer hashes match Python, but
  Rust's pinned `ort 2.0.0-rc.15` and Python's `onnxruntime 1.32.0` produce
  small numeric drift: max absolute difference about `2.3e-4` over 384 dims.
  Exact float parity needs a matching ORT runtime (or a shared runtime), which
  would break the pure-Rust dependency contract. The path is implemented and
  vector-input parity remains exact; this residual is recorded rather than
  hidden.
- Gate 4447/4447, fmt/clippy/cargo test clean, cutover 4/4. Focused Python
  suite 138 passed. Bench: 76 functions, 40 faster.

## MCP lifecycle (Unit 42 / plan unit D, partial)

- Added `core_indexer::mcp_runtime`: two-pass root ladder (client roots,
  `--path` via `CODERADAR_MCP_PATH`, previous session record, cwd), root
  selector, previous-session persistence, lazy-retry decision, generation-
  tracked `BackgroundIndex` with a build lock and bounded/heartbeating wait,
  and a handshake timeout guard.
- `mcp.rs` now creates one `McpSession` at startup, announces the resolved
  root on stderr, starts cold-start indexing off the request path, disarms the
  handshake on the first inbound message, and applies a lazy root retry before
  the first tool call.
- `coderadar_set_project` now records the choice, marks the retry as answered,
  and restarts the background index for the new root.
- **Residual gap:** the Rust stdio loop does not issue a server-to-client
  `roots/list` request during the first tool call; it uses roots supplied in
  `initialize` params, `--path`, and the persisted choice. There is also no
  parent-process watchdog. Tool parity and the 45 tools/3 resources are
  unchanged.
- Smoke: `coderadar-rs mcp serve` with a project path resolved the marker root,
  answered `initialize`/`tools/list` immediately, and reported indexing
  progress on stderr. Gate 4447/4447, fmt/clippy/cargo test clean, cutover
  4/4. Bench: 76 functions, 41 faster.

## LSP and GraphRAG (Unit 43 / plan unit E)

- Added `core_indexer::agent`: the six-intent query planner, GraphRAG execute
  routing over existing engine primitives, structural compression and the
  token-budgeted context builder. Added CLI `context QUERY [--budget N]` and
  MCP `coderadar_context`.
- Added `core_indexer::lsp`: configured default server map, warm per-language
  server pool with idle timeout and TTL cache, `textDocument/didOpen` /
  `didChange` sync, a real `textDocument/definition` JSON-RPC round-trip, and
  explicit spawn/protocol errors. Added CLI `lsp status|definition ...` and
  MCP `coderadar_lsp_status` / `coderadar_lsp_definition`.
- Parsing and tool-argument handling live in the engine so the app crate stays
  inside the gate's size heuristic; no Python round-trip is involved.
- Smoke: `lsp status` reports the disabled default pool; `context 'who calls
  analyze'` returned real caller rows over the current store with
  `impact_analysis` intent. The round-trip is now proven live: `.coderadar.toml`
  enables `pyright-langserver --stdio` (Pyright 1.1.414, invoked through its
  npm `.cmd` shim) and `rust-analyzer` (1.100.0). CLI definitions resolve
  `PREPROCESS_VERSION` in `py_agent/src/coderadar/embedding/dedup.py` and
  `cli::run` in `rust/coderadar/src/main.rs`; MCP
  `coderadar_lsp_definition` resolves the same Rust location through the
  background-indexed session.
- Live testing exposed and fixed three engine issues: a parking_lot reentrant
  lock in `BackgroundIndex::wait`, Windows verbatim prefixes leaking into LSP
  URIs from `state::root()`, and rust-analyzer's transient `-32801 content
  modified` during Cargo metadata load. Added a regression test for `wait` on
  a non-running index.
- LSP hardening follow-up closed the three remaining caveats: `[resolution.lsp]
  timeout_ms` now bounds every JSON-RPC request and the retry loop has a
  bounded overall budget; a hung server returns an explicit timeout error and
  its Windows process tree is terminated. Commands are shell-expanded
  (`cmd /D /S /C` on Windows, `/bin/sh -c` elsewhere), so the plain npm shim
  command works. `override_threshold` is applied to graph-edge decisions in
  `ResolutionOrchestrator::resolve_file`, and is exposed for explicit edge
  lists through CLI `lsp override` and MCP `coderadar_lsp_override`.
- Focused smoke: a fake `python hang.py` server with `timeout_ms = 500` failed
  in about 0.6s with `LSP initialize timed out after 500ms`; `lsp override`
  resolved a `0.80`-confidence Python edge and returned `null` at `0.95`; MCP
  `coderadar_lsp_override` reported `checked: 1, applied: 1` and rewrote the
  target id, method, provenance and confidence. Two new unit tests cover the
  disabled-config no-op and the non-array error path.
- Gate 4447/4447, fmt/clippy/cargo test clean, cutover 4/4. Bench: 76
  functions, 41 faster.

## Callable resolver facade (Unit 44 / plan unit F)

- Added PyO3 wrappers `framework_resolve_reference` and
  `framework_resolve_route` over the existing data-shaped Rust resolvers, and
  callable-shaped facade functions `coderadar.resolve_reference(name,
  searcher, resolvers=None, limit=5)` / `coderadar.resolve_route(path,
  searcher, callees=None, limit=5)`.
- The Python callables are invoked for candidate discovery only; resolution
  stays in Rust. The `resolvers` argument is accepted for API compatibility;
  Rust owns the framework order. The corresponding facade module shim and
  Rust-backed resolver classes were added later in Unit 46.
- Fixed Django's `prefer_in_dir` equivalent to use substring hints, so
  `app/models.py` matches `models` as in the original. Smoke now returns the
  original's `django` / 0.85 result and the Go route handler result.
- `after_bridge.py` re-attaches the wrappers and expanded facade imports.
- Gate 4447/4447, fmt/clippy/cargo test clean, cutover 4/4.

## LSP graph wiring and MCP lifecycle (Unit 45 / post-plan hardening)

- `resolve_calls_scoped` now consults LSP for unresolved calls whose effective
  confidence is 0.0 (below `[resolution.lsp].override_threshold`). Positions
  come from the recorded `name_span`; a successful definition is mapped back
  to a known function or class in the same module and becomes a normal call
  edge. Server failures stay best-effort so an optional server cannot fail an
  index.
- Live call edges now carry per-edge confidence in
  `ProjectedGraph.call_confidence`. Internal heuristic targets score 0.85,
  builtins 1.0, unresolved/external 0.0, and a successful LSP override 1.0.
  Scoped re-resolution re-checks already-resolved internal edges below the
  threshold and replaces both the `ResolvedCall` and the edge target when the
  language server disagrees. A fake-LSP smoke changed `caller() -> wrong()`
  to `caller() -> right()` and moved the caller row between the two entities.
- Smoke fixture: `cast(Service, value).run()` left the call unresolved with
  LSP disabled; with Pyright enabled the rebuild added `go -> Service.run`,
  and `callers service.py::Service.run` changed from 0 to 1.
- MCP now emits one server-to-client `roots/list` request on the first tool
  call when the current root is unconfirmed or indexing is not ready. Inbound
  messages are parsed off the dispatcher thread so the matching reply can
  arrive while the tool call waits; the wait is bounded by
  `CODERADAR_ROOTS_TIMEOUT_MS` (default 1000ms). Client roots are fed back into
  `LazyRootRetry` for the existing retarget ladder. Server requests use
  negative ids so they cannot collide with client tool ids.
- Added a best-effort parent watchdog: parent pid discovery uses PowerShell
  on Windows and `/proc` or `ps` on Unix, with a one-second poll that exits
  the MCP server when the parent disappears. `CODERADAR_PARENT_WATCHDOG=0`
  disables it for embedded callers.
- Unit tests cover one-shot root probing/reply parsing and live/dead parent
  detection. Gate 4447/4447, fmt/clippy/cargo test clean (625 core tests),
  cutover 4/4. Bench: 76 functions, 40 faster in the final run.

## Rust-backed resolver classes (Unit 46 / source-parity closure)

- Moved the framework reference-resolution heuristics out of the PyO3 crate
  into `core_indexer::frameworks` so CLI, MCP and the new facade classes share
  one pure-Rust implementation (M-FFI-TRANSLATES). Gate size dropped to 2.9x.
- Added native functions `framework_claims_reference`, `framework_detect`,
  `framework_extract` and `framework_resolve_one`. Added facade exports and
  re-attachment rules in `after_bridge.py`.
- Added `coderadar.resolvers` to the Rust facade package: `base.py` keeps the
  original dataclass/ABC shape, `resolution.py` delegates all five helpers to
  Rust, and `frameworks.py` exposes all 13 resolver classes as thin delegates
  whose `name`, `detect`, `claims_reference`, `extract` and `resolve` methods
  execute in Rust. `ALL_RESOLVERS` matches the original order.
- `tests/test_facade_resolvers.py` proves all 13 names, inherited Rust methods,
  detection, claims, one-file extraction, Django resolution and the
  `resolution.py` helpers through the compiled extension.
- Gate 4447/4447, fmt/clippy/cargo test clean, cutover 4/4.

## File-scoped route refresh closure (post-plan)

- `core_indexer::api::live_snapshot()` exposes the live engine projection.
  `state::projection()` prefers it over the `Arc` cached by `state::load`,
  while still returning `None` before a load. Engine mutations commit a new
  `Arc`, so this closes the gap where `refresh_framework_file` upserted a
  route but `ops.resolve('/health')` read a stale snapshot.
- Added `port::analyze_with_routes`, a best-effort DR-10 hook that runs
  whole-tree extraction after `core_indexer::api::analyze`. The facade's
  `analyze`/`reindex_op` and the CLI's `init` now use it, matching Python's
  `analyze()` route pass without changing the analyze report.
- `tests/test_facade_routes.py` proves the initial `/health` route, an added
  `/orders/:id` route surviving a fresh load, and removal of `/health` after
  a file update. Both facade and CLI smoke now resolve routes.
- Gate 4447/4447, clippy/fmt/cargo test clean (628 core tests), cutover 4/4,
  facade pytest 3 passed.

## Fresh CodeRadar completion audit (Unit 49)

Refresh: `coderadar reindex --full` after Units 39–48, then CodeRadar
queries over `py_agent/src/coderadar` and the Rust facade/export surface.
The store is fresh; `diagnose --unresolved` lists only builtins, external
methods and test helpers, no structural edge failures.

### Measurements

- Store: **345 modules**, **735 classes**, **5,548 functions**, **2,769
  imports**, **394 constants**, **12,243 call edges**, **0 unresolved
  structural edges**, 345 files.
- `py_agent/src/coderadar`: **768 callables**, **292 methods**, **322** with
  `caller_count >= 2`, **116** with `caller_count >= 5`. Area counts are
  unchanged from Unit 38 (mcp 134, resolvers 126, `__init__.py` 120,
  `ops.py` 99, `cli.py` 89, `render.py` 60, query 27, visualizers 26, lsp 15,
  embedding 11, skills 10, agent 9, coldstart/framework/mutation 8 each,
  excludes 6, project_state 4, docs/markers 3, config 2).
- Exact-name bridge coverage: **232/768 (30.2%)** against native exports plus
  facade `__all__` (158 names), up from 203/768 (26.4%). Caller-weighted
  coverage (`max(caller_count, 1)`) is **42.8%**, up from 34.7%. Allowing the
  `_op` / leading-underscore normalizations raises those to 272/768 (35.4%)
  and 49.6%; all remain undercounts because CLI/MCP/facade front doors reach
  many helpers without a same-named export.
- CLI: **55/55 shared commands**, **0 local-only** (`cutover.py --verify`).
- MCP: Python **35/35 tool names** present in Rust; Rust advertises **49
  tools** (14 extras: history/temporal, LSP, `context` and other Rust-only
  front doors) and **3/3 bundled resources**.
- Public `CodeGraph`: **55/55** Python methods have Rust-backed facade
  implementations — 52 call `_native` directly; `batch`, `watch` and
  `watch_git` delegate to Rust-backed `BatchContext` / `Watcher` /
  `GitWatcher` helpers.
- Gate: **4447/4447** recorded cases; bench 76 functions, 42 faster, median
  **1.569x**.

### Estimate

- **User-visible surface parity: ~98%.** Every CLI command, MCP tool,
  resource and public facade method has a Rust implementation. The only
  user-visible implementation gap left is exact embedding float parity;
  shadow cutover is an operational approval, not code.
- **Callable-weighted internal coverage: ~43% by exact name**, ~50% with
  naming normalization. This metric still understates the front doors but is
  the honest literal source-parity number.
- **Remaining work:** **1–3 days** to align or explicitly tolerate the ONNX
  Runtime 1.31.0/1.32.0 vector drift, plus cutover approval. A literal
  callable-by-callable port of the remaining internal helpers is another
  **8–12 days** to near-100% source parity; no new front-door gap has been
  found by CodeRadar.

### Ranked residual units

1. Exact embedding float parity: pin/align the native ONNX Runtime or record
   an explicit gate tolerance (~2e-4 max drift), 1–3 days.
2. Shadow cutover: `python rust/cutover.py --install DIR` once approved,
   minutes.
3. Optional internal callable parity: no caller-edge gap at the CLI/MCP/facade
   boundary; only needed if every private Python helper must be bridged by
   name, 8–12 days.

## Functional-surface audit (Unit 50)

The Unit 49 audit counted names. This one probes the **contract**: every
Python CLI option, every MCP tool parameter, and a differential run of the
framework extractors. Method: `coderadar query`/`calls` for the graph,
`coderadar <cmd> --help` for both front doors, AST extraction of the Python
MCP signatures, and 23 framework fixtures executed against both packages.

### Verdict

- **Rust binary (`coderadar-rs`)**: 55/55 CLI command names, 35/35 Python
  MCP tool names, 49 tools total, 3/3 resources. No Python process, no
  Python DLL. Functionally complete at the *name* level.
- **Python facade (`rust/coderadar/python`, 1,395 LOC)**: 69 of 94 functions
  call `_native` directly; the rest are dataclass/plan shape helpers and the
  `Watcher`/`GitWatcher`/`BatchContext` process wrappers, whose work
  (`watch_changes_op`, `sync_git_op`, `git_head`) is Rust.
- **Legacy `py_agent` (20,088 LOC)**: still a full Python implementation of
  the front doors over the shared Rust `_core` engine. It is the reference
  implementation, not a runtime dependency of the Rust binary.

### Measured contract gaps

1. **CLI options are not transferred.** 42 of 55 Python commands accept
   `--format`; only **12** Rust commands do (6 native + 6 captured:
   `visualize`, `rebuild-store`, `get-smells`, `find-clones`,
   `find-scaffolding`, `reindex`). `coderadar-rs node <id> --format json`
   fails with `unexpected argument`.
   25 shared commands are missing at least one Python flag:
   - `--format` (30 commands, e.g. `node`, `callers`, `affected`, `query`)
   - `as-of --query`, `explore --query/--direction/--max-files`,
     `traverse --edge-kinds`, `open-items --scope`
   - `export-annotations --out/--store`,
     `import-annotations --store/--on-conflict`
   - `ingest-history --repo/--from/--to/--store` (Rust spells them
     `--from-sha`/`--to-sha`)
   - `verify-history --from/--to`, `changes-between --t1/--t2`
2. **Captured commands ignore `--help`.** `diagnose`, `docs`, `history`,
   `load-snapshot`, `migrate-to-commit-time`, `module-children`,
   `rebuild-store`, `reindex`, `resolve`, `revise-annotation`,
   `retire-annotation`, `store-repair`, `visualize`,
   `annotation-history`, `comment-history`, `find-clones`,
   `find-scaffolding`, `get-smells` **execute the command** with
   `"--help"` as its argument. `reindex --help` starts a full reindex and
   hangs. Only `exclude`, `git` and `skills` print usage.
3. **MCP tools silently ignore documented parameters.** 35/35 names are
   present; these parameters are never read:
   - `coderadar_node.include_neighbors` (hardcoded `false`)
   - `coderadar_explore.symbols/direction/max_files` (hardcoded
     `None`/`"both"`/`8`)
   - `coderadar_traverse.edge_kinds` (always `None`)
   - `coderadar_reindex.with_embeddings/full` (ignored, so
     `with_embeddings: true` does nothing)
   - `coderadar_verify_history.all_generations` (Rust reads `"all"`)
   - `coderadar_set_project.confirm` (ignored)
   - mutation tools read `apply` instead of Python's `dry_run` (recorded
     decision, but breaking for Python-shaped clients)

### Framework parity

23 fixtures from `tests/test_port_unit{17,18,19,22}*` were executed against
both packages: **23/23 identical** `routes`/`handlers`/`frameworks`.

Those four test files were also **silently testing the Python reference**:
`pyproject.toml` puts `py_agent/src` on `sys.path`, so
`from coderadar.framework import ...` resolved to `py_agent`. They now load
the bridge under a private alias (`tests/_rust_bridge.py`) and assert the
Rust extraction counts equal the Python reference on every fixture. The
stale `test_go_middleware` expectation (`>= 2`) was corrected to the shared
behaviour (`1`): neither implementation mints an edge for `r.Use(...)`.

### Estimate

- CLI flags + `--format` for 30 commands: **1.5–2 days**.
- `--help` routing for 18 captured commands: **0.5 day**.
- MCP parameter gaps (7 tools): **0.5 day**.
- **Total ≈ 2.5–3 days** to make the contracts, not just the names, match.
- Exact embedding float parity and shadow cutover are unchanged from Unit 49.

## Contract execution (Unit 51)

The completion plan in [`FULL_PORT_PLAN.md`](FULL_PORT_PLAN.md) was executed
through its first two phases.

**MCP protocol (done).** `tools/list` now returns real `Tool` objects built
from a catalogue captured from the Python server
(`rust/golden/mcp_tools.json`, 35 tools) plus 14 Rust-only tools and an
additive override file. Tool errors are `result.isError` rather than JSON-RPC
errors, `initialize` negotiates the protocol version and reports
`CodeRadar 0.16.0` with the Python instructions block, and notifications,
`ping`, `prompts/list` and `resources/templates/list` are handled. Argument
defaults come from the schema, undeclared arguments are rejected, and the
seven previously ignored parameters (`include_neighbors`, `explore`
direction/symbols/max_files, `edge_kinds`, `reindex` flags,
`all_generations`, `confirm`) are wired. Mutations accept Python's `dry_run`
with `apply` as its inverse.

**CLI contract (done, one engine residual).** All 42 Python commands that
advertise `--format` accept it; `as-of`, `explore`, `traverse`, `open-items`,
`export/import-annotations`, `ingest-history`, `verify-history`,
`changes-between` and `ingest-status` gained the missing flags (old spellings
kept as aliases); captured commands answer `--help` from
`rust/golden/cli_usage.json`; help/version exit 0, usage errors exit 2. The
binary reports `coderadar 0.16.0 (spec v3.6)`.

Residual **R0**: persisted rows are narrower than the Python engine's
(`search` lacks `embedding_hash` / `name_span_*`; `query` differs in field
set) because `core_indexer/src/storage.rs` serializes the stored shape rather
than `api::entity_json`. `rust/cli_parity.py` records the 11 affected
commands instead of hiding them.

**Python surface (partial, measured).** `rust/coderadar/python/coderadar`
now ships `ops`, `render`, `docs`, `skills`, `coldstart`, `excludes`,
`markers`, `project_state`, `config`, `embedding`, `query`, `visualizers`,
`lsp`, `mutation`, `mcp`, `framework` and the 13 per-framework resolver
modules, backed by 21 new native exports. `tests/test_import_surface.py`
measures 166/342 public names (48.5%); the remainder is recorded in
`rust/golden/import_surface_pending.json` and consists of `agent.graphrag`,
`embedding.dedup`, `query.planner/executor`, and the four internal
`mcp.{lazy,lifecycle,roots,startup}` modules.

**Embeddings.** `core_indexer` gained an opt-in `dynamic-ort` feature that
loads an external ONNX Runtime via `ORT_DYLIB_PATH`, so a release can match
the Python wheel's runtime; the default build keeps fastembed's pinned
prebuilt runtime. The drift measurement needs a pinned reference env
(onnxruntime 1.32 / fastembed 0.7.4), which this machine does not have.

**Verification.** `python rust/mcp_conformance.py` (49 tools, 3 resources),
`python rust/cli_parity.py` (72/59 commands, 18 payloads compared),
`python -m pytest tests/test_mcp_conformance.py tests/test_import_surface.py`,
`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`
(628 + 13 + 17) and `rust-anything gate rust/coderadar` (4447/4447, 3.0x).

## Row-shape parity and a deterministic CLI harness (Unit 52)

R0 is closed. The gap was not one bug: `api::entity_json` had drifted from
`ffi_convert.rs`, and the parity harness had been comparing two CLIs that
each rewrote the live store between runs, so its "payload differences" mixed
port gaps with cold-start churn.

What changed:

- `api::entity_json` now emits the binding's full field set (classes gained
  `name_span_*` and `embedding_hash`, modules `embedding_hash`, imports
  `name_span_*`/`has_embedding`/`embedding_hash`, constants and aliases
  `span_end`/`has_embedding`/`embedding_hash`).
- New `api::module_children` returns full entity rows plus the binding's
  import/constant projections; `traverse` reports its start at depth 0 and
  echoes `edge_kinds: null`; `open_items` returns `{scope, items, count}`;
  scaffolding rows always carry `snippet`.
- `rust/cli_parity.py` no longer probes the repository. It builds
  `core_indexer`, installs `_core.pyd` beside `py_agent/src` (so the Python
  reference runs the same engine revision), indexes
  `rust/golden/parity_fixture` once, then runs each probe against a private
  copy with `-C` for both sides. Rows are sorted by rank then id before
  comparison because `graph.functions` is a `HashMap` and tie order is not a
  contract.

Result: **15/15 payloads identical**, 72/59 command inventory, `--help` and
exit codes green. Guard tests live in
`core_indexer/src/graph/tests/api_rows_tests.rs`.

Remaining plan units: S5 (`query.executor`), S7
(`mcp.{lazy,lifecycle,roots,startup}`), E2 (embedding
drift contract), R1/R3 (artifact rename + packaging), R2/A2 (shadow cutover
approval + final audit) — about **5-6 days**.

## Embedding dedup, query planner and GraphRAG shims (Unit 53)

Three more Python modules now delegate to the engine instead of carrying
their own logic:

- `embedding.dedup` — `api::embed_body` owns the embedded text,
  `api::embed_cache_key` the `{model}#pp{N}#{hash}` key,
  `api::stored_key_model` the legacy bare-hash case, `api::content_hash`
  the xxHash3-64 value (the shim keeps Python's SHA-256 fallback when the
  `xxhash` package is absent), and `api::embedding_batch` the
  cache-hit/miss split plus the model call.
- `query.planner` — `api::plan_query` is the keyword ladder, in the same
  order, returning intent/method/params/top_k; the shim only builds the
  `QueryIntent`/`QueryPlan` dataclasses.
- `agent.graphrag` — `api::graphrag_build_context` compresses rows
  (`signatures_only` / `structural` / `full`), skeletonises bodies and
  enforces the token budget; `api::graphrag_execute` plans, executes and
  compresses. The shim exposes `ContextStrategy`, `GraphRAGResult`,
  `GraphRAGContextBuilder`, `GraphRAGPipeline` and `QueryPlanner`.

`rust/import_surface.py` is now the committed measurement tool: it loads the
bridge under a private alias, records every public name under
`py_agent/src/coderadar`, and writes
`rust/golden/import_surface_pending.json` (or `--check` for CI). Coverage is
**181/342 (52.9%)** with five modules left: `query.executor` and
`mcp.{lazy,lifecycle,roots,startup}`.

`rust/after_bridge.py` had been left with literal newlines inside string
literals (invalid Python), so a re-bridge would have failed at the facade
step. It is repaired and now also re-attaches the dedup/planner/GraphRAG
exports.

## MCP root ladder (Unit 54)

`coderadar.mcp.roots` now delegates to `core_indexer::mcp_runtime`. The
`ResolvedRoot` dataclass, `uri_to_path`, `resolve_project_root`,
`resolve_from_client_roots`, `resolve_selector`, `adopt_project_root` and
`describe` are shims over native functions; only `client_roots(session)`
stays in Python because it calls the MCP SDK's session object (the decision
about the answer is still Rust's).

Two engine bugs surfaced while matching the reference tests:

- `find_marker` walked past `$HOME`, so a stray `~/.coderadar` was adopted as
  the root of every project underneath it. The walk now stops at home and
  never treats home or its ancestors as a project root, like Python.
- Canonical paths kept the Windows `\\?\` verbatim prefix, so a resolved root
  never compared equal to `Path.cwd()`. Canonicalisation now strips it, and
  `uri_to_path` decodes without requiring the path to exist (Python's
  `uri_to_path` does not check the filesystem; `_canonical` does).

`resolve_non_strict` also replaced `canonicalize` in `canonical_dir`, so a
tool call naming a file below a real directory resolves to that directory
instead of failing — the same rule Python applies.

Guard tests: `tests/test_facade_roots.py` (10 cases mirroring
`tests/mcp/test_root_resolution.py`) plus the existing `mcp_runtime` unit
tests, which were updated for the non-verbatim canonical form.

Surface: **189/342 (55.3%)**; four modules remain (`query.executor` and
`mcp.{lazy,lifecycle,startup}`).

## MCP handles and the last absent module (Unit 55)

The Python MCP server's process handles are now engine-backed:

- `mcp.startup` — `BackgroundIndex.start/restart/wait/status/elapsed` call
  native `startup_*` functions over `mcp_runtime::BackgroundIndex` (the same
  type the Rust server uses): one build lock, generation-guarded status,
  bounded wait. A custom `analyze` stub still works because the native worker
  calls back into Python, and `startup_wait` releases the GIL while blocking.
- `mcp.lifecycle` — threads/timers stay in Python (they are host objects),
  but the liveness probe is `mcp_runtime::process_alive` and the exit reasons
  are shared.
- `mcp.lazy` — `LazyRootRetry` keeps only the "already attempted" flag and
  the async `roots/list` call; `lazy_should_ask` and `lazy_trade_up` decide.
- `query.executor` — `MacrameQuery`/`MacrameSnapshot` delegate traverse,
  find, list-by-kind, callers/callees and stats to the engine; the three
  methods that were unwired in the reference still return `[]`.
- `mcp.server` gained `serve` (SDK stdio loop + lifecycle guards) and
  `set_launch_cwd`.

**Every module under `py_agent/src/coderadar` now imports from the bridge**
(0 absent), at **210/342 public names (61.4%)**. The remaining 134 live
inside nine resolving modules: `cli` (65 click wrappers), `render` (42),
`__init__` (17 mutation dataclasses/exceptions), and a handful of helpers.

New tests: `tests/test_facade_mcp_handles.py` (10 cases — custom build,
failure message, restart, watchdog, handshake, lazy ask/trade-up/retarget).

## Embedding drift contract (Unit 56)

`fastembed` on both sides means the *same* model file
(`qdrant/bge-small-en-v1.5-onnx-q`, `model_optimized.onnx`) and tokenizer, but
the linked ONNX Runtime differs (Python 1.32.0 vs the `ort` crate's 1.31.0),
and the quantized kernels do not round identically. E2 turns that from a
known unknown into a recorded contract:

- `rust/embedding_parity.py --python <reference-interpreter>` records the
  corpus, its vectors and the reference versions into
  `rust/golden/embedding_parity.json`.
- `core_indexer/src/graph/tests/embedding_parity_tests.rs` (ignored by
  default, loads the model) asserts the width, a max component drift within
  the recorded tolerance, identical top-3 ranking, and pairwise cosine
  agreement.

Measured: **4.328e-4** max component drift, 2.460e-5 mean, 1.254e-4 max
cosine drift, **identical ranking** on all 12 texts. Tolerance is 5e-4 —
deliberately too tight for a different model to slip through.

Note for CI: the reference runtime is not a dependency of this repo. On this
machine it lives in the `coderadar-rs` uv tool venv
(`uv/tools/coderadar-rs/Scripts/python.exe`); re-record with that path.

## Packaging, smoke and one audit entry point (Unit 57)

- **The artifact is `coderadar`.** `[[bin]] name = "coderadar"`; the harnesses
  go through `rust/binaries.py`, which prefers the product name and still
  finds pre-rename `coderadar-rs` builds.
- **`docs/INSTALL.md`** is the install contract: platform matrix, statically
  linked ORT (no separate DLL), MSVC CRT/DirectML note on Windows, model cache
  locations and offline warm-up, environment knobs, verification and
  uninstall.
- **`rust/smoke.py`** is the clean-machine check: fresh directory, system-only
  `PATH`, scratch `HOME`, `--version` -> `init` -> `status` -> `query` -> MCP
  `tools/list`, then a PE string scan for Python DLL references. Six checks,
  all green against the release build (49 tools; no Python DLL).
- **`rust/audit.py`** is the single verification entry point: fmt, clippy,
  workspace tests, `rust-anything gate`, CLI parity, MCP conformance, import
  surface, the E2 embedding contract and the smoke test, with `--quick` and
  `--json`. Current run: 9 checks, 149 s, all green.
- Shadow cutover stays unapplied: `rust/cutover.py --verify` passes 4/4, and
  `--install DIR` is ready, but putting the Rust binary on `PATH` in place of
  the Python CLI still needs explicit approval.
