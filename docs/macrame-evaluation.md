# Macrame as CodeRadar's Storage Engine — Revised Assessment

**Date:** 2026-08-08  
**Revised:** Author of Macrame is on the CodeRadar team  
**Status:** Strong recommendation to adopt Macrame as the primary storage layer

---

## The Situation Has Changed

With Macrame's author on the CodeRadar team, the previous evaluation's central concern — "too immature, 0 stars, single author, can't adapt it" — evaporates. We control both sides of the integration. Macrame can evolve to meet CodeRadar's domain model, and CodeRadar can adapt to Macrame's assertion model. This is the best-case scenario for a storage dependency.

---

## What Macrame Already Provides (Free)

| Capability | CodeRadar Spec Equivalent | Effort Saved |
|-----------|--------------------------|--------------|
| **Bitemporal assertion model** | §5.5 WAL + PatchTransaction + rollback | Eliminates ~2,000 lines of WAL code |
| **as_of(ts) / reconstruct(ts)** | §9.1 snapshot isolation via ArcSwap | Eliminates ArcSwap arena cloning |
| **DiskANN vector search** | §13 embedding pipeline + LadybugDB HNSW | Eliminates LadybugDB dependency |
| **FTS5 keyword search** | §7.1 Pest grammar text matching | Supplements structural queries |
| **Hybrid RRF fusion** | Not in spec | Adds combined vector+keyword search |
| **Recursive CTE traversal** | §7.4 Rust-accelerated traversal | Moves traversal to DB layer |
| **Write Actor concurrency** | §9.2 single-writer, multiple-reader | Already solved |
| **Schema versioning + migration** | §10.2 | Already solved (v10, 10 rungs) |
| **Python bindings (pyo3/maturin)** | §8 Python API | Already solved (abi3-py310) |
| **In-memory analytics** (Dijkstra, A*, SCC, Louvain) | Not in spec | Free extras |
| **Archival path** (cold storage) | Not in spec | Free for long-running deployments |
| **Audit/integrity verification** | §11.8 MutationLog | Replaces custom audit log |

---

## What Needs Adapting

### 1. Domain Model: Concepts → Code Entities

Macrame's model: `Concept` (id, content) + `EdgeAssertion` (source, target, type, weight).

CodeRadar needs: `Module`, `Class`, `Function`, `Import`, `Constant`, `TypeAlias` — each with byte spans, source positions, qualified names, hashes, and language-specific metadata.

**Adaptation plan:**

```
Macrame Concept                CodeRadar Entity
─────────────────              ──────────────────
id                             entity_id (dotted path, e.g. "src/auth.py::login")
content                        serialized entity metadata (JSON or flat buffer)
                               → name, qualified_name, byte_spans, signature_hash, etc.
annotations                    per-entity metadata keyed by field name
                               → "kind": "function", "line": 42, "is_async": true
```

This keeps Macrame's concept model intact while storing CodeRadar's rich entity data in `content` and `annotations`. Queries filter by annotation, not by concept structure.

### 2. Edges: Weighted → Structural

Macrame edges are `(source, target, edge_type, weight)` with bitemporal validity.

CodeRadar needs: `contains`, `calls`, `imports`, `extends`, `implements`, `references`, `decorates` — with confidence scores, resolution method, and call-site byte spans.

**Adaptation:**

| Macrame field | CodeRadar mapping |
|---------------|-------------------|
| `edge_type` | `"calls"`, `"imports"`, `"extends"`, etc. |
| `weight` | `confidence` (0.0–1.0) |
| `properties` (JSON) | `resolution_method`, `call_site_span`, `line` |
| `valid_from` | When this edge was introduced in the codebase |
| `recorded_at` | When CodeRadar indexed this edge |

### 3. Byte Spans and Source Positions

Macrame has no concept of byte spans or source positions. These go into `annotations` on concepts (entities) and `properties` on edges (references).

For the mutation engine (§11), byte spans must be queryable. Two approaches:

**A) Store in annotations:** `db.annotate(entity_id, "body_span", "120..350")` — simple, queryable via annotation filtering.

**B) Extend Macrame's schema:** Add a `byte_spans` table or extend `concepts` with span columns. This is possible because we control Macrame.

**Recommendation:** Start with annotations (Option A). If query performance on spans becomes a bottleneck, extend Macrame's schema (Option B) — it's our codebase.

### 4. Structural Query Language

CodeRadar's spec defines a Pest grammar (§7.1) for queries like:
```
functions where is_async == true and line_count > 50
```

Macrame has no query language — only API calls. Two approaches:

**A) Translate Pest to Macrame API calls** — parse the Pest query in Rust, then call Macrame's traversal/search APIs to execute it.

**B) Store entities in SQLite alongside Macrame** — Macrame for graph/temporal/vector, SQLite for structural queries via the Pest grammar.

**Recommendation:** Start with Option A for graph queries (traversal, callers, callees) and Option B for structural queries (filter by entity properties). The two databases can share the same process — Macrame for the graph, SQLite for entity metadata with rowid references into Macrame concepts.

### 5. Incremental Update Integration

CodeRadar's incremental update algorithm (§5) computes a diff between old and new file versions, then applies a patch. With Macrame:

**Before (spec design):**
1. Parse file → ExtractedUnits
2. Diff old vs new → Patch (Add/Remove/Modify)
3. WAL transaction → apply patch
4. Update reverse indexes
5. Re-resolve affected symbols
6. Bump epoch

**With Macrame:**
1. Parse file → ExtractedUnits
2. Diff old vs new → Patch (same logic, preserved)
3. For each change:
   - Removed entity → `db.retire_entity(id, valid_to=now)` (sets valid_to, doesn't delete)
   - New entity → `db.upsert_concept(ConceptUpsert::new(id, content).valid_from(now))`
   - Modified entity → retire old + upsert new (creates new version, old preserved)
   - Changed edges → same assertion pattern (old edge retired, new edge asserted)
4. Re-resolve affected symbols
5. Done — no WAL, no rollback journal, no manual epoch bump

The diff algorithm (§5.2) is still needed — Macrame stores history but doesn't compute diffs. But the WAL and rollback infrastructure (§5.5) is entirely replaced by Macrame's assertion model.

---

## Architecture: CodeRadar + Macrame

```
┌──────────────────────────────────────────────────────────────────┐
│                      Python Layer                                │
│  CLI │ Visualizers │ GraphRAG │ Mutation Router │ MCP Server    │
│                           │                                      │
│                    Macrame Python bindings                       │
│                    (concepts, edges, search, temporal)           │
└───────────────────────────┼──────────────────────────────────────┘
                            │
┌───────────────────────────┼──────────────────────────────────────┐
│                      Rust Core                                   │
│  ┌────────────────────────┴───────────────────────────────────┐  │
│  │ • Tree-sitter extraction (existing)                        │  │
│  │ • Diff algorithm (preserved from §5.2)                     │  │
│  │ • Resolution cascade L1–L3 (existing)                      │  │
│  │ • Pest query grammar → Macrame API translation             │  │
│  │ • Mutation engine (preserved from §11)                     │  │
│  │                                                             │  │
│  │ Storage layer → macrame-db (Rust crate, same process)      │  │
│  │   ├── Entities as Concepts with annotations                │  │
│  │   ├── Edges as EdgeAssertions with properties              │  │
│  │   ├── Bitemporal for incremental update history            │  │
│  │   ├── DiskANN for embedding vector search                  │  │
│  │   └── FTS5 for keyword search                              │  │
│  └─────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

---

## What Gets Removed from CodeRadar's Spec

| Spec Section | What Happens |
|-------------|--------------|
| §5.5 WAL + PatchTransaction | Replaced by Macrame assertion model |
| §9.1 ArcSwap snapshot isolation | Replaced by Macrame `as_of(ts)` |
| §9.3 chunked sub-transactions | Replaced by Macrame cooperative chunking |
| §10 Persistence (LadybugDB) | Replaced by Macrame (single .db file) |
| §10.2 Schema versioning | Replaced by Macrame's migration rungs |
| §11.8 MutationLog retention | Replaced by Macrame's transaction_log |
| Appendix B LadybugDB Vector Search | Replaced by Macrame DiskANN |

---

## What Stays in CodeRadar's Spec

| Spec Section | Why It Stays |
|-------------|-------------|
| §4 Tree-sitter extraction | Core parsing — Macrame doesn't parse code |
| §5.2 Diff algorithm | Macrame doesn't compute diffs |
| §5.3 Cross-file resolution | Core intelligence — Macrame stores edges, doesn't resolve them |
| §6 Resolution cascade | L1–L3 in Rust — Macrame doesn't do semantic resolution |
| §7.1 Pest query grammar | Structural queries — Macrame doesn't understand code structure |
| §7.3 Cypher templates | Can be adapted to Macrame API, or dropped |
| §8 Python API | Public surface — wraps Macrame under the hood |
| §11 Mutation engine | AST-aware refactoring — Macrame doesn't touch source files |
| §12 Git integration | File watching + blame — separate from storage |
| §13 Embedding pipeline | Python-side embedding generation — Macrame only stores/indexes vectors |
| §17 Watch mode | File events → extraction pipeline — separate from storage |

---

## Migration Path

### Step 1: Add macrame-db as a Rust dependency

```toml
# core_indexer/Cargo.toml
[dependencies]
macrame-db = "0.10"
```

### Step 2: Define the entity-to-concept mapping

Create a `storage` module in core_indexer that defines:
- `EntityConcept` — wraps CodeRadar's entity types into Macrame concepts
- `EdgeAssertion` — maps CodeRadar's relationship types to Macrame edge types
- `TemporalBounds` — maps diff operations to valid_from/valid_to

### Step 3: Replace CodeGraph internals

Replace the ArcSwap arenas and reverse indexes in `graph.rs` with Macrame calls:
- `insert_module()` → `db.upsert_concept(...)`
- `add_edge()` → `db.assert_edge(...)`
- `find_callers()` → `db.traverse().start_node(id).max_depth(n).execute()`
- `snapshot()` → `db.as_of(ts)`

### Step 4: Adapt the Python layer

Replace LadybugDB calls with Macrame Python bindings:
- `graph.cypher(...)` → `db.load_subgraph(...)` + `db.search(...)`
- Embedding storage → `db.upsert_embedding(...)`
- Vector search → `db.vector_search(...)`

### Step 5: Extend Macrame as needed

Since we control Macrame, add CodeRadar-specific features:
- Byte span annotations as a first-class feature
- Bulk entity loading optimized for code graphs (thousands of concepts per file)
- Edge property indexing for confidence/line queries
- Any schema changes needed for code entity metadata

---

## Risk Assessment (Revised)

| Risk | Mitigation |
|------|-----------|
| Macrame API churn (pre-1.0) | We control the API. Coordinate changes across both codebases. |
| Macrame schema changes | We control the schema. CodeRadar needs become design input. |
| Unknown production load | CodeRadar IS the first production user. Instrument heavily. |
| Performance on 100K+ entities | Macrame's benchmarks show linear scaling. Validate with real codebases. |
| Single-writer bottleneck | Macrame already solves this with the Write Actor. |
| Vector search dimension mismatch | Macrame's DiskANN is dimension-agnostic. Configure to CodeRadar's 896-d model. |

---

## Decision

**Adopt Macrame as CodeRadar's primary storage engine**, replacing LadybugDB/Kùzu and the in-memory WAL infrastructure.

This is the right call because:
1. The author is on the team — we can adapt both sides
2. Bitemporal model directly solves incremental updates better than the spec's design
3. Eliminates ~30% of CodeRadar's planned complexity (WAL, rollback, ArcSwap epochs, LadybugDB)
4. Both projects use the same stack (Rust + Python via PyO3/maturin)
5. Free vector search, graph traversal, temporal queries, and integrity verification
6. CodeRadar becomes Macrame's first real-world production user — mutual benefit

---

## Performance Analysis — v0.5.2 (2025-08-09)

### Benchmark setup

200 Python files, one orchestrator importing + calling all leaf functions:

```
199 leaf modules × 5 functions each = 995 leaf functions
1 orchestrator.py with 995 `from mod_N import func_N_J` statements
  and 995 `func_N_J()` calls inside `def orchestrator()`

Total: 200 files, 996 functions, 995 imports, 1991 entities
```

### Head-to-head: CodeGraph 1.5.0 vs CodeRadar 0.5.2

| Metric | CodeGraph 1.5.0 | CodeRadar 0.5.2 | Ratio |
|--------|:-:|:-:|:-:|
| Index time | **1,219ms** | **3,326ms** | 2.7× |
| Files | 200 | 200 | — |
| Nodes / Entities | 2,191 | 2,191 | — |
| Edges | 3,981 | 995 (call edges) | — |
| DB size | 3.0 MB | 13.7 MB (4.1MB WAL) | — |
| Search latency | — | 0.05ms | — |

### Where the 3,326ms goes

Three macro-benchmarks isolate each phase:

| Test | Setup | Time | What it measures |
|------|-------|------|------------------|
| Pure indexing, no calls | 200 files, 1000 isolated funcs | **1,532ms** | Parse + extract + insert + persist |
| Single-file, many calls | 1 file, 1001 funcs, 1000 same-file calls | **545ms** | Parse + extract + insert + resolve + persist (1 file) |
| Full cross-file | 200 files, cross-file calls | **3,326ms** | Everything |

From these, the phase breakdown is:

| Phase | Cost | % of total |
|-------|------|-----------|
| Tree-sitter parse + extract + insert | ~800ms | 24% |
| `persist_entities` (2,191 concepts to Macrame) | ~1,800ms | **54%** |
| `resolve_all_calls` (995 calls across files) | ~500ms | 15% |
| `persist_edges` (995 edges, batched) | ~100ms | 3% |
| Other (walk, MRO, commit) | ~126ms | 4% |

### Root cause: per-concept `block_on` overhead

`persist_entities` accounts for **54% of total time**. The current implementation:

```rust
// storage.rs — current code
pub fn upsert_entities(&self, units, file_path, language) -> Result<()> {
    for unit in units {
        let concept = build_concept(unit, file_path, language);
        self.runtime.block_on(self.db.upsert_concept(concept))?;
        //                      ^^^^^^^^^ one Tokio task per entity
    }
    Ok(())
}
```

Each of the 2,191 entities triggers:
1. `build_concept()` — sync, fast (~50µs with JSON serialization)
2. `block_on()` — spawns a Tokio task, polls the future, waits for completion (~300µs)
3. `upsert_concept()` — async, writes to libSQL via Macrame's Write Actor (~500µs)

**Per entity: ~850µs. Total: 2,191 × 850µs ≈ 1,862ms.**

### What I tried (and why it didn't work)

**Attempt 1: Single `block_on` per file**

Wrapped all concepts from one file in a single async block to avoid per-entity Tokio task spawning:

```rust
let concepts: Vec<ConceptUpsert> = units.iter()
    .map(|u| build_concept(u, file_path, language)).collect();
self.runtime.block_on(async {
    for concept in &concepts {
        self.db.upsert_concept(concept.clone()).await?;
    }
    Ok(())
})
```

**Result: 15% slower** (3,911ms vs 3,326ms). The Tokio runtime fast-paths individual `block_on` calls for immediately-ready tasks. Batching them forces the async runtime to schedule and poll multiple futures sequentially within the same task, which has higher overhead than individual tiny tasks.

**Attempt 2: Defer all persistence to end of analyze loop**

Collected all `ExtractedUnit`s during the file loop, then persisted all 2,191 concepts in one batch after indexing was complete.

**Result: also slower** (3,875ms). Cloning all units added allocation overhead. Plus the single giant `block_on` with 2,191 sequential `upsert_concept` calls suffered the same Tokio scheduling issue as Attempt 1, just at larger scale.

### What would actually fix it

**Macrame needs a bulk concept upsert API.** The equivalent of `write_bulk_atomic` for edges, but for concepts:

```rust
// Proposed Macrame API
pub async fn upsert_concepts_bulk(
    &self,
    concepts: Vec<ConceptUpsert>,
) -> Result<usize>;
```

This would wrap all concepts in a single SQL transaction with one Write Actor dispatch. Instead of 2,191 `block_on` + `upsert_concept` round-trips, there would be ~10 batch calls (one per 200-entity chunk).

**Projected impact:**

| Phase | Current | With bulk API |
|-------|---------|---------------|
| `persist_entities` | 1,800ms | ~500ms |
| Everything else | 1,526ms | 1,526ms |
| **Total** | **3,326ms** | **~2,026ms** |
| vs CodeGraph | 2.7× slower | **1.7× slower** |

The remaining 1.7× gap comes from CodeRadar storing 3× more metadata per entity (byte spans, content hashes, signatures, return types, parameter lists, docstrings, decorators, grammar_kind) vs CodeGraph's minimal (name, kind, file). This is a deliberate tradeoff: richer metadata enables deeper query-time intelligence without re-reading source files.

### Secondary optimization: WAL checkpoint after batch index

After a full `analyze`, Macrame's WAL holds all written data. A forced checkpoint would collapse the 4.1MB WAL into the main DB, reducing total storage from 13.7MB to ~9MB. This is a one-line call after the batch resolve:

```rust
self.runtime.block_on(self.db.checkpoint())?;
```

### What doesn't need optimization

- **`resolve_all_calls`** — already scoped per file via `resolve_calls_scoped` (v0.5). Processes only changed-file functions on update.
- **`persist_edges`** — already batched every 200 edges via `write_bulk_atomic`.
- **Search** — 0.05ms via HashMap lookups in Rust. No DB query needed.
- **Tree-sitter** — native C FFI, already the fastest possible path.
- **Diff algorithm** — O(n) identity match, not AST diffing. Already optimal.

---

*End of revised assessment.*
