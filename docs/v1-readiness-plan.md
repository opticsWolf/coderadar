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

## Phase 4: Structural Cleanup (post-v1 / stretch)

### 4.1 `class.methods` Denormalization Strategy

- `class.methods` is always `vec![]` (footgun). Populating it creates a second
  source of truth alongside `projection.functions`.
- **v1:** leave as `vec![]`; document the invariant clearly. Do **not**
  populate it as a cache (sync risk > footgun).
- **Post-v1:** file an issue to migrate all scan sites (CBO, override
  detection, smell rules) to read from a populated `class.methods`, removing
  the scan-`projection.functions`-by-`parent_class` pattern.

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
