# Open Items

> **Date:** 2026-08-23 · **Baseline:** v0.6.47 (`93b82f9`, `dev`)
> **Purpose:** one reconciled list of everything the four planning artifacts
> left open, plus everything the v0.7 release smoke-check found and did not fix.
> This is the input to [`v0.8-improvement-plan.md`](v0.8-improvement-plan.md).

Every line below was checked against the code on `dev` at `93b82f9`, not
against the plan text that proposed it. Items the plans list as deferred but
which have since been closed are recorded in §5 so they are not re-planned.

Sources reconciled:

| Document | Status |
|---|---|
| [`v0.7-improvement-plan.md`](v0.7-improvement-plan.md) | **Complete** — 26/26 tasks, Phases 0–6. Its own "out of scope" list survives into §2. |
| [`v1-readiness-plan.md`](v1-readiness-plan.md) | **Complete** — Phases 1–4 all ✅. Its "Explicitly Deferred" block survives into §2. |
| [`graph-split-plan.md`](graph-split-plan.md) | **Complete** — `graph.rs` is now 13 files + `tests/`, 3,037 lines. Nothing open. |
| [`traversal-matrix.md`](traversal-matrix.md) | **Complete** — steps A–F landed; §5's "open decision for the maintainer" (step D, resolve backfill) was decided *for* and shipped. Two residues survive into §2. |

---

## 1. Found by the v0.7 release smoke-check — all resolved by v0.12.0

These are the items the four smoke passes (`b140cbf`, `acadb64`, `d269768`,
`93b82f9`) surfaced and deliberately did **not** act on, because each was a
scope decision rather than a bug fix. Each carries its v0.12.0 status below;
the diagnosis text stands as the record.

### 1.1 `explore()` could only follow call edges — **superseded in v0.12.0**

[`__init__.py`](../py_agent/src/coderadar/__init__.py) ·
`EDGE_KIND_CALLS = "calls"`

`CodeGraph.explore(start_id, direction, max_depth, edge_kinds)` BFSes over
`_get_incoming`/`_get_outgoing`, which are `callers_of`/`callees_of` — one
edge kind. Asking for `imports`, `extends`, or `overrides` now returns `[]`
rather than silently returning call edges labelled as imports, which is
correct but not useful.

Meanwhile the Rust `traverse` binding handles all four kinds, backed by real
indexes since the `traversal-matrix.md` step-D backfill. The facade simply
does not call it.

**Shape of the fix:** route `explore` through `_core.traverse` and delete the
Python BFS, the same move `traversal-matrix.md` step B made for `executor.py`.
`explore` is one of the four primary MCP tools, so this is agent-visible.

> **Status (v0.12.0):** modern `explore` returns names/files/relationships
> via `ops.explore`; the old call-graph walk survives only as the deprecated
> `explore(start_id=…)` path pointing at `traverse`. No action.

### 1.2 Every CLI invocation re-indexes the project

[`cli.py`](../py_agent/src/coderadar/cli.py) · `_ensure_graph`

The graph lives only in the process that built it. `_ensure_graph` was added
in `b140cbf` so that `coderadar stats` after `coderadar init` answers instead
of saying "No graph loaded — run coderadar init first", advice the user had
just taken. It answers by **re-analyzing from source on every invocation**.

On the measured real-repo baseline (`v1-readiness-plan.md` §1.4:
`codegraph-main`, 605 files) a cold `analyze()` is **~46 s**. So
`coderadar query` on a large project now costs 46 seconds instead of failing
in 50 ms. That is the right trade for correctness and the wrong one for use.

The real fix is cold-start from the Macrame ledger — see §2.1. `_ensure_graph`
is the interim state and should be documented as such, not left to look like
the design.

> **Status (v0.12.0): resolved.** Ledger cold start shipped (v0.8) and the
> cheap-path discipline is pinned (§3.2: stale load + changed-files-only,
> `full`/config forces re-walk). No action.

### 1.3 C and C# are the two Tier-1 languages with no signature test

[`tests/test_language_signatures.py`](../tests/test_language_signatures.py)

The README's Tier-1 table promises 12 languages. `CASES` covers 10: Python,
PHP, JavaScript, TypeScript, Kotlin, Go, Rust, Ruby, Java, C++. **C and C#
are absent.** C matters most: `93b82f9`'s finding was precisely that C and C++
hang the parameter list off the declarator chain rather than the function
node, so C is the sibling of the case that broke, tested only by proxy.

> **Status (v0.12.0): resolved.** `tests/test_language_signatures.py`
> `CASES` covers all 12 Tier-1 languages including `s.c` and `s.cs`.
> No action.

### 1.4 Three packages are reachable only from tests

| Path | Lines | Only importer |
|---|---|---|
| `py_agent/src/coderadar/lsp/` | ~400 | `tests/test_python_layer.py:482–533` |
| `py_agent/src/coderadar/agent/` | ~300 | `tests/test_python_layer.py:124` |
| `py_agent/src/coderadar/mutation/tool_router.py` | 215 | `mutation/__init__.py` re-export + `tests/test_python_layer.py:341–437` |

No production path reaches any of them. `tool_router.py` was already named in
`v0.7-improvement-plan.md` §4 ("either wire it to the real facade or delete it
and the tests") and was the one §4 item not resolved.

`lsp/` was the one the README sold — the resolution cascade listed it as
**L5, confidence 1.00, "optional, disabled by default"**, which reads as
shipped-but-off rather than absent. **Resolved before the v0.7.0 tag:** the row
now reads "**Planned** — not wired into the cascade", with a note that L1–L4
are what run. The code stays and the claim goes, so the remaining question for
`lsp/` is ordinary scheduling rather than a documentation gap.

`agent/` and `tool_router.py` are still open on cut-or-wire, and neither is
sold in the README.

> **Status (v0.12.0): decided (DR-16).** All three kept as named seams
> with wiring triggers in their headers. No action.

This is the same call §3 and §4 of the v0.7 plan made twice already (~100 inert
config knobs, ~4,300 lines of dead code, the `mutations` CLI command): **wire
it or cut it.** Tests that exercise only a package's `graph=None` path are what
kept all three alive.

### 1.5 No CHANGELOG

`git log` and the README's highlights sections are doing that job. The project
publishes to PyPI as `coderadar-rs`; the four smoke-pass commits between
0.6.43 and 0.6.47 changed user-visible CLI behaviour (`mutations` removed,
`export`/`update` exit codes, visualizers now failing where they used to draw)
and a consumer has no single place to read that.

> **Status (v0.12.0): resolved.** `CHANGELOG.md` exists and carries 0.12.0.
> No action.

---

## 2. Deferred by the plans, still deferred (except §2.1, landed)

Carried forward verbatim in intent; re-verified against v0.12.0 — §2.1 landed,
the rest still open.

### 2.1 Cold-start from the Macrame ledger — Phase 3B **(done in v0.8/v0.12)**

*(`v0.7-improvement-plan.md` "out of scope"; `v1-readiness-plan.md`
"Explicitly Deferred"; `traversal-matrix.md` §3)*

Pre-v0.8, `export_snapshot`/`load_snapshot`/`load()` raised
`PyNotImplementedError` (honest, CLI exit 1). Since v0.8 `load_snapshot`
replays the ledger and `coderadar.coldstart.build_graph` loads-then-refreshes;
§3.2 pins the discipline. `export_snapshot` remains unimplemented by design
(snapshots are digests, never bytes — see `store-and-retention.md`).

**This was the highest-value open item**, because §1.2 made it load-bearing.
Landed: ledger cold start (v0.8 `da185b8`) + §3.2 read-path discipline pinned
in v0.12. No action.

### 2.2 `as_of` upstream and bidirectional traversal

*(`v1-readiness-plan.md` §2.5)*

Pre-v0.12, `direction` in `{"in", "both"}` with an `as_of` timestamp raised
(`test_as_of_upstream_and_both_rejected`) — Macrame's builder was out-edge-only
and downstream `as_of` was round-trip tested but upstream was a hole.

> **Status (v0.12.0): decided, not built (§3.3 NO-GO, DR-9).** Temporal
> topology reads the reconstructed state (`bfs_over_state`); `query` and
> upstream temporal walks raise `TemporalUnsupported` honestly instead of
> answering from the live graph. Re-check triggers: first agent trace blocked
> on callers-at-T, or Macrame reverse-edge support. No action.

### 2.3 Arg-span indexing for the `update_signature` call-site cascade

*(`v1-readiness-plan.md` "Explicitly Deferred")*

Mitigated for v1 by §2.4's `unverified_sites` warning, and further by v0.7
§0.1's byte-span verification. Still the reason `update_signature` cannot
rewrite its own call sites.

> **Status (dev_0.13): landed in four slices.** `UnresolvedRef.args_span`
> indexed at extraction (`walker`/`single_pass` via `call_args_span`),
> persisted as function-concept `call_sites` (parallel to
> `resolved_calls`) with cold-start rehydrate, consumed by
> `plan_signature_update` two ways: explicit `call_site_values` become
> real edits on disk verification, and `inject_defaults` synthesizes
> `name=default` keywords for pure-addition Python signatures. Removals,
> reorderings, required params, non-Python callers and stale spans keep
> manual review. `update-file` needed no change (re-extraction carries
> spans; scoped re-resolution keeps `calls ∥ resolved_calls` aligned).
> Commits `d535cf5`, `5888633`, `37c8f0b` (+ wording `dada196`).

### 2.4 Smell-metric approximations

*(`v1-readiness-plan.md` "Explicitly Deferred")*

Substring cyclomatic counting and base/call-derived CBO stay as-is;
field-type-aware CBO deferred. Now covered by golden snapshots
(`v1-readiness-plan.md` §3.2), so the approximations are pinned rather than
drifting.

> **Status (dev): landed.** The "substring cyclomatic" premise is stale:
> Phase 4.1 computes cyclomatic/nesting/returns from AST decision points
> (`smells/metrics.rs`), not substring matching. The remaining half --
> field-type-aware CBO -- now ships: `coupling_between_objects` unions
> field-annotation type names resolved through the base tiers
> (same-module, import-aware, global-unique); stdlib names have no
> candidates and are skipped like external bases, ambiguous/self
> annotations couple to nothing. Golden pins hold (god-class fixture has
> no annotated fields; CBO stays 5.0).

### 2.5 Full `tsconfig.json` / `pyproject.toml` path-map parsing

*(`v1-readiness-plan.md` §2.2, "Defer: post-v1")*

`find_module_by_dotted_name` normalizes `@/...` and `~/...` → `src/...` by
convention. Projects with a non-conventional path map resolve by suffix match
or not at all.

> **Status (dev): slice A landed.** New `graph::path_maps` module:
> `PathMaps::discover(root)` reads `<root>/tsconfig.json`
> (`compilerOptions.baseUrl` + `paths`, following `extends` chains with
> cycle/depth guards, JSONC comments stripped) and `<root>/pyproject.toml`
> (setuptools `find.where` / `package-dir`, poetry `from`, hatch
> `packages`; only existing dirs become roots). Discovery never fails —
> missing/unparseable configs yield empty maps. Stored on the projection
> (analyze + cold-start load re-discovers, so edited configs take effect
> without re-analyze); `find_module_by_dotted_name` tries alias rewrites
> (exact, longest-prefix, tsconfig target order), then baseUrl (TS/JS
> importers) and src-roots (Python importers) before the old suffix
> fallback. Priority-only: config-less projects resolve byte-identically.
> Tests: 5 discover unit tests + 5 resolve tests (alias gap→fixed on
> fast and scan paths, target order, unmapped fallback, baseUrl anchor,
> src-root win) + end-to-end import edge; only nested-monorepo configs
> stay a documented gap. New `toml 0.8` dep (cached, offline-clean).
> Remaining: live rebuild + verify (new Python
> `test_pathmap_alias_import_edge` fails honestly pre-build), then
> Python-side docs touch-ups if the surface needs them (no API changed).
>
> (Verified live 2026-10-26 with slice A in the rebuilt `_core.pyd`:
> new Python `test_pathmap_alias_import_edge` passes — the import edge
> lands on the mapped `lib/` target, not the `aaa/` decoy; full file
> 123/123, embeddings 20/20.)
>
> Slice B (dev): nested monorepo configs. `discover` now walks the
> analyzed tree (same ignore rules as indexing, 64-config cap) and
> parses every `tsconfig.json` / `pyproject.toml` into a `ConfigScope`
> anchored to its own directory; the importer's nearest scope wins
> (root scope always exists as fallback — no ancestor fallback on a
> miss, matching TS). `extends` chains anchor inherited entries to the
> PARENT's dir. Two reorder fixes fell out: aliases now run on the raw
> specifier before the `@/`→`src/` convention (the convention consumed
> `@/x` and hid it from aliases), and configured candidates — plus the
> convention rewrite — get an exact-path tier first, so a nested
> same-tail file can't steal the resolution by id-order. Tests: nested
> discover + cross-dir extends + nearest-wins resolve + nested pyproject
> roots (+ all slice-A tests migrated to scopes). §2.5 done: no gaps
> remain. Remaining: live rebuild + verify (new Python
> `test_pathmap_nested_scope_wins` fails honestly pre-build).
>
> (Verified live 2026-10-26 with slice B in the rebuilt `_core.pyd`:
> new Python `test_pathmap_nested_scope_wins` passes — the web
> importer lands on `packages/web/src/x.ts`, the root importer on
> `src/x.ts`; full file 124/124, embeddings 20/20.)

### 2.6 Four edge kinds are asserted; nine exist

*(`traversal-matrix.md` §1.1, §2)*

[`storage.rs:40`](../core_indexer/src/storage.rs#L40) declares `CONTAINS`,
`CALLS`, `IMPORTS`, `EXTENDS`, `IMPLEMENTS`, `REFERENCES`, `DECORATES`,
`INSTANTIATES`, `OVERRIDES`. `ALL_EDGE_KINDS`
([`lib.rs:1349`](../core_indexer/src/lib.rs#L1349)) is `["calls", "extends",
"imports", "overrides"]` — the four the resolve backfill populates.
`IMPLEMENTS` is the notable absence: interface implementation is folded into
`extends`, so Java/Go/TypeScript interface relationships are indistinguishable
from class inheritance in a traversal.

> **Status (dev): slice A landed.** `resolve_class_hierarchy` partitions
> resolved bases by target declaration — interface declarations
> (`interface_declaration` grammar kind: TypeScript, Java, C#, PHP) land in
> `Class.resolved_interfaces` + the `implementers` reverse index and persist
> as IMPLEMENTS edges (keep-set and STRUCTURAL lists cover the fifth kind);
> `traverse` walks `implements` both directions. Extraction extended where
> `implements` was missed entirely (Java `super_interfaces`, C# `base_list`,
> PHP `class_interface_clause`; matched by node kind — field names are
> unreliable across grammar versions). MRO still folds (reads unresolved
> `bases`). Go/Kotlin/Protocol/Swift interfaces are now detected too
> (dev): Go `type I interface` — embedded interfaces in `interface_type`
> unfold as bases (`type_elem`-wrapped, `method_elem` excluded so
> parameter types never leak); Kotlin `interface Foo` —
> `delegation_specifier` unfolds like C# `base_list`; Python
> `class P(Protocol)` — matched on the base name. `class_declares_interface`
> (walker.rs) decides at extraction; `emit_class` records a
> `/interface`-suffixed `grammar_kind` (round-trips through concept JSON,
> so cold-start re-partitioning agrees) and `is_interface` matches the
> suffix. Java annotations are descended (`extract_decorators` recurses
> into `modifiers` for `marker_annotation`/`annotation`; C#
> `attribute_list` in the same arm), and Java `@interface` declarations
> are tagged as classes so annotation targets resolve. Remaining
> micro-gaps: Go struct embedding (`struct { I }` — promotion, not
> inheritance) and PHP `#[...]` attributes (the grammar parses the file
> as text — parser-level, out of scope). Commit `e3eb350` (+ this one).
>
> Slice C (dev): DECORATES edges. `resolve_decorators` reduces decorator
> text (`@validate`, dotted `@helpers.route("/x")`, applied, Rust `#[..]`,
> C# `[..]`) to a name and resolves through the base tiers over classes
> *and* functions (`import_target_modules` factored out of mro.rs for
> reuse); unique hits only. `Function`/`Class.resolved_decorators` +
> `decorated_by` reverse index, DECORATES ledger loop, keep-set,
> STRUCTURAL 5->6, traverse both directions. Extraction fix inside:
> `emit_class` hardcoded `decorators: Vec::new` — class decorators were
> dropped for every language. Remaining: `references`/`instantiates`
> still declared-unpopulated; Java `annotation` nodes not descended.
> (Verified live 2026-10-26 after rebuilding `_core.pyd`: new Python
> `test_traverse_decorates_kind` passes; full file 121/121.)
>
> Slice D (dev): INSTANTIATES edges. Constructor invocations already
> resolved (`ResolvedCall::Constructor`) and persisted as CALLS; slice D
> asserts the same pairs as INSTANTIATES (overlap, not partition) so
> traversal can tell calling from constructing. New `resolve_instantiations`
> pass builds the `instantiated_by` reverse index from resolved calls
> (cascade order matters: after call resolution in all four sites --
> analyze, update-file, load, test helper). Traverse both directions,
> keep-set, STRUCTURAL 6->7, kind list 6->7. Remaining: `references`
> still declared-unpopulated.
>
> (Verified live 2026-10-26 with slices C+D in the rebuilt `_core.pyd`:
> new Python `test_traverse_instantiates_kind` passes alongside the
> decorates one; full file 121/121.)
>
> Slice E (dev): REFERENCES edges. `Function.refs`/`resolved_refs`
> (function-valued references: callbacks, dispatch tables, `return
> handler`) were resolved but deliberately never persisted as CALLS.
> Slice E asserts them as REFERENCES (overlap-free by construction:
> a value reference is not a call, and the traverse test pins the
> calls arm silent). New `resolve_references` pass builds the
> `referenced_by` reverse index; traverse both directions, keep-set,
> STRUCTURAL 7->8, kind list 7->8. §2.6 is now 8 of 9 edge kinds
> asserted; only `references`-adjacent gaps (none structural) remain.
> Remaining: nothing structural -- the kind list is complete.
>
> (Verified live 2026-10-26 with slice E in the rebuilt `_core.pyd`:
> new Python `test_traverse_references_kind` passes (calls arm pinned
> silent); full file 122/122, embeddings 20/20.)

### 2.7 Stack Graphs / L1 resolution

*(`v0.7-improvement-plan.md` "out of scope")*

Correctly deferred; the placeholder module was deleted in Phase 4 and the
README documents the omission. Nothing to do — listed so it is not
rediscovered as a gap.

### 2.8 TypeScript throughput

*(`v0.7-improvement-plan.md` "out of scope" → `performance-roadmap.md`)*

Phase 2 removed the algorithmic cliffs (the O(F²) scans, the whole-projection
clone). Closing the remaining throughput gap to CodeGraph is a separate
programme.

> (2026-10-27) Fixed on dev. Root cause, found by file logging
> (abort swallows captured output): not the parser but
> `compute_c3_mro`, which recursed per base with no cycle guard —
> and the new §2.6 Kotlin `delegation_specifier` unfold fed it a
> self-base: `Comparable<SubK>` descended into the type argument
> and collected `SubK` itself (`SubK` bases were
> `[OpenBase, Drawable, Comparable, SubK]`), alternating
> SubK↔OpenBase ~2900 frames to overflow. The 0bc0498 baseline
> binary crashes on the same file (so the fixture is hostile to
> the old code too — unlocated, likely a different latent
> recursion, since the old code collected no Kotlin bases at
> all). Two fixes: (1) type-argument subtrees
> (`type_arguments`, `type_argument_list`) are never descended
> into in any base-unfold arm (Java `super_interfaces`, C#
> `base_list`, Kotlin `delegation_specifier`, Go
> `interface_type` — the C#/Java arms had the same latent
> wrong-edge bug: `Base<Foo>` collected `Foo`); (2)
> `compute_c3_mro_guarded` carries a visiting set — cycles stop
> instead of recursing (class still heads its own MRO). Regression
> pins: type-args-are-not-bases (Kotlin + C#), MRO cycle
> terminates (A↔B). Verified live: torture.kt, kernel-parity (36
> files), full __tests__ (289 files) analyze clean; previously
> crashing `test_real_repo_traversal_latency` passes; full suite
> 1124 green with nothing deselected.

### 2.9 SQLite PRAGMAs, string interning, columnar extraction

*(`v1-readiness-plan.md` "Explicitly Deferred" — "high-risk, explicitly
post-v1")*

### 2.10 Hybrid search (RRF fusion) — blocked on vector persistence (DR-11 → 0.13)

*(`docs/v0.12-architecture-implementation.md` §1.10: explicit non-goal)*

Macrame (`macrame-db-0.19.1 `vector/hybrid.rs`) ships three arms: `keyword_search`
(FTS5 BM25, wired as `search_symbols`), `search_vector` (DiskANN cosine) and
`HybridSearch` (RRF fusion, `RRF_K=60`, `depth=max(5×top_k,50)`). CodeRadar wires
only the keyword arm; the vector arm is Phase-1 in-memory cosine over
`EmbeddingVec` (`lib.rs:search_similar`), which dies on restart by design.
Fusing `keyword + vector` before vectors persist would fuse what is true now
with what vanishes on reload — the same half-applied-bound trap Macrame's
`as_of_valid` docs warn about. Land after DR-11 persistence (0.13), then fuse
with per-arm `depth` reads and deterministic `score-desc/id-asc` ordering.

> **Status (dev_0.13): landed with DR-11 slices 1–4.** Per-model tables
> (`embeddings_<model>`, DiskANN) persist vectors; `search_hybrid` fuses
> via macrame's `HybridSearch` (default depth, RRF_K=60) over the store's
> diagnostic connection, surfaced as `ops.search_hybrid` (auto-compute
> retry mirroring `search_similar`), MCP `coderadar_search_hybrid`, and
> CLI `search-hybrid`. Commits `1c84de4`, `ea948fd`, `48d9bfc` + this one.
> **Epilogue (dev): `search_similar` reads the table too.** The
> vector arm was still memory-only, so a fresh analyze forced a recompute
> before it could answer. `search_similar` now dispatches to the single
> registered model table at the query width (ambiguous widths error
> naming the tables; no table falls back to the memory scan, preserving
> storeless and never-persisted flows). Verified live 2026-10-26 after
> rebuilding `_core.pyd`: `TestSimilarOnTables` passes (no-recompute
> after re-analyze, raw vector scores from the table), full embeddings
> suite 20/20, `test_traverse_implements_kind` passes with the §2.6
> binary. Commit `67c1ba8`.

---

## 3. Engineering hygiene

### 3.1 The lint backlog, and CI's `continue-on-error`

Measured on `dev` @ `93b82f9`:

| Check | Count |
|---|---|
| `cargo clippy --package core_indexer --all-targets` | **100 warnings** |
| `uvx ruff check py_agent/src tests` | **785 errors** (633 auto-fixable) |
| `cargo fmt --all --check` | **444 diffs** |

[`ci.yml:75`](../.github/workflows/ci.yml#L75) marks the whole lint job
`continue-on-error: true`. That was deliberate — v0.7 §5.1 shipped the lint leg
as advisory so it could land without a 1,300-item cleanup blocking it — but
§5.2's intent was to flip it to blocking once the backlog was cleared, and the
backlog was never cleared.

An advisory lint job that has never once been green is a job nobody reads.

> **Status (v0.12.0): resolved (P4).** Clippy 0 (`--all-targets`, `-D warnings`),
> `cargo fmt` clean, `ruff check` clean (format not adopted), CI lint leg
> flipped to blocking. No action.

### 3.2 The test-debt pattern behind all four smoke passes

`v0.7-improvement-plan.md` §"Cross-cutting test debt" named three gaps. All
three are closed. The pattern they were instances of is not:

> **Green tests were asserting broken behaviour.**

- 12 visualizer tests asserted the fabricated demo diagrams. One was named
  `test_graphviz_fallback_without_graph` and checked for `BaseModel`.
- `coderadar.cli` was imported by **nothing** in the suite before `acadb64`.
- MCP guidance strings — the text an agent trusts most — were never checked
  against the tool registry before `d269768`.
- `extract_parameters` was tested only against Python, so eight languages'
  parameters were dropped under a green suite.

Each pass found the same shape: a surface that produced *plausible output* and
exited 0. None of them crashed. The remaining question is which surfaces still
have that property and no test — see the v0.8 plan §4.

> **Status (dev): first smoke-hunt done, pattern confirmed twice more.**
> Mapped all 24 MCP tools to test files, separating shape coverage
> (exit-0 batteries, drift/parity tests) from content assertions:
>
> - `diagnose` had exit-0-only coverage (`battery_round2` `rc == 0`) at
>   every layer — a report silently dropping its sections would print a
>   clean bill of health and pass. Now pinned: ops content
>   (`test_diagnose_reports_unresolved_and_ambiguous` — known unresolved
>   target attributed to its caller, ambiguous `Base` counted) plus the
>   rendered MCP text naming both (`TestDiagnoseContent`).
> - `find_clones` / `find_scaffolding` had exit-0-only battery coverage;
>   engine goldens existed but nothing asserted above `_core`. Now
>   pinned at ops level (both clone instances named; TODO marker found).
> - Audited clean (content assertions exist): `search_hybrid` (fused
>   ranks + ids), `explore` (the KeyError catch is documented in-file),
>   mutation tools (e2e), traverse kinds, smells/dead-code goldens,
>   staleness banner inputs.
> - Watch-list, mostly closed (dev): the `cr_edit_tests` batteries were
>   manual dogfood (no CI reference — verified) — `battery_round2.py`
>   (all 22 CLI commands + all MCP tools against self-contained
>   `r2proj`, ~90s) is now a CI `dogfood` job, gated on a new nonzero
>   exit code; it immediately paid off, catching two stale pins (tool
>   count 22→27 with the `tools/call` probe silently gated behind it,
>   and an `external::Store` anchor that same-file constructor
>   resolution had outgrown — now pins `store.ts::Store`). `render.py`
>   formatters have direct unit tests (`tests/test_render.py`, 27
>   tests, no `_core`); `callees` pinned at ops level.
>   Remaining manual: `battery_readonly` (needs the repo's own
>   gitignored store), `battery_mutation` (rewrites demos with
>   dry_run=False), `battery_slop` (synthetic option matrix,
>   unmeasured runtime); `as_of` content and MCP-text `module_children`
>   stay thin.

---

## 4. Not gaps

Recorded so they are not re-raised:

- **`graph.rs` split** — complete, all 16 steps.
- **`.coderadar.toml`** — wired (v0.7 §3); the store path contradiction, the
  dead `load_harness_config`, and the three-way embedding-model disagreement
  are all resolved to `BAAI/bge-small-en-v1.5` in one place.
- **Zero-vector embedding poisoning** — `dedup.py:_model_embed` now raises when
  `fastembed` is missing instead of storing zeros.
- **`search_entities` kind coverage** — now scans functions, classes,
  constants, modules, and imports with per-kind rank weights.
- **Dead code** — `flatbuffer.py`, `buffers.rs`, the five parallel `mcp/`
  tool modules, `update/diff.rs`, `update/patch.rs`, `executor.py`,
  `pipeline.py`, `walk_and_extract`, `stack_graph.rs`, `SmellRegistry`,
  `resolve_symbol`, and the duplicate `Watcher`/`as_of`/`callers_of`/
  `callees_of` definitions are all gone.
- **`mutations` CLI command** — removed rather than wired; `MutationLog` never
  existed.

---

## 5. Summary

| Bucket | Items | Blocking a 0.7.0 tag? |
|---|---|---|
| §1 smoke-check residue | 5 | No — all are scope decisions, none is a defect |
| §2 planned deferrals | 10 | No — deferred deliberately, honestly failing where user-visible |
| §3 hygiene | 2 | No |

Nothing in this list is a correctness defect on a documented path. The v0.7
plan's 26 tasks are closed, the v1-readiness plan's four phases are closed, the
graph split is complete, and the traversal matrix's step D shipped. The four
smoke passes converted ~10 latent defects into fixes and 46 new tests.

The list is what v0.8 is for.
