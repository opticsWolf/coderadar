# Implementation & Bug-Fixing Plan: v1 Readiness

> Branch: `v1-gap-cleanup` (off `main` @ `8d99850`).
> Status: planning artifact. Consolidates the strategic read-back and the
> verified findings in `docs/traverse-smell-status.md` §7 into an ordered,
> dependency-aware work plan.

---

## Phase 1: Diagnostics & Documentation (Immediate, < 1 hour)

*Goal: ground the inventory in verified reality and de-risk the resolver
heuristic before implementing it.*

### 1.1 Update `docs/traverse-smell-status.md` §7

Add the four verified findings to the living checkpoint:

- **Golden-test gap** — 3 rules (`deep-nesting`, `brain-method`,
  `excessive-returns`) have zero regression coverage; `god-class` has only a
  negative/AND-boundary test (never asserts it fires).
- **Silent degradation** — `neighbors_of` and `traverse_bfs` silently skip
  unresolved/unknown entities (empty neighbors, or depth-0-only results);
  `target_class_of` maps unresolved bases to `None` in CBO.
- **Concurrency mechanism** — `GLOBAL_GRAPH` is `parking_lot::RwLock`;
  `with_graph` takes a shared read lock; `get_smells` runs the engine under
  this read lock (safe for N concurrent readers, briefly blocks writers).
- **Traversal latency** — no real-repo benchmark exists; the bench harness is
  synthetic (50/200/100 modules).

### 1.2 Resolver Lift Measurement (gate for 2.1) — ✅ done

Measured on `codegraph-main` (605 files, 1176 classes, 36 base references):

| Outcome | Count |
|---|---|
| resolved (current 2 tiers) | 28 |
| ambiguous (→ `None` today) | 4 |
| not-found (external/builtin) | 4 |

- Same-directory filter fixes: **0** of the 4 ambiguous.
- Same-top-level-package filter fixes: **0**.

The 4 ambiguous cases are disambiguable only by semantics, not by package path:
- `Base` appears 3× in the *same* directory (kernel-parity fixture: Dart/Rust/
  C++ copies) — a same-package filter cannot split same-package.
- `FakeWorker extends PoolWorker` (test pkg) with `PoolWorker` in `src/mcp`
  and `src/resolution` — neither matches the caller's package.

**Gate result: lift = 0 < 5 → SKIP 2.1.** Same-package precedence would not
move the needle on this codebase; go straight to Phase 3.

### 1.3 Update `docs/traversal-matrix.md` columns

- IMPORTS/EXTENDS/OVERRIDES edges are now persisted (since `c0bc99e`); the
  matrix's "Populated?" / "not persisted" columns are stale.
- Sync them; only `as_of` reads remain deferred.

### 1.4 Real-repo traversal latency benchmark — ✅ done (`tests/test_v1_gaps.py`)

- Added `test_real_repo_traversal_latency` (skips if `codegraph-main` absent;
  path overridable via `CODEGRAPH_MAIN` env var).
- Measured: cold-start `analyze()` = **~46s** (605 files, 7646 entities);
  depth-3 `traverse` = **0.4ms / 28 nodes**. Numbers recorded in
  `traverse-smell-status.md` §7.9.

### 1.5 Concurrent-read smoke test — ✅ done (`tests/test_v1_gaps.py`)

- Added `test_concurrent_reads`: 8 threads × 3 iters × 13 entity ids run
  `traverse` + `get_smells` concurrently; asserts exact result count (312),
  zero errors, and no deadlock (60s join timeout).
- Covers the `parking_lot::RwLock` N-reader property end-to-end through the
  FFI (`py.allow_threads` releases the GIL during BFS/engine run).
- The "writer must still acquire the write lock" half is not asserted (hard
  to schedule deterministically); its starvation concern is tracked under 2.6.

---

## Phase 2: Core Heuristics & MCP Visibility (v1 blockers)

*Goal: eliminate the top user-facing friction — silent refactoring
degradation, failed class resolution, silent traversal truncation, and the
`as_of` NotImplementedError.*

### 2.1 Base-resolution ambiguity — ✅ done (2.1a/2.1b/2.1c)

Replaces the (correctly-killed) package-path idea with three signal-aware fixes:

- **2.1a Language-family filtering — done.** `Language::same_family` (TS/JS =
  one family); `base_candidates` tier-3 filters the global fallback to the
  caller's language family. Fixes the 3 `Base` parity fixtures (ambiguous
  4 → 1 on codegraph-main).
- **2.1b Ambiguity findings — done.** `resolve_class_hierarchy` pushes
  `AmbiguousBase { class_name, base_name, candidates }` (new `ProjectedGraph`
  field) instead of silently returning `None`; `index_edge_stats` exposes
  `ambiguous_bases` count + `ambiguous_base_details` (truncated to 20).
- **2.1c Import-aware resolution — done.** `import_aware_base` reads the
  caller module's `FromImport`/`RelativeImport` names and matches against the
  resolved `Import.resolution` module; `resolve_imports` now runs *first* in
  the cascade (reordered in `analyze` + `update_file`). Unit-tested; the last
  codegraph-main case (`FakeWorker implements PoolWorker` via a relative TS
  import) stays unresolved only because `find_module_by_dotted_name` can't
  resolve `../src/...` — that is the §7.3/2.2 ceiling, not a 2.1c bug.
- **Constraint (unchanged):** keep `External`/`Builtin` bases unresolved.
- **Tests:** +3 Rust (`test_language_family_filters_base_candidates`,
  `test_ambiguous_base_emits_finding`, `test_import_aware_base_resolution`).
  203 Rust + 358 Python = **561, 0 failures**.

### 2.2 Basic Alias Awareness in `find_module_by_dotted_name` — ✅ done

- **Alias normalization:** `@/...` and `~/...` → `src/...` (common Vite/Next/
  tsconfig convention) before suffix matching. No config parsing (post-v1).
- **Root-cause fix (the real blocker):** TS/JS `import { X } from '...'` was
  misclassified as `ModuleImport { module: "" }`, losing both the module and
  the names — so `import_aware_base` had nothing to key on, and the empty
  module "resolved" to an arbitrary `.ts` file. `parse_import_statement` now
  parses the `string` source + `import_clause`/`named_imports`/`import_specifier`
  (incl. `type X` and default imports), producing a proper `FromImport`/
  `RelativeImport` (with a slash-trim after the leading dots).
- **Result:** ambiguous bases on codegraph-main **4 → 0** (the `FakeWorker
  implements PoolWorker` case now resolves via its `type` import).
- **Side effect (correctness):** `resolved_imports` dropped 1968 → 884 because
  the old empty-module bug counted every TS named import as "resolved" to an
  arbitrary module; they now resolve correctly or stay `Unresolved`.
- **Defer:** full `tsconfig.json`/`pyproject.toml` path-map parsing → post-v1.
- **Tests:** `test_alias_aware_module_resolution`,
  `test_ts_typeonly_import_aware_base_resolution`.
  205 Rust + 358 Python = **563, 0 failures**.

### 2.3 Traversal Degradation Visibility — ✅ done

- Added `CodeGraph::count_unresolved_targets` (counts `External` + `Unresolved`
  calls and `Unresolved` imports — downstream only; upstream reverse indexes
  are complete). `Builtin` is excluded (ubiquitous, expected).
- Added `traverse_unresolved(start_id, max_depth, edge_kinds, direction)`
  pyfunction (mirrors `traverse`'s BFS, returns the skipped-target count).
- `_traverse` (server.py) now appends
  `⚠️ Traversal incomplete: N outgoing target(s) could not be resolved and
  were excluded from the walk.` when N > 0.
- Tests: `test_count_unresolved_targets` (Rust) +
  `test_traverse_unresolved_counts` (Python). 206 Rust + 359 Python = **565, 0 failures**.

### 2.4 MCP `unverified_sites` Loud Warning — ✅ done

- `_format_mutation_plan` (dry-run) and `_format_mutation_applied` (apply) now
  emit `⚠️ **WARNING: N call site(s) could not be verified/rewritten. Manual
  review required.**` when `unverified_sites` is non-empty.
- The apply path previously **dropped** unverified sites entirely (only the
  dry-run showed them); all 4 mutation tools (`replace_body`,
  `update_signature`, `rename`, `create_entity`) now pass
  `plan.unverified_sites` through to the applied-result renderer.
- Test: `test_unverified_sites_warning`. 206 Rust + 360 Python = **566, 0 failures**.

### 2.5 `as_of` Traversal Adapter & Binding — ✅ done

- **Blocking bugs found + fixed:** `persist_edges` asserted edges with
  `valid_from = TS_OPEN` (the 9999-12-31 *open* sentinel meant for `valid_to`),
  and `build_concept`'s inline date math double-added the 719468 epoch offset
  (every timestamp was ~year 5910). Both fixed — `now_iso8601()` now produces a
  correct UTC timestamp, used for concepts AND edges.
- `CodeGraphStore::traverse_at(start, depth, edge_types, ts)` added (the old
  `traverse` now delegates with `"now"`).
- `traverse(as_of=<ts>)` now routes **downstream** traversals to Macrame's
  `load_subgraph_with(ts)`; edge kinds map `calls→CALLS`, `imports→IMPORTS`,
  `extends→EXTENDS`, `overrides→OVERRIDES`. Upstream/both `as_of` raises
  `NotImplementedError` (Macrame's builder is out-edge-only).
- `subgraph_bfs` recomputes BFS depth from the `Subgraph` (which stores
  topology + edge types but not depth).
- Round-trip test: analyze (a→b), capture ts, mutate (a→c), re-analyze, then
  `as_of(ts)` returns b but NOT c. 206 Rust + 361 Python = **567, 0 failures**.
- **Follow-up (review Item 7):** the `as_of` path now snapshots the
  `Arc<ProjectedGraph>` + `Arc<CodeGraphStore>` under the read guard and
  releases it BEFORE `traverse_at` — mirroring 2.6, so a slow DB traversal no
  longer blocks a writer. Also added `test_as_of_upstream_and_both_rejected`.

### 2.6 Release the `get_smells` read lock before engine run — ✅ done

- Added `with_graph_snapshot` (clones the `Arc<ProjectedGraph>` under the read
  guard, then drops the guard BEFORE running the closure).
- `get_smells` now uses it — the smell engine runs on the owned snapshot with
  the `GLOBAL_GRAPH` read lock released, so an in-flight smell run no longer
  blocks a writer (`reindex`/`update_file`).
- Output unchanged; covered by the existing `test_concurrent_reads` (1.5) +
  `test_smells.py`. 206 Rust + 361 Python = **567, 0 failures**.
- **Follow-up (review test gap):** `test_get_smells_releases_read_lock_for_writer`
  races `analyze` (writer) against an in-flight engine run on a synthetic
  4000-class graph and asserts the writer completes while the engine is still
  running — a direct regression guard for the pre-2.6 deadlock/starve bug.
  (Also renamed `ts_open` → `ts_now`, `macrame_dir` → `macrame_direction`.)

### 2.7 Populate `class.methods` (derived denormalization) — ✅ done

- Added `CodeGraph::populate_class_methods` — derives `Class.methods` from
  `projection.functions` grouped by `parent_class` on every resolve cascade
  (read-only denormalization; single source of truth stays `functions` +
  `parent_class`). No separate write path.
- Called in BOTH production cascades: `analyze` (lib.rs) and `update_file`
  (graph.rs) — so it runs after all fragments are merged and captures
  cross-file methods (e.g. Rust `impl` in another file), not just same-file.
- Deterministic order (methods sorted by EntityId).
- Side benefit: query `method_count` (`cls.methods.len()`) now returns real
  values instead of 0.
- Smell rules (CBO/WMC) and override detection still scan `functions`
  directly, so their output is byte-identical. Added `test_class_methods_populated_27`.
  207 Rust + 361 Python = **568, 0 failures**.

---

## Phase 3: Test & Correctness Hardening (v1 blocker) — ✅ done

*Goal: protect against the substring-approximation risk in the smell engine.*

### 3.1 Add Fixtures for Uncovered Rules — ✅ done

- `tests/fixtures/python/smells/golden/deep_nesting.py` — 4 nested `if`s.
- `tests/fixtures/python/smells/golden/brain_method.py` — `brain()` cyclomatic
  15 + `helper()` 5 → WMC 20.
- `tests/fixtures/python/smells/golden/excessive_returns.py` — 6 `return`s.
- `tests/fixtures/python/smells/golden/god_class.py` — **positive** god-class:
  4 methods × 12 `if`s (WMC 52) + 5 same-file bases (CBO 5).
  (NB: the actual god-class rule is WMC >= 47 AND CBO >= 5 — not "fields +
  methods" as the original plan text said.)

### 3.2 Upgrade Python Assertions to Golden Snapshots — ✅ done

- `tests/test_smells_golden.py` asserts exact `rule_id`, `severity`,
  `entity_name`, and `signals` for all four rules (subset match — findings
  also carry `entity_id`/`message`, which are path/format-dependent).
- Locked values:
  - deep-nesting → Medium, `{"nesting_depth": 4.0}`
  - excessive-returns → Medium, `{"return_count": 6.0}`
  - brain-method → High, `{"max_method_cyclomatic": 15.0, "WMC": 20.0}`
  - god-class → Medium, `{"WMC": 52.0, "CBO": 5.0}`

---

## Phase 4: Release Engineering

### 4.1 Publish the missing sdist — ✅ done (`8aaca76`)

- **Current state:** `.github/workflows/release.yml` already had an `sdist`
  job (`maturin sdist`) plus a `publish` job that uploads wheels + sdist.
- **Root cause of the missing v0.6.4 sdist:** `maturin sdist` produced a
  tarball without the LICENSE file (absent from `[tool.maturin] include`), so
  PyPI rejected the sdist while accepting the wheels. The fix (`4f40d1f`:
  `[tool.maturin] include = ["LICENSE"]`) landed *after* the v0.6.4 tag and
  was never re-released.
- **Done:** added a regression guard to the `sdist` job — a "Verify LICENSE
  included in sdist" step (`tar -tzf dist/*.tar.gz | grep -qE '(^|/)LICENSE'`)
  that fails the job fast and blocks `publish` if LICENSE ever drops out.
- **Remaining:** confirm the v0.6.5 tag push publishes both wheels and the
  sdist to PyPI (verify on PyPI — no further code change expected).

---

## Explicitly Deferred (do not touch for v1)

- **Snapshot I/O / cold-start (Phase 3B)** — `export_snapshot`/`load_snapshot`
  keep raising; every session re-analyzes from source.
- **Arg-span indexing** — required for a true `update_signature` call-site
  cascade; mitigated for v1 by 2.4's warning.
- **Smell-metric approximations** — substring cyclomatic + base/call CBO stay
  as-is; field-type-aware CBO deferred (tractable post-`91021ac`).
- **SQLite PRAGMAs, string interning, columnar extraction** — high-risk,
  explicitly post-v1.
