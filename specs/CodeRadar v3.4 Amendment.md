# CodeRadar — Specification Amendment v3.4

> **Status:** Amendment to v3.3 Consolidated Specification  
> **Date:** 2026-08-08  
> **Triggers:**
> - Production review of CodeGraph (`codegraph-main`, shipping product, 15+ agents, 25+ languages)
> - Adoption of Macrame (`macrame-db` 0.10.0) as primary storage engine
> - Macrame's author on the CodeRadar team (co-development possible)
>
> **Scope:** This amendment revises the storage architecture (§10), FFI contract (§8, Appendix A), update algorithm (§5.5, §9), query engine (§7), resolution engine (§6), and adds agent-interface and validation-methodology sections derived from CodeGraph's production experience. It does **not** change the extraction layer (§4), diff algorithm (§5.2), resolution cascade layers 1–3 (§6.2–6.4), mutation engine (§11), or CLI (§16).

---

## Table of Amendments

| # | Section | Change | Impact |
|---|---------|--------|--------|
| A1 | §3.4, §9.1 | Remove ArcSwap arenas, epoch-based snapshot isolation | Replaced by Macrame bitemporal `as_of(ts)` |
| A2 | §5.5 | Remove WAL, PatchTransaction, rollback, TxBegin/TxAck | Replaced by Macrame assertion model |
| A3 | §9.3 | Remove chunked sub-transactions | Replaced by Macrame Write Actor + cooperative chunking |
| A4 | §10 | Replace LadybugDB with Macrame as primary store | Single .db file, embedded, no server |
| A5 | §10.1–10.3 | Remove LadybugDB schema, Cypher DDL, vector index setup | Replaced by Macrame schema §10 (new) |
| A6 | §11.8 | Remove MutationLog retention tiers | Replaced by Macrame transaction_log |
| A7 | Appendix B | Remove LadybugDB Vector Search API | Replaced by Macrame DiskANN |
| A8 | §8, Appendix A | Flat-buffer FFI contract (one boundary per file) | Replaces per-entity PyO3 struct passing |
| A9 | §6 | Add "partial coverage is worse than none" principle | Resolution cascade must close flows end-to-end |
| A10 | §7.3 | Cypher templates → Macrame API + optional SQLite structural queries | Query language adaptation |
| A11 | NEW §26 | Agent interface design principles (from CodeGraph) | Tool design, error handling, explore budgets |
| A12 | NEW §27 | Validation methodology (from CodeGraph) | Parity gates, agent A/B evaluation |
| A13 | NEW §28 | Framework resolver interface (from CodeGraph) | detect/resolve/extract pattern |
| A14 | §4 | Deferred-to-recovery error handling (from CodeGraph) | Per-file safety valve for parse errors |
| A15 | §8.1 | Entity wrappers carry epoch → carry temporal bounds | StaleHandle → TemporalAmbiguity |
| A16 | NEW §10 (Macrame) | Full Macrame schema for CodeRadar entities | Replaces §10 LadybugDB schema |

---

## A1–A3: Removal of In-Memory MVCC Infrastructure

### Original (§3.4, §9.1, §9.3)

CodeRadar v3.3 specifies:
- Per-entity `Arc<Entity>` stored in `ArcSwap<SlotMap<Entity>>` arenas
- Queries take `QuerySnapshot { epoch, arena_refs }` by cloning arena pointers
- Updates clone the `SlotMapInner` vector, mutate, then `ArcSwap::store`
- Chunked sub-transactions at 200 files per chunk for reader latency

### Amendment

**Remove all in-memory MVCC infrastructure.** Macrame provides equivalent guarantees through its bitemporal model:

| v3.3 Concept | Macrame Equivalent |
|-------------|-------------------|
| `ArcSwap<SlotMap<Entity>>` arena | Macrame `concepts` table |
| `QuerySnapshot { epoch }` | `db.as_of(ts)` — query at any valid time |
| `ArcSwap::store(new_inner)` | Assert new edges/concepts (supersedes old) |
| Epoch bump on commit | Transaction commit (implicit in assertion) |
| Chunked sub-transactions | Write Actor + cooperative chunking (90 edges / 3ms) |
| Rollback on conflict | Never needed — assertions are immutable, only superseded |

### Rationale

Macrame's assertion model is strictly simpler: facts are never overwritten, only superseded by new assertions with later `valid_from` timestamps. A query at time `ts` sees the graph as it existed at `ts` — no cloning, no epoch bumps, no manual snapshot management. The Write Actor already enforces single-writer semantics with cooperative yielding every ~3ms.

### What stays

- The diff algorithm (§5.2) — Macrame stores history but doesn't compute diffs
- Reverse indexes — these become Macrame edge queries (e.g., `find_callers` → `traverse().incoming("calls")`)
- Resolution cache (§5.4) — still needed for in-memory caching of resolution results during ingestion

---

## A4–A7, A16: Storage Architecture — Macrame replaces LadybugDB

### Original (§10, §11.8, Appendix B)

CodeRadar v3.3 specifies LadybugDB (Kùzu Cypher dialect) for persistence with:
- Node tables: `Module`, `File`, `Function`, `Class`, `Method`, `Variable`, `Parameter`, `Import`
- Relationship tables: `CONTAINS_MODULE`, `DECLARES_CLASS`, `CALLS`, `IMPORTS`, `EXTENDS`, etc.
- HNSW vector indexes: `func_embedding_idx`, `method_embedding_idx`, `class_embedding_idx`
- MutationLog with tiered retention (7-day full, 30-day summary, then prune)
- Schema versioning with additive/destructive migrations

### Amendment

**Replace LadybugDB with Macrame as the sole storage engine.** All entity, edge, vector, and audit data lives in a single `.codegraph/codegraph.db` file.

#### Macrame Schema for CodeRadar (NEW §10)

```sql
-- Concepts table (Macrame built-in, extended with CodeRadar annotations)
-- Each CodeRadar entity is a Macrame Concept.
-- Entity metadata stored in annotations, not in separate tables.

-- Annotations on concepts carry entity-specific fields:
--   "kind"        → "module" | "class" | "function" | "method" | "import" |
--                    "variable" | "constant" | "type_alias" | "enum" | "field"
--   "file_path"   → source file path (denormalized for query convenience)
--   "language"    → "python" | "typescript" | "rust" | ...
--   "line"        → start line (1-based)
--   "end_line"    → end line
--   "start_byte"  → byte offset of definition start
--   "end_byte"    → byte offset of definition end
--   "name_span_start" / "name_span_end"
--   "body_span_start" / "body_span_end"
--   "params_span_start" / "params_span_end"
--   "signature"   → function/method signature text
--   "docstring"   → extracted docstring
--   "is_async"    → "true" | absent
--   "is_static"   → "true" | absent
--   "decorators"  → NUL-joined list of decorator names
--   "content_hash" → xxHash of file content at extraction time
--   "parse_quality" → "clean" | "partial" | "tainted"

-- Edges table (Macrame built-in, via EdgeAssertion)
-- Edge types map to CodeRadar relationship kinds:
--   "contains"    → structural containment (file→class, class→method)
--   "calls"       → function/method call
--   "imports"     → import relationship
--   "extends"     → class inheritance
--   "implements"  → interface/trait implementation
--   "references"  → variable/constant reference
--   "decorates"   → decorator → decorated entity
--   "instantiates" → constructor call
--   "overrides"   → method override

-- Edge properties (JSON in Macrame's properties field):
--   "confidence"      → resolution confidence (0.0–1.0)
--   "resolution_method" → "stack_graph" | "import_constrained" | "signature_match"
--   "call_site_line"  → line number of the call
--   "call_site_span"  → byte span of the call site
--   "provenance"      → "tree-sitter" | "heuristic" | "framework"

-- Temporal columns (Macrame built-in):
--   valid_from    → when this entity/edge was introduced in the codebase
--   valid_to      → when it was removed (NULL = still present)
--   recorded_at   → when CodeRadar indexed this fact
--   seq_id        → monotonic sequence number for reconstruct()

-- Embedding tables (Macrame built-in, via register_model / upsert_embedding)
-- One table per model, e.g. embeddings_jina_896 for 896-d vectors.
-- Keyed by concept_id (entity id), immutable per version.
-- Not included in transaction_log payloads (Doctrine VII).

-- FTS5 index (Macrame built-in, concepts_fts)
-- Full-text search across entity names, qualified names, signatures, docstrings.
```

#### Entity Lifecycle

```
Creation:
  db.upsert_concept(ConceptUpsert::new(entity_id, content)
      .valid_from(now)
      .annotate("kind", "function")
      .annotate("line", "42")
      ...)

Modification (e.g., body change):
  // Old version is NOT deleted — it's superseded
  db.retire_entity(entity_id, valid_to=now)
  db.upsert_concept(ConceptUpsert::new(entity_id, new_content)
      .valid_from(now)
      ...)  // same id, new valid_from — old version still queryable via as_of()

Removal (e.g., function deleted from file):
  db.retire_entity(entity_id, valid_to=now)
  // Entity no longer appears in current queries, but as_of(past) still sees it

Edge assertion:
  db.assert_edge(EdgeAssertion::new(caller_id, callee_id, "calls")
      .valid_from(now)
      .weight(confidence)
      .property("resolution_method", "stack_graph")
      .property("call_site_line", "142"))
```

#### Vector Search

```rust
// Register embedding model (once per database)
db.register_model("jina-code-896", 896, DistanceMetric::Cosine)?;

// Store embedding
db.upsert_embedding("jina-code-896", entity_id, &vector)?;

// Search
let results = db.vector_search("jina-code-896", &query_vector, 10)?;

// Hybrid search (vector + keyword)
let results = db.hybrid_search("jina-code-896", &query_vector, "auth login", 10)?;
```

#### Query Patterns

```rust
// Find all functions in a file
let funcs = db.traverse()
    .start_node(file_id)
    .edge_type("contains")
    .filter_annotation("kind", "function")
    .execute(&conn, None)?;

// Find callers of a function (reverse traversal)
let callers = db.traverse()
    .start_node(func_id)
    .edge_type("calls")
    .direction(Incoming)
    .max_depth(3)
    .execute(&conn, None)?;

// Temporal: what did the call graph look like last week?
let past = db.as_of("2026-08-01T00:00:00Z")?;
let old_callers = past.traverse()
    .start_node(func_id)
    .edge_type("calls")
    .direction(Incoming)
    .execute(&conn, None)?;
```

### What is removed

- LadybugDB as a dependency
- All Cypher DDL (CREATE NODE TABLE, CREATE REL TABLE)
- HNSW index setup (CALL create_hnsw_index) — replaced by Macrame DiskANN
- MutationLog table with tiered retention — replaced by Macrame transaction_log
- Schema versioning code — replaced by Macrame migration rungs
- LadybugDB Python driver dependency

### What stays

- The conceptual entity model (Module, Class, Function, etc.) — stored as Macrame concepts
- The edge types (contains, calls, imports, etc.) — stored as Macrame edge assertions
- Embedding configuration (§10.3) — model name, dimension, batch size unchanged
- `EmbeddingDedup` logic (§13.1) — content-addressed dedup still runs, now stores via Macrame

---

## A8: Flat-Buffer FFI Contract

### Original (§8, Appendix A)

CodeRadar v3.3 passes rich Rust structs through PyO3 per entity: `ParsedFile`, `ParsedFunction`, `ParsedClass`, `ParsedImport`, `ParsedReference`, each with `#[pyo3(get)]` fields crossing the FFI boundary.

### Amendment

**Replace per-entity PyO3 struct passing with a flat-buffer contract** — one boundary crossing per file, regardless of entity count.

Inspired by CodeGraph's kernel architecture (5 binary buffers: meta, nodes, edges, refs, arena), adapted for CodeRadar's richer entity model.

#### Wire Format

```
extract_file(path, content, language) → (meta: Buffer, entities: Buffer, edges: Buffer, arena: Buffer)

meta (40 bytes):
  0   u32  ABI version (= 1)
  4   u32  entity count
  8   u32  edge count
  12  u32  arena byte length
  16  u32  errors-JSON arena offset (NONE = no errors)
  20  u32  errors-JSON byte length
  24  f64  kernel-side wall duration (ms)
  32  [8]  reserved

entity row (128 bytes):
  0   u8   EntityKind index
  1   u8   Language index
  2   u16  flags (is_async:1, is_static:2, is_exported:4, is_type_checking_only:8,
             is_generator:16, has_docstring:32)
  4   u32  start_line
  8   u32  end_line
  12  u32  start_column
  16  u32  end_column
  20  str  name (offset, len into arena)
  28  str  qualified_name
  36  str  id (dotted path)
  44  str  docstring
  52  str  signature
  60  str  return_type
  68  str  decorators (NUL-joined)
  76  str  parent_id (enclosing entity id, NONE if top-level)
  84  u64  signature_hash
  92  u64  body_hash
  100 u64  content_hash
  108 str  extra_json (language-specific metadata)
  116 u32  byte_span_start
  120 u32  byte_span_end
  124 u32  name_span_start  // inline for mutation targeting
  128 (end)

edge row (56 bytes):
  0   u32  source entity row index
  4   u32  target entity row index (NONE → use target_id_str)
  8   u8   EdgeKind index
  9   u8   provenance (0=tree-sitter, 1=resolution, 2=heuristic, 3=framework)
  10  u16  pad
  12  f32  confidence
  16  u32  line
  20  u32  column
  24  str  metadata_json (resolution_method, call_site_span, etc.)
  32  str  source_id_str
  40  str  target_id_str
  48  u32  call_site_span_start
  52  u32  call_site_span_end
  56 (end)

ref row (48 bytes):  // unresolved references for Python-side resolution
  0   u32  from_entity row index
  4   u8   ReferenceKind
  5   u8   flags
  6   [2]  pad
  8   u32  line
  12  u32  column
  16  str  reference_name
  24  str  context (enclosing function/module name)
  32  str  candidates (NUL-joined possible targets)
  40  u32  name_span_start
  44  u32  name_span_end
  48 (end)
```

#### Rationale

CodeGraph's flat-buffer approach yields a 5–8× reduction in FFI overhead. CodeRadar's spec §22.1a already targets this: "PyO3 crossings per file: 1". The flat buffer delivers it. Additionally:

- **Zero-copy decoding** on the Python side (read u32 offsets, slice the arena)
- **One GIL release** per file instead of per entity
- **Deterministic layout** — ABI version gating prevents mismatched decoder crashes
- **Byte spans inline** — mutation engine gets span data without extra FFI calls

---

## A9: Partial Coverage Principle

### Amendment (NEW: add to §6.1)

Add the following principle after the five-layer cascade description:

> **6.1a Partial Coverage Principle**
>
> A partially-resolved flow is **worse** than an unresolved one. When the resolution cascade resolves some edges in a call chain but leaves gaps, the agent receives an incomplete graph and falls back to reading source files — often reading *more* files than if it had no graph at all, because the resolved edges surface the next hop without providing its target.
>
> **Rule:** Every resolution path (L1→L5) must either close a flow end-to-end or mark the entire chain as unresolved with a specific, actionable reason. Never emit a graph where a `calls` edge points to an entity whose own `calls` edges are missing — this creates a "dead end" that triggers agent file-reading.
>
> **Validation:** For each framework and language, test a canonical end-to-end flow (e.g., HTTP request → router → handler → service → database). The graph must connect all hops, or report the specific boundary where resolution failed. Measured by: `codegraph_explore` with the flow's symbol names returns a complete path (0 Read/Grep by the agent).

This principle is derived from CodeGraph's production experience: bridging React's `setState`→`render` but not `render`→child component *increased* agent reads from 0–2 to 5–7. Only closing the full flow end-to-end produced clean runs.

---

## A10: Query Language Adaptation

### Original (§7.1–7.3)

CodeRadar v3.3 specifies two query interfaces:
- **Pest grammar** for in-memory structural queries (§7.1)
- **Cypher** (delegated to LadybugDB) for persisted queries with vector search (§7.3)

### Amendment

**Retain the Pest grammar for structural queries.** Pest queries compile to Macrame API calls:

| Pest Query | Macrame Translation |
|-----------|-------------------|
| `functions where is_async == true` | `traverse().filter_annotation("kind", "function").filter_annotation("is_async", "true")` |
| `classes where method_count > 20` | `traverse().filter_annotation("kind", "class")` + Python-side count |
| `functions where caller_count == 0` | `traverse().filter_annotation("kind", "function")` + `traverse().incoming("calls").count() == 0` |
| `classes select module.name, count(*) group by module.name` | `traverse()` + Python-side aggregation |
| `calls where unresolved_reason == "TypeInferenceRequired"` | `traverse().edge_type("calls").filter_property("resolution_method", "unresolved")` |

**Drop Cypher templates (§7.3).** Macrame has no Cypher support. The six template queries (scope exploration, impact analysis, call chain, similarity search, dependency graph, definition lookup) are reimplemented as Macrame API calls:

| Template | Macrame Implementation |
|----------|----------------------|
| Scope exploration | `traverse().start_node(file).edge_type("contains").max_depth(2)` + `vector_search()` |
| Impact analysis | `traverse().start_node(target).edge_type("calls").direction(Incoming).max_depth(depth)` |
| Call chain | `traverse().start_node(src).edge_type("calls").max_depth(n).filter_target(target)` |
| Similarity search | `vector_search(model, query_vector, top_k)` |
| Dependency graph | `traverse().start_node(file).edge_type("imports").max_depth(depth)` |
| Definition lookup | `search_concepts(name)` + `traverse().start_node(id).edge_type("*").max_depth(1)` |

**Optional: SQLite for ad-hoc structural queries.** For complex filters not expressible via Macrame's annotation system (e.g., `where line_count > 50 AND is_async == true AND decorators contains "deprecated"`), a small SQLite database mirrors entity metadata keyed by concept ID. This is optional — start with Macrame-only and add SQLite if structural query performance becomes a bottleneck.

---

## A11: Agent Interface Design (NEW §26)

### 26.1 Tool Design Principles

Derived from CodeGraph's production experience with 9 agent targets over 2+ years:

1. **Precise input, precise output.** Agents reliably call tools with symbol names (e.g., `codegraph_explore("UserService.create")`). They do NOT reliably pick among multiple specialized tools. Design one primary tool that takes symbol names and returns complete flows.

2. **Adapt the tool to the agent — don't try to change the agent.** The MCP `initialize` instructions and tool descriptions are low-salience channels. Agents won't change their tool-choice behavior based on description wording. Meet them where they already go.

3. **Errors teach abandonment.** One or two `isError: true` responses early in a session and the agent stops calling the tool entirely. Return success-shaped responses carrying guidance for every expected condition (symbol not found → `{found: false, suggestions: [...]}`, not an error).

4. **Keep the surface small.** CodeGraph removed `codegraph_context` (fuzzy input, wrong results) and `codegraph_trace` (under-picked by agents). The tool surface is now: `explore` (primary), `node` (depth), `search` (discovery), `affected` (impact). Start with these four; add only when an agent reliably asks for something those can't answer.

### 26.2 Recommended MCP Tool Surface

```
codegraph_explore(symbols: string[], direction?: "downstream" | "upstream" | "both")
  → primary tool, 80%+ of agent calls
  → returns: flow path, source snippets, confidence annotations

codegraph_node(id: string, include_neighbors?: boolean)
  → depth tool, called after explore identifies a specific entity
  → returns: full entity details, immediate callers/callees

codegraph_search(query: string, kind?: string, top_k?: number)
  → discovery tool, hybrid keyword + vector search
  → returns: ranked entity list with snippets

codegraph_affected(id: string, max_depth?: number)
  → impact analysis, "what calls this, transitively?"
  → returns: tree of dependent callers
```

### 26.3 Explore Budget Scaling

Output size scales with repo size to stay within agent context windows:

| Indexed Files | Max Explore Calls | Max Output Chars |
|--------------|-------------------|------------------|
| < 500 | 1 | 18,000 |
| < 5,000 | 2 | 28,000 |
| < 15,000 | 3 | 35,000 |
| < 25,000 | 4 | 38,000 |
| ≥ 25,000 | 5 | 38,000 |

**Invariant:** A larger tier must never get a smaller per-file budget than a smaller tier. Test this on every change.

---

## A12: Validation Methodology (NEW §27)

### 27.1 Extraction Parity Gate

Before claiming a language is supported, prove byte-identical extraction against a reference implementation:

1. Index a real repository (small: ~150 files, medium: ~3,000 files, large: ~10,000 files)
2. Compare every entity and edge against the reference (wasm extractor or golden file)
3. Files with parse errors route to a recovery path — never silently mis-extracted
4. Gate passes when all three repo sizes produce identical graphs

### 27.2 Agent A/B Evaluation

Before claiming a feature works (resolution, queries, mutations), validate with real agent runs:

1. **Pick a canonical flow** for the feature (e.g., "how does a login request reach the database?")
2. **Run with-vs-without CodeRadar**, minimum 2 runs per arm (variance is high)
3. **Metrics:** duration, total tool calls, Read count, Grep count
4. **Pass bar:** 0 Read/Grep for the flow question; runs faster with CodeRadar than without

### 27.3 Incremental Update Equivalence

The spec's `incremental_matches_full` property test (§23.2) is the gold standard. Extend it:

1. Generate random edit sequences (single-token renames, body rewrites, file splits, moves, corrupted saves)
2. Apply incrementally AND run full re-analysis from scratch
3. Assert: entity count, edge count, and graph connectivity are identical
4. Run on all three reference repo sizes

### 27.4 Mutation Validation

For each mutation tool:
1. **Dry-run plan** against a real repo → verify diff preview is syntactically valid
2. **Apply + re-index** → verify entity/edge counts are stable (no leakage, no orphaned edges)
3. **Agent recovery** → inject a deliberate syntax error, verify the agent can repair from the `SyntaxDiagnostic[]` response
4. **Hash guard** → modify a file externally between plan and apply, verify `RejectedStale`

---

## A13: Framework Resolver Interface (NEW §28)

### 28.1 Pattern

CodeGraph's 28 framework resolvers follow a three-method interface. Adopt this for CodeRadar's Python framework support (§4.3, §6 extensions):

```rust
trait FrameworkResolver {
    /// Can this resolver handle this project?
    /// Check: requirements.txt, pyproject.toml, setup.py, or sentinel files (manage.py)
    fn detect(&self, project_root: &Path) -> bool;

    /// Does this resolver claim to resolve this reference?
    /// Return true if the reference name matches a framework pattern.
    fn claims_reference(&self, name: &str) -> bool;

    /// Extract synthetic nodes and edges from a single file.
    /// Called during indexing for every file in a detected project.
    fn extract(&self, file_path: &str, source: &str) -> FrameworkExtraction;

    /// Resolve a single reference that claims_reference returned true for.
    /// Returns the target entity ID and confidence, or None if unresolved.
    fn resolve(&self, ref: &UnresolvedRef, graph: &CodeGraph) -> Option<ResolvedTarget>;
}

struct FrameworkExtraction {
    nodes: Vec<SyntheticNode>,     // route, component, etc.
    edges: Vec<SyntheticEdge>,     // route→handler, component→render, etc.
}
```

### 28.2 Phase 1 Resolvers (Python)

| Resolver | detect() | extract() | resolve() |
|----------|----------|-----------|-----------|
| **Django** | `manage.py` exists | `path()`/`re_path()`/`url()` → route nodes + handler edges | `*Model` → models.py classes, `*View` → views.py classes |
| **Flask** | `@app.route` patterns | `@app.route(...)` decorators → route nodes + handler edges | Blueprint registration |
| **FastAPI** | `APIRouter` imports | `@app.get(...)` / `@router.post(...)` → route nodes | Dependency injection chains |

### 28.3 Synthetic Edge Provenance

All framework-synthesized edges carry `provenance: "heuristic"` and `metadata.synthesizedBy: "<resolver_name>"`. Agents see these annotations in explore output so they can distinguish structural edges from synthesized ones.

---

## A14: Per-File Safety Valve

### Amendment (modify §4.5)

Add after the parse quality classification:

> **4.5a Deferred-to-Recovery Path**
>
> When a file's parse tree contains errors (`tree.root_node().has_error()`), the extractor returns a typed deferral rather than producing potentially-incorrect entities. The ingestion pipeline falls back to a recovery extractor (slower, more conservative, handles error recovery canonically). This is a per-file safety valve — no file is ever silently mis-extracted.
>
> The deferral is NOT an error. It produces a `ParseQuality::Deferred` marker. The file's previous graph slice (if any) is retained. On the next successful parse, the graph is updated normally.

---

## A15: Entity Handle Lifetimes

### Amendment (modify §8.1)

Replace:

> Wrappers carry `(epoch, SlotMap key)`; calls that find a stale epoch raise `coderadar.StaleHandle`

With:

> Wrappers carry `(entity_id, as_of_timestamp)`. A call that finds the entity has changed since `as_of_timestamp` receives the latest version with a `stale: true` flag — never an error. The caller decides whether to use the stale data or re-fetch.
>
> This follows the agent-trust principle (§26.1.3): expected staleness is guidance, not a failure.

---

## A16: Full Macrame Schema (NEW §10 — replaces original §10)

*See [macrame-evaluation.md](macrame-evaluation.md) §"Macrame Schema for CodeRadar" for the complete schema definition. This section is extracted here for the consolidated spec.*

### 10.1 Database Location

```
.codegraph/codegraph.db    # Single file, Macrame format
.codegraph/backups/         # Snapshot backups (Macrame snapshot cadence)
```

### 10.2 Entity Storage

All CodeRadar entities are stored as Macrame Concepts. Entity metadata is carried in annotations:

| Annotation Key | Type | Example |
|---------------|------|---------|
| `kind` | string | `"function"`, `"class"`, `"method"` |
| `file_path` | string | `"src/auth/login.py"` |
| `language` | string | `"python"` |
| `line` | u32 | `42` |
| `end_line` | u32 | `58` |
| `start_byte` | u32 | `1200` |
| `end_byte` | u32 | `1850` |
| `name_span` | string | `"1200..1215"` |
| `body_span` | string | `"1230..1840"` |
| `params_span` | string | `"1216..1229"` |
| `signature` | string | `"def login(email: str, password: str) -> User:"` |
| `docstring` | string | `"Authenticate a user..."` |
| `is_async` | string | `"true"` (absent if false) |
| `is_static` | string | `"true"` |
| `decorators` | string | `"staticmethod\0cache\0deprecated"` |
| `content_hash` | string | `"a1b2c3d4e5f6"` |
| `parse_quality` | string | `"clean"` |
| `return_type` | string | `"User"` |

### 10.3 Edge Storage

All relationships are Macrame EdgeAssertions:

| Edge Type | Source Kind | Target Kind | Properties |
|-----------|------------|-------------|------------|
| `contains` | file, class, module | class, function, method, variable | — |
| `calls` | function, method | function, method | confidence, resolution_method, line, call_site_span |
| `imports` | file | file, module | module_name, is_relative |
| `extends` | class | class | confidence |
| `implements` | class | class, interface | confidence |
| `references` | function, method | variable, constant | value_ref (bool) |
| `decorates` | function, class | function | — |
| `instantiates` | function, method | class | confidence, line |
| `overrides` | method | method | confidence |

### 10.4 Vector Storage

```rust
// One-time setup
db.register_model("code-embeddings-896", 896, DistanceMetric::Cosine)?;

// Per-entity storage (called by Python embedding pipeline)
db.upsert_embedding("code-embeddings-896", entity_id, &vector)?;

// Search
let similar = db.vector_search("code-embeddings-896", &query_vector, 10)?;
```

### 10.5 Temporal Queries

```rust
// Current state (default)
let graph = db.traverse().start_node(root).execute(conn, None)?;

// State at a past valid time
let past = db.as_of("2026-07-01T00:00:00Z")?;

// What we believed at a past transaction time
let belief = db.reconstruct("2026-08-01T12:00:00Z")?;
```

### 10.6 Integrity

```rust
// Verify derivative state matches ledger
db.verify_integrity()?;

// Rebuild derivative state from ledger (idempotent)
db.rebuild_current()?;

// Verify snapshot chain consistency
db.verify_snapshot_chain()?;
```

---

## Summary of Changes

| Original v3.3 | Amended v3.4 | Rationale |
|---------------|-------------|-----------|
| LadybugDB (Kùzu Cypher) | Macrame (bitemporal graph ledger) | Author on team, simpler, embedded, bitemporal |
| ArcSwap MVCC + epochs | Macrame `as_of(ts)` / `reconstruct(ts)` | Free temporal queries, no arena cloning |
| WAL + PatchTransaction | Macrame assertion model | Immutable assertions, no rollback code |
| Per-entity PyO3 structs | Flat buffers (one per file) | 5–8× FFI reduction, proven by CodeGraph |
| Cypher templates | Macrame API calls | No Cypher engine needed |
| MutationLog retention tiers | Macrame transaction_log | Built-in, immutable, no pruning code |
| HNSW via LadybugDB | DiskANN via Macrame | Faster, same file, no separate DB |
| `StaleHandle` exception | `stale: true` flag | Agent-trust principle |
| No agent validation | Agent A/B methodology | Production-proven by CodeGraph |
| No framework resolver interface | `FrameworkResolver` trait | 28-resolver pattern from CodeGraph |

---

*End of Amendment v3.4. Apply to CodeRadar v3.3 Consolidated Specification.*
