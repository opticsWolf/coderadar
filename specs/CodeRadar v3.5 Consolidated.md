# CodeRadar — Consolidated Architecture Specification v3.5

> **Status:** Consolidated from v3.3 + v3.4 Amendment + v3.4 Review Response. Production review of the shipping CodeGraph product and adoption of Macrame as the storage engine have been incorporated.
> **Scope:** A Rust core (tree-sitter extraction, diff engine, resolution cascade, mutation engine, in-memory projected graph) + Macrame embedded bitemporal database + Python shell (CLI, GraphRAG, LSP pool, embedding pipeline).
> **Date:** 2026-08-08

---

## Table of Contents

1. [Overview & Design Pillars](#1-overview--design-pillars)
2. [Architecture](#2-architecture)
3. [Data Models](#3-data-models)
4. [Tree-Sitter Extraction Layer](#4-tree-sitter-extraction-layer)
5. [Incremental Update Algorithm](#5-incremental-update-algorithm)
6. [Semantic Resolution Engine](#6-semantic-resolution-engine)
7. [Query Engine](#7-query-engine)
8. [Python API & FFI Contract](#8-python-api--ffi-contract)
9. [Concurrency and Storage Architecture](#9-concurrency-and-storage-architecture)
10. [Macrame Storage Engine](#10-macrame-storage-engine)
11. [AST-Aware Mutation Engine](#11-ast-aware-mutation-engine)
12. [Git Integration](#12-git-integration)
13. [Embedding & GraphRAG Pipeline](#13-embedding--graphrag-pipeline)
14. [LSP — Optional Persistent Warm Pool](#14-lsp--optional-persistent-warm-pool)
15. [Configuration](#15-configuration)
16. [Command-Line Interface](#16-command-line-interface)
17. [Watch Mode](#17-watch-mode)
18. [Visualizers](#18-visualizers)
19. [Error Handling and Fault Tolerance](#19-error-handling-and-fault-tolerance)
20. [Multi-Language Support](#20-multi-language-support)
21. [Observability & Diagnostics](#21-observability--diagnostics)
22. [Performance Targets & Benchmarking](#22-performance-targets--benchmarking)
23. [Testing Strategy](#23-testing-strategy)
24. [Build and Distribution](#24-build-and-distribution)
25. [Out of Scope](#25-out-of-scope)
26. [Agent Interface Design](#26-agent-interface-design)
27. [Validation Methodology](#27-validation-methodology)
28. [Framework Resolver Interface](#28-framework-resolver-interface)

**Appendices:**
- [Appendix A: Flat-Buffer FFI Wire Format](#appendix-a-flat-buffer-ffi-wire-format)
- [Appendix B: Derived Field Reference](#appendix-b-derived-field-reference)
- [Appendix C: Review Response Register](#appendix-c-review-response-register)
- [Appendix D: Type Glossary & Module Layout](#appendix-d-type-glossary--module-layout)
- [Appendix E: Confidence-Band Reference](#appendix-e-confidence-band-reference)
- [Appendix F: Open-Question Decisions](#appendix-f-open-question-decisions)

---

## 1. Overview & Design Pillars

CodeRadar is a hybrid Python/Rust tool that maintains a **live, incrementally updatable semantic graph** of a source codebase, enabling LLMs and developer tools to both **query** and **safely rewrite** code through a unified pipeline.

### 1.1 Design Pillars

1. **Rust core, Python shell.** Tree-sitter parsing, diff engine, resolution cascade (L1–L3), mutation engine, and in-memory projected graph — all in Rust. Python provides the CLI, GraphRAG orchestration, embedding pipeline, LSP pool, and MCP server. FFI uses flat buffers (one boundary crossing per file).

2. **Incremental by design.** After a file change, only affected symbols and their transitive dependents are recomputed. The diff algorithm matches entities by identity, not position.

3. **Name-based resolution with Stack Graphs.** Primary resolution uses the `stack-graphs` crate (12 languages). Falls back through import-constrained matching, signature matching, embedding similarity, and optional LSP — each with disjoint confidence bands.

4. **Partial coverage is worse than none.** The resolution cascade must close internal flows end-to-end or mark the entire chain unresolved. Internally-dead-end edges are suppressed. External edges (library calls) are always emitted with `target_kind: "external"`.

5. **Resilient to broken code.** Parse failures follow a deferred-to-recovery path rather than producing tainted symbols. Tainted updates are rejected (old graph slice retained). The graph never enters an inconsistent state.

6. **Read-write, not just read-only.** The MutationEngine provides AST-aware refactoring via four LLM tool calls: `replace_entity_body`, `update_signature`, `rename_symbol`, `create_entity`. All edits are byte-accurate, verified, atomic, and audited.

7. **Bitemporal persistence via Macrame.** All entities, edges, vectors, and audit data live in a single embedded `.db` file. The bitemporal model (valid time + transaction time) provides `as_of(ts)` and `reconstruct(ts)` for free — no custom WAL, no rollback journal, no epoch bumps.

8. **Hybrid query architecture.** Structural Pest queries execute against an in-memory projected graph (sub-ms). Agent traversals and temporal queries execute against Macrame (1.72 ms 3-hop). Vector search uses Macrame's built-in DiskANN.

### 1.2 Phased Language Support

| Phase | Languages | Resolution | Mutation |
|-------|-----------|------------|----------|
| 1 | Python | Stack Graphs + full cascade | Full tool suite |
| 2 | TypeScript / JavaScript | Stack Graphs + full cascade | Full tool suite |
| 3 | Go, Rust, Java, C#, C, C++ | Stack Graphs | Full tool suite |
| 4 | Swift, Scala, Lua, Elixir, + more | Import + Signature | `replace_entity_body`, `create_entity` |
| 5 | (Cross-cutting) Rename detection via similarity hashing | — | Phase 5 only |

### 1.3 Non-Goals

- Type inference, type checking, or abstract interpretation
- Runtime behavior analysis (no execution)
- IDE / LSP integration as a primary feature (LSP is an optional fallback only)
- Semantic equivalence checking of refactored code
- Build-system parsing (we consume config, not build logic)
- Cross-language type bridges
- Security analysis / SAST
- Code metrics beyond structural counts

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                         Python Layer                                 │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │ CLI      │ │ Visual-  │ │ GraphRAG │ │ Embedding│ │ Mutation  │  │
│  │ (click)  │ │ izers    │ │ Pipeline │ │ Pipeline │ │ Tool      │  │
│  │          │ │ (mermaid,│ │          │ │(fastembed│ │ Router    │  │
│  │          │ │ graphviz)│ │          │ │ + dedup) │ │           │  │
│  └──────────┘ └──────────┘ └────┬─────┘ └────┬─────┘ └─────┬─────┘  │
│                                 │            │             │         │
│                          Macrame Python bindings                     │
│                          (concepts, edges, search, temporal)         │
│                                 │                                    │
│                          PyO3   │    Flat buffers (one crossing      │
│                                 │    per file, not per entity)       │
└─────────────────────────────────┼────────────────────────────────────┘
                                  │
┌─────────────────────────────────┼────────────────────────────────────┐
│                         Rust Core                                    │
│  ┌──────────────────────────────┴─────────────────────────────────┐  │
│  │ • In-memory Projected Graph (reverse indexes, Pest queries)    │  │
│  │ • Tree-sitter parsing + two-pass extraction + decorator pass   │  │
│  │ • Incremental update engine (diff algorithm)                   │  │
│  │ • Semantic Resolution Engine (5-layer cascade):                │  │
│  │     L1: Stack Graphs (0.90-1.00)                               │  │
│  │     L2: Import Graph + Scope (0.80-0.89)                       │  │
│  │     L3: Signature Matching (0.40-0.79)                         │  │
│  │   [L4/L5 run in Python: Embedding + LSP]                       │  │
│  │ • Resolution cache with precise invalidation                   │  │
│  │ • Query engine (Pest grammar → in-memory projected graph)      │  │
│  │ • MutationEngine (byte spans, rope edits, indent normalize,    │  │
│  │     WriteGuard, 4 refactoring tools)                           │  │
│  │ • File watcher (notify, debounced, trace_id per batch)         │  │
│  │ • Git integration (branch detection, blame, .gitignore)        │  │
│  │                                                                 │  │
│  │ Storage layer → Macrame (embedded .db, same process)           │  │
│  │   ├── Entities as Concepts with annotations                    │  │
│  │   ├── Edges as EdgeAssertions with properties                  │  │
│  │   ├── Bitemporal for incremental update history                │  │
│  │   ├── DiskANN for embedding vector search                      │  │
│  │   └── FTS5 for keyword search                                  │  │
│  └────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────┘
```

### 2.1 Layering Rules

1. The Python layer never holds Rust references across `await` boundaries or GIL releases.
2. Every `#[pyfunction]` and `#[pymethod]` releases the GIL during long Rust operations (`py.allow_threads`).
3. FFI uses flat buffers (Appendix A) — one boundary crossing per file. The Python decoder validates ABI version before unpacking.
4. Structural Pest queries execute against the in-memory projected graph. Agent traversals and temporal queries execute against Macrame.
5. Mutations stage in Rust, write to Macrame, then update the projected graph — all within the writer lock.

---

## 3. Data Models

### 3.0 Identity Model

Entities are identified by stable dotted-path IDs (e.g., `"src/auth.py::UserService.create"`). These are used as Macrame concept IDs. Qualified names are *labels* — used for diff matching and display, not identity.

Diff matching uses a tiered key — see §5.2.

### 3.1 Entity IDs

```rust
/// Stable entity identifier — used as Macrame concept ID.
pub type EntityId = String;   // e.g. "src/auth.py::login"

/// Specific key types for type-safe referencing within Rust.
pub struct ModuleKey(pub EntityId);
pub struct ClassKey(pub EntityId);
pub struct FunctionKey(pub EntityId);
pub struct ImportKey(pub EntityId);
pub struct ConstantKey(pub EntityId);
pub struct TypeAliasKey(pub EntityId);

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum SymbolId {
    Module(EntityId),
    Class(EntityId),
    Function(EntityId),
    Import(EntityId),
}
```

### 3.2 Core Entities

```rust
pub struct Module {
    pub id: EntityId,
    pub name: String,                 // dotted import path, e.g. "foo.bar.baz"
    pub path: PathBuf,
    pub language: Language,
    pub package: Option<EntityId>,
    pub exports: Vec<Export>,
    pub star_exports: Option<Vec<String>>,
    pub classes: Vec<EntityId>,        // top-level classes declared in this module
    pub functions: Vec<EntityId>,      // top-level free functions declared in this module
    pub imports: Vec<EntityId>,        // import statements in this module
    pub constants: Vec<EntityId>,
    pub type_aliases: Vec<EntityId>,
    pub parse_quality: ParseQuality,
    pub file_version: u64,
    pub content_hash: u64,             // xxHash of file bytes; spans valid only while this matches disk
}

pub struct Class {
    pub id: EntityId,
    pub name: String,
    pub parent_module: EntityId,       // module that declares this class
    pub parent_class: Option<EntityId>, // enclosing class for nested classes
    pub bases: Vec<UnresolvedRef>,
    pub resolved_bases: Vec<EntityId>,
    pub mro: Vec<MroNode>,
    pub mro_error: bool,
    pub methods: Vec<EntityId>,
    pub fields: Vec<Field>,
    pub source: SourceType,
    pub decorators: Vec<String>,
    pub effective: EffectiveClass,
    pub is_type_checking_only: bool,
    pub line: usize,
    pub exit_line: usize,
    pub docstring: Option<String>,
    pub parse_quality: ParseQuality,
    pub content_hash: u64,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
    pub body_span: ByteSpan,
    pub decorators_span: Option<ByteSpan>,
}

pub struct Function {
    pub id: EntityId,
    pub name: String,
    pub parent_module: EntityId,
    pub parent_class: Option<EntityId>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub calls: Vec<UnresolvedRef>,
    pub resolved_calls: Vec<ResolvedCall>,
    pub decorators: Vec<String>,
    pub setter_of: Option<EntityId>,
    pub line: usize,
    pub exit_line: usize,
    pub docstring: Option<String>,
    pub kind: FunctionKind,
    pub is_async: bool,
    pub is_generator: bool,
    pub source: SourceType,
    pub signature_hash: u64,
    pub body_hash: u64,
    pub is_type_checking_only: bool,
    pub parse_quality: ParseQuality,
    pub content_hash: u64,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
    pub params_span: ByteSpan,
    pub body_span: ByteSpan,
    pub decorators_span: Option<ByteSpan>,
}

pub struct Import {
    pub id: EntityId,
    pub raw: String,
    pub kind: ImportKind,
    pub resolution: ImportResolution,
    pub line: usize,
    pub is_type_only: bool,
    pub name_span: ByteSpan,
}

pub struct Constant {
    pub id: EntityId,
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

pub struct TypeAlias {
    pub id: EntityId,
    pub name: String,
    pub target: String,
    pub source: SourceType,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}
```

### 3.3 Supporting Types

```rust
pub struct Parameter {
    pub name: String,
    pub annotation: Option<String>,
    pub default_value: Option<String>,
    pub is_varargs: bool,
    pub is_kwargs: bool,
    pub is_positional_only: bool,
    pub is_keyword_only: bool,
}

pub struct Field {
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub is_class_var: bool,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

pub struct Export {
    pub name: String,
    pub source: ExportSource,
    pub file_type: FileType,
}

pub enum ExportSource {
    Local,
    ReExport { from: EntityId, original_name: String },
}

pub enum ImportKind {
    ModuleImport { module: String, alias: Option<String> },
    FromImport   { module: String, names: Vec<(String, Option<String>)> },
    RelativeImport { level: usize, module: Option<String>, names: Vec<(String, Option<String>)> },
    StarImport   { module: String },
    Side         { module: String },
}

pub enum ImportResolution {
    Unresolved,
    Module(EntityId),
    Symbol(SymbolId),
    Wildcard { module: EntityId, exposed: Vec<String> },
    Dynamic,
    External { distribution: Option<String> },
}

pub struct UnresolvedRef {
    pub name: String,
    pub path: Vec<String>,
    pub line: usize,
    pub col: usize,
}

pub enum ResolvedCall {
    Function(EntityId),
    Method { receiver: ReceiverShape, method: EntityId },
    Constructor(EntityId),
    Builtin(String),
    External(String),                // standard library, third-party package
    Unresolved { reason: UnresolvedReason, raw: UnresolvedRef },
}

pub enum ReceiverShape {
    SelfRef,
    ClassRef(EntityId),
    ModuleRef(EntityId),
    LocalVar,
    Unknown,
}

pub enum UnresolvedReason {
    NameNotInScope,
    TypeInferenceRequired,
    DynamicImport,
    WildcardImportShadow,
    ParseError,
    IncompleteFlow,                  // internal dead-end suppressed per §6.1a
}

pub enum FunctionKind {
    Free, Method, StaticMethod, ClassMethod, Property,
    PropertySetter, PropertyDeleter, CachedProperty,
    AbstractMethod, DataclassSynthesized { from_class: EntityId },
}

pub enum EffectiveClass {
    Plain,
    Dataclass { frozen: bool, eq: bool, order: bool },
    NamedTuple,
    TypedDict { total: bool },
    Protocol,
    Enum { variant: EnumVariant },
    Abstract,
}

pub enum EnumVariant { Plain, IntEnum, Flag, StrEnum, Other(String) }

pub enum ParseQuality { Clean, Partial, Tainted, Deferred }  // Deferred added for §4.5a
pub enum FileType { Impl, Stub }
pub enum SourceType { Impl, Stub }

pub enum Language {
    Python, TypeScript, JavaScript, Go, Rust, Java,
    C, Cpp, Ruby, Php, CSharp, Kotlin,
    Other(String),
}

impl Language {
    pub fn from_extension(ext: &str) -> Language;
    pub fn tier(&self) -> u8;
    pub fn parser_crate(&self) -> &'static str;
}

pub enum MroNode {
    Class(EntityId),
    External { name: String },
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}
impl ByteSpan {
    pub fn len(&self) -> usize { self.end - self.start }
    pub fn is_empty(&self) -> bool { self.start == self.end }
}
```

### 3.3a Extraction Intermediate Types

```rust
pub enum ExtractedUnit {
    Module(ExtractedModule),
    Class(ExtractedClass),
    Function(ExtractedFunction),
    Import(ExtractedImport),
    Field(ExtractedField),
    Constant(ExtractedConstant),
    TypeAlias(ExtractedTypeAlias),
}

pub struct TaggedTree<'a> {
    pub source: &'a str,
    pub tags: HashMap<usize, TagInfo>,
}

pub struct TagInfo {
    pub tag: Tag,
    pub capture_name: &'static str,
}

pub enum FrameKind {
    Module,
    Class(EntityId),
    Function,
}

pub fn hash_signature(params: &[Parameter], ret: &Option<String>,
                      decorators: &[String], kind: &FunctionKind, is_async: bool) -> u64;
pub fn hash_body(source: &str, body_span: ByteSpan) -> u64;
pub fn hash_content(source: &[u8]) -> u64;
```

### 3.4 In-Memory Projected Graph

The projected graph is a **rebuildable derivative** of Macrame's ledger. It provides O(1) reverse-index lookups for structural Pest queries and L1/L2 resolution. It is NOT the source of truth — Macrame is. On startup, it is built from Macrame's `links_current`. On integrity violation, it is rebuilt from scratch.

```rust
pub struct ProjectedGraph {
    modules:      HashMap<EntityId, Arc<Module>>,
    classes:      HashMap<EntityId, Arc<Class>>,
    functions:    HashMap<EntityId, Arc<Function>>,
    imports:      HashMap<EntityId, Arc<Import>>,
    constants:    HashMap<EntityId, Arc<Constant>>,
    type_aliases: HashMap<EntityId, Arc<TypeAlias>>,

    // File-level structure
    file_to_modules: HashMap<PathBuf, Vec<EntityId>>,
    module_by_dotted_name: HashMap<(Language, String), EntityId>,

    // Reverse indexes (critical for sub-ms structural queries — see §9.3)
    importers:          HashMap<EntityId, BTreeSet<EntityId>>,
    callers_by_callee:  HashMap<EntityId, BTreeSet<EntityId>>,
    callees_by_caller:  HashMap<EntityId, BTreeSet<EntityId>>,
    subclasses:          HashMap<EntityId, BTreeSet<EntityId>>,
    overridden_by:      HashMap<EntityId, BTreeSet<EntityId>>,

    // Stack Graphs resolver (Rust-native, L1)
    stack_graph_resolver: StackGraphResolver,

    // Import graph (StableDiGraph for O(1) removal)
    import_graph: ImportGraph,

    // Call graph (StableDiGraph for cycle-safe traversals)
    call_graph: CallGraph,

    // Resolution cache
    resolution_cache: ResolutionCache,

    // Graph config (frozen at build time)
    config: GraphConfig,
}

pub struct CodeGraph {
    /// The current projected graph, behind an RwLock.
    /// Reads clone the Arc (one atomic increment); writes build a new
    /// ProjectedGraph and swap the Arc — same pattern as Macrame's
    /// links_current swap but at a vastly simplified scale.
    projection: RwLock<Arc<ProjectedGraph>>,

    /// Macrame database handle — source of truth for all data.
    db: macrame_db::Database,

    config: GraphConfig,
}
```

**Startup reconciliation.** On `CodeGraph::open()`:
1. Macrame loads `links_current` → produces all current entity/edge assertions
2. ProjectedGraph is built from these assertions (~10–50 ms for typical repos)
3. Watcher begins; incremental updates keep projection in sync

**Integrity rebuild.** On `verify_integrity()` failure:
1. Drop the projection
2. Rebuild from Macrame ledger
3. Log warning; agent continues uninterrupted (reads briefly fall back to Macrame traversals during rebuild)

### 3.4a Supporting Graph Types

```rust
use petgraph::stable_graph::{StableDiGraph, NodeIndex};

pub struct ImportGraph {
    graph: StableDiGraph<ImportNode, ()>,
    path_to_node: DashMap<SmolStr, NodeIndex>,
    node_to_path: DashMap<NodeIndex, SmolStr>,
    exports: DashMap<SmolStr, Vec<Export>>,
}

pub struct ImportNode {
    pub path: PathBuf,
    pub module_id: Option<EntityId>,
    pub language: Language,
}

impl ImportGraph {
    pub fn remove_file(&self, file_path: &str);                              // O(1)
    pub fn transitive_imports(&self, file_path: &str, max_depth: usize) -> Vec<ImportNode>;
}

pub struct CallGraph {
    graph: StableDiGraph<CallNode, CallEdge>,
    path_to_node: DashMap<SmolStr, NodeIndex>,
}

pub struct CallNode { pub entity_id: String, pub qualified_name: String }
pub struct CallEdge {
    pub confidence: f32,
    pub resolution_method: ResolutionMethod,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
}

impl CallGraph {
    pub fn find_callers(&self, target_id: &str, max_depth: usize) -> Vec<(CallNode, usize)>;
    pub fn find_call_chain(&self, source_id: &str, target_id: &str,
                           max_depth: usize) -> Option<Vec<CallNode>>;
}

pub struct ResolvedEdge {
    pub source_id: String,
    pub target_id: String,
    pub confidence: f32,
    pub method: ResolutionMethod,
    pub kind: ReferenceKind,
    pub line: usize,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
    pub target_kind: TargetKind,       // §6.1a: Internal | External
}

pub enum TargetKind {
    Internal,                          // resolved to a codebase entity
    External(String),                  // standard library, third-party; always emitted
}

pub enum ResolutionMethod { StackGraph, ImportConstrained, SignatureMatch, Embedding, Lsp }

pub struct GraphConfig {
    pub resolution: ResolutionConfig,
    pub stack_graph: StackGraphConfig,
    pub import_graph: ImportGraphConfig,
    pub signature: SignatureConfig,
    pub memory: MemoryConfig,
    pub mutation: MutationConfig,
    pub query: QueryConfig,
    pub git: GitConfig,
}
```

---

## 4. Tree-Sitter Extraction Layer

### 4.1 Tag Enum

```rust
pub enum Tag {
    Class, ClassBase, Function, FunctionParam, FunctionReturn,
    Import, ImportFromClause, ImportSpecifier,
    Call, CallReceiver, Decorator, Docstring, Field,
}
```

### 4.2 Tagging + Walker (Two-Pass Extraction)

**Pass 1 — Tagging.** Tree-sitter `.scm` queries tag nodes with coarse classifications. `.scm` captures map to `Tag` variants.

**Pass 2 — Hierarchy Walker.** A typed stack-frame walker traverses the tagged tree:
- `FrameKind::Module` — root
- `FrameKind::Class(EntityId)` — class body; methods pushed here are classified as methods
- `FrameKind::Function` — function body; nested defs are closures, not methods

```rust
// DESIGN PSEUDOCODE — core_indexer/src/extract/walker.rs
fn walk_and_extract(node: Node, ctx: &mut WalkContext) {
    let pushed = if let Some(info) = ctx.tags.tags.get(&node.id()) {
        emit_for_node(node, info, ctx)
    } else { None };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_and_extract(child, ctx);
    }

    if let Some(frame_kind) = pushed {
        let popped = ctx.stack.pop();
        debug_assert!(matches!(popped, Some(f) if
            std::mem::discriminant(&f.kind) == std::mem::discriminant(&frame_kind)));
    }
}
```

### 4.3 Decorator Semantics

**Known-decorator table (Python):**

| Decorator | Effect |
|-----------|--------|
| `@staticmethod` | `FunctionKind::StaticMethod` |
| `@classmethod` | `FunctionKind::ClassMethod` |
| `@property` | `FunctionKind::Property` |
| `@<prop>.setter` | `FunctionKind::PropertySetter` |
| `@<prop>.deleter` | `FunctionKind::PropertyDeleter` |
| `@functools.cached_property` | `FunctionKind::CachedProperty` |
| `@abstractmethod` | `FunctionKind::AbstractMethod`; class becomes `EffectiveClass::Abstract` |
| `@dataclass` | `EffectiveClass::Dataclass`; synthesizes `__init__`, `__repr__`, `__eq__` |
| Unknown | Recorded in `decorators` field; no semantic effect |

### 4.4 Docstring Extraction

**Python:** First statement of a module/class/function body, if it is a string literal.

**TypeScript:** JSDoc immediately preceding `comment` whose end-row is `target.start_row - 1`.

### 4.5 Parse Quality

- `Clean` — subtree has no errors
- `Partial` — subtree has errors but identifying fields intact
- `Tainted` — errors affect identifying fields; extraction is best-effort
- `Deferred` — file parse contains errors; routed to recovery extractor (§4.5a)

### 4.5a Per-File Safety Valve (Deferred-to-Recovery)

When a file's parse tree contains errors (`tree.root_node().has_error()`), the extractor returns `ParseQuality::Deferred` rather than producing potentially-incorrect entities. The ingestion pipeline falls back to a recovery extractor (slower, more conservative, handles error recovery canonically). This is a per-file safety valve — no file is ever silently mis-extracted.

The deferral is NOT an error. The file's previous graph slice (if any) is retained. On the next successful parse, the graph is updated normally.

### 4.6 Byte Span Extraction

Every entity and reference records byte spans from tree-sitter's native byte offsets:

| Entity | Spans |
|--------|-------|
| Function | `span`, `name_span`, `params_span`, `body_span`, `decorators_span` |
| Class | `span`, `name_span`, `body_span`, `decorators_span` |
| Constant | `span`, `name_span` |
| Import | `name_span` |

---

## 5. Incremental Update Algorithm

### 5.1 Update Flow

When a file changes:

1. **Parse** with tree-sitter → new `ExtractedUnit`s.
2. **Retrieve previous slice** from the projected graph.
3. **Diff** old vs new units (§5.2) → `Patch` of `Add | Remove | Modify`.
4. **Compute affected dependents** via reverse indexes.
5. **Write to Macrame** (assert/retire entities and edges).
6. **Update in-memory projected graph** (remove retired edges, insert new ones).
7. **Re-resolve only affected symbols.**
8. **Invalidate stale resolution cache entries.**
9. **Return `UpdateReport`.**

### 5.2 The Diff Algorithm

**Match key, in order of preference:**

1. `(kind, qualified_name, signature_hash, body_hash)` identical → no-op.
2. `(kind, qualified_name, signature_hash)` match, `body_hash` differs → `Modify { id, new_body_hash }`. No caller rebuild.
3. `(kind, qualified_name)` match, `signature_hash` differs → `Modify { id, full_fields }`. Affected callers re-resolve.
4. Unmatched old → `Remove { id }`.
5. Unmatched new → `Insert { unit }`.

**Ordering within a patch:**
1. Insert new modules → classes → functions → imports.
2. Modify existing entities.
3. Resolve forward references.
4. Remove obsolete entities (reverse dependency order).

### 5.3 Cross-File Resolution

See §6 for the full resolution cascade.

### 5.4 Resolution Cache

```rust
pub struct ResolutionCache {
    name_in_module: HashMap<(EntityId, String), Resolution>,
    method_in_class: HashMap<(EntityId, String), EntityId>,
    import_target: HashMap<(EntityId, String), ImportResolution>,
}

pub enum Resolution {
    Symbol(SymbolId),
    External,
    Unresolved(UnresolvedReason),
}
```

**Invalidation rules:**

| Change | Invalidate |
|--------|------------|
| Module M added/removed/renamed | All `name_in_module` and `import_target` entries involving M |
| Class C added/removed | All `method_in_class[(C, _)]`; all `name_in_module[(_, C.name)]` in C's module |
| Class C bases changed | All `method_in_class[(C, _)]`; all `method_in_class` entries for transitive subclasses |

**MRO invalidation.** MRO for class `C` is invalidated when one of `C`'s direct bases changes, or any transitive ancestor of `C` changes its bases. Tracked via the `subclasses` reverse index: when class `B`'s bases change, walk `subclasses[B]` transitively. **Bounded invalidation safeguard:** the walk is capped at `max_mro_invalidation_depth` (default 50). If the walk exceeds the depth bound — pathological on deeply nested hierarchies — the entire `method_in_class` resolution-cache section for all affected classes is flushed rather than clearing entries one at a time, preventing O(n²) blowup. Python inheritance chains rarely exceed 10 levels, so the default bound is safe.

### 5.5 Incremental Update Semantics (Macrame)

With Macrame as the storage engine, incremental updates use the assertion model:

1. **Retire old entities/edges** — `db.retire_entity(id, valid_to=now)` sets `valid_to`, doesn't delete. Old versions remain queryable via `as_of(ts)`.
2. **Assert new entities/edges** — `db.upsert_concept(...)` / `db.assert_edge(...)` with `valid_from=now`. Supersedes old versions.
3. **Update projected graph** — remove retired edges from reverse indexes, insert new edges.
4. **No WAL, no rollback journal, no epoch bump** — the assertion model IS the audit log.

**Crash recovery:** On restart, Macrame's ledger is always consistent (append-only, never overwritten). The projected graph is rebuilt from `links_current`. No replay, no `TxAck`, no journal.

---

## 6. Semantic Resolution Engine

### 6.1 Five-Layer Resolution Cascade

```
LAYER 1  Stack Graphs            confidence 0.90 – 1.00   (12 languages)
LAYER 2  Import Graph + Scope    confidence 0.80 – 0.89
LAYER 3  Signature Matching      confidence 0.40 – 0.79
LAYER 4  Embedding Fallback      confidence 0.20 – 0.39   (Python)
LAYER 5  LSP Override            confidence 1.00           (optional)
```

Bands are **disjoint by construction**: every resolver clamps its output into its band.

### 6.1a Partial Coverage Principle

A partially-resolved flow through **internal** entities is worse than an unresolved one. When the resolution cascade resolves some internal edges in a call chain but leaves gaps to other internal entities, the agent receives an incomplete graph and falls back to reading files — often reading *more* files than if it had no graph at all.

**Rules:**
1. An edge to an **external** target (standard library, third-party package, builtin) is always emitted with `target_kind: "external"` and the target name. These are NOT dead ends — agents understand they terminate at external boundaries.
2. An edge to an **internal** target whose own edges are missing MUST NOT be emitted. Mark the source's resolution as `Unresolved { reason: IncompleteFlow }` instead.
3. The `::toplevel` sentinel (§6.6) is always internal.

**Validation:** For each framework and language, test a canonical end-to-end internal flow. The graph must connect all internal hops, or report the specific boundary where resolution failed. Measured by agent A/B: 0 Read/Grep for the flow question.

### 6.2 Layer 1 — Stack Graphs

Uses the `stack-graphs` crate (0.14+) with language-specific `.tsg` rule files.

```rust
pub struct StackGraphResolver {
    graph: StackGraph,
    language_rules: HashMap<Language, TsgRules>,
    file_fragments: LruCache<FilePath, FragmentNodes>,
    spill_dir: PathBuf,
}
```

**Incremental indexing.** `index_file` evicts the previous fragment for `file_path`, builds fresh fragment nodes. Other files' fragments stay valid because stack-graph node ids are file-local.

**Path scoring:** `score_path(path).clamp(0.90, 1.00)` — rewards shorter, less-ambiguous paths.

### 6.3 Layer 2 — Import Graph + Scope

BFS over the import graph (up to `max_import_depth`, default 3), collecting modules that export the reference name. Confidence: single match → 0.89; multiple → 0.80.

#### 6.3a Stub/Impl Merge Rules (Python)

When a module has both an implementation file (`.py`) and a stub file (`.pyi`), merged exports are computed as follows:

| Case | `exports` | `star_exports` |
|------|-----------|----------------|
| Only stub has `__all__` | Use the stub list; every entry `file_type = Stub` | The stub list |
| Only impl has `__all__` | Use the impl list; every entry `file_type = Impl` | The impl list |
| Both have `__all__` | **Union.** Names in both → one entry, `file_type = Impl` (impl wins for identity). Names only in stub → `Stub`. Names only in impl → `Impl`. | **Impl's `__all__` only** (Python runtime semantics) |
| Neither has `__all__` | Union of public top-level names from both files; impl-sourced names `Impl`, stub-only names `Stub` | None → resolution falls back to public top-level names |

**Explicit-import resolution.** `from foo import X` checks whether `X` exists anywhere in the merged module namespace **regardless of `file_type` or `__all__` membership** — Python explicit imports bypass `__all__`.

**Star-import resolution.** `from foo import *` uses `star_exports` only when present; otherwise it falls back to public top-level names (no underscore prefix) from both files.

**Annotation provenance rule.** Annotations in stub files are *not* backfilled into impl entities — an impl `Constant`/`Field`/`Parameter` whose annotation is only present in the stub retains a `None` annotation in the graph (lossless: the stub entity keeps it). This prevents the type-checker's view from silently overriding the impl author's view.

### 6.4 Layer 3 — Signature Matching

Matches by name + arity, biased toward proximity. Weights: name=0.4, arity=0.3, proximity=0.3. Confidence: `(0.40 + score * 0.39).clamp(0.40, 0.79)`.

### 6.5 Layers 4–5 — Python-Side Fallback

After the Rust engine returns unresolved references:
- **Layer 4 — Embedding.** Cosine search in Macrame DiskANN. Threshold 0.85. Confidence 0.20–0.39.
- **Layer 5 — LSP.** If enabled and edge resolved below `override_threshold` (0.90), LSP `definition()` overrides with confidence 1.00.

### 6.6 `::toplevel` Sentinel

Every edge gets a valid source. References outside any function attach to the file's synthetic `{path}::toplevel` node.

### 6.7 Staged Two-Phase Commit

```rust
impl SemanticEngine {
    pub fn stage_file(&mut self, parsed: &ParsedFile, source: &str,
                      tree: &Tree) -> StagedChange;
    pub fn commit_staged(&mut self, staged: StagedChange);
    pub fn rollback_staged(&mut self, staged: StagedChange);
}
```

**Orchestration:**
1. Rust `stage_file()` → `StagedChange` (pure diff).
2. Python: embed unresolved references, run LSP fallback.
3. On success → Rust `commit_staged()` writes to Macrame + updates projected graph.
4. On failure → Rust `rollback_staged()`.

---

## 7. Query Engine

CodeRadar provides a **Pest-based query language** for fast in-memory structural queries. Agent traversals and temporal queries execute against Macrame directly.

### 7.1 Pest Grammar (In-Memory)

Operator precedence: `NOT > AND > OR`. Executed against the in-memory projected graph for sub-ms performance.

```pest
WHITESPACE = _{ " " | "\t" | "\n" | "\r" }
COMMENT    = _{ "--" ~ (!"\n" ~ ANY)* }

keyword = { "where" | "group" | "by" | "order" | "asc" | "desc" | "limit"
          | "and" | "or" | "not" | "true" | "false" | "null"
          | "select" | "as" | "in" | "contains" | "matches" }

identifier = @{ !keyword ~ (ASCII_ALPHA | "_") ~ (ASCII_ALPHANUMERIC | "_")* }
path       = @{ identifier ~ ("." ~ identifier)* }

string = @{ "\"" ~ ("\\\"" | (!"\"" ~ ANY))* ~ "\"" }
number = @{ "-"? ~ ASCII_DIGIT+ ~ ("." ~ ASCII_DIGIT+)? }
bool   = { "true" | "false" }
null   = { "null" }
list   = { "[" ~ value ~ ("," ~ value)* ~ "]" }
value  = { string | number | bool | null | list }

derived_call = { identifier ~ "(" ~ value ~ ("," ~ value)* ~ ")" }
operand      = { path | value | derived_call }
comp_op      = { "==" | "!=" | "<=" | ">=" | "<" | ">" | "contains" | "matches" | "in" }
predicate    = { operand ~ comp_op ~ operand }

atom     = { "(" ~ or_expr ~ ")" | "not" ~ atom | predicate }
and_expr = { atom    ~ ("and" ~ atom)* }
or_expr  = { and_expr ~ ("or"  ~ and_expr)* }
where_clause = { "where" ~ or_expr }

agg_func = { "count" | "sum" | "avg" | "min" | "max" }
agg_expr = { agg_func ~ "(" ~ (path | "*") ~ ")" ~ "as" ~ identifier }
select_item   = { path | agg_expr }
select_clause = { "select" ~ select_item ~ ("," ~ select_item)* }
group_by_clause = { "group" ~ "by" ~ path ~ ("," ~ path)* }
order_by_clause = { "order" ~ "by" ~ path ~ ("asc" | "desc")? }
limit_clause    = { "limit" ~ number }

entity = { "modules" | "classes" | "functions" | "imports" | "calls" | "fields" }

query = { SOI ~ entity ~ select_clause? ~ where_clause? ~ group_by_clause?
          ~ order_by_clause? ~ limit_clause? ~ EOI }
```

**Execution modes.**
- **Streaming** (no `group by`, no aggregation): walk the relevant arena, apply `where`, yield lazily.
- **Aggregated** (any `group by` or aggregation): materialize groups, sort if `order by`.

Both operate against the in-memory projected graph (<100 ms full scan target).

### 7.2 Derived Field Catalog

See Appendix B for the complete derived field reference.

### 7.2a Python Query Iterator (FFI)

```rust
#[pyclass]
pub struct QueryIterator {
    inner: Box<dyn Iterator<Item = QueryRow> + Send>,
    cancelled: Arc<AtomicBool>,
    check_interval: usize,           // default 64
    items_since_check: usize,
}
```

### 7.3 Agent-Facing Queries (Macrame)

Agent traversals execute against Macrame directly:

| Query Pattern | Macrame API |
|--------------|-------------|
| Scope exploration | `traverse().start_node(file).edge_type("contains").max_depth(2)` |
| Impact analysis | `traverse().start_node(target).edge_type("calls").direction(Incoming).max_depth(depth)` |
| Call chain | `traverse().start_node(src).edge_type("calls").max_depth(n).filter_target(target)` |
| Similarity search | `vector_search(model, query_vector, top_k)` |
| Dependency graph | `traverse().start_node(file).edge_type("imports").max_depth(depth)` |
| Definition lookup | `search_concepts(name)` + `traverse().start_node(id).max_depth(1)` |

### 7.4 Rust-Accelerated Traversal

`call_chain` and `impact_analysis` intents are served from the in-memory `CallGraph` (O(1) neighbor access via `StableDiGraph`).

---

## 8. Python API & FFI Contract

### 8.1 Flat-Buffer FFI

Extraction crosses the FFI boundary exactly **once per file**, regardless of entity count. The wire format is a typed flat buffer (see Appendix A). The Python decoder validates ABI version before unpacking.

```rust
/// One FFI crossing per file.
pub fn extract_file(path: &str, content: &str, language: &str)
    -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);
    //   meta     entities  edges     arena
```

### 8.1a Python-Side Decoder (Mandatory)

```python
EXPECTED_ABI_VERSION = 1

@dataclass(frozen=True)
class FlatBufferDecoder:
    abi_version: int
    _buf: bytes
    _entity_row_size: int = 132
    _edge_row_size: int = 56
    _ref_row_size: int = 48

    @classmethod
    def from_meta(cls, meta: bytes) -> "FlatBufferDecoder":
        abi = struct.unpack_from("<I", meta, 0)[0]
        if abi != EXPECTED_ABI_VERSION:
            raise ABIError(
                f"Kernel ABI {abi} != expected {EXPECTED_ABI_VERSION}. "
                f"Reinstall coderadar or rebuild the Rust extension."
            )
        return cls(abi_version=abi)

    def decode_entity(self, arena: bytes, offset: int) -> Entity:
        ...
```

Any wire format change bumps `EXPECTED_ABI_VERSION` in both `buffers.rs` and the decoder. Mismatch produces a clear error — never a segfault.

### 8.2 Python API

```python
import coderadar

# Initial analysis
graph = coderadar.analyze("src/")

# Update after file change
report = graph.update_file("src/core/engine.py")
report = graph.update_file("src/core/engine.py", content=new_content)

# Batch updates
with graph.batch() as b:
    b.update_file("src/a.py")
    b.update_file("src/b.py")

# Streaming query (Pest grammar)
for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
    print(cls.name, [m.name for m in cls.methods])

# Aggregated query
rows = graph.query(
    "classes select module.name, count(*) as n "
    "group by module.name order by n desc limit 10"
)

# Agent exploration (Macrame)
flow = graph.explore(["UserService.create"], direction="downstream")

# ID-based access
fn = graph.get_function("src/auth.py::validate_user")
callers = graph.callers_of("src/auth.py::validate_user")

# Mutation (LLM-driven)
plan = graph.plan_body_replacement(
    entity_id="src/auth.py::validate_user",
    new_body="    return bool(re.match(r'^[^@]+@[^@]+$', email))",
    expected_hash="abc123...",
    dry_run=True
)
result = graph.apply(plan)

# Watch mode
with coderadar.watch("src/") as w:
    for event in w:
        print(event.affected_files, event.elapsed_ms)

# Temporal queries
past = graph.as_of("2026-08-01T00:00:00Z")
old_callers = graph.callers_of("src/auth.py::login", as_of="2026-08-01T00:00:00Z")
```

### 8.3 Entity Handles

Entity wrappers carry `(entity_id, as_of_timestamp)`. A call that finds the entity has changed since `as_of_timestamp` receives the latest version with a `stale: True` flag — never an error. The caller decides whether to use the stale data or re-fetch.

### 8.4 `UpdateReport`

```python
@dataclass(frozen=True)
class UpdateReport:
    affected_files: list[str]
    changed_symbols: list[SymbolChange]
    new_unresolved_references: list[UnresolvedRef]
    newly_resolved_references: list[ResolvedRef]
    elapsed_ms: float
    parse_quality: ParseQuality
    parse_errors: int
    fully_applied: bool
    temporal_bounds: tuple[str, str]     # (valid_from, recorded_at)
```

---

## 9. Concurrency and Storage Architecture

### 9.1 Hybrid Architecture

```
                    ┌──────────────────────────┐
                    │      Macrame (.db)        │
                    │  • Source of Truth        │
                    │  • All writes go here     │
                    │  • Bitemporal history     │
                    │  • DiskANN vectors        │
                    │  • Agent traversals       │
                    │  • Audit / integrity      │
                    └──────────┬───────────────┘
                               │
                    ┌──────────▼───────────────┐
                    │  In-Memory Projected Graph │
                    │  • Derived from Macrame   │
                    │  • Reverse indexes        │
                    │  • Pest query execution   │
                    │  • L1/L2 resolution cache │
                    │  • Rebuildable from ledger│
                    └──────────────────────────┘
```

**Ingestion:**
1. Parse file → ExtractedUnits (tree-sitter, unchanged)
2. Diff old vs new → Patch (unchanged)
3. Write to Macrame: retire old → assert new
4. Update projected graph: remove retired edges → insert new edges
5. Re-resolve affected symbols

**Queries:**
- Structural (Pest): → In-memory projected graph (< 100 ms)
- Agent exploration: → Macrame traversals (1.72 ms 3-hop)
- Temporal (as_of): → Macrame bitemporal queries
- Vector search: → Macrame DiskANN
- Full-text: → Macrame FTS5

### 9.2 Single-Writer, Multiple-Reader

One ingestion worker holds the write lock. Query threads read the projected graph snapshot (one `Arc` clone, one atomic increment). The MutationEngine (§11) acquires the same writer lock during apply. Macrame's Write Actor enforces single-writer semantics with cooperative yielding.

### 9.3 Why In-Memory Reverse Indexes Are Mandatory

Macrame's measured performance (220 µs/assertion, 1.72 ms 3-hop traversal) is excellent for persistence and agent exploration. But it is the wrong tool for the Pest Query Engine's inner loop:

| Operation | In-Memory Projected Graph | Macrame API | Differential |
|-----------|--------------------------|-------------|--------------|
| Single reverse-index lookup | ~10 ns | ~20 µs | **2,000×** |
| 50,000-function `caller_count == 0` scan | ~0.5 ms | ~1,000 ms | **2,000×** |

The §22.1 <100 ms structural query target is only achievable with in-memory reverse indexes. Macrame handles everything else: persistence, bitemporality, agent traversals, and vector search.

### 9.4 Writer Throughput

Target: 1,000 file changes ingested in < 30 s. The projected graph swap is O(projection size) — in practice ~1–10 ms for typical repos.

### 9.5 Lock Hierarchy

Projected graph: one `RwLock<Arc<ProjectedGraph>>`. Readers clone the `Arc`. Writer builds a new `ProjectedGraph`, swaps. Resolution cache uses its own `RwLock`. Macrame's Write Actor handles its own concurrency.

### 9.6 GIL Handling

Long-running Rust methods wrap in `py.allow_threads()`. Query iterator's `__next__` calls `py.check_signals()` at configured intervals.

---

## 10. Macrame Storage Engine

### 10.1 Database Location

```
.codegraph/codegraph.db    # Single file, Macrame format
.codegraph/backups/         # Snapshot backups
```

### 10.2 Entity Storage (Concepts)

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

### 10.3 Edge Storage (Assertions)

| Edge Type | Source Kind | Target Kind | Properties |
|-----------|------------|-------------|------------|
| `contains` | file, class, module | class, function, method, variable | — |
| `calls` | function, method | function, method | confidence, resolution_method, line, call_site_span |
| `imports` | file | file, module | module_name, is_relative |
| `extends` | class | class | confidence |
| `implements` | class | class, interface | confidence |
| `references` | function, method | variable, constant | value_ref |
| `decorates` | function, class | function | — |
| `instantiates` | function, method | class | confidence, line |
| `overrides` | method | method | confidence |

### 10.4 Entity Lifecycle

```
Creation:
  db.upsert_concept(ConceptUpsert::new(entity_id, content)
      .valid_from(now)
      .annotate("kind", "function")
      .annotate("line", "42"))

Modification:
  db.retire_entity(entity_id, valid_to=now)
  db.upsert_concept(ConceptUpsert::new(entity_id, new_content).valid_from(now))

Removal:
  db.retire_entity(entity_id, valid_to=now)
```

### 10.5 Vector Storage

```rust
// One-time setup
db.register_model("code-embeddings-896", 896, DistanceMetric::Cosine)?;

// Store
db.upsert_embedding("code-embeddings-896", entity_id, &vector)?;

// Search
let similar = db.vector_search("code-embeddings-896", &query_vector, 10)?;

// Hybrid
let results = db.hybrid_search("code-embeddings-896", &query_vector, "auth login", 10)?;
```

### 10.6 Temporal Queries

```rust
// Current state
let graph = db.traverse().start_node(root).execute(conn, None)?;

// Past valid time
let past = db.as_of("2026-07-01T00:00:00Z")?;

// Past belief (transaction time)
let belief = db.reconstruct("2026-08-01T12:00:00Z")?;
```

### 10.7 Integrity

```rust
db.verify_integrity()?;         // Verify derivative state matches ledger
db.rebuild_current()?;          // Rebuild from ledger (idempotent)
db.verify_snapshot_chain()?;    // Verify snapshot chain consistency
```

### 10.8 Mutation Audit

Every mutation writes a Macrame concept with annotation `kind: "mutation_log"` carrying `tool`, `entity_id`, `affected_files`, `edit_count`, `status`, `trace_id`, `timestamp`. Macrame's bitemporal model provides retention naturally — no tiered pruning, no separate audit table.

---

## 11. AST-Aware Mutation Engine

### 11.1 Design

The MutationEngine extends CodeRadar from read-only to read-write. The LLM decides **what** to change semantically; the Rust core computes **where** and **how** at the byte level.

### 11.2 Four Refactoring Tools

```rust
impl MutationEngine {
    pub fn plan_body_replacement(&self, entity_id: &str, new_body: &str,
         expected_hash: Option<String>, dry_run: bool) -> Result<MutationPlan>;
    pub fn plan_signature_update(&self, entity_id: &str, new_signature: &str,
         call_site_values: &HashMap<String, String>,
         inject_defaults: bool, dry_run: bool) -> Result<MutationPlan>;
    pub fn plan_rename(&self, entity_id: &str, new_name: &str,
         include_strings: bool, dry_run: bool) -> Result<MutationPlan>;
    pub fn plan_create_entity(&self, target_file: &str, anchor: &str,
         code: &str, dry_run: bool) -> Result<MutationPlan>;
    pub fn apply(&mut self, plan: &MutationPlan) -> MutationResult;
}
```

#### `replace_entity_body`
Replaces the function/method body. One edit replacing `body_span`. Indent-normalized before rope edit.

#### `update_signature`
Rewrites the definition `params_span` AND every verified call site's argument list. Preflight parse, parameter diff, call-site enumeration (confidence ≥ 0.8). Unverified sites listed for manual review.

#### `rename_symbol`
Rewrites definition `name_span`, all L1/L2-resolved reference `name_span`s, and import symbol slots. Scope-aware — shadowing locals in other modules untouched.

#### `create_entity`
Anchored after an existing entity or at file `top`/`end`. Indent-normalized to sibling level. Parse-checked in context.

### 11.3 Rope-Based Multi-Edit Application

```rust
pub fn apply_edits_to_file(source: &str, edits: &[MutationEdit]) -> Result<String> {
    let mut rope = Rope::from_str(source);
    let mut ordered: Vec<&MutationEdit> = edits.iter().collect();
    ordered.sort_by(|a, b| b.span.start.cmp(&a.span.start));
    for edit in ordered {
        let (s, e) = rope_clamped_char_bounds(&rope, edit.span)?;
        rope.remove(s..e);
        rope.insert(s, &edit.replacement);
    }
    Ok(rope.to_string())
}
```

### 11.4 Indent Normalization

LLMs paste code at column 0. The engine normalizes indentation before rope application and parse verification.

```rust
pub struct IndentStyle { pub unit: char, pub width: usize }
pub fn detect_indent_style(source: &str) -> IndentStyle;
pub fn normalize_indent(new_code: &str, target: &str, style: IndentStyle,
                        verbatim_spans: &[ByteSpan]) -> String;
```

### 11.5 WriteGuard — Watcher Self-Write Suppression

```rust
pub struct WriteGuard {
    suppressed: DashMap<PathBuf, (String, Instant)>,  // path → (expected hash, expiry)
}
```

### 11.6 Mutation Apply Pipeline

```
1. Policy check: allow/deny globs, budgets, git cleanliness
2. Acquire single-writer lock
3. Hash guard: on-disk xxHash == edit.expected_hash → mismatch → RejectedStale
4. Snapshot originals → .harness/backups/{plan_id}/
5. Per file: indent normalize → Rope apply → candidate content
6. VERIFY: re-parse every candidate with Tree-sitter
     NEW ERROR nodes → full rollback from snapshot, return SyntaxDiagnostic[]
7. Register paths in WriteGuard
8. Atomic write: temp file + rename()
9. Synchronous reindex: write to Macrame → update projected graph → re-resolve
10. Release writer lock; audit entry; metrics
11. Return MutationResult
```

### 11.7 Mutation Policy

Hard guarantees:
- **Never commits** — mutations only touch working tree
- **Never touches deny-listed paths**
- **Stale contexts always rejected** (`expected_hash`)
- **All-or-nothing across files**
- **Every mutation audited** — audit entry + backup + trace_id

### 11.8 Mutation Result Types

```rust
pub enum MutationStatus {
    Applied,          // files written, reindex succeeded
    RolledBack,       // step 6 found new ERROR nodes; nothing persisted
    RejectedStale,    // step 3 hash mismatch; nothing written
}
```

The three status values map to distinct LLM-recovery contracts: `Applied` requires read-back; `RolledBack` requires repair; `RejectedStale` requires fresh context.

---

## 12. Git Integration

### 12.1 Branch-Switch Detection

Uses `git2::Diff::foreach` to detect HEAD changes. Returns list of changed files for re-indexing.

### 12.2 .gitignore Integration

Via the `ignore` crate: `.gitignore` + `.harnessignore` + built-in defaults.

### 12.3 Blame

Uses `git2::Blame` with `newest_commit(true)`. Refreshes lazily.

---

## 13. Embedding & GraphRAG Pipeline

### 13.1 Content-Addressed Deduplication

Before embedding, compute `xxHash` of the body. If hash matches existing vector in Macrame → skip, reuse. This is the dominant steady-state win: >85% of entity bodies unchanged between edits.

### 13.2 Embedding Generation & Backpressure

Embeddings produced by `fastembed` (ONNX, `jinaai/jina-code-embeddings-0.5b`, 896-d). Per-batch embedding budget (default 2000 ms); if exceeded, batch splits — embedded entities proceed, remainder re-queues as lower priority.

### 13.3 GraphRAG Query Execution

1. **Query Planner** classifies intent into one of six classes: `scope_exploration`, `impact_analysis`, `call_chain`, `similarity_search`, `dependency_graph`, `definition_lookup`.
2. **Execution** — agent traversals against Macrame; structural queries against projected graph.
3. **Rust-accelerated traversal** — `call_chain` and `impact_analysis` from in-memory `CallGraph`.
4. **Context Builder** — `grep-ast` structural compression with token budget. Three strategies: `signatures_only`, `structural`, `full`.

---

## 14. LSP — Optional Persistent Warm Pool

Disabled by default (`[resolution.lsp] enabled = false`). When enabled, LSP servers run as persistent, warm processes — never spawned per request.

```python
class LSPPool:
    def ensure_server(self, language: str, workspace_root: str) -> ManagedServer: ...
    def sync_file(self, path: str, text: str) -> None: ...
    def definition(self, path, line, col, content_hash) -> LspResult: ...
    def override_batch(self, low_confidence_edges): ...
```

Confidence contract: LSP results carry confidence **1.00** and `resolution_method = Lsp`. They are authoritative — overrides, never merges.

---

## 15. Configuration

```toml
# .coderadar.toml

[project]
languages = ["python"]
roots     = ["src/", "tests/"]
exclude   = ["**/migrations/**", "**/__pycache__/**", "**/.venv/**"]

[python]
sys_path = ["src/"]
follow_type_checking_imports = false
strict_wildcard_imports = true

[resolution]
min_confidence = 0.3

[resolution.stack_graph]
max_path_depth = 10
incremental = true

[resolution.import_graph]
max_import_depth = 3
include_same_package = true

[resolution.signature]
min_score = 0.5
name_weight = 0.4
arity_weight = 0.3
proximity_weight = 0.3

[resolution.lsp]
enabled = false
result_ttl_s = 300
idle_timeout_s = 600
timeout_ms = 5000
override_threshold = 0.90

[embedding]
model = "jinaai/jina-code-embeddings-0.5b"
dimension = 896
truncated_dimension = 64
max_body_tokens = 2000
batch_size = 32
workers = 2

[macrame]
db_path = ".codegraph/codegraph.db"
backup_dir = ".codegraph/backups"

[ingestion]
batch_chunk_size = 200
embedding_budget_ms = 2000
defer_low_priority_below = 0.6

[memory]
stack_graph_mb = 60
call_graph_mb = 40
resolution_cache_mb = 20
projected_graph_mb = 200
spill_compression = "zstd"

[mutation]
enabled = true
default_dry_run = true
max_files_per_plan = 100
max_edits_per_plan = 500
max_body_tokens = 4000
backup_dir = ".harness/backups"
backup_retention_hours = 24
post_verify = true
max_repair_attempts = 3
require_clean_git = false
allow = ["src/", "lib/", "tests/", "scripts/"]
deny  = [".git/", ".harness/", ".codegraph/", "/migrations/", "/*.lock", "/generated/"]

[query]
max_depth = 5
default_top_k = 10
cache_ttl_seconds = 300
cache_max_size = 256
use_rust_graph_for_traversal = true

[git]
enabled = true
reindex_on_branch_switch = true

[llm]
provider = "openai"
model = "gpt-4o"
max_context_tokens = 8192
context_strategy = "structural"
temperature = 0.1
api_key_env = "OPENAI_API_KEY"

[performance]
worker_threads = 4
debounce_ms = 50
query_check_interval = 64

[output]
snapshot_path = "./.coderadar/snapshot.bin"
```

```toml
# .harness/config.toml
[general]
watch_paths = ["src/", "tests/"]
exclude_patterns = [".generated", ".pb.go", ".g.dart"]
debounce_ms = 500
max_file_size_bytes = 1_048_576
log_level = "info"

[languages.python]
extensions = [".py", ".pyi"]
parser = "tree-sitter-python"
tags_query = "tags.scm"
lsp_command = "pyright-langserver --stdio"
```

---

## 16. Command-Line Interface

```
coderadar init <path>                  Initial analysis
coderadar analyze <path>               One-shot analysis
coderadar update <file> [--content -]  One-shot update
coderadar watch <path>                 Long-running watcher; JSONL on stdout
coderadar query "<query string>"       Execute Pest query
coderadar explore <symbol> [--direction]  Agent exploration (Macrame)
coderadar shell                        REPL with persistent graph
coderadar export <path> [--format f]   Export snapshot
coderadar load <snapshot>              Load snapshot
coderadar rebuild --full               Full re-index
coderadar stats                        Counts, parse-error summary, memory
coderadar warnings [--category c]      List warnings
coderadar resolve <qualified-name>     Show resolution chain
coderadar callers <qualified-name>     List callers
coderadar visualize <type> <args>      Run visualizer
coderadar mutations --last 20          Audit trail
coderadar diagnose --unresolved        Show unresolved references
coderadar status                       Daemon health check
```

---

## 17. Watch Mode

Uses `notify` (Rust) for cross-platform file events.

**Pipeline:** `fs events → notify → debounce (50ms) → dedupe → parse (rayon) → commit (serial MPSC)`

**Debouncing & coalescing.** Notify events are buffered in a 50 ms window (configurable `debounce_ms`). Within the window:
1. Multiple writes to the same path collapse to a single update keyed on the latest on-disk hash.
2. Create + modify on a never-seen path collapse to an insert.
3. Modify + delete on a known path collapse to a remove.

**Burst handling.** >10 files within 100 ms are batched into a single transaction with one `trace_id` (a fresh ULID per debounced batch). Sub-transactions are chunked at 200 files so the `RwLock` on the `ProjectedGraph` is never held >2 s during a massive burst — each chunk writes to Macrame + swaps the projection independently. A mid-batch crash loses at most one chunk; the survivor reconciles from disk hashes on restart.

**WriteGuard:** Every mutation path registers with `WriteGuard`; watcher's `should_drop()` check suppresses self-write events. The TTL is a safety net: if the synchronous reindex crashes, the guard entry expires and the watcher re-captures the file normally — no event is ever lost.

---

## 18. Visualizers

- **Class Hierarchy (Mermaid/Graphviz)** — inheritance DAG with MRO annotation
- **Module Dependency Graph (Graphviz)** — imports with SCC cycle highlighting
- **Call Graph (Mermaid/Graphviz)** — fan-out/fan-in with confidence-based styling

---

## 19. Error Handling and Fault Tolerance

### 19.1 Error Categories

| Category | Default behavior | `--strict` |
|----------|-----------------|------------|
| Parse error | Defer to recovery extractor; continue | Exit 1 |
| Resolution failure | Mark `Unresolved`; continue | Continue (expected) |
| I/O error | Drop file's slice; warn | Exit 1 |
| Macrame integrity | Rebuild projected graph; warn | Exit 1 |
| Internal invariant | `debug_assert!` panic; log+abort in release | Same |

### 19.2 Tainted Update Policy

When `update_file` rejects a tainted update, the old graph slice is retained and `UpdateReport.fully_applied = false`.

---

## 20. Multi-Language Support

### 20.1 Language Tiers

| Tier | Languages | Resolution | Mutation |
|------|-----------|------------|----------|
| Tier 1 | Python, TypeScript, JavaScript, Rust, Go, Java, C, C++, Ruby, PHP, C#, Kotlin | Stack Graphs → Import → Signature | Full tool suite |
| Tier 2 | Swift, Scala, Lua, Elixir, Erlang, Haskell, OCaml, Zig, Nim, Dart, R, Julia, Perl | Import → Signature | replace_body, create_entity |
| Tier 3 | Shell, SQL, HTML, CSS, YAML, TOML, JSON, Markdown + 280 more | Signature Match only | replace_body, create_entity |

### 20.2 Adding a New Language

1. Add `tree-sitter-<lang>` to Cargo.toml
2. Write `queries/<lang>.scm` with standard capture names
3. Implement language-specific walker extensions
4. Write `.tsg` rule file (Tier 1) or configure patterns (Tier 2)
5. Add fixture files under `tests/fixtures/<lang>/`
6. Add golden resolution tests

---

## 21. Observability & Diagnostics

### 21.1 Structured Logging

All components emit structured JSON via `tracing` (Rust) and `structlog` (Python).

### 21.2 Metrics

**Ingestion:** `files_watched`, node/edge totals, `embedding_cache_hit_rate`, parse errors.

**Mutation:** `mutations_total{tool,status}`, `mutation_edits_total`, `mutation_rollback_rate`.

**System:** `memory_rss_mb`, `db_size_mb`, per-component residency vs budget.

### 21.3 Cross-Boundary Trace Correlation

ULID `trace_id` generated by the watcher, carried through PyO3 into Python and back. Greppable end-to-end.

---

## 22. Performance Targets & Benchmarking

### 22.1 Targets

| Metric | Target | Backend |
|--------|--------|---------|
| Initial analysis (cold, 5k files) | < 30 s | Tree-sitter + Macrame |
| Structural query (streaming, simple `where`) | < 5 ms to first result | In-memory projected graph |
| Structural query (aggregated, full scan) | < 100 ms | In-memory projected graph |
| Agent traversal (3-hop explore) | < 5 ms | Macrame (measured: 1.72 ms) |
| Agent traversal (impact, depth 5) | < 20 ms | Macrame |
| Vector search (top-10) | < 5 ms | Macrame DiskANN (measured: 264 µs) |
| Single-file update (save → graph update) | < 500 ms (p95) | Macrame assertion + projection update |
| Startup graph projection (cold, 5K files) | < 5 s | Build from Macrame links_current |
| Integrity rebuild (10K files) | < 30 s | Full rebuild from ledger |
| Idle CPU usage | < 1% | Watcher running |
| Steady-state dedup hit rate | > 85% | After initial full index |
| Mutation plan (≤100 files, dry run) | < 100 ms | Planning only |
| Mutation apply (≤100 files) | < 200 ms | Excluding reindex |
| Memory (steady-state, 5k files) | < 500 MB | All in-memory components |

### 22.2 Memory Budgets

| Component | 10K files | 100K files | Eviction |
|-----------|-----------|------------|----------|
| Stack-graph fragments | 60 MB | 400 MB | LRU; cold spill (zstd) |
| Import graph | 15 MB | 120 MB | Never evicted |
| Call graph | 40 MB | 300 MB | LRU; edges < 0.60 evicted |
| Projected graph (entities) | 60 MB | 400 MB | Never evicted |
| Reverse indexes | 40 MB | 200 MB | Never evicted |
| Resolution cache | 40 MB | 200 MB | LRU |

---

## 23. Testing Strategy

### 23.1 Test Pyramid

Unit 70% / Integration 25% / E2E 5%.

### 23.2 Critical Property Test

```rust
proptest! {
    #[test]
    fn incremental_matches_full(edits in arbitrary_edit_sequence(1..100)) {
        let mut graph_inc = analyze(initial_fixture());
        let mut fs = initial_fixture();
        for edit in &edits {
            apply_edit_to_fs(&mut fs, edit);
            graph_inc.update_file(&edit.path).unwrap();
        }
        let graph_full = analyze_in_memory(&fs);
        assert_graphs_equivalent(&graph_inc, &graph_full);
    }
}
```

### 23.3 Key Tests

**Semantic engine:** golden resolution fixtures per language, cyclic call graph termination, O(1) file removal, toplevel sentinel coverage.

**Diff/patch/MRO:** round-trip proptest, C3 linearization proptest, snapshot round-trip, extraction snapshot tests.

**Mutation:** body_replacement preserves signature, indent normalization, signature cascade, stale hash rejection, syntax error full rollback, CRLF accuracy, multi-byte offsets, descending offset stability.

**Python:** ingestion pipeline, embedding dedup, query execution (Pest + agent exploration), mutation tool routing, policy enforcement, LSP pool lifecycle.

---

## 24. Build and Distribution

- **Build tool:** `maturin` (PEP 517)
- **Rust crates:** `pyo3`, `tree-sitter` + language packs, `stack-graphs`, `petgraph`, `ropey`, `similar`, `dashmap`, `lru`, `rayon`, `xxhash-rust`, `git2`, `zstd`, `pest`, `pest_derive`, `parking_lot`, `notify`, `ignore`, `crossbeam-channel`, `tracing`, `serde`, `smol_str`, `ulid`, `macrame-db` (≥0.10.0, pinned)
- **Python deps:** `pydantic>=2.0`, `click`, `rich`, `fastembed`, `litellm`, `grep-ast`, `structlog`
- **Removed from v3.3:** `ladybug`, `slotmap`, `arc-swap` (replaced by Macrame + ProjectedGraph)
- **Wheel matrix:** Linux x86_64, Linux aarch64, macOS x86_64, macOS arm64, Windows x86_64. Python 3.9–3.13 (abi3).
- **CI:** `cibuildwheel`; publish to PyPI on tagged releases.

---

## 25. Out of Scope

| Out of scope | Alternative |
|---|---|
| Type inference | Consume annotations as strings |
| Runtime behavior | No execution |
| IDE / LSP integration (primary) | LSP is optional fallback only |
| Semantic refactoring | MutationEngine handles structural rewriting |
| Cross-language type bridges | Explicitly unsupported |
| Build-system parsing | Consume config, not build logic |
| Security analysis | Not a SAST tool |
| Code metrics beyond counts | `line_count` exposed |
| Plugin API for decorators | Deferred to post-v1 |
| Distributed snapshots | Deferred to post-v1 |

---

## 26. Agent Interface Design

### 26.1 Tool Design Principles

Derived from CodeGraph's production experience:

1. **Precise input, precise output.** Agents reliably call tools with symbol names. Design one primary tool that takes symbol names and returns complete flows.

2. **Adapt the tool to the agent — don't try to change the agent.** MCP instructions and tool descriptions are low-salience channels. Meet agents where they already go.

3. **Errors teach abandonment.** One or two `isError: true` responses early in a session and the agent stops calling the tool entirely. Return success-shaped responses carrying guidance for every expected condition.

4. **Keep the surface small.** Start with four tools; add only when an agent reliably asks for something those can't answer.

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

| Indexed Files | Max Explore Calls | Max Output Chars |
|--------------|-------------------|------------------|
| < 500 | 1 | 18,000 |
| < 5,000 | 2 | 28,000 |
| < 15,000 | 3 | 35,000 |
| < 25,000 | 4 | 38,000 |
| ≥ 25,000 | 5 | 38,000 |

**Invariant:** A larger tier must never get a smaller per-file budget than a smaller tier.

---

## 27. Validation Methodology

### 27.1 Extraction Parity Gate

Before claiming a language is supported, prove byte-identical extraction against a reference implementation on three repo sizes (~150, ~3,000, ~10,000 files).

### 27.2 Agent A/B Evaluation

Before claiming a feature works:
1. Pick a canonical flow
2. Run with-vs-without CodeRadar, minimum 2 runs per arm
3. Metrics: duration, total tool calls, Read count, Grep count
4. Pass bar: 0 Read/Grep for the flow question

### 27.3 Incremental Update Equivalence

Extend the §23.2 property test with random edit sequences: single-token renames, body rewrites, file splits, moves, corrupted saves. Assert entity count, edge count, and graph connectivity are identical after incremental vs full.

### 27.4 Mutation Validation

For each mutation tool:
1. Dry-run plan → verify diff preview is syntactically valid
2. Apply + re-index → verify entity/edge counts stable
3. Agent recovery → inject deliberate syntax error, verify agent can repair
4. Hash guard → modify file externally, verify `RejectedStale`

---

## 28. Framework Resolver Interface

### 28.1 Pattern

```rust
trait FrameworkResolver {
    /// Can this resolver handle this project?
    fn detect(&self, project_root: &Path) -> bool;

    /// Does this resolver claim to resolve this reference?
    fn claims_reference(&self, name: &str) -> bool;

    /// Extract synthetic nodes and edges from a single file.
    fn extract(&self, file_path: &str, source: &str) -> FrameworkExtraction;

    /// Resolve a single reference.
    fn resolve(&self, ref: &UnresolvedRef, graph: &CodeGraph) -> Option<ResolvedTarget>;
}

struct FrameworkExtraction {
    nodes: Vec<SyntheticNode>,
    edges: Vec<SyntheticEdge>,
}
```

### 28.2 Phase 1 Resolvers (Python)

| Resolver | detect() | extract() | resolve() |
|----------|----------|-----------|-----------|
| **Django** | `manage.py` exists | `path()`/`re_path()` → route nodes + handler edges | `*Model` → models.py, `*View` → views.py |
| **Flask** | `@app.route` patterns | route registration → route nodes + handler edges | Blueprint registration |
| **FastAPI** | `APIRouter` imports | `@app.get()` / `@router.post()` → route nodes | Dependency injection chains |

### 28.3 Synthetic Edge Provenance

All framework-synthesized edges carry `provenance: "heuristic"` and `metadata.synthesizedBy: "<resolver_name>"`. Agents see these annotations in explore output.

---

## Appendix A: Flat-Buffer FFI Wire Format

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

entity row (132 bytes):
  0   u8   EntityKind index
  1   u8   Language index
  2   u16  flags
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
  76  str  parent_id
  84  u64  signature_hash
  92  u64  body_hash
  100 u64  content_hash
  108 str  extra_json
  116 u32  byte_span_start
  120 u32  byte_span_end
  124 u32  name_span_start
  128 u32  name_span_end
  132 (end)

edge row (56 bytes):
  0   u32  source entity row index
  4   u32  target entity row index
  8   u8   EdgeKind index
  9   u8   provenance
  10  u16  pad
  12  f32  confidence
  16  u32  line
  20  u32  column
  24  str  metadata_json
  32  str  source_id_str
  40  str  target_id_str
  48  u32  call_site_span_start
  52  u32  call_site_span_end
  56 (end)

ref row (48 bytes):
  0   u32  from_entity row index
  4   u8   ReferenceKind
  5   u8   flags
  6   [2]  pad
  8   u32  line
  12  u32  column
  16  str  reference_name
  24  str  context
  32  str  candidates (NUL-joined)
  40  u32  name_span_start
  44  u32  name_span_end
  48 (end)
```

---

## Appendix B: Derived Field Reference

### B.1 Functions

| Derived Field | Return | Cost | Source |
|---------------|--------|------|--------|
| `caller_count` | i64 | O(1) | `callers_by_callee[id].len()` |
| `line_count` | i64 | O(1) | Extracted at parse time |
| `module.name` | String | O(1) | `parent_module → Module.name` |
| `is_async` | bool | O(1) | Extracted metadata |
| `has_method(name)` | bool | O(k) | Linear scan of `Class.methods` |
| `decorators` | List\<String\> | O(1) | Extracted metadata |
| `unresolved_reason` | Enum | O(1) | Resolution results |

### B.2 Classes

| Derived Field | Return | Cost | Source |
|---------------|--------|------|--------|
| `inherits_from(name)` | bool | O(m) | MRO linear scan |
| `mro_names` | List\<String\> | O(n) first, O(1) after | C3 linearization |
| `method_count` | i64 | O(1) | `methods.len()` |
| `subclasses` | List\<String\> | O(d) | `subclasses[id]` reverse index |

---

## Appendix C: Review Response Register

All findings from the CodeGraph Engine v3.1 review, the v3.2.1 amendment review, and the v3.4 review are resolved in this consolidated document.

### From CodeGraph Review (v3.4 Amendment):

| Finding | Resolution |
|---------|------------|
| Per-entity PyO3 overhead | Flat-buffer FFI (§8.1, Appendix A) |
| Agent validation methodology | §27 Agent A/B evaluation |
| Partial coverage principle | §6.1a with external-node exception |
| Framework resolver pattern | §28 FrameworkResolver trait |
| Error handling for agents | §26.1.3 success-shaped responses |
| Explore budget scaling | §26.3 tiered budget table |
| SQLite (LadybugDB) vs Macrame | Macrame adopted (§10) |

### From v3.4 Review Response:

| Finding | Resolution |
|---------|------------|
| Latency gap: Macrame too slow for structural queries | Hybrid architecture: projected graph for Pest, Macrame for agent traversals (§9.1, §9.3) |
| Partial coverage needs external-node exception | `target_kind: "external"` always emitted (§6.1a) |
| SQLite mirror is a bad idea | Withdrawn; projected graph handles structural queries |
| FFI unpacking needs safety docs | Typed decoder with ABI gating (§8.1a) |
| Performance budgets need updating | Split into in-memory (Pest) and Macrame (agent) targets (§22.1) |

### From v3.3:

All CG-ARCH-003-R1 findings and v3.2.1 amendment patches are resolved per the v3.3 register. Key resolutions carried forward:

- LSP warm pool (not per-request spawn)
- ImportGraph O(1) removal via StableDiGraph
- Cycle-safe traversals with visited set + depth cap
- Disjoint confidence bands
- Content-addressed embedding dedup
- Staged two-phase commit with rollback
- Tiered MutationLog retention → natural in Macrame bitemporal

---

## Appendix D: Type Glossary & Module Layout

```
core_indexer/src/
  types.rs                 # §3 entities + ByteSpan + enums
  extract/
    tagger.rs              # Pass 1 .scm queries
    walker.rs              # Pass 2 hierarchy walker
    decorators.rs          # known-decorator table
    spans.rs               # byte-span extraction
  update/
    diff.rs                # Tiered diff algorithm
    patch.rs               # Apply flow
  resolve/
    stack_graph.rs         # Layer 1 + LRU spill
    import_graph.rs        # Layer 2
    signature.rs           # Layer 3
    cache.rs               # ResolutionCache
    orchestrator.rs        # cascade + ::toplevel sentinel
  query/
    grammar.rs             # Pest grammar
    exec.rs                # streaming + aggregated
  mutation/
    mod.rs                 # MutationEngine
    edit.rs                # rope apply
    indent.rs              # normalize
    write_guard.rs         # WriteGuard
  fs/
    watcher.rs             # notify + debounce + WriteGuard
    git.rs                 # git2 integration
  graph.rs                 # ProjectedGraph + reverse indexes
  storage.rs               # Macrame concept/edge mapping
  buffers.rs               # Flat-buffer serialization

py_agent/src/coderadar/
  pipeline.py              # ingestion orchestration
  embedding/dedup.py       # content-addressed dedup
  agent/graphrag.py        # query planner + context builder
  lsp/pool.py              # LSPPool
  mutation/tool_router.py  # tool schema routing
  query/{planner,templates,executor,cache}.py
  buffers.py               # Flat-buffer decoder (ABI-gated)
  config.py                # Pydantic config models
  cli.py                   # Click CLI
```

---

## Appendix E: Confidence-Band Reference

| Band | Layer | Method | When |
|------|------|--------|------|
| 0.90–1.00 | L1 | StackGraph (or L0 hand-rolled fallback) | vendored `.tsg` rules exist |
| 0.80–0.89 | L2 | ImportConstrained | Stack Graphs returned None |
| 0.40–0.79 | L3 | SignatureMatch | L1+L2 returned None |
| 0.20–0.39 | L4 | Embedding (Python) | cosine ≥ 0.85 |
| 1.00 | L5 | Lsp (Python) | LSP maps to known entity; overrides L1–L4 |
| unset | — | Unresolved | no layer matched; reason recorded |

---

## Appendix F: Open-Question Decisions

### F.1 Wildcard Import Multi-Hop (Q1 — Resolved)

Follow `from x import *` chains up to 3 hops when every intermediate module's `__all__` is statically determinable. Beyond 3 hops: first-hop module with `WildcardImportShadow` warning.

### F.2 Plugin API Recipe (Q2 — Deferred to post-v1)

Python entry-point group `coderadar.extractors` for `.scm` tag queries and known-decorator handlers. Marshalled effect table forwarded to Rust core at startup.

### F.3 Distributed Snapshots (Q3 — Deferred to post-v1)

Single-writer-per-shard with 2PC sketched. Not warranted until single-writer throughput ceiling is hit.

### F.4 `__all__` Static Analysis (Q4 — Open for v3.6)

Whether `__all__ += [...]` and `__all__.extend([...])` should be AST-pattern-special-cased rather than falling back to non-determinable. Left for evidence from real-world fixtures.

---

*End of consolidated specification — CodeRadar v3.5*
