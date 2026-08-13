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

### 1.2 Resolver Lift Measurement (gate for 2.1)

Before implementing same-package precedence, measure the actual ROI on
`codegraph-main`.

- Write a diagnostic (Python via MCP, or a Rust unit test) that runs
  `resolve_base_by_name` over all classes in `codegraph-main`.
- Count classes that currently return `None` due to ambiguity. For each,
  check whether a same-package (or same-module) filter reduces the candidate
  set to exactly 1.
- **Gate:** lift < 5 resolved subclasses → skip 2.1, go straight to Phase 3.
  Lift ≥ 5 → proceed to 2.1.

### 1.3 Update `docs/traversal-matrix.md` columns

- IMPORTS/EXTENDS/OVERRIDES edges are now persisted (since `c0bc99e`); the
  matrix's "Populated?" / "not persisted" columns are stale.
- Sync them; only `as_of` reads remain deferred.

### 1.4 Real-repo traversal latency benchmark

- The bench harness is synthetic (50/200/100 modules). Add a real-repo
  variant against `codegraph-main` measuring cold-start `analyze()` and
  depth-3 `traverse` BFS latency.
- Record the numbers in `traverse-smell-status.md` §7.

### 1.5 Concurrent-read smoke test

- Pin the verified concurrency mechanism: N concurrent `get_smells` /
  `traverse` reads against one `GLOBAL_GRAPH` must all return correct results
  and must not deadlock; a concurrent writer must still acquire the write lock.
- Guards the `parking_lot::RwLock` (N-reader) property against regressions.

---

## Phase 2: Core Heuristics & MCP Visibility (v1 blockers)

*Goal: eliminate the top user-facing friction — silent refactoring
degradation, failed class resolution, silent traversal truncation, and the
`as_of` NotImplementedError.*

### 2.1 Same-Package Precedence in `resolve_base_by_name`

- When ambiguity occurs (multiple classes share a name), do **not** return
  `None` immediately:
  1. Filter candidates by matching the caller's package/module path.
  2. Exactly one match → return it.
  3. Multiple remain → deterministic tiebreak (alphabetical by module path),
     rather than silent failure.
- **Constraint:** keep `External`/`Builtin` bases unresolved by design.
- **Test:** Rust unit test — two classes named `Service` in different
  packages; assert a subclass in package A resolves to the base in package A.

### 2.2 Basic Alias Awareness in `find_module_by_dotted_name`

- Currently path-suffix only; `@/components/Button` fails to resolve to
  `src/components/Button`.
- Implement a lightweight, heuristic alias resolver for v1 (common
  conventions: `@/*` → `src/*`, `~/*` → `src/*`). No full `tsconfig.json` /
  `pyproject.toml` parser.
- **Defer:** full config parsing + path mapping → post-v1.
- **Test:** Rust unit test — `find_module_by_dotted_name("@/models/user", …)`
  resolves to `…/src/models/user`.

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

### 4.1 Publish the missing sdist

- **Current state:** `.github/workflows/release.yml` already has an `sdist`
  job (`maturin sdist`) plus a `publish` job that uploads wheels + sdist.
- **Root cause of the missing v0.6.4 sdist:** `maturin sdist` requires the
  LICENSE file inside the tarball; it was absent until `4f40d1f` added
  `[tool.maturin] include = ["LICENSE"]` to `pyproject.toml` — *after* the
  v0.6.4 tag, so that fix was never released.
- **Action:** verify the `sdist` job end-to-end (command, `--out`, artifact
  merge in `publish`), apply any remaining release.yml fix if the v0.6.5 run
  reveals one, and confirm the v0.6.5 tag push publishes both wheels and the
  sdist to PyPI.

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
