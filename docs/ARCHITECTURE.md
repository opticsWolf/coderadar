# CodeRadar Architecture

> **Living document.** Baseline: v0.12.0 (`dev_0.12`), `macrame-db` 0.19.1,
> store schema v22. Update with feature commits the way `CHANGELOG.md` is
> updated — a section that describes behavior the code no longer has is a bug.
> Design *history* (why things are the way they are) lives in the
> [decision register](#16-decision-index) and the OKFgraph knowledge base
> (§15); this file describes the system as it is.

CodeRadar is a **code graph with a temporal ledger**: it indexes a source tree
into entities and relations, serves them to agents and humans over three
surfaces (MCP, CLI, Python), rewrites code through guarded mutations, and
keeps full source history as content-addressed blobs so any past state can be
read back byte-for-byte.

Five ideas explain most of the design:

1. **AST-only resolution works.** No Stack Graphs, no LSP in the hot path
   (DR-1, DR-2). Tree-sitter grammars + a language-aware resolve cascade
   reach 100/100 precision/recall on the benchmark corpus.
2. **Honesty over coverage.** A missing capability raises a named error; it
   never returns plausible fiction. The error taxonomy (§8) is part of the
   architecture, not an afterthought.
3. **One name per operation.** Every graph operation is `coderadar_<op>`
   (MCP), `coderadar <op>` (CLI), `CodeGraph.<op>()` (Python) — pinned by
   parity tests, so one fix in `ops` reaches all three surfaces (DR-17).
4. **Macrame owns time; CodeRadar owns code.** The `macrame-db` ledger is the
   temporal engine (bitemporal assertions, state folds, blobs). CodeRadar
   never re-implements folding, scanning, or hashing around it.
5. **History is bytes, facts are rows.** Full file generations live as
   SHA-256 blobs; the ledger holds facts + digests. Diffs compute on read.

---

## 1. System map

```
┌─ Surfaces ──────────────────────────────────────────────┐
│  MCP server (26 coderadar_* tools)  ·  CLI (click)       │
│  Python: CodeGraph facade  ·  skills/coderadar-{mcp,cli} │
└───────────────────────┬─────────────────────────────────┘
                        │  one shared op layer
┌─ Python agent ────────▼─────────────────────────────────┐
│  ops.py (OPERATIONS, validated args, errors)            │
│  coldstart · resolvers · framework · query · embedding  │
│  mutation guard · render · visualizers · config          │
└───────────────────────┬─────────────────────────────────┘
                        │  PyO3 (dicts across the boundary, DR-4)
┌─ Rust core (_core) ───▼─────────────────────────────────┐
│  extract (single-pass, 41 grammars) → resolve → graph   │
│  smells · clones · deadcode · cfg · centrality          │
│  query engine · mutation plans · snapshots              │
└───────────────────────┬─────────────────────────────────┘
                        │
┌─ Stores ──────────────▼─────────────────────────────────┐
│  Macrame ledger (bitemporal facts)  ·  SQLite hot store │
│  blob files (sha256)  ·  archive file (cold)            │
│  concepts_fts (FTS5)  ·  embedding table (Phase 1: key) │
└─────────────────────────────────────────────────────────┘
```

Process model: indexing and serving are **library calls in one process**.
The CLI builds (or cold-loads) the graph per invocation; the MCP server holds
it warm and refreshes incrementally. There is no daemon, no second server —
multi-project work is one server + `coderadar_set_project`, never two servers
(BUGS_QUIRKS #7).

---

## 2. Repository layout

```
CodeRadar/
  core_indexer/                 # Rust crate (PyO3 module `coderadar._core`)
    src/
      lib.rs                    # _core module root: analyze/traverse/blob/…
      ffi_convert.rs            # PyO3 dict converters (boundary types)
      ffi_config.rs             # set_config from Python-side TOML
      ffi_git.rs                # worktree_clean / blame / changed_files
      ffi_watcher.rs            # file-watcher start/next/stop batches
      ffi_synthetic.rs          # synthetic edges/routes/embeddings/modules
      types.rs                  # entity/edge/import-kind enums, ByteSpan
      storage.rs                # ledger access, blobs, FTS, archive, repair
      extract/                  # single-pass cursor: walker, tagger,
                                # single_pass, spans, decorators, docstring
      graph/                    # call_graph, import_graph, inheritance, mro,
                                # receiver_types, module_resolution,
                                # resolve_calls, cfg, centrality, embeddings,
                                # config, indexing, persistence, cold_start,
                                # projection_ops, deadcode/, tests/
      resolve/                  # orchestrator, import_graph, cache, signature
      query/                    # grammar.pest + grammar.rs + exec + schema
      mutation/                 # plan/apply: edit, indent, write_guard
      smells/                   # engine + rules/ (12 rules)
      clones/                   # tokens, minhash, lsh_index, apted
      scaffold/                 # scaffolding detector + secrets
      scoring/  fs/             # scoring helpers; git + watcher backends
      update/                   # wal.rs (tombstoned; see DR-6)
    queries/*.scm               # 41 tree-sitter query files
  py_agent/src/coderadar/
    __init__.py                 # CodeGraph facade, Snapshot, errors
    ops.py                      # OPERATIONS: validated shared op layer
    cli.py                      # click CLI (graph ops + local-only cmds)
    mcp/server.py               # 26 tools, thin over ops
    coldstart.py                # ledger load + changed-files refresh
    config.py                   # .coderadar.toml (pydantic, §12)
    resolvers/ framework.py     # L2–L4 cascade + framework routes (DR-10)
    query/                      # query-language client
    embedding/                  # dedup keys, model agreement, bulk set
    mutation/                   # apply pipeline, tool_router (named seam)
    render.py  visualizers/     # human + JSON output, graph diagrams
    excludes.py markers.py      # ignore rules, entity markers
    project_state.py            # cwd-rooted serving state, reindex-on-demand
    agent/graphrag.py           # named seam → P2 context work
    lsp/pool.py                 # named seam → L4 override (DR-2/DR-16)
  skills/coderadar-{mcp,cli}/   # in-repo agent skill truth (P2)
  tests/                        # 1078 Python tests + Rust 434 (lib + integ)
  docs/                         # live set only (see §15)
```

`lib.rs` was split per DR-28 (4,750 → ~3,700 lines): bindings stay,
converters/git/watcher/config/synthetics live in `ffi_*`.

---

## 3. The Rust core (`coderadar._core`)

PyO3 functions, grouped by job. Dicts cross the boundary (DR-4); the Python
side never touches ledger handles.

- **Index:** `analyze` (full walk), `update_file` / `remove_file`
  (incremental), `default_excludes`, `indexed_root_py`, `is_path_excluded`.
- **Traverse/lookup:** `traverse`, `traverse_unresolved`,
  `unresolved_targets`, `callers_of`, `callees_of`, `call_sites`,
  `lookup_entity`, `lookup_entity_at(s)`, `normalize_timestamp`,
  `graph_stats`, `index_edge_stats`, `module_children`.
- **History/blobs:** `blob_get`, `blob_stats`, `entity_source_ref_at`,
  `archive`.
- **Search:** `search_entities`, `search_symbols` (FTS5), `search_similar`
  (vectors), `query_graph` (query language).
- **Analysis:** `get_smells`, `find_dead_code`, `rank_by_centrality`,
  `find_clones`, `find_scaffolding`.
- **Mutations:** `plan_body_replacement`, `plan_signature_update`,
  `plan_rename`, `plan_create_entity`, `apply_mutation`.
- **Snapshots/ops:** `load_snapshot`, `store_repair`, `set_config`.
- **Local-only:** `git_*` (3), watcher (3), synthetic registration (edges,
  routes, embeddings, star exports) — test and route scaffolding.

---

## 4. The Python layer

- **`CodeGraph`** (`__init__.py`) — the facade. Owns the serving state:
  already-serving means cwd-rooted; `reindex` on demand; `ensure_ready`
  bounded wait with warming-vs-hung log signal. `Snapshot` objects serve
  history: `read_bytes` (exact bytes-at-T) vs `read_source` (display form).
- **`ops`** — `OPERATIONS` (26 names) with validated arguments and the
  shared error taxonomy. One fix here reaches MCP + CLI + Python.
- **`coldstart`** — `build_graph`: load the ledger, then refresh only
  changed files. Stale load + changed-files-only is the pinned discipline
  (§3.2); `full` or config mismatch forces a re-walk.
- **`resolvers` + `framework`** — the L2–L4 cascade (imports → framework
  routes → signatures) over the Rust-resolved base. Routes are canonical
  `route` concepts with persisted route→handler edges (DR-10).
- **`query/`** — client for the query language (`query-language.md`
  is generated from the grammar; drift is a test failure).
- **`embedding/`** — Phase 1 contract (DR-11): dedup key
  `{model}#pp{PREPROCESS_VERSION}#{xxh3}`, write/query dimension gates,
  embed-what = signature + docstring, `recompute` flag on every surface.
  Default model `BAAI/bge-small-en-v1.5`, dim 384, single process-wide
  agreement. Persistence is 0.13.
- **`mutation/`** — apply pipeline with post-write parse validation and
  auto-diagnose (rollback needs its own design: BUGS_QUIRKS #1-half).
  `tool_router.py` is a named seam for P2 routing (DR-16).
- **`render` / `visualizers`** — human and JSON output; diagrams fail loudly
  instead of drawing fiction (open-items §3.2 lineage).
- **`excludes`, `markers`, `project_state`** — ignore rules, entity markers,
  cwd-rooted serving state.
- **`agent/graphrag.py`, `lsp/pool.py`** — kept as named seams with wiring
  triggers in their headers (DR-16); neither is on any hot path.
- **`mcp/server.py`** — 26 tools, thin over `ops`, plus server instructions
  (language count, staleness guidance — README-tested).
- **`cli.py`** — graph ops plus local-only commands (`init`, `visualize`,
  `shell`, `git`, `exclude`, `watch`, `load-snapshot`, `store-repair`,
  `--delete`); CLI text never names `coderadar_*` tools (surface verdict).

---

## 5. Data model

**Concepts** (ledger facts, forward-slash canonical ids — DR-19): `module`
(whole file), `function`, `class`, `constant`, `import`, `field`, `route`
(Django/Flask/FastAPI/Express/Rails route → handler), `type_alias`.
Internal: `EmbeddingVec`, `ProjectedGraph`.

**Edges** — nine kinds exist, four are asserted by the backfill:

| Kind | Populated? | Notes |
|---|---|---|
| `contains` | yes (structural) | module → members |
| `calls` | yes | asserted + external + unresolved + route shares (DR-32) |
| `imports` | yes | incl. `FromImport` / `ModuleImport` / `StarImport` / `RelativeImport` |
| `extends` | yes | `IMPLEMENTS` folded in (DR-24, open-items §2.6) |
| `overrides` | yes | |
| `implements` `references` `decorates` `instantiates` | declared, unpopulated | open-items §2.6 |

`graph_stats` breaks out asserted / external / unresolved / route shares —
the outer number and the ledger rows disagree by category, never by loss
(DR-32: "which count is truth" was a category error).

**Identity & spans.** Ids are canonical forward-slash paths + symbol scope;
legacy spellings resolve via fallbacks. Spans are tree-sitter extents
(`ByteSpan`); byte offsets are the merge key for mutation verification.

**`extra.coderadar.source_blob`.** The file module concept carries
`{digest (sha256 hex), format: 1}` — digest ONLY, never bytes. Put-then-assert
ordering: bytes land before the digest fact (DR-25). Cumulative `BLOB_STATS`
reset on analyze/load.

---

## 6. Indexing pipeline

1. **Walk** the tree minus excludes (default belt + TOML + secret patterns).
   Steady state performs **zero walks** — routes and config are loaded, not
   scanned (§1.3).
2. **Single-pass extraction** (DR-8): one `QueryCursor` pass per file emits
   entities, spans, docstrings, decorators, and raw call/import events.
   41 tree-sitter grammars; Tier-1 (12: Python, PHP, JS, TS, Kotlin, Go, Rust,
   Ruby, Java, C++, C, C# — signature-tested) vs Tier-2 (29, best-effort).
3. **Module resolution**: dotted-name lookup with suffix-winner ranking
   (same-language → smallest id, DR-14); conventional `@/~/` → `src/`
   aliases (custom tsconfig/pyproject maps are 0.13: §2.5); Rust `use`
   parsing with groups/globs/aliases and `crate`/`super`/`self` roots,
   `a::b::c` dual edges, `Self::assoc` and `Enum::Variant` (DR-13).
   Python method dispatch stays unresolved without trait-impl attribution
   (documented, out of scope).
4. **Call resolution cascade**: L1 AST facts → L2 imports → L3 framework
   routes → L4 signatures (L5 LSP planned, pool kept as seam). Star imports
   bind top-level functions; glob `use` binds modules.
5. **Assert** entities + edges to the ledger with `valid_from = now`
   (index time ≈ world time, pragmatic and documented), and put source
   generations as blobs when `store_source_blobs` (default on, DR-30).
6. **Incremental**: `update_file` re-extracts one file and retracts its
   facts; `reindex` re-reads `.coderadar.toml` (exclude changes force a
   full walk via `indexed_config.json`, DR-31) and retracts on probe.

---

## 7. Storage & history

One store directory, four artifacts:

```
.coderadar/store/
  coderadar.db            # Macrame ledger (facts) + SQLite hot tables
  coderadar.db.blobs/     # content-addressed generations (sha256 filenames)
  coderadar_archive.db    # cold generations + archivable ledger rows
  indexed_config.json     # honored excludes (DR-31 staleness tripwire)
```

- **The ledger is bitemporal.** `valid_from/valid_to` = world time,
  `recorded_at` = learned time. Code history means **recorded time**:
  `reconstruct(T)` folds facts with `recorded_at <= T` (`bfs_over_state`).
  Production never walks the ledger temporally without the fold (DR-9) —
  the rename-fixture reproducer proves why: live projection wears current
  names, `as_of_recorded` alone misses retired edges.
- **Reads at T.** `Snapshot.find` folds once per call (empty-symbols skips
  the fold; non-empty storeless raises). Topology AND bodies come from the
  one reconstruct — no loader temporal reads (fixture `repro_walk_retired`
  pins this; rename the fixture and re-prove).
- **Blobs.** Full file bytes per generation, sha256-addressed, xxh3 for
  staleness. `read_bytes` = exact bytes-at-T (CRLF on Windows preserved);
  `read_source` = display form. Missing blob → `ContentUnavailable`
  (names the digest); absent-at-T → `None`. Never confused (§3.0).
- **Retention.** `archive_after_days` = days-hot cutoff, or an explicit
  `cutoff` stamp; only `CodeGraph.archive()` / `ops.archive()` moves bytes
  (analyze never auto-archives). Cold blobs land in `cold.blobs` inside the
  `<store-stem>_archive.db` sibling, alongside archivable ledger rows. Cold
  stays readable via hot+cold union (hot entries naming cold-only blobs copy
  them back); lost cold file → `ContentUnavailable`. Backup = quiesce →
  `wal_checkpoint(TRUNCATE)` → copy hot + cold.
- **FTS5.** Trigger-maintained `concepts_fts` over concept text (bodies
  unindexed by decision). Escaped-by-default, `raw=True` opt-in,
  `top_k` in [1, 50], empty → `[]`, storeless → `InvalidRequest`
  ("stored graph"), missing table → `Engine(Misuse)` naming
  `concepts_fts` + `rebuild_fts`. 1s latency gate; single-digit ms live
  (DR-34).
- **Vectors (Phase 1).** Keyed dedup, dim gates, exact-count enumeration,
  zero storage over the FTS table. Persistence is 0.13 (DR-11).
- **Macrame 0.19.1.** `hydrate_historical` + `AttributeMode::{Current,
  AtTime, Omit}` repaired the historical loader (upstream #3); production
  stays on the state fold by decision. `load_subgraph_with(traversal, ts,
  byte_budget)` unchanged; unstated `attribute_mode` with instants is a
  hard error. `libsql` co-pinned at 0.9.30, one copy in `cargo tree`.

---

## 8. Read paths & the honesty model

Every read goes through the §3.2 discipline: serve served state, refresh
changed files, never a surprise full analyze. Failures use the shared
taxonomy — each is a behavior contract, not a string:

| Error | Meaning |
|---|---|
| `NoIndex` | no graph (nothing served here yet) |
| `NotFound` | unknown entity / absent-at-T |
| `InvalidRequest` | bad args; also storeless FTS, temporal-unsupported topology |
| `TemporalUnsupported` | honest refusal: callers-at-T, upstream-at-T (§3.3 NO-GO) |
| `ContentUnavailable` | fact exists, bytes missing (names the digest) |
| `EngineError` | ledger/FTS misuse (names the fix, e.g. `rebuild_fts`) |

Release-gate proofs (§0.1(b), P2): `as_of` returns old/new names **and**
bytes; 0 retirements on unchanged trees; blob bytes round-trip; parity
across MCP/CLI/Python (8 tests); latency gates history-aware
(1.5s + 20µs/log-row + 60s hang guard, DR-33).

---

## 9. Mutations

`plan_*` (pure, reviewable) → `apply_mutation` (guarded write):

- Write guard: backup → atomic write → post-parse → restore on failure.
  The written file is validated to parse (BUGS_QUIRKS #1 root cause fixed);
  auto-rollback on introduced errors needs its own apply-semantics design
  (carried half).
- Byte-span verification: edits apply at tree-sitter spans, with
  first-line indent re-basing (`normalize_body_for_splice`).
- `update_signature` warns `unverified_sites` — call-site cascade needs
  arg-span indexing (§2.3).
- Self-hosting: `[mutation] allow` is root-anchored and pinned, deny-wins,
  `default_dry_run = true` until the D18 fix (DR-27).

---

## 10. Analysis engines

- **Smells** (12 rules, one finding per rule×entity): god-class,
  long-method, data-class, dead-code/branch, RTA layering, and seven more —
  see `smell-rules-reference.md`. Metric approximations (substring
  CBO/CYC) are golden-pinned, not drifting (§2.4).
- **Clones**: token → MinHash (fixed seeds) → LSH → APTED/TED verify.
  Deterministic by construction: canonical member order + total group order
  before the cut, byte-identical across fresh processes (DR-12).
- **Dead code**: entry points × reachability over asserted edges; self-corpus
  100 → 57 findings across v0.12 as resolution improved (fewer false
  `external::`).
- **CFG / centrality / scaffolding**: per-function control-flow graphs,
  PageRank-style ranking, scaffold detector with secret patterns.
- **Call/import/inheritance graphs + MRO + receiver types**: the substrate
  `explore`, `affected`, and `diagnose` read.

---

## 11. Surfaces & parity

`ops.OPERATIONS` = 26 names. By group: **read** (explore, node, search,
affected, resolve, query, search_similar, module_children, callers, callees,
diagnose, as_of, traverse); **analysis** (get_smells, dead_code, find_clones,
find_scaffolding, compute_embeddings); **write** (replace_body,
update_signature, rename, create_entity, reindex, update_file); **project**
(status, set_project).

By design, not by omission: `search_symbols` + `archive` are Python-API-only
(DR-34 spec, §3.4); `visualize`, `shell`, `git`, `exclude`, `watch`, `init`,
`load-snapshot`, `store-repair`, `--delete` are CLI-only (local-process
concerns); `set_project` is `-C/--project` on the CLI with no Python form
(it works on the cwd). Proven by `test_surface_parity.py` + `test_mcp.py` +
`test_skill_drift.py` (skills are repo truth; old spellings warn until 0.13,
DR-22).

---

## 12. Configuration (`.coderadar.toml`)

One file, every key read by something, `reindex` names keys it could not
use. Sections: `project`, `import_graph`, `signature`, `resolution`
(incl. `lsp` pending its consumer), `embedding` (one model + dimension,
process-wide agreement), `database` (path, `store_source_blobs` default
true, `blob_exclude` secret belt), `retention` (`archive_after_days`,
unset = no default), `mutation` (allow-list, deny-wins, dry-run),
`query`, `watch`. ~100 inert knobs were cut in v0.7 (DR-15); two survive
with documented pending consumers.

---

## 13. Quality gates & release policy

- **Tests:** 1512 (434 Rust incl. 5 integration + 1078 Python). Gate bundle:
  release-gate history, read-path discipline, call-edge accounting, snapshot
  honesty, surface parity, skill drift, Rust cross-module, blob reads +
  2 latency gates.
- **Lint:** clippy 0 (`--all-targets -D warnings`), `cargo fmt` clean,
  `ruff check` clean (format not adopted), CI lint blocking (P4).
- **Version policy (DR-26, process-open):** `pyproject.toml` + workspace
  `Cargo.toml` together, +0.0.1 per implemented feature, one bump commit
  named in the feature's DR row. Spec/test/verification-only and dep
  patches: no bump. Minor cut accumulates (0.12.0).
- **Register currency is a release-gate input:** DR rows update the same
  session as the code.

---

## 14. Temporal model (the §3.3 decision)

Upstream-at-T and query-at-T raise `TemporalUnsupported` — a NO-GO for
0.12, not a gap in the fence (DR-9, open-items §2.2). Macrame 0.19 has no
reverse-edge ledger; zero agent traces needed callers-at-T. Re-check on:
first blocked trace, or Macrame reverse-edge support. A future GO needs no
ledger change (reverse-BFS over reconstructed state is available).

---

## 15. Knowledge & docs

- **OKFgraph `kb.db`** is the project memory: the decision register
  (concept `coderadar-decision-register`, all DR rows + statuses), the
  architecture review + v0.12 plan trio (frozen research corpus), and
  `thoughts/coderadar-v0.12-impl/` (per-landing reasoning). GPU-indexed
  (`cuda`, fp16); register rows update same-session.
- **This directory holds the live set only** (`v0.12-consolidated.md` =
  architecture baseline + plan + deviations diary, store-and-retention
  + query-language (generated) + smell reference + open-items + BUGS_QUIRKS +
  CHANGELOG). Removed content lives in OKFgraph — never deleted, only moved.
- **`v0.12-consolidated.md` Part III** is the step-4 surface verdict and the
  implementation diary: one name/behavior per op, CLI-only/API-only calls.
- **Release process:** feature → bump → register → thought → dogfood
  self-host → gate → merge. `main` + tag push only on explicit release.

---

## 16. Decision index

Full rationale per row lives in the register (`coderadar-decision-register`
in kb.db). Statuses at v0.12.0:

| ID | Decision | Status |
|---|---|---|
| DR-1 | Stack Graphs L1 deferred, then deleted | closed |
| DR-2 | LSP L5 never wired; pool kept as seam | carried (seam) |
| DR-3 | LadybugDB+Cypher → Macrame + direct ops | closed |
| DR-4 | Flat-buffer FFI → PyO3 dicts | closed |
| DR-5 | MVCC/ArcSwap → epoch + locks | closed |
| DR-6 | WAL → assertions + pipeline rollback | closed |
| DR-7 | MutationLog never built; `mutations` CLI removed | closed |
| DR-8 | Two-pass → single-pass extraction | closed |
| DR-9 | Snapshot honesty: fold-at-T + `bfs_over_state` + raisers; §3.3 NO-GO | closed |
| DR-10 | Framework routes as canonical concepts | closed (v0.11.9) |
| DR-11 | Embeddings Phase 1 (keyed, gated); persistence → 0.13 | closed/carried |
| DR-12 | Clone determinism (canonical order) | closed (v0.11.6) |
| DR-13 | Rust cross-module resolution | closed (v0.11.15) |
| DR-14 | Same-language smallest-id suffix winner | closed (v0.11.2) |
| DR-15 | ~100 inert knobs cut; 2 carried | carried |
| DR-16 | Test-only packages kept as named seams | closed |
| DR-17 | One-name surface + ops layer | closed |
| DR-18 | Ledger cold start; `_ensure_graph` retired | closed |
| DR-19 | Canonical ids + store migration | closed |
| DR-20 | fossil-mcp detector port (TED, tiers) | closed |
| DR-21 | Precision over coverage (v0.10) | closed |
| DR-22 | Deprecations live until 0.13 | carried |
| DR-23 | TS throughput gap accepted | carried |
| DR-24 | IMPLEMENTS folded into extends | carried |
| DR-25 | Source history in blobs (§0.6/§1.9/§3.0/§3.4) | closed |
| DR-26 | Version policy +0.0.1/feature | open (process) |
| DR-27 | Self-host allow-list, deny-wins, dry-run | closed |
| DR-28 | `lib.rs` split, clippy/ruff/CI gates | closed |
| DR-29 | Header + `.pyc` residue | closed (v0.11.1) |
| DR-30 | Blobs default-on-with-notice | closed |
| DR-31 | Config-change forces re-walk | closed (v0.11.3) |
| DR-32 | Call-edge accounting (category error) | closed (v0.11.13) |
| DR-33 | History-aware load gate | re-baselined |
| DR-34 | FTS5 keyword search | closed (v0.11.10) |

---

## 17. Open items & non-goals

Live list: `open-items.md` (all §1 + §2.1 + §3.1 resolved in v0.12;
§2.3 arg-spans, §2.5 path maps, §2.6 edge kinds, §2.8 TS throughput, §2.9
SQLite internals still deferred; §2.2/§2.7 decided). Bugs/quirks:
`CODERADAR_BUGS_QUIRKS.md` (9 fixed, #10 harness-side carried,
#1 rollback half needs apply-semantics design).

---

## 18. Glossary

- **Ledger** — the Macrame store of bitemporal facts. **State fold** —
  facts reduced to the live projection. **Reconstruct(T)** — the projection
  as of recorded-time T. **Generation** — one file's bytes at one index run.
- **Asserted / external / unresolved / route** — call-edge shares: both
  halves in-repo / target outside / target unknown / via route concept.
- **Fold-at-T** — Snapshot reads materialize at-T state before answering.
- **Cheap path** — serve + refresh-changed, never an unasked full walk.
- **Named seam** — kept code with a documented wiring trigger (DR-16).
- **Default-on-with-notice** — blobs ship on; the counts on every report
  are the notice; the kill-switch is documented (DR-30).
