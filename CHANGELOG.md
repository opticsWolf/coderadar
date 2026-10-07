# CodeRadar Changelog

Patch releases accumulate toward 0.12.0 (+0.0.1 per implemented feature,
bump commit named in the feature's DR row). The v0.12 plan history lives
in `docs/` (v1 frozen, consolidated trio authoritative).

## Unreleased (toward 0.12.0)

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
