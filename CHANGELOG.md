# CodeRadar Changelog

Patch releases accumulate toward the next minor (+0.0.1 per implemented feature,
bump commit named in the feature's DR row). The v0.12 plan history lives
in `docs/` (v1 frozen, consolidated trio authoritative).

## Unreleased

## 0.12.0 (2026-10-07)

- P2 agent enablement: `skills/coderadar-mcp` + `skills/coderadar-cli`
  (repo source of truth, installed to `~/.pi/agent/skills`),
  drift-tested against the tool registry (`test_skill_drift.py`);
  `codegraph-init` retired aside. Serving install verified current
  (version now resolves from pyproject.toml: 0.11.14 in both venvs).
  Old CLI spellings carry the 0.13-removal notice (pinned).
- Release-gate history proof: `as_of(before/after)` → old/new names AND
  bytes in one flow (`test_release_gate_history.py`).
- BUGS_QUIRKS triage: 9/10 fixed-with-evidence (#1 root-caused: re-base
  from lines[1..] + regression test; #10 carried, harness-side).
- §0.3 Rust cross-module resolution (DR-13): `use` parsing + `::` chains +
  `crate`/`super`/`self` roots + `Self::`/`Enum::Variant` + glob binding;
  fixture 1/6 → 6/6 in-repo, self corpus asserted 999 → 1320,
  dead-code 100 → 57 (`tests/test_rust_cross_module.py`). Trait-impl
  method attribution explicitly out of scope (stays external).
- macrame-db 0.19.1 uptake (no bump: dep patch, no user-facing change):
  `hydrate_historical` + `AttributeMode` fix upstream #3 through the
  bitemporal composition; `repro_walk_retired.rs` re-proven on a rename
  probe and un-ignored as regression pins; production stays on
  `bfs_over_state` by decision. libsql single 0.9.30, clippy 0, fmt clean.
- §3.2 read-path discipline pinned E2E (unchanged-tree reindex: revision
  stable, `indexed_at` never advances, 0 new blob bytes); §3.3 upstream
  traversal NO-GO for 0.12 with evidence (no ledger in-edges in Macrame
  0.19, no agent-trace need; re-check triggers defined). No bump.
- P4 hygiene: clippy 0 (`--all-targets`, `-D warnings`), rustfmt clean,
  `ruff check` clean, lint leg blocking in CI. Dead code deleted
  (`subgraph_bfs`, `is_ws_gap`, `resolve_method_call`).
- DR-33 latency gate re-baselined history-aware (1.5s + 20µs/log-row;
  wall printed for trends). No bump (test-only).
- DR-27: self-host `[mutation] allow` fixed to own dirs, deny-wins
  stated, `default_dry_run=true` kept explicit until D18.
- DR-29: `resolve/mod.rs` header aligned with the orchestrator cascade;
  no tracked `.pyc`, no flatbuffer sources (no-action half verified).
- Wire-or-cut: `agent/graphrag`, `lsp/pool`, `mutation/tool_router` KEPT
  as named P2/L4 seams with wiring triggers (cutting = churn).
- C + C# signature cases added to the Tier-1 table.
- Effective-excludes header states the matcher boundary (baseline+config
  counted, `.gitignore` walk-level listed-not-counted).

## v0.11.14 — §3.1 embeddings Phase 1 (DR-11)

- Dedup keys: `{model}#pp{version}#{xxh3}` — model id + preprocessing IN
  the key (switches retire by missing, never silent reuse); legacy bare
  hashes miss-and-heal once; format blob-ready for 0.13.
- One dimension per projection: Rust write gate (derives expected from
  stored) + query gate, explicit errors naming `recompute=True`.
- Embed-what decided: `signature + docstring` (function/class/module),
  `signature or name` (rest); routes carry no vectors.
- Silent 10k-per-kind cap replaced by exact-count enumeration.
- Model switches need `recompute=True` (clear-all + regenerate),
  threaded ops/API/MCP/CLI; configured dimension checked pre-store.
- Removed dead `truncated_dimension` (config + dedup).

## v0.11.13 — DR-32 call-edge accounting split

- `graph_stats` breaks out asserted/external/unresolved/route shares:
  `call_edges == asserted + external + unresolved + route_edges`, with
  asserted equal to live ledger CALLS (invariant pinned E2E).

## v0.11.12 — §3.4 retention + backup (DR-25)

- `ops.archive` / `CodeGraph.archive` over Macrame archive sessions;
  days-hot cutoff (`[retention] archive_after_days`, never auto).
- Cold-yet-readable; hot+cold backup pair with restore test (incl.
  pre-0.19 cold files). Operator doc `docs/store-and-retention.md`.

## v0.11.11 — §3.0 blob read path (DR-25)

- `Snapshot.read_bytes` (raw, byte-exact) + `read_source` (display) over
  reconstructed state; new `ops.ContentUnavailable` distinct from `None`
  (absent at T). Modules read whole-file; CRLF+CJK exactness proven.

## v0.11.10 — §1.10 FTS5 keyword search (DR-34)

- `search_symbols(query, top_k, raw)` over trigger-maintained
  `concepts_fts`: live-only, escaped-by-default, `[1,50]` clamp, 1s gate.
  MCP/CLI binding deferred to the surface pass.

## v0.11.9 — §1.3 framework extraction (DR-10)

- Routes are canonical `route` concepts (additive 7th kind) with
  persisted route→handler edges; `resolve` follows them; `_extract_star_exports`
  deleted. Steady state: zero tree walks via cached detection.

## v0.11.8 — step-4 surface rework (DR-17 follow-on)

- One name per operation across `CodeGraph`, `ops`, CLI, MCP
  (`callers`/`callees`, `diagnose`, `store-repair`, `open_project`;
  `reindex(full)`, `compute_embeddings(model_name)`).

## v0.11.7 — DR-25 §1.9 put-on-index

- Blob write path with DR-30 policy: content-addressed puts, digest in
  `extra.coderadar.source_blob`, cumulative `BLOB_STATS`, secret belt
  (`blob_exclude`), kill-switch `[database] store_source_blobs`.

## v0.11.6 — DR-12 clone determinism

- Canonical fps + members + total group order pre-cut; MinHash seeds
  fixed. Fresh-process byte-identical clone output pinned.

## v0.11.5 — DR-9 snapshot structural honesty

- `normalize_timestamp`, `lookup_entity_at(s)` over one reconstruct
  fold; downstream traverse walks at-T state; the rest raise
  `TemporalUnsupported`. Temporal topology reads `reconstruct` state
  (`bfs_over_state`), never loader walks.

## v0.11.4 — DR-25 macrame-db 0.18 → 0.19

- Registry bump with co-aligned libsql; blob put/get round-trip proven
  (64 lowercase hex, absent-not-error). DR-30 decided
  (default-on-with-notice + kill-switch).

## v0.11.3 — DR-31 staleness by content

- `.coderadar/indexed_config.json` sidecar; config changes force full
  reindex by content comparison (mtime defeated by store touches).

## v0.11.2 — DR-14 suffix winners

- Same-language-first smallest-id via `pick_suffix_winner`; stable-wrong
  possible and logged.

## v0.11.1 — DR-29 residue

- One-liner corrections (resolve header, stale artifacts).

## v0.11.0 — shared ops layer

- One name per operation on MCP, CLI, and the Python API behind the
  shared `ops` seam; v0.12 improvement plan published.
- CLI commands for every operation (`--format json`, stderr + exit 2/1,
  dry-run-unless-`--apply`); `-C/--project` with marker walk-up;
  `status` reports project, config, store, freshness.
- Renamed spellings warn (removal in 0.13): `analyze`/`rebuild` →
  `reindex`, `codegraph_*` tools → `coderadar_*`, `find` → `node`,
  `callers_of`/`callees_of` → `callers`/`callees`.

## v0.10.0 — precision

- Findings you can act on: dead-code 269 → 121 on the self corpus,
  extraction recall ~85 % → 99.97 %, resolution 100/100 held.
- `self`/typed-local/ctor calls resolve; relative imports, aliases,
  local imports, initializers produce edges.
- Dead code: external-base overrides and `Protocol` members are entry
  points; framework packs table-driven opt-in; findings carry evidence.
- Query language: `methods`/`constants`/`entities`, starts/ends-with,
  "did you mean" on unknown fields; reference generated from schema.
- Rename rewrites attribute call sites; unresolvable sites reported as
  `unverified_sites`. Canonical ids + `store-repair` migration path.

## v0.9.1–v0.9.2 — platform + lint

- Windows 8.3 short-path aliasing fixed (`canonical_file_form`);
  ruff backlog to zero (pinned, blocking); 41 query files, one per
  Tier-1/2 language.

## v0.8.15–v0.9.0 — dogfood round 2

- All CLI commands + MCP tools green (104/118 → 121/121), each red a
  tracked finding with a battery anchor.
- External callees visible; transitive re-export chains; rename heals
  the whole chain; synthetic edges never persist as CALLS.
- Strict surfaces: stderr logging, machine-readable stdout, `json`
  formats, validated env knobs. macrame-db 0.15 → 0.17.

## v0.8.1–v0.8.14 — dogfood batch

- Mutation-engine P0s fixed at root (clone panic, allow-list anchoring,
  brace-language splices, stale-plan healing, friendly errors).
- One canonical id form; one shared exclude matcher; star-export pass
  stops rglobbing `.venv` (17.6 s → 0.16 s).
- `store-repair`, `init --force` ledger rebuild, single version source.

## v0.8.0 — ledger cold start + agent UX

- `load_snapshot` replays the ledger (sub-second vs 10–15 s analyze);
  `build_graph` loads + refreshes stale files only; last project
  persists across `mcp serve` restarts.
- Multi-token search with OR semantics; `create_entity` full signatures;
  textual call-site backstop for rename/signature plans; OS-native id
  storage with forgiving display forms. macrame-db 0.18 (schema v21).

## v0.7.3–v0.7.18 — fossil-mcp detector port

- Re-derived detector suite: write-path integrity, confidence tiers +
  strictness profiles, dead code, MinHash/LSH/TED clones, scaffolding +
  secrets, CFG metrics, harmonic centrality, RTA-lite dispatch.
- Smell engine 9 → 12 rules; golden tests per stage.

## v0.7.2 — runtime project switching

- `set_project` re-runs startup against a new root mid-connection;
  `project_path` accepts files/subdirs via marker walk-up; refusals
  name the way out; mutation confinement follows the switched root.

## v0.7.0 — correctness pass

- Write path that works (span-verified rename, reachable class rename,
  FFI-boundary policy); unified-diff previews; ledger retirement +
  scoped persists; bulk writes; GIL released in analyze/update_file.
- Visualizers read real indexes (empty graph errors); exit codes fixed;
  all Tier-1 languages get real signatures and keywords; ~100 inert
  config knobs removed; ~4,300 dead lines retired.

## v0.6.6 — base resolution + honesty

- Language-family base filtering, import-aware bases, `@/`→`src/`
  aliases; `traverse_unresolved` + `unverified_sites` warnings;
  valid_from sentinel fix; smell golden snapshots.

## v0.6.5 — native smell engine

- 9 structural smells with severity tiers; AST metrics pass (no
  re-parse); class fields populated; native 4-kind `traverse`;
  subclasses/importers/overrides backfill; cross-file MRO.

## v0.6.4 — query engine fixed

- WHERE clauses match; `and`/`or` folds rewritten; `imports` by
  `target_kind`; traverse edge filter; anonymous functions skipped.

## v0.6.3 — mutation safety hardened

- Stale-write content hashes (`RejectedStale`); rollback on tainted
  updates; process-wide write guard; language-aware `create_entity`.

## v0.6.0 — full MCP surface

- 17 MCP tools; embeddings pipeline (compute/store/search_similar,
  BGE-small, xxHash dedup); plan-review-apply mutations; 13 framework
  resolvers; 41 languages across 3 tiers.

## v0.5.6–v0.5.7 — resolvers + languages

- Framework resolvers 9 → 13 (Rails, NestJS, Vue/ React Router);
  +10 languages (28 total); natural-language QueryPlanner.

## v0.5.4 — single-pass extraction

- QueryCursor-driven single pass (37 % faster on TS); parallel
  pipeline; per-language `.scm` with compile validation; query
  caching; live file watcher; Graphviz renderer; docstring + `__all__`
  + annotation capture.
