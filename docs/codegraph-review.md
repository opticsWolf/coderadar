# CodeGraph-Main Review — What CodeRadar Learned & Adopted

**Date:** 2026-08-08 (original), updated 2026-08-10 (v0.4.1)
**Source:** `D:\User\Documents\Python\codegraph-main`
**For:** CodeRadar v0.1.0 → v0.4.1 implementation
**Status:** 11 techniques adopted across v3.6 + v0.4.1 (see `docs/v3.6-codegraph-plan.md` for item-level status)

---

## Executive Summary

CodeGraph is a **shipping, real-world product** (~3,000+ GitHub stars, npm published, 15+ agents supported) that does what CodeRadar's spec designs. It has 2+ years of production learning baked into its architecture, validation methodology, and edge-case handling. This document catalogs the architectural patterns, design decisions, and hard-won lessons that should directly inform CodeRadar's implementation.

---

## 1. Architecture Comparison

| Dimension | CodeGraph (real) | CodeRadar (spec v3.3) |
|-----------|-----------------|----------------------|
| **Runtime** | Node.js (bundled, no external dep) + Rust napi kernel | Python + Rust PyO3 |
| **Storage** | SQLite (WAL, FTS5) via `node:sqlite` | LadybugDB (Kùzu Cypher + HNSW vectors) |
| **Extraction engine** | Rust napi kernel with flat-buffer output | Rust PyO3 with in-process arenas |
| **Resolution** | TS-side: import-resolver, name-matcher, framework synthesizers | Rust-side: Stack Graphs L1 → Import L2 → Signature L3, Python for L4/L5 |
| **Mutation** | None (read-only) | AST-aware MutationEngine (4 tools) |
| **Incremental update** | Full re-index on change (watcher + reindex) | Incremental diff algorithm with WAL |
| **Language count** | 25+ languages, 28 framework resolvers | 12 Tier 1 + Tier 2/3 planned |
| **Agent surface** | MCP server (tools: explore, node, search, affected, trace) | LLM tool router for mutations |
| **Graph shape** | Flat nodes + edges in SQLite, FTS5 for text search | In-memory ArcSwap arenas + persistent LadybugDB |

### Key takeaway

CodeGraph is simpler than CodeRadar's spec in several dimensions (no incremental updates, no mutations, flat SQLite storage) — but it ships and works at scale. CodeRadar's more ambitious design (incremental updates, semantic resolution cascade, mutations) carries implementation risk; CodeGraph proves which parts deliver the most value per unit of complexity.

---

## 2. The Flat Buffer Contract — Brilliant and Should Be Copied

CodeGraph's Rust kernel communicates with TypeScript through **five flat binary buffers** per file: `meta`, `nodes`, `edges`, `refs`, `arena`. This is the single most elegant architectural decision in the codebase.

### How it works

```
Rust kernel                          TypeScript
───────────                          ──────────
parse source with tree-sitter
walk tree, emit into flat Tables
serialize as 5 Buffer objects  ────►  decode.ts deserializes into
                                     ExtractionResult (identical to
                                     the wasm extractor's output)
```

All strings go into a single UTF-8 **arena** (append-only byte buffer). Every string field is an `(offset, len)` pair — u32 offset into the arena. Fixed-width rows (96-byte nodes, 44-byte edges, 40-byte refs) allow zero-copy decoding in TS.

### Why this matters for CodeRadar

CodeRadar currently passes rich Rust structs through PyO3 — each entity crossing the FFI boundary incurs serialization overhead. CodeGraph's flat-buffer approach does **one boundary crossing per file** regardless of entity count. This is a 5–8× reduction in FFI overhead (matching the spec's own performance targets in §22.1a).

**Recommendation:** Replace the per-entity PyO3 struct passing with a flat-buffer contract. PyO3 supports `PyBuffer` natively. One `extract_file(path, content, language) -> Buffer` call per file, decoded on the Python side into the same in-memory arena structures.

---

## 3. Validation Methodology — The Gold Standard

CodeGraph's CLAUDE.md and SEARCH_QUALITY_LOOP.md document a validation methodology that CodeRadar's spec only sketches (§23.2). CodeGraph does this for **every language and framework** before claiming support. Key practices:

### 3.1 The Parity Gate

Every language extraction must be **byte-identical** between the Rust kernel and the wasm fallback before routing to the kernel. This is enforced by `scripts/kernel-parity.mjs` which runs extract-and-diff on real repos (express, excalidraw, vscode). A language stays on wasm until it passes.

**For CodeRadar:** The spec's `incremental_matches_full` property test (§23.2) is the right idea. CodeGraph extends it with:
- Per-language fixture repos (small/medium/large)
- Byte-level diff against a reference implementation
- Deferred files (parse errors route to recovery path)

### 3.2 Agent A/B Evaluation

CodeGraph validates every change with **real agent runs** against real repos:
- Same prompt, with-vs-without codegraph
- Minimum 2 runs per arm (variance is high)
- Metrics: duration, tool calls, Read count, Grep count
- **Pass bar: 0 Read/Grep for flow questions**

**For CodeRadar:** The spec has no agent-level evaluation. This should be added before claiming mutation or query capabilities work. The mutation engine's "verified, atomic, audited" guarantees (§11.6) are meaningless without end-to-end agent validation.

### 3.3 Dynamic Dispatch Coverage Principle

> **"Partial coverage is WORSE than none."** — CodeGraph CLAUDE.md

Bridging one dynamic-dispatch boundary (e.g., React `setState`→`render`) but not the next (JSX child component) **increased** Read/Grep usage because the agent drilled into the gap. Only closing the full flow end-to-end produced clean runs.

**For CodeRadar:** The resolution cascade (L1→L5) risks this. If L1 resolves 95% of references but L2/L3 fail on the remaining 5%, the agent gets partial information and falls back to reading files. The cascade must either resolve the full flow or clearly mark unresolved edges with reasons the agent can act on (not just `Unresolved`).

---

## 4. The Agent Interface — Precise Input, Precise Output

### 4.1 Tool Design Principles (Hard-Won)

CodeGraph removed several tools after discovering they didn't work:

| Removed Tool | Why It Failed |
|-------------|---------------|
| `codegraph_context` | Took a **description** (fuzzy), surfaced wrong features. Agents need **precise symbol names**. |
| `codegraph_trace` | Under-picked by agents. `explore` does the same job better. |

**Principle: "Adapt the tool to the agent — don't try to change the agent."**

Agents reliably call `codegraph_explore` with symbol-name queries. They do NOT reliably pick among multiple specialized tools. The tool surface must meet the agent where it already goes.

### 4.2 Error Handling That Doesn't Burn Trust

> **"One or two `isError: true` responses early in a session and the agent stops calling codegraph entirely."**

Every expected condition (project not indexed, symbol not found, file not in index) returns a **success-shaped response** carrying guidance — never an error. `isError` is reserved for genuine malfunctions.

**For CodeRadar:** The mutation router's error handling should follow this pattern. A `HashMismatch` should return a structured result the LLM can act on (re-fetch context, retry), not an error that makes the agent abandon the tool.

### 4.3 Explore Budget Scaling

CodeGraph scales its explore output with repo size using fixed tiers:

| Files | Explore calls | Chars/call |
|-------|--------------|------------|
| <500  | 1 | 18K |
| <5000 | 2 | 28K |
| <15000 | 3 | 35K |
| ≥25000 | 5 | 38K |

The invariant: **a larger tier must never get a smaller per-file budget than a smaller tier.** A regression here silently forces agents back to Read.

---

## 5. SQLite Over GraphDB — Pragmatism Wins

CodeGraph uses **SQLite with FTS5** (full-text search), not a graph database. The Cypher templates in CodeRadar's spec (§7.3) assume LadybugDB/Kùzu. CodeGraph proves that for this use case:

- **SQLite is sufficient.** Nodes, edges, and FTS5 index cover all query patterns.
- **No vector search is needed.** Symbol-name matching + framework resolvers + import resolution handle semantic search adequately. Embeddings are deferred/optional.
- **Zero operational overhead.** SQLite is a single file, no daemon, no connection pool.

**For CodeRadar:** Consider SQLite as the primary store (at least for Phase 1). LadybugDB can remain as the "vector search when needed" upgrade path. The spec already acknowledges this in Open Question 5 (§26, Appendix F.3).

---

## 6. Framework Resolvers — The Secret Sauce

CodeGraph has **28 framework-specific resolvers** that synthesize edges between framework constructs (routes → handlers, ORM models → queries, React state → render). These are NOT tree-sitter extraction — they're regex + AST pattern matching against known framework conventions.

Example from `python.ts` (Django):
```typescript
// path('url', handler, name=...)
const routeRegex = /\b(path|re_path|url)\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+(?:\s*\([^)]*\))?)/g;
```

Each resolver has three hooks:
1. **`detect(context)`** — is this framework used in the project?
2. **`resolve(ref, context)`** — can this unresolved reference be resolved by framework knowledge?
3. **`extract(filePath, content)`** — emit synthetic route/component nodes + edges

**For CodeRadar:** The spec's decorator table (§4.3) and known-decorator system is a start. CodeGraph's framework resolvers go much further — they handle URL routing, ORM dispatch, React re-render chains, and cross-language bridging (Swift↔ObjC). The plugin API deferred in §26/Appendix F.2 should follow this pattern: regex + AST pattern matching with a detect/resolve/extract interface.

---

## 7. The `src/textutil.rs` / Utility Layer

CodeGraph's kernel has a small but dense utility module (`textutil.rs`) with functions that appear in every extractor:

- `line_starts(source)` — pre-computed byte offsets of each line start (used for constant-time line/column lookups)
- `col16(source, line_starts, row, byte_offset)` — UTF-16 column position (for JS/TS column convention)
- `is_generated_file(path)` — detects `.generated.`, `.pb.go`, `.g.dart`, etc.
- `simple_name()` / `qualified_import()` — regex constants for common name patterns
- `init_signature(text)` — first line of a value expression, truncated
- `paren_conversion()` — regex for converting `foo(...)` to `foo`

**For CodeRadar:** These small, frequently-used utilities should be centralized in the Rust core (similar to CodeRadar's `SpanExtractor`). CodeRadar currently scatters these across modules.

---

## 8. Specific Patterns Worth Adopting

### 8.1 The `definedFnNames` / `fnRefCands` Pattern

CodeGraph's Python extractor collects function references differently from other references:

1. During the tree walk, candidate function references are **batched** into `fn_ref_cands: Vec<Cand>`
2. At the end of extraction, `flush_fn_ref_candidates()` resolves them against `defined_fn_names` and `imported_names`
3. Only references to **known** symbols (defined in this file or imported) are emitted

This prevents the graph from being polluted with references to external/builtin symbols that can't be resolved, while still capturing intra-file and import-based call edges.

**For CodeRadar:** The `calls: Vec<UnresolvedRef>` field on `Function` should use this pattern — batch collection, then flush with name resolution at file scope before committing to the graph.

### 8.2 Deferred-to-Wasm Error Recovery

When the Rust kernel encounters a parse error (`tree.root_node().has_error()`), it returns `Err("defer: ...")` and the TS layer falls back to the wasm extractor. This is a **per-file safety valve** — no file is ever silently mis-extracted.

**For CodeRadar:** The spec's `ParseQuality::Tainted` and tainted-update rejection (§4.5, §19.2) serves the same purpose. CodeGraph adds: the fallback path is always available, and it's deterministic.

### 8.3 The Shadow Prune in `flush_value_refs`

CodeGraph's Python value-reference pass prunes **shadowed** names — if a name is re-declared within a scope more times than it appears at the top level, it's removed from the value-reference targets. This prevents spurious references to shadowed local variables.

**For CodeRadar:** The scope-chain resolution (§5.3.3) should include shadow detection. The existing walker's `FrameKind` stack can track this.

### 8.4 Cooperative Yield in Resolution

`src/resolution/cooperative-yield.ts` splits resolution into chunks that yield to the event loop every N references. This prevents the main thread from blocking during resolution of large codebases.

**For CodeRadar:** The query iterator's `check_interval` (§7.2a) serves this purpose for queries. Resolution during ingestion should have a similar mechanism.

---

## 9. What CodeRadar Should NOT Copy

### 9.1 Regex-Based Framework Resolvers

CodeGraph's framework resolvers use regex on source text, not AST analysis. This is fragile — it breaks on multiline arguments, string interpolation, and formatting variations. CodeRadar's tree-sitter-based approach (extracting decorator semantics from the AST directly) is more robust.

### 9.2 No Incremental Updates

CodeGraph re-indexes entire files on change. For large repos, this is fine because extraction is fast (~1-5ms/file in Rust). But for repositories with 100K+ files, the spec's incremental diff algorithm (§5) would be valuable.

### 9.3 SQLite FTS5 Text Search

CodeGraph uses FTS5 for text search. It works but is not semantic. CodeRadar's planned embedding-based search (even if deferred) would be more powerful for "find functions similar to X" queries.

---

## 10. Implementation Prioritization for CodeRadar

Based on CodeGraph's proven value, here is the recommended implementation order:

### Phase 1 (high value, proven by CodeGraph)

1. **Flat-buffer extraction kernel** — one FFI boundary per file, not per entity
2. **SQLite storage** with node/edge/file tables — simpler than LadybugDB for Phase 1
3. **Import resolution** (Python `import`/`from` statements) — the highest-value resolution
4. **Call graph** — intra-file function references + import-based call resolution
5. **MCP server** exposing `explore`, `node`, `search` tools — the agent interface
6. **Framework resolvers for Python** (Django, Flask, FastAPI) — follow CodeGraph's detect/resolve/extract pattern

### Phase 2 (spec design, lower complexity than spec assumes)

7. **Incremental update** — diff-based file patching
8. **Stack Graphs resolution** — for multi-file call resolution beyond imports
9. **Embedding pipeline** — vector search for similarity queries
10. **Pest query grammar** — in-memory query language

### Phase 3 (CodeRadar-unique, highest complexity)

11. **Mutation engine** — the four refactoring tools (unique to CodeRadar)
12. **LadybugDB integration** — Cypher queries + vector indexes
13. **LSP warm pool** — optional resolution fallback

---

## 11. Key Numbers from CodeGraph

| Metric | Value | Source |
|--------|-------|--------|
| Kernel ABI version | 2 | `buffers.rs` |
| Node row size | 96 bytes | `buffers.rs` |
| Edge row size | 44 bytes | `buffers.rs` |
| Ref row size | 40 bytes | `buffers.rs` |
| Node kinds | 23 | `buffers.rs` |
| Edge kinds | 12 | `buffers.rs` |
| Languages (kernel) | 17 | `lib.rs` match arms |
| Frameworks (resolvers) | 28 | `resolution/frameworks/` |
| Agent targets | 9 (Claude, Cursor, Codex, opencode, Hermes, Gemini, Antigravity, Kiro, Copilot) | `installer/targets/` |
| Explore budget tiers | 5 (<500, <5K, <15K, <25K, ≥25K files) | `mcp/tools.ts` |
| Validation: agent runs per change | ≥2 runs/arm, ≥3 flow prompts | `CLAUDE.md` |
| Test count | ~47 parameterized installer tests, kernel-parity for each language | `__tests__/` |
| Cost reduction (with codegraph) | 35% cost, 57% tokens, 46% time, 71% tool calls | `CLAUDE.md` |

---

## 12. Files Worth Studying in Detail

| File | Why |
|------|-----|
| `codegraph-kernel/src/python.rs` | Complete Rust tree-sitter extraction walker — clean, well-structured, handles edge cases |
| `codegraph-kernel/src/buffers.rs` | Flat buffer wire format — the contract between Rust and TS |
| `codegraph-kernel/src/ids.rs` | Deterministic node ID scheme (`kind:hash32`, `file:<path>`) |
| `src/resolution/frameworks/python.ts` | Framework resolver pattern (Django/Flask/FastAPI) |
| `src/resolution/callback-synthesizer.ts` | Dynamic dispatch edge synthesis (EventEmitter, React, ORM) |
| `src/mcp/tools.ts` | Agent tool implementations (explore, node, search, affected) |
| `src/mcp/server-instructions.ts` | The single source of truth for agent-facing guidance |
| `src/context/index.ts` | Context builder — formats graph results for LLM consumption |
| `src/extraction/kernel/decode.ts` | TS-side flat buffer decoder |
| `docs/design/rust-kernel-migration-plan.md` | How they migrated from wasm to native Rust — process and gates |
| `CLAUDE.md` | Developer onboarding, validation methodology, dynamic dispatch coverage playbook |

---

*End of review.*
