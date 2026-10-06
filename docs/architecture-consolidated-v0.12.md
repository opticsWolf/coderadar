# CodeRadar Architecture & Decisions — Consolidated v0.12

> **Date:** 2026-10-06 · **Branch:** `dev_0.12` @ `0b18a36` · **Code:** v0.11.0, `macrame-db 0.18`
> **Consolidates:** graph `coderadar-architecture-review`, graph `coderadar-decision-register`,
> `docs/architecture-review-amendment-1.md`, `docs/v0.12-re-review-amendment-2.md` §§A8–A9/A14–A16.
> **Status:** this file is the authoritative merged view. The four sources above are frozen history;
> where they disagree, this file wins. Open work is marked **open**; closed/carried/residue as stated.
> **Method:** every factual claim below was re-checked against the working tree on the baseline above
> (file:line citations are the check, not the plan text).

## 0. Verification header (today's code truth)

| Claim | Code check 2026-10-06 | Result |
|---|---|---|
| Version | `pyproject.toml:7` + `Cargo.toml:6` = `0.11.0`; `core_indexer/Cargo.toml:28` `macrame-db = "0.18"`, `Cargo.lock` 0.18.0 | v0.12 not started; no bump commit landed |
| `lib.rs` size/exports | 4,197 lines; 47 `#[pyfunction]`, 1 `#[pymethods]` block (`lib.rs:190`) | review "48 py fns" corrected to 47+methods; god-module verdict stands |
| `Snapshot` dishonest | `__init__.py:980` class; `query:997`, `callers:1006`, `callees:1010`, `traverse:1014` all delegate to `self._graph`; no `find` on `Snapshot` (`find` lives on `CodeGraph:469`) | D9 stands, wording tightened: `ops.as_of` does not call a missing method — it `getattr(snapshot,"find",None)` (`ops.py:818`) and gets `None`, so every symbol is `None` |
| `ops.as_of` / `read_source` | `ops.py:792` validates with `datetime.fromisoformat` (accepts naive/offset); `read_source:485` opens today's disk UTF-8 `errors="replace"`, slices lines, prefixes line numbers | byte-history cannot go through `read_source` unchanged (see §5) |
| `extra` shape | `storage.rs:772` `coderadar_extra` → `{"coderadar":{"format":1,"kind","file_path","content_hash"}}`; content `meta_version:2` (`storage.rs:925,956`) is a different version | `extra["source_blob"]` corrected to `extra.coderadar.source_blob`; `format` vs `meta_version` must not be conflated |
| Clones determinism | `clones/mod.rs:161-163,178-191,248,265,359`, `lsh_index.rs:12,26,46`; final sort only size+similarity (`mod.rs:359-412` sort); FFI `max_groups` cut at `lib.rs:3096` | D13 stands; `(file,start_line)` + `(size,first-id)` is not a total order |
| Stale header | `resolve/mod.rs:1-3` — line 1 fine, lines 2-3 stale ("Five-layer cascade: Stack Graphs → …") | one edit covers lines 2-3 |
| WAL tombstone | `core_indexer/src/update/wal.rs` 5 comment lines; no `mod update` in `lib.rs` | verified uncompiled, no action |
| Stale `.pyc` | `py_agent/src/coderadar/__pycache__/flatbuffer.cpython-313.pyc` present on disk | delete; untracked hygiene |
| Own allow-list | `.coderadar.toml` `[mutation] allow = ["src/","lib/","tests/","scripts/"]`, repo has none of `src/ lib/ scripts/`; `default_dry_run = true` | D18 stands + dry-run note |
| Deny precedence | `deny = [".git/",".coderadar/","/migrations/","/*.lock","/generated/"]` | deny-wins must be stated once D18 derives defaults |
| `UpdateReport` blob fields | `__init__.py:104` + `types.rs:1150` have no `blob_skipped_oversize` | oversize reporting needs a new report field on every surface |
| Excludes baseline | Rust `DEFAULT_EXCLUDES` (`lib.rs:534`) + `FALLBACK_BASELINE` (`excludes.py:30`) name venv/node/target/build/pycache/git/coderadar — no `.env`/secret patterns | blob-by-default needs DR-30 policy first |
| Branch use | no `branch`/`Branch` use in `storage.rs`/`lib.rs`/facade except comments/git rev | CodeRadar lives on the trunk; blobs branch-global is correct for now |
| Spans | `types.rs:133` `ByteSpan{start,end}` byte offsets, exclusive end | historical slicing must use raw bytes + these spans |

## 1. Design lineage (why the code looks like this)

Five-deep lineage; code follows the amendments, not v3.3:

0. `specs/Codegraph engine 3.2.1.md` — CodeGraph origin (flat-buffer pipeline).
1. `specs/design_file.md` (v2) — two-pass tagging+walker, WAL, C3 MRO, Pest grammar.
2. `specs/CodeRadar v3.3 Consolidated.md` — the nominal spec (5-layer cascade L1 Stack Graphs §6.2 / L5 LSP §6.5/§14, LadybugDB+Cypher §7.3, flat-buffer FFI §8, MVCC §9.1, WAL §5.5, MutationLog §11.8). **Superseded in practice.**
3. `specs/CodeRadar v3.4 Amendment.md` — the great simplification: A1–A3 remove MVCC; A4–A7/A16 Macrame replaces LadybugDB; A8 flat-buffer contract; A9 partial coverage; A10 query adaptation; A11 agent/MCP surface.
4. `specs/CodeRadar v3.5 Consolidated.md` — true code baseline (initial commit `a802add` tagged spec v3.5).
5. `docs/v3.6-consolidated.md` + plan + code-review — tree-sitter-language-pack, 41 `.scm` files, noise filtering, 4-layer cascade; Stack Graphs + LSP formally deferred (`a4df088`, `b922a8d`).

Era history (379 commits `a802add`→`0b18a36`): v0.1–0.2 spec-v3.5 model + `efd7c0a` (Cypher/flatbuffers cut, MacrameQuery replaces templates); v0.3 languages + persistence; v0.4–0.5 CodeGraph-informed single-pass + 13 framework resolvers; v0.6 safety (WriteGuard, xxh3, smell engine, Module concepts, graph-split); v0.7 write-path truth + fossil-mcp port + fiction removal; v0.8 ledger cold start (`da185b8` Concept JSON v2) + dogfood F/R2; v0.9 release; v0.10 precision (85%→99.97% recall); v0.11 one-name surface (`9cbac41`+`337a17d` shared `ops` layer).

Meta-lessons kept: dogfooding finds what unit tests cannot; the project deletes boldly (~4,300 lines in v0.7 §4).

## 2. Current architecture (as built, corrected)

```
Python (13,958 lines): CLI → ops → CodeGraph facade → PyO3 → Rust core
                              ↘ render (text)   ↘ MCP server (stdio, background index)
Rust (38,115 lines): extract → resolve → graph → query / mutation / smells / clones
Macrame 0.18 (schema v21): bitemporal ledger, downstream-only traversal
```

CLI `cli.py` 1,574 (per-op commands + `--format json`; `init` runs framework + star-export passes that die with the process). MCP `mcp/server.py` 1,472 (+roots/lifecycle). Facade `__init__.py` 1,439 (`CodeGraph` + dishonest `Snapshot` + coldstart). Ops `ops.py` 1,161 (23 ops; `set_project` absent by comment `ops.py:74-78`). Render `render.py` 923 (MCP-flavoured on CLI). 13 framework resolvers. FFI `lib.rs` 4,197 (bindings + 7 converters + git + watcher + config + synthetics — god module vs App. E). Extract (single-pass cursor + retained tagger/walker hybrid). Resolve orchestrator 816 + `graph/resolve_calls.rs` 775. Graph 13 files. Query exec 1,610 + grammar 704 + schema 669 (Pest + Macrame traversals + in-memory cosine). Mutation mod 3,712 + edit/indent/write_guard. Smells engine 568 + 12 rules. Clones mod 896 + apted/lsh/minhash/tokens. Storage `storage.rs` 2,528 (Macrame wrapper, Concept JSON v2, sanitize-at-boundary, FK safety). Types `types.rs` 2,026. Unwired `lsp/` ~400, `agent/` ~300, `mutation/tool_router.py` 215 (test-only).

Live self-index shape (dev_0.12): ~245 files / 245 modules / 468 classes / 2,981 functions / 1,668 imports / 5,477 call edges; reproduces the stale-retirement anomaly (11 stale edges on an unchanged tree).

## 3. Deviations D1–D20 (merged, with standing)

- **D1 Stack Graphs** — specified, stubbed, deleted (`a4df088`→`f73f711`); `ResolutionMethod::StackGraph` kept as stored vocabulary. *Residue:* header `resolve/mod.rs:2-3`. Fix: one edit, P4.
- **D2 LSP L5** — never wired; pool exists, no production path; config accepts `[resolution.lsp]` (undocumented). README honest. *Carried; wire-or-cut in P4.*
- **D3 Ladybug+Cypher → Macrame** — cut in `efd7c0a`; headers state "No Cypher — Macrame IS the API". Follow-on: Macrame 0.18 vector API unused (vectors in-process `EmbeddingVec`, die on restart). *Intended, sequenced (Phase 1 in v0.12, persistence 0.13).*
- **D4 Flat-buffer FFI → PyO3 dicts** — built then removed in same cut. *Residue:* stale `flatbuffer` `.pyc` on disk. Delete, P4.
- **D5 MVCC → AtomicU64 epoch + locks** — v3.4 A1–A3; GIL released in analyze/update_file. *Residue:* delete-drop reports fabricated `epoch_before/after 0/0` (fix in §1.7). Architecture closed.
- **D6 WAL → tombstone + 2 real mechanisms** — `update/wal.rs` 5 lines, uncompiled; atomicity from backup→write→post-parse→restore + Macrame assertions. *Closed.*
- **D7 MutationLog** — never existed; `mutations` CLI removed. *Closed by deletion.*
- **D8 Two-pass → single-pass (hybrid residue)** — cursor drives emission ("37% faster"), 41 `.scm` + cache; tagger+walker retained. *Works (99.97% recall); layering murkier than documented. No v0.12 action.*
- **D9 Snapshot/as_of (P0)** — timestamp stored, never used; all `Snapshot` methods delegate to live graph; `ops.as_of` gets `find=None` and returns all-`None`. Only `_core.traverse(as_of=)` downstream is real (upstream raises). *Open P0; fix = §0.1 (recorded-time reconstruct lookup + raise where unsupported) + §3.0 (bytes).*
- **D10 Framework edges (P1)** — `cli.py:348` init-only, lost at exit; MCP never runs; store path exists but nothing persists from CLI. Same shape for `_extract_star_exports:354`. *Open P1; move into `coldstart.build_graph`/`analyze` + persist + count, or remove + retract README.*
- **D11 Unified surface, unfinished (P1)** — `render.py` still names `coderadar_*` on CLI; `callers/callees` bypass `ops` via `MacrameQuery` without `--format json`; `err_console` binds stderr at import; `shell` not op-backed; `set_project` has no Python form. *Open P1, sequenced as surface step.*
- **D12 Embedding holes (P3)** — signature-or-name only, silent 10k-per-kind cap, no model id in dedup key, no dim error, index/query model can disagree. *Open, scoped Phase 1.*
- **D13 Clone nondeterminism (P0)** — HashMap/HashSet iteration into grouping + `max_groups` cut; final sort not total. *Open P0; acceptance = total order at every boundary + byte-identical JSON incl. `max_groups` truncation (see §5).*
- **D14 Rust cross-module → `external::` (P0)** — `crate::/super::/self::` + `use` aliases unmapped. *Open P0, largest engine item with D10.*
- **D15 Stale retirements (P0)** — 236+46 on 0.11.0, 11 edges reproduced dev_0.12 on unchanged tree. *Open P0, diagnose-first, first in sequencing.*
- **D16 Config 100→2** — ~100 inert keys cut (`4958d7b`), header rule kept; carried `[resolution.signature]` weights + `[resolution.lsp]`. *Carried, documented in-file.*
- **D17 Test-only packages** — `agent/ lsp/ tool_router.py` only via `tests/test_python_layer.py`. *Open hygiene; wire-or-cut.*
- **D18 Self-hosting gap (P4)** — own `allow` matches none of own dirs; plus `default_dry_run=true` means even a fixed list would not mutate by default; deny-wins must be stated. *Open (DR-27).*
- **D19 FFI god module (P4)** — 4,197 lines vs App. E. *Open (DR-28), mechanical split.*
- **D20 Performance** — 0.39× self win / 1.75× 558-file TS loss, cause named. *Tracked, out of v0.12 except §3.2.*

## 4. Decision register DR-1–DR-30 (merged)

DR-1–DR-24 unchanged from the graph register (closed/carried/open as stored; P0s DR-9/12/13/14 open; DR-11 carried-scoped; DR-10 open P1). The amendments' proposals are **accepted here as open rows** (this resolves re-review A8 — no longer "proposed", but still without landing hashes):

| ID | Decision | Standing |
|---|---|---|
| DR-25 | Source history in Macrame-0.19 content-addressed blobs; digest at `extra.coderadar.source_blob` on the file module concept; `validate_digest` (64 lowercase hex) at the boundary; diffs computed on read, never stored; retention via the archive reference scan; raw-bytes-at-recorded-time + explicit unavailable/missing/invalid outcomes; PyO3 round-trip required | **part-landed** v0.11.4 (§0.6 surface) + v0.11.7 (§1.9 write path: put-then-assert on analyze/update/watcher, kill-switch + secret belt per DR-30, `blob_stats()` notice on every report; §3.0 reads + §3.4 retention remain) |
| DR-26 | Version policy: +0.0.1 per implemented feature (`pyproject.toml` + workspace `Cargo.toml` together, one bump commit, named in the feature's DR row); patches accumulate to 0.12.0 | **open**, process-active now |
| DR-27 | Self-hosting allow-list: derive or fix `[mutation] allow` to own dirs; state deny-wins; keep `default_dry_run` decision explicit (recommend keep `true` until D18 lands) | **open** (was P-A; P4/D18) |
| DR-28 | Split `lib.rs` per App. E (bindings + converters + git/watcher/config/synthetics out) | **open** (was P-B; P4/D19) |
| DR-29 | Residue: `resolve/mod.rs:2-3` header + delete stale flatbuffer `.pyc` | **open** (was P-C; P4 first) |
| DR-30 | Source-blob security/retention: opt-in vs default-on-with-notice; at-rest coverage for hot DB, cold archive, backups/exports; permissions, retention, deletion/erasure; secret-sentinel fixture | **open** (blocks default blob writes) |
| DR-34 | FTS5 keyword search over concept text as the find-symbols-mentioning-X surface (§1.10): `macrame::vector::keyword_search` + `escape_fts5_query` over existing v2 JSON, escaped-by-default with opt-in raw, live-only, bodies stay in blobs; ops contract now, MCP/CLI binding in step 4 | **proposed** (spike-proven 2026-10-06; number skips graph-held DR-31–33) |

Process rule (DR-26 + plan gate): no item is done until its DR row carries status + commit hash in the same session; the release gate reads the register, not memory.

## 5. Tightened acceptance (what Amendment 2 added, kept here)

- **Extra path:** `extra.coderadar.source_blob`, module concept only; declare whether `coderadar.format` stays 1 or advances; legacy-vs-new extra + ledger-reconstruction tests; archive scan must see the nested digest.
- **History axis:** code-history means **recorded time** (what CodeRadar had learned at T) via `reconstruct(T)`; valid-time is a different question. Normalize inputs to Macrame canonical UTC or reject as `InvalidRequest`; surface `predates_recorded_history` distinctly. `§0.1` splits into (a) structural honesty landable on 0.18 and (b) byte reads waiting on §0.6+§1.9+§3.0.
- **Byte fidelity:** new raw-byte path (digest-at-T → `blob_get` → slice stored `ByteSpan`s) separate from display formatting; explicit API + errors for missing body / bad digest / bad span; CRLF + non-ASCII byte-equality tests.
- **Oversize/partial writes:** one consistent outcome for analyze/reindex/update_file/watcher when over the blob cap (graph kept, body unavailable, typed count on every report surface); put-bytes-before-assert-digest ordering; failure-injection between put and assertion proving idempotent retry and no live-digest-to-absent-bytes; orphan fate documented/bounded.
- **Backup:** quiesced/atomic hot+cold (+snapshots/checkpoint) procedure + restore test (every retained digest `blob_get`s expected bytes); include pre-0.19 cold (no `blobs` table → absent, not error) and newer cold with blobs.
- **Clones:** total key `(file, span.start, span.end, entity_id)` for fingerprints, canonical member/instance order, ordered bucket keys + candidate pairs, final sort by size/similarity then type + canonical member IDs; multi-seed + tie + `max_groups` JSON-bytes tests.

## 6. Net assessment (unchanged)

Smaller in concept than the spec, larger in value: removals have rationale commits or loud refusals; additions (smells, clones, scaffold/secrets, CFG, centrality, RTA-lite, resolvers, 41 languages, ledger cold start, one-name surface) are test-pinned. Remaining dishonesty is enumerated (D9/D10/D13/D15 = v0.12 P0 set) plus hygiene D18/D19/residue. macrame-db 0.19.0 is a registry bump (no git dependency); `libsql` pin moves with Macrame's.
