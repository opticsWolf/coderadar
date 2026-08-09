# CodeRadar Performance Roadmap

**Date:** 2025-08-09  
**Baseline:** v0.5.2, 200-file cross-file benchmark at commit `2f52739`  
**Reference:** CodeGraph 1.5.0 cross-file = 1,219ms (CodeRadar approximately 1.5× slower)

---

## Current State

| Phase | Cost | Parallelized? |
|-------|------|---------------|
| Walk filesystem + read source | ~5ms | No (I/O-bound) |
| Parallel tree-sitter parse + extract | ~200ms | Yes (4 threads) |
| Parallel import graph edge building | ~50ms | Yes (4 threads) |
| Sequential projection insert (200 files) | ~350ms | **No** |
| `resolve_all_calls` (995 calls) | ~580ms | **No** |
| `write_concepts` (Macrame bulk) | ~75ms | N/A (Macrame actor) |
| `persist_edges` (995 edges) | ~100ms | N/A (batched) |
| MRO + other | ~50ms | No |
| **Total** | **~1,800ms** | |

Remaining gap to CodeGraph: ~580ms. The two largest sequential phases
(projection insert + resolve_all_calls) account for ~930ms.

---

## Improvement Candidates

Candidates are ranked by **combined score** = (likelihood × impact), where:

- **Likelihood** (1–5): how likely this is to land without breaking things.
  5 = nearly certain, 1 = speculative.
- **Impact** (1–5): expected ms saved on the cross-file benchmark.
  5 = >400ms, 4 = 200–400ms, 3 = 100–200ms, 2 = 50–100ms, 1 = <50ms.

---

### 1. Parallel `resolve_all_calls` — **Score: 20** (5 × 4)

**Likelihood: 5**
**Impact: 4** (~230ms on 2 threads, ~280ms on 4 threads; floor ~380ms)

Extract `resolve_one_function` as a pure function, then parallelize the
inner loop using `std::thread::scope`. Projection reads are shared across
threads; writes are collected and applied sequentially.

Detailed plan in: `docs/macrame-evaluation.md` § "Plan: Parallel resolve_all_calls"

**Risk:** Medium. Brace nesting is deep; the first attempt was reverted.  
**Mitigation:** Two-step approach — extract pure function first (verify tests),
then add parallel dispatch. Gated behind 50-item minimum chunk threshold.  
**Dependencies:** None.  
**Interaction:** `update_file` stays sequential (small files fall below threshold).

---

### 2. Parallel projection insert — **Score: 16** (4 × 4)

**Likelihood: 4**
**Impact: 4** (~200ms on 4 threads)

Each parse thread currently returns `(units, concepts)`. The main thread
then does sequential `insert_extracted` for all 200 files. Instead, each
thread could build its own `ProjectedGraph` fragment locally, then all
fragments are merged at the end with `HashMap::extend`.

**Approach:**

```rust
// Phase 2 (parallel) — each thread produces:
struct ChunkResult {
    projection: ProjectedGraph,  // local fragment
    concepts: Vec<ConceptUpsert>,
}

// Phase 3 (sequential merge):
for chunk in all_results {
    main_projection.modules.extend(chunk.projection.modules);
    main_projection.functions.extend(chunk.projection.functions);
    main_projection.classes.extend(chunk.projection.classes);
    // ... etc for all HashMap fields
    all_concepts.extend(chunk.concepts);
}
```

**Risk:** Medium. `ProjectedGraph` has 11 `HashMap` fields — the merge must
cover all of them. Adding a new field later must update the merge.  
**Mitigation:** Derive a `Merge` trait or macro to keep merge in sync with
struct definition. Test with same fixture as cross-file benchmark.  
**Dependencies:** None — pure refactor of the existing parallel phase.  
**Interaction:** Import graph edges built in parallel (already done) would
need to be built from the fragment-level units, not the shared import graph.
The fragment approach actually simplifies this because each thread has its
own `InsertExtracted` context.

---

### 3. Pre-computed `line_starts` for O(1) byte→line — **Score: 12** (4 × 3)

**Likelihood: 4**
**Impact: 3** (~50–100ms across 200 files)

Currently tree-sitter's `Node::start_position()` is O(1) — the point is
stored on the node during parsing. So this optimization does NOT apply to
line numbers. However, several places compute column offsets from byte
positions (e.g., call-site spans in the walker). Those currently use
`node.start_position().column` which tree-sitter computes from the stored
byte offset via a character-width scan.

**Actual win:** Replace `start_position().column` with a pre-computed
`line_starts` + binary search → column from `byte_offset - line_starts[row]`.
Only beneficial for files with wide characters (Unicode), which Python
source files rarely have.

**Verdict:** Marginal for Python-heavy workloads. Worth doing if CodeRadar
expands to JavaScript/TypeScript (where UTF-16 column semantics matter).  
**Risk:** Low. The `line_starts` function is 5 lines from CodeGraph.  
**Dependencies:** None.

---

### 4. `Arc<String>` / string interning for entity names — **Score: 12** (3 × 4)

**Likelihood: 3**
**Impact: 4** (~150ms across 200 files, mostly in projection insert phase)

Entity names like `"func_0_0"`, `"func_0_1"`, etc. are cloned into every
`Function`, `Class`, `Import` struct. For the cross-file benchmark, `"orch.py"`
is cloned 996 times (once per function in the orchestrator file). String
interning or `Arc<str>` would share the allocation.

**Approach:** Store `name: Arc<str>` instead of `name: String` in entity
types. Use a per-file or per-session string interner (`HashMap<&str, Arc<str>>`)
during extraction.

**Risk:** High. Touches every entity type (`Function`, `Class`, `Import`,
`Constant`, `TypeAlias`, `Module`, `Field`, `Parameter`). Every consumer
of `.name` must be updated. Knock-on effects in Python bindings (PyO3
conversion), query engine, MCP server, serialization.

**Mitigation:** Start with `Function.name` only — it's the most duplicated
string. Measure impact before expanding to other types.  
**Dependencies:** None.

---

### 5. Skip `Arc::clone` on unchanged functions during resolve — **Score: 10** (5 × 2)

**Likelihood: 5**
**Impact: 2** (~30ms)

In `resolve_calls_scoped`, every function with resolved calls is cloned,
mutated, and re-inserted:

```rust
let mut updated = (**func_arc).clone();
updated.resolved_calls = resolved.clone();
projection.functions.insert(func_id.clone(), Arc::new(updated));
```

For functions whose resolved calls are identical to the previous set
(e.g., no code change), this clone+insert is wasted. A simple equality
check on `resolved_calls` before the clone would skip it.

**Risk:** Very low. Two-line change.  
**Dependencies:** None.

---

### 6. SQLite PRAGMAs for Macrame — **Score: 8** (2 × 4)

**Likelihood: 2**
**Impact: 4** (~200–400ms)

CodeGraph sets `cache_size = -64000` (64MB), `mmap_size = 268435456` (256MB),
and `temp_store = MEMORY` on every connection. Macrame's `configure()` is
private and doesn't expose these. Adding them to Macrame would improve
read-heavy workloads (reconstruct, as_of, search) and potentially reduce
write latency for `write_concepts` and `write_bulk_atomic`.

**Risk:** Low (just PRAGMAs), but blocked on Macrame's private `configure()`.  
**Blocked by:** Macrame API change. Would need either:
- Macrame to add these PRAGMAs to `configure()` (one-line change)
- Macrame to expose a `set_pragma()` method
- CodeRadar to open a second libSQL connection alongside Macrame (hacky)

**Dependencies:** Macrame upstream change.

---

### 7. Skip `remove_file_entities` scan on initial index — **Score: 8** (4 × 2)

**Likelihood: 4**
**Impact: 2** (~30ms for 200 files)

`analyze()` currently uses `index_file_accumulate` which calls
`index_file_inner` → `insert_extracted`. The `insert_extracted` method
checks for existing entities with the same ID and removes them first
(via the `imports.remove` / `functions.remove` pattern in the helper closure).
For an initial index (empty projection), these removes are no-ops but still
execute HashMap lookups.

A flag `is_initial_index: bool` could skip the remove-on-insert pattern.

**Risk:** Low. One boolean parameter threaded through `insert_extracted`.  
**Dependencies:** None.

---

### 8. Tree-sitter query caching — **Score: 7** (2 × 3.5)

**Likelihood: 2**
**Impact: 3.5** (~100ms across 200 files)

CodeRadar runs tree-sitter queries (`.scm` files) via `Query::new()` for
every file. The `Query` is constructed from the query source string, which
is compiled into a state machine. This compilation is O(query size) and
repeated for every file of the same language.

**Approach:** Cache compiled `Query` objects in a `OnceLock` or `LazyLock`
per language. The `tag_tree` function currently creates a new `Query` per
invocation.

**Risk:** Low for correctness, but `tree_sitter::Query` has lifetime
constraints (borrows the `Language` reference). A global cache requires
`'static` lifetimes, which may require `unsafe` or `once_cell::sync::Lazy`.

**Dependencies:** tree-sitter API constraints. May need `tree_sitter_language_pack`
to expose static language references.

---

### 9. Batched `commit_projection` — **Score: 6** (3 × 2)

**Likelihood: 3**
**Impact: 2** (~20–40ms)

Currently `insert_extracted` commits the projection after every file
(200 commits for 200 files). Each commit is `ArcSwap::store` (an atomic
pointer swap) which is very fast (~50ns). The real overhead is the
`ArcSwap::load` + `clone` of the previous projection for each file:

```rust
let mut projection = (*self.snapshot()).clone();
```

For 200 files, the projection grows from empty to 2,191 entities. Each
clone copies an increasingly large `ProjectedGraph`. Early clones are
cheap; late clones copy ~2,000 entities.

**Approach:** Batch 20–50 files per commit instead of 1.

**Risk:** Low. But the savings are small (~20ms) because `Arc::clone` on
the inner data structures (which use `Arc<Class>`, `Arc<Function>`) is O(1)
reference counting, not deep copy.

**Dependencies:** None.

---

### 10. Memory-mapped source files — **Score: 4** (2 × 2)

**Likelihood: 2**
**Impact: 2** (~20ms for I/O)

Currently `analyze()` reads all source files into `String` upfront
(via `fs::read_to_string`). For 200 files × ~2KB = 400KB, this is
negligible. For larger codebases (thousands of files, MBs each),
memory-mapped I/O (`memmap2` crate) could reduce allocation pressure.

**Risk:** Low but limited benefit for the benchmark scale.  
**Dependencies:** `memmap2` crate.

---

### 11. Columnar/flat-buffer extraction — **Score: 3** (1 × 3)

**Likelihood: 1**
**Impact: 3** (~100ms)

CodeGraph uses fixed-width binary rows + string arena per file (5 buffers:
meta, nodes, edges, refs, arena). This eliminates per-entity heap allocations.
CodeRadar allocates a `Vec<ExtractedUnit>` with heap-allocated `String` for
every name, signature, docstring, etc.

**Approach:** Replace `ExtractedUnit` with a flat `Vec<u8>` buffer +
offset/length indices. The walker writes into the buffer, and
`insert_extracted` reads from it.

**Risk:** Very high. Touches every language walker (18 languages),
the extraction pipeline, the projection insertion, and the Python bindings.
Effectively a rewrite of the extraction layer.

**Verdict:** Not worth the complexity for a 100ms saving. The parallel
improvements above deliver much more per unit of effort.

---

## Summary Table

| # | Improvement | Likelihood | Impact | Score | Effort |
|---|-------------|-----------|--------|-------|--------|
| 1 | Parallel `resolve_all_calls` | 5 | 4 | **20** | Medium |
| 2 | Parallel projection insert | 4 | 4 | **16** | Medium |
| 3 | Pre-computed `line_starts` | 4 | 3 | 12 | Small |
| 4 | `Arc<String>` interning | 3 | 4 | 12 | Large |
| 5 | Skip Arc::clone on unchanged | 5 | 2 | 10 | Trivial |
| 6 | SQLite PRAGMAs (Macrame) | 2 | 4 | 8 | Small (blocked) |
| 7 | Skip removes on initial index | 4 | 2 | 8 | Small |
| 8 | Tree-sitter query caching | 2 | 3.5 | 7 | Medium |
| 9 | Batched commit_projection | 3 | 2 | 6 | Small |
| 10 | Memory-mapped source files | 2 | 2 | 4 | Small |
| 11 | Columnar/flat-buffer extraction | 1 | 3 | 3 | Very Large |

## Recommended Implementation Order

1. **#5 — Skip Arc::clone on unchanged** (trivial, immediate win)
2. **#1 — Parallel resolve_all_calls** (highest score, plan documented)
3. **#2 — Parallel projection insert** (second highest, natural extension of existing parallel phase)
4. **#6 — SQLite PRAGMAs** (unblock via Macrame change, then one-line)
5. **#7 — Skip removes on initial index** (low risk cleanup)

After these five, CodeRadar would be at approximately **~1,200ms** cross-file —
essentially parity with CodeGraph 1.5.0. The remaining candidates (#3, #4, #8,
#9, #10, #11) are either marginal, high-risk, or both.

## Projected Timeline

| Step | Cumulative cross-file | vs CodeGraph |
|------|----------------------|-------------|
| Current (v0.5.2) | ~1,800ms | 1.5× |
| + #5 (clone skip) | ~1,770ms | 1.45× |
| + #1 (parallel resolve) | ~1,490ms | 1.22× |
| + #2 (parallel insert) | ~1,290ms | 1.06× |
| + #6 (PRAGMAs) | ~1,190ms | 0.98× |
| + #7 (skip initial removes) | ~1,170ms | 0.96× |

At parity or better, the benchmark itself becomes the bottleneck — 200 files
is below CodeGraph's own recommended minimum for meaningful measurement.
Further work shifts to profiling real-world codebases (Django, CPython, Linux
kernel) rather than synthetic benchmarks.

---

*End of roadmap.*
