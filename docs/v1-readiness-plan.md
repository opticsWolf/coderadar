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

### 2.3 Traversal Degradation Visibility

- `traverse_bfs` silently skips unresolved bases, returning a truncated graph
  with no warning.
- Update the MCP traversal wrapper: when the walk is known to have unresolved
  outgoing edges (or the result set is smaller than expected), append a
  warning: `⚠️ Traversal incomplete: N targets were unresolved and excluded.`
- **Note:** may require `traverse_bfs` to return a count of skipped targets
  rather than silently filtering.

### 2.4 MCP `unverified_sites` Loud Warning

- In the `update_signature` renderer, check the `unverified_sites` count.
  If > 0, prepend/append a visible warning:
  `⚠️ WARNING: N call sites could not be verified/rewritten. Manual review required.`
- Converts silent correctness degradation into an operational signal, without
  arg-span indexing.

### 2.5 `as_of` Traversal Adapter & Binding (moved from Phase 4)

- `CodeGraphStore::traverse` exists but is unexposed; our persisted edge kinds
  (`calls`/`imports`/`extends`/`overrides`) don't map 1:1 to Macrame's
  `TraversalBuilder` semantics.
- **Action:**
  1. Write an adapter mapping our 4 edge kinds to Macrame's traversal types.
  2. Expose `traverse(as_of=<ts>)` via PyO3.
  3. Round-trip test: analyze a repo, mutate it, traverse `as_of` the original
     state, assert in-memory BFS matches persisted `as_of` results.
- **Scope:** "plumbing-with-a-test" — unblocks the temporal-read surface.

### 2.6 Release the `get_smells` read lock before engine run

- `with_graph` holds the `GLOBAL_GRAPH` read guard for the duration of the
  closure, so `get_smells` runs the engine *under* the read lock — an
  in-flight smell run briefly blocks a writer (`reindex`/`update_file`).
- Snapshot the `Arc<ProjectedGraph>`, drop the read guard, then run the
  engine on the owned clone (snapshot semantics are fine for a read).
- **Test:** covered by 1.5 — concurrent `get_smells` + a writer must not
  deadlock and the writer must not be starved.

### 2.7 Populate `class.methods` (derived denormalization)

- `class.methods` is always `vec![]` — a footgun every consumer trips on
  exactly once, and methods are looked up far more often than fields.
- Populate it in `build_fragment` as a **derived** field computed from
  `projection.functions` by `parent_class` on every build (mirroring the
  `class.fields` fix in `91021ac`). Recomputed each index/analyze/update, so
  there is no separate manual source of truth to drift — the single source
  remains `functions` + `parent_class`.
- **Do not** introduce a second write path; read-only denormalization only.
- **Test:** assert `class.methods` is non-empty for a known class, and that
  CBO / override detection / smell rules still produce identical results.

---

## Phase 3: Test & Correctness Hardening (v1 blocker)

*Goal: protect against the substring-approximation risk in the smell engine.*

### 3.1 Add Fixtures for Uncovered Rules

- `deep-nesting` — a function with 4+ levels of nested `if`/`for`.
- `brain-method` — high cyclomatic + high LOC + deep nesting.
- `excessive-returns` — a function with > 5 `return` statements.
- **Positive `god-class`** — both too many fields AND too many methods
  (complements the existing negative test).

### 3.2 Upgrade Python Assertions to Golden Snapshots

- Replace `"rule_id" in findings` presence checks with strict golden
  snapshots: exact `rule_id`, `severity`, `entity_name`, and `signals`.
- Severity values are `Info` / `Medium` / `High` / `Critical` (from
  `Severity::as_str`). Example (matches the existing fixture):
  ```python
  assert {
      "rule_id": "long-parameter-list",
      "severity": "Info",
      "entity_name": "too_many_params",
      "signals": {"param_count": 5},
  } in findings
  ```

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
