# CodeRadar — Consolidated Architecture Specification v3.3

> **Status:** Consolidated from CodeRadar v2 (`design_file.md`) + CodeGraph Engine v3.2.1 (`Codegraph engine 3.2.1.md` + amendment). v3.3.1 review pass: all referenced content inlined, undefined types defined, open questions resolved.
> **Scope:** Merges the algorithmic rigor of v2 with the feature breadth of v3.2.1 into a single, implementation-ready specification.
> **Date:** 2026-07-27 (v3.3.1)

---

## Table of Contents

1. [Overview & Design Pillars](#1-overview--design-pillars)
2. [Architecture](#2-architecture)
3. [Data Models](#3-data-models)
4. [Tree-Sitter Extraction Layer](#4-tree-sitter-extraction-layer)
5. [Incremental Update Algorithm](#5-incremental-update-algorithm)
6. [Semantic Resolution Engine](#6-semantic-resolution-engine)
7. [Query Engine](#7-query-engine)
8. [Python API](#8-python-api)
9. [Concurrency, Locking, and Snapshot Isolation](#9-concurrency-locking-and-snapshot-isolation)
10. [Persistence (LadybugDB)](#10-persistence-ladybugdb)
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
26. [Open Questions](#26-open-questions)

**Appendices:**
- [Appendix A: PyO3 Data Structures (FFI Boundary)](#appendix-a-pyo3-data-structures-ffi-boundary)
- [Appendix B: LadybugDB Vector Search API](#appendix-b-ladybugdb-vector-search-api)
- [Appendix C: Review Response Register](#appendix-c-review-response-register)
- [Appendix D: Derived Field Reference](#appendix-d-derived-field-reference)
- [Appendix E: Type Glossary & Module Layout](#appendix-e-type-glossary--module-layout)
- [Appendix F: Open-Question Decisions](#appendix-f-open-question-decisions)

---

## 1. Overview & Design Pillars

CodeRadar is a hybrid Python/Rust tool that maintains a **live, incrementally updatable semantic graph** of a source codebase's logical structure, enabling LLMs and developer tools to both **query** and **safely rewrite** code through a unified pipeline.

### 1.1 Design Pillars

1. **Rust core, Python shell.** Mutable graph storage, tree-sitter parsing, Stack Graphs resolution, differential updates, mutation engine, query execution — all in Rust. Python (PyO3) provides CLI, visualizers, GraphRAG orchestration, embedding pipeline, LadybugDB integration, and a high-level API.

2. **Incremental by design.** After a file change, only affected symbols and their transitive dependents are recomputed. The diff algorithm matches entities by identity, not position.

3. **Name-based resolution with Stack Graphs.** Primary resolution uses the `stack-graphs` crate (12 languages). Falls back through import-constrained matching, signature matching, embedding similarity, and optional LSP — each with disjoint confidence bands.

4. **Resilient to broken code.** Parse failures produce tainted symbols rather than aborts. Tainted updates are rejected (old graph slice retained) with optional `force=True` override. The graph never enters an inconsistent state.

5. **Read-write, not just read-only.** The MutationEngine provides AST-aware refactoring via four LLM tool calls: `replace_entity_body`, `update_signature`, `rename_symbol`, `create_entity`. All edits are byte-accurate, verified, atomic, and audited.

6. **Persistent and queryable.** LadybugDB provides ACID persistence with Cypher queries, HNSW vector indexes, and schema versioning. The in-memory Rust graphs use ArcSwap-based snapshot isolation for zero-lock reads.

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
│                    LadybugDB ◄──┴────────────┴─────────────┘         │
│                    (Kùzu Cypher, HNSW vectors, ACID)                 │
│                                 │                                    │
│                          PyO3   │   (staged changes, embeddings,     │
│                                 │    query results, mutation plans)  │
└─────────────────────────────────┼────────────────────────────────────┘
                                  │
┌─────────────────────────────────┼────────────────────────────────────┐
│                         Rust Core                                    │
│  ┌──────────────────────────────┴─────────────────────────────────┐  │
│  │ • CodeGraph (ArcSwap arenas, MVCC epochs, SlotMap keys)        │  │
│  │ • Tree-sitter parsing + two-pass extraction + decorator pass   │  │
│  │ • Incremental update engine (diff + patch + WAL)               │  │
│  │ • Semantic Resolution Engine (5-layer cascade):                │  │
│  │     L1: Stack Graphs (0.90-1.00)                               │  │
│  │     L2: Import Graph + Scope (0.80-0.89)                       │  │
│  │     L3: Signature Matching (0.40-0.79)                         │  │
│  │   [L4/L5 run in Python: Embedding + LSP]                       │  │
│  │ • Resolution cache with precise invalidation                   │  │
│  │ • Query engine (Pest grammar + Cypher delegation)              │  │
│  │ • MutationEngine (byte spans, rope edits, indent normalize,    │  │
│  │     WriteGuard, 4 refactoring tools)                           │  │
│  │ • File watcher (notify, debounced, trace_id per batch)         │  │
│  │ • Git integration (branch detection, blame, .gitignore)        │  │
│  └────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────┘
```

### 2.1 Layering Rules

1. The Python layer never holds Rust references across `await` boundaries or GIL releases.
2. Every `#[pyfunction]` and `#[pymethod]` releases the GIL during long Rust operations (`py.allow_threads`).
3. Rust types crossing the FFI boundary are either `Copy` IDs, owned strings/bytes, or `PyObject`s built inside the Rust call.
4. The query engine and update engine share read access via snapshot isolation (§9); they never hold each other's locks.
5. Mutations stage in Rust, Python commits to LadybugDB, then Rust commits in-memory — a two-phase commit straddling the FFI boundary (§6.7).

---

## 3. Data Models

### 3.0 Identity Model

**Stable identity is the `SlotMap` key, not the qualified name.**

- `ModuleId`, `ClassId`, `FunctionId`, `ImportId`, `ConstantId`, `TypeAliasId` are stable across the lifetime of the entity.
- A key is invalidated only when the entity is removed. After removal, the slot may be reused; SlotMap's generational keys prevent ABA confusion.
- Qualified names are *labels* — used for diff matching and display, not identity.
- The Python wrappers expose IDs as opaque integers. Code that stores these IDs across updates must be prepared for `None` lookups.

**Diff matching uses a tiered key** — see §5.2.

### 3.1 Unique Identifiers (SlotMap)

```rust
use slotmap::{SlotMap, new_key_type};

new_key_type! { pub struct ModuleId; }
new_key_type! { pub struct ClassId; }
new_key_type! { pub struct FunctionId; }
new_key_type! { pub struct ImportId; }
new_key_type! { pub struct ConstantId; }
new_key_type! { pub struct TypeAliasId; }

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum SymbolId {
    Module(ModuleId),
    Class(ClassId),
    Function(FunctionId),
    Import(ImportId),
}
```

### 3.2 Core Entities

```rust
pub struct Module {
    pub name: String,                 // dotted import path, e.g. "foo.bar.baz"
    pub path: PathBuf,
    pub language: Language,
    pub package: Option<ModuleId>,
    pub exports: Vec<Export>,
    pub star_exports: Option<Vec<String>>,
    pub classes: Vec<ClassId>,        // top-level classes declared in this module
    pub functions: Vec<FunctionId>,   // top-level free functions declared in this module
    pub imports: Vec<ImportId>,        // import statements in this module
    pub constants: Vec<ConstantId>,
    pub type_aliases: Vec<TypeAliasId>,
    pub parse_quality: ParseQuality,
    pub file_version: u64,
    pub content_hash: u64,            // xxHash of file bytes; spans valid only while this matches disk
}

pub struct Class {
    pub name: String,
    pub parent_module: ModuleId,      // module that declares this class
    pub parent_class: Option<ClassId>, // enclosing class for nested classes
    pub bases: Vec<UnresolvedRef>,
    pub resolved_bases: Vec<ClassId>,
    pub mro: Vec<MroNode>,
    pub mro_error: bool,             // true if C3 produced no consistent MRO (set EffectiveClass::Abstract)
    pub methods: Vec<FunctionId>,
    pub fields: Vec<Field>,
    pub source: SourceType,
    pub decorators: Vec<String>,
    pub effective: EffectiveClass,
    pub is_type_checking_only: bool,
    pub line: usize,
    pub exit_line: usize,            // end line of the class body
    pub docstring: Option<String>,
    pub parse_quality: ParseQuality,
    pub content_hash: u64,
    // Byte spans (for MutationEngine)
    pub span: ByteSpan,
    pub name_span: ByteSpan,
    pub body_span: ByteSpan,
    pub decorators_span: Option<ByteSpan>,
}

pub struct Function {
    pub name: String,
    pub parent_module: ModuleId,      // module containing this function (for `module.name` derived field)
    pub parent_class: Option<ClassId>, // Some(Method) — enclosing class; None for free functions
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub calls: Vec<UnresolvedRef>,
    pub resolved_calls: Vec<ResolvedCall>,
    pub decorators: Vec<String>,
    pub setter_of: Option<FunctionId>, // getter this setter/deleter pairs with (§4.3)
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
    pub content_hash: u64,           // content hash of the *file* — spans valid only while this matches
    // Byte spans (for MutationEngine)
    pub span: ByteSpan,
    pub name_span: ByteSpan,
    pub params_span: ByteSpan,
    pub body_span: ByteSpan,
    pub decorators_span: Option<ByteSpan>,
}

pub struct Import {
    pub raw: String,
    pub kind: ImportKind,
    pub resolution: ImportResolution,
    pub line: usize,
    pub is_type_only: bool,          // `if TYPE_CHECKING:` or `import type` (TS)
    pub name_span: ByteSpan,
}

pub struct Constant {
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

pub struct TypeAlias {
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
    ReExport { from: ModuleId, original_name: String },
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
    Module(ModuleId),
    Symbol(SymbolId),
    Wildcard { module: ModuleId, exposed: Vec<String> },
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
    Function(FunctionId),
    Method { receiver: ReceiverShape, method: FunctionId },
    Constructor(ClassId),
    Builtin(String),
    Unresolved { reason: UnresolvedReason, raw: UnresolvedRef },
}

pub enum ReceiverShape {
    SelfRef,
    ClassRef(ClassId),
    ModuleRef(ModuleId),
    LocalVar,
    Unknown,
}

pub enum UnresolvedReason {
    NameNotInScope,
    TypeInferenceRequired,
    DynamicImport,
    WildcardImportShadow,
    ParseError,
}

pub enum FunctionKind {
    Free,
    Method,
    StaticMethod,
    ClassMethod,
    Property,
    PropertySetter,
    PropertyDeleter,
    CachedProperty,
    AbstractMethod,
    DataclassSynthesized { from_class: ClassId },
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

/// Enum flavor — affects synthesized members (e.g. `__members__`, `value`).
pub enum EnumVariant {
    Plain,                                // `class C(Enum): ...`
    IntEnum,
    Flag,                                 // bitwise-flag semantics
    StrEnum,
    Other(String),                        // any subclass whose name we recognize as a literal
}

pub enum ParseQuality { Clean, Partial, Tainted }
pub enum FileType { Impl, Stub }
pub enum SourceType { Impl, Stub }

/// Tier-1 languages have full Stack Graphs support; Tier 2/3 languages use the
/// `Other` variant and fall through to Layers 2–3 resolution.
pub enum Language {
    Python, TypeScript, JavaScript, Go, Rust, Java,
    C, Cpp, Ruby, Php, CSharp, Kotlin,
    Other(String),                        // canonical lowercase name, e.g. "swift", "lua", "sql"
}

impl Language {
    /// Map a file extension to a Language. Unknown extensions become `Other`.
    pub fn from_extension(ext: &str) -> Language;
    pub fn tier(&self) -> u8;             // 1 | 2 | 3 — selects resolution path
    pub fn parser_crate(&self) -> &'static str; // cargo feature to enable
}

pub enum MroNode {
    Class(ClassId),
    External { name: String },
}

/// Byte-accurate span for mutation targeting.
/// All offsets are byte offsets (not char offsets), valid for &source[start..end].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,    // exclusive
}

impl ByteSpan {
    pub fn len(&self) -> usize { self.end - self.start }
    pub fn is_empty(&self) -> bool { self.start == self.end }
}
```

### 3.3a Extraction Intermediate Types

These are produced by the tagger + walker (§4) and consumed by the diff/patch engine (§5). They are not persisted.

```rust
/// Output of a single extraction pass for one file; a list of units in declaration order.
pub enum ExtractedUnit {
    Module(ExtractedModule),
    Class(ExtractedClass),
    Function(ExtractedFunction),
    Import(ExtractedImport),
    Field(ExtractedField),
    Constant(ExtractedConstant),
    TypeAlias(ExtractedTypeAlias),
}

/// Per-file map of tree-sitter node id -> coarse Tag classification + raw capture text.
pub struct TaggedTree<'a> {
    pub source: &'a str,
    pub tags: HashMap<usize, TagInfo>,   // keyed by tree-sitter node id
}

pub struct TagInfo {
    pub tag: Tag,
    pub capture_name: &'static str,      // the .scm capture that produced this
}

/// Walker stack frame kinds — used to determine `is_method` from the immediate parent.
pub enum FrameKind {
    Module,
    Class(ClassId),     // patched up after arena insert (placeholder during walk)
    Function,           // nested defs are closures, not methods
}

/// Hash functions (FNV-1a, 64-bit) — inputs are the parsed string forms, so
/// trivial whitespace-only edits do not change hashes unless they move nodes.
pub fn hash_signature(params: &[Parameter], ret: &Option<String>,
                      decorators: &[String], kind: &FunctionKind, is_async: bool) -> u64;
pub fn hash_body(source: &str, body_span: ByteSpan) -> u64;
pub fn hash_content(source: &[u8]) -> u64;   // xxHash3 of raw file bytes (mutation dedup)
```

### 3.4 Graph Container with Reverse Indexes

```rust
pub struct CodeGraph {
    // Primary storage (one arena per kind, each wrapped in arc-swap::ArcSwap)
    modules:      ArcSwap<SlotMap<ModuleId,   ModuleEntry>>,
    classes:      ArcSwap<SlotMap<ClassId,    ClassEntry>>,
    functions:    ArcSwap<SlotMap<FunctionId, FunctionEntry>>,
    imports:      ArcSwap<SlotMap<ImportId,   ImportEntry>>,
    constants:    ArcSwap<SlotMap<ConstantId, ConstantEntry>>,
    type_aliases: ArcSwap<SlotMap<TypeAliasId, TypeAliasEntry>>,

    // File-level structure
    file_to_modules: HashMap<PathBuf, Vec<ModuleId>>,
    module_by_dotted_name: HashMap<(Language, String), ModuleId>,

    // Reverse indexes
    importers:       HashMap<ModuleId, BTreeSet<ModuleId>>,
    callers_by_callee: HashMap<FunctionId, BTreeSet<FunctionId>>,
    callees_by_caller: HashMap<FunctionId, BTreeSet<FunctionId>>,
    subclasses:      HashMap<ClassId, BTreeSet<ClassId>>,
    overridden_by:   HashMap<FunctionId, BTreeSet<FunctionId>>,

    // Stack Graphs (Rust-native resolution)
    stack_graph_resolver: StackGraphResolver,

    // Import graph (StableDiGraph for O(1) removal)
    import_graph: ImportGraph,

    // Call graph (StableDiGraph for cycle-safe traversals)
    call_graph: CallGraph,

    // Resolution cache
    resolution_cache: ResolutionCache,

    // Concurrency / versioning
    epoch: AtomicU64,
    config: GraphConfig,
}

pub struct ModuleEntry    { inner: Arc<Module> }
pub struct ClassEntry     { inner: Arc<Class> }
pub struct FunctionEntry  { inner: Arc<Function> }
pub struct ImportEntry    { inner: Arc<Import> }
pub struct ConstantEntry  { inner: Arc<Constant> }
pub struct TypeAliasEntry { inner: Arc<TypeAlias> }
```

**Why `Arc` per entry?** Snapshot isolation (§9.1) requires readers to safely observe a consistent past state without blocking writers. Wrapping each entity in `Arc` lets the writer install a new `Arc` in the slot atomically while readers hold the old one.

**Why `BTreeSet` for reverse indexes?** O(log n) insert/remove with deterministic iteration order (reproducible snapshots) and no duplicate edges.

### 3.4a Supporting Graph Types (defined here, used in §6)

```rust
use petgraph::stable_graph::{StableDiGraph, NodeIndex};

/// Import graph — edges are `importer -> imported` file relationships.
/// StableDiGraph keeps NodeIndex valid across removal (O(1) file removal, no rebuild).
pub struct ImportGraph {
    graph: StableDiGraph<ImportNode, ()>,
    path_to_node: DashMap<SmolStr, NodeIndex>,
    node_to_path: DashMap<NodeIndex, SmolStr>,
    exports: DashMap<SmolStr, Vec<Export>>,   // dotted-name -> exports
}

pub struct ImportNode {
    pub path: PathBuf,
    pub module_id: Option<ModuleId>,
    pub language: Language,
}

impl ImportGraph {
    /// O(1) removal. StableDiGraph guarantees surviving NodeIndex values stay valid.
    pub fn remove_file(&self, file_path: &str);
    /// Depth-limited BFS over transitive imports; `visited` set bounds work on cycles.
    pub fn transitive_imports(&self, file_path: &str, max_depth: usize) -> Vec<ImportNode>;
}

/// Call graph — used for Rust-accelerated `call_chain` / `impact_analysis`.
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
    /// Reverse BFS with explicit visited set + depth cap; safe on recursive graphs.
    pub fn find_callers(&self, target_id: &str, max_depth: usize) -> Vec<(CallNode, usize)>;
    /// Shortest call chain via BFS with parent tracking; bounded by max_depth.
    pub fn find_call_chain(&self, source_id: &str, target_id: &str,
                          max_depth: usize) -> Option<Vec<CallNode>>;
}

/// Query snapshot — produced per query; holds the arena pointers read at query start.
pub struct QuerySnapshot {
    pub epoch: u64,
    pub arenas: SnapshotArenas,           // cloned SlotMap inner pointer vecs
}

/// Resolution cascade output — clamped into the resolver's band.
pub struct ResolvedEdge {
    pub source_id: String,               // caller; `::{path}::toplevel` sentinel for module-level refs
    pub target_id: String,
    pub confidence: f32,
    pub method: ResolutionMethod,
    pub kind: ReferenceKind,
    pub line: usize,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
}

pub enum ResolutionMethod {
    StackGraph,          // L1
    ImportConstrained,   // L2
    SignatureMatch,      // L3
    Embedding,           // L4 (Python)
    Lsp,                 // L5 (Python)
}

/// Aggregate configuration — loaded from .coderadar.toml (§15) and frozen at graph build.
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
    Class,
    ClassBase,
    Function,
    FunctionParam,
    FunctionReturn,
    Import,
    ImportFromClause,
    ImportSpecifier,
    Call,
    CallReceiver,
    Decorator,
    Docstring,
    Field,
}
```

### 4.2 Tagging + Walker (Two-Pass Extraction)

**Pass 1 — Tagging.** Tree-sitter `.scm` queries tag nodes with coarse classifications. The `.scm` captures are identical to CodeRadar v2 §4.2 (Python `queries/python.scm`, TypeScript `queries/typescript.scm`).

**Pass 2 — Hierarchy Walker.** A typed stack-frame walker traverses the tagged tree:
- `FrameKind::Module` — root
- `FrameKind::Class(ClassId)` — class body; methods pushed here are classified as methods
- `FrameKind::Function` — function body; nested defs are closures, not methods

This fixes the v1 bugs: the context stack is popped only for frames the walker pushed, and `is_method` is determined by the immediate parent frame's kind, not stack depth.

```rust
// DESIGN PSEUDOCODE — core_indexer/src/extract/walker.rs
fn walk_and_extract(node: Node, ctx: &mut WalkContext) {
    let pushed = if let Some(info) = ctx.tags.tags.get(&node.id()) {
        emit_for_node(node, info, ctx)               // Option<FrameKind>
    } else { None };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_and_extract(child, ctx);
    }

    // Pop ONLY frames *this* invocation pushed — fixes the v1 over-pop bug.
    if let Some(frame_kind) = pushed {
        let popped = ctx.stack.pop();
        debug_assert!(matches!(popped, Some(f) if
            std::mem::discriminant(&f.kind) == std::mem::discriminant(&frame_kind)));
    }
}

fn emit_for_node(node, info, ctx) -> Option<FrameKind> {
    match info.tag {
        Tag::Class => {
            let name       = extract_text(node.child_by_field_name("name")?, ctx);
            let bases      = extract_class_bases(node, ctx);
            let decorators = extract_decorators_for(node, ctx);
            let docstring  = extract_docstring(node, ctx);
            let fields     = extract_class_body_fields(node, ctx);
            let spans      = extract_byte_spans(node);    // §4.6
            ctx.units.push(ExtractedUnit::Class(ExtractedClass {
                name, qualified_name, bases, decorators, docstring, fields, spans, ..
            }));
            ctx.stack.push(Frame { qualified, kind: FrameKind::Class(ClassId::null()) });
            Some(FrameKind::Class(ClassId::null()))    // patched up after arena insert
        }
        Tag::Function => {
            // is_method tests the IMMEDIATE parent frame's kind — not stack depth.
            let is_method = matches!(ctx.stack.last(),
                Some(Frame { kind: FrameKind::Class(_), .. }));
            let kind = derive_function_kind(&decorators, is_method, is_async(node));
            ctx.units.push(ExtractedUnit::Function(ExtractedFunction {
                name, qualified_name, params, return_type, decorators, docstring,
                kind, is_async, is_generator, calls: extract_call_sites(node, ctx),
                signature_hash, body_hash, parse_quality, spans, ..
            }));
            // Push a Function frame so nested defs become closures, not methods.
            ctx.stack.push(Frame { qualified, kind: FrameKind::Function });
            Some(FrameKind::Function)
        }
        Tag::Import     => { emit_import(node, info, ctx); None }
        Tag::Call       => { /* attached to enclosing function's `calls` */ None }
        Tag::Docstring | Tag::Field
        | Tag::ClassBase | Tag::FunctionParam | Tag::FunctionReturn
        | Tag::Decorator | Tag::CallReceiver
        | Tag::ImportFromClause | Tag::ImportSpecifier => None,
    }
}
```

After emission, a single follow-up pass walks `ctx.units` in declaration order and back-patches `FrameKind::Class(ClassId::null())` placeholders with the real `ClassId` returned from the arena insert — this resolves the forward-reference problem during the initial walk.

### 4.3 Decorator Semantics

**Known-decorator table** (Python):

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
| `@dataclass(frozen=True)` | As above, `frozen: true` |
| Unknown | Recorded in `decorators` field; no semantic effect |

**Synthesized methods** are added to the class's `methods` list with `FunctionKind::DataclassSynthesized { from_class }`. They are recomputed whenever the class's fields change.

**Property pairing:** `@x.setter` records `setter_of: FunctionId`, looking up the getter by name within the same class.

**TypeScript/JS decorators:** Stage-3 decorators are recognized syntactically but no semantic effects are applied (Phase 2).

### 4.4 Docstring Extraction

**Python:** First statement of a module/class/function body, if it is a string literal. Enforced by the `.` anchor in `.scm` queries.

**TypeScript:** JSDoc comments extracted in the walker — immediately preceding `comment` whose end-row is exactly `target.start_row - 1` and text begins with `/**`.

### 4.5 Parse Quality and Tainted Symbols

- `ParseQuality::Clean` — subtree has no errors
- `ParseQuality::Partial` — subtree has errors but identifying fields (name, position) are intact
- `ParseQuality::Tainted` — errors affect identifying fields; extraction is best-effort

**Update behavior:** If a file's new version is tainted (any tainted symbol or >5% of the file is `ERROR` nodes), `update_file` returns `fully_applied: false` and **does not commit** — the old graph slice is retained. Override via `update_file(..., force=True)`.

### 4.6 Byte Span Extraction

During extraction, every entity and reference records byte spans (from tree-sitter's native byte offsets):

| Entity | Spans recorded |
|--------|---------------|
| Function | `span` (entire def), `name_span` (identifier), `params_span` (`(...)` ), `body_span` (block, signature excluded), `decorators_span` |
| Class | `span`, `name_span`, `body_span`, `decorators_span` |
| Variable/Constant | `span`, `name_span` |
| Import | `name_span` (symbol slot for rename rewriting) |

All span dereferences go through `slice_span(source, span) -> Result<&str>` which validates char boundaries before slicing.

---

## 5. Incremental Update Algorithm

### 5.1 Update Flow

When a file changes (content, deletion, or creation):

1. **Parse** with tree-sitter → new `ExtractedUnit`s via tagger + walker.
2. **Retrieve previous slice** for the file from `file_to_modules` and dependent indexes.
3. **Diff** old vs new units (§5.2) → `Patch` of `Add | Remove | Modify`.
4. **Compute affected dependents** via reverse indexes:
   - Changed/removed class → all subclasses (transitively) and importers.
   - Changed function signature → all callers (via `callers_by_callee`).
   - Changed function body only → no caller rebuild; update `body_hash`.
   - Changed module → all modules in `importers[module]`.
   - New/changed wildcard import → all symbols using unqualified names from `x`.
5. **Apply patch** under WAL transaction (§5.5).
6. **Re-resolve only affected symbols**:
   - Import targets in changed file.
   - Call sites in changed file.
   - Call sites in affected callers.
   - MRO of affected classes.
7. **Update reverse indexes**.
8. **Bump file version**; bump graph epoch.
9. **Invalidate stale resolution cache entries** (§5.4).
10. **Return `UpdateReport`**.

### 5.2 The Diff Algorithm

**Match key, in order of preference:**

1. **Exact match**: `(kind, qualified_name, signature_hash, body_hash)` identical → no-op.
2. **Same identity, body changed**: `(kind, qualified_name, signature_hash)` match, `body_hash` differs → `Modify { id, new_body_hash }`. No caller rebuild.
3. **Same identity, signature changed**: `(kind, qualified_name)` match, `signature_hash` differs → `Modify { id, full_fields }`. Affected callers re-resolve.
4. **Unmatched old** → `Remove { id }`.
5. **Unmatched new** → `Insert { unit }`.

**Identity collision:** When two overloads have identical `(kind, qualified_name, signature_hash)`, fall back to position-based matching (earliest declaration line number). Phase 5 adds similarity-based rename detection.

**Ordering within a patch:**
1. Insert new modules.
2. Insert new classes (forward-reference placeholders for not-yet-inserted bases).
3. Insert new functions.
4. Insert new imports.
5. Modify existing entities.
6. Resolve forward references.
7. Remove obsolete entities (reverse dependency order: imports → functions → classes → modules).

### 5.3 Cross-File Resolution

CodeRadar performs **static, name-based resolution** using Stack Graphs as the primary engine, with fallback layers.

#### 5.3.1 Import Resolution (Python)

| Form | Resolution |
|------|------------|
| `import foo.bar` | Lookup `module_by_dotted_name[(lang, "foo.bar")]`. If not found, search source roots. If still not found → `External { distribution }`. |
| `from foo.bar import baz` | Resolve `foo.bar` → module `M`. Look for `baz` in `M`'s `exports`. Follow re-export chains. |
| `from . import x` | Resolve `current_module.package`; walk up `k` packages for `level=k`. |
| `from foo import *` | Resolve `foo`. If `__all__` is statically determinable, expose those names. Otherwise fall back to public top-level names (no underscore prefix). |
| `if TYPE_CHECKING: import foo` | Resolved normally; stored with `type_only: true`. |
| `importlib.import_module(name)` | `ImportResolution::Dynamic` unless `name` is a string literal. |

#### 5.3.2 `__all__` and Re-Export Tracking

1. If `__all__ = ["a", "b"]` is present as a top-level string-list literal → only those names exported.
2. If no `__all__` → all top-level names not starting with underscore exported.
3. Re-exports detected when imported name appears in `__all__`. Chain followed during resolution.

**Limits.** `__all__ += [...]`, `__all__.extend([...])`, conditional `__all__`, and f-string entries are treated as non-statically-determinable → fall back to "public top-level names."

#### 5.3.2a `__all__` Merge Rules for Stub/Impl Pairs

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

#### 5.3.3 Call Resolution

For each `UnresolvedRef` in a function's `calls`:

**Classify call shape** → resolve based on shape:

| Shape | Resolution strategy |
|-------|-------------------|
| `Name(...)` | Scope chain: function locals → enclosing locals → module top-level → imports → builtins |
| `self.method(...)` | Enclosing class MRO walk |
| `cls.method(...)` | Same as `self.method` (classmethod context) |
| `Class.method(...)` | Resolve `Class` as name, then MRO lookup |
| `module.name(...)` | Resolve `module` → module's exports |
| `obj.method(...)` | `Unresolved { reason: TypeInferenceRequired }` |
| `chain().method(...)` | `Unresolved { reason: TypeInferenceRequired }` |

#### 5.3.4 MRO Computation (C3 Linearization)

For each class, the MRO is computed lazily on first access and cached on `Class.mro` using the standard C3 algorithm:

```text
L[C] = C + merge(L[B1], L[B2], ..., [B1, B2, ...])
```

`merge` repeatedly takes the head of the first list whose head does **not** appear in the tail (any non-first position) of any other list, removes it from all lists, and appends it to the result.

```rust
// DESIGN PSEUDOCODE
fn c3_linearize(class: ClassId, graph: &CodeGraph) -> (Vec<MroNode>, bool) {
    let bases = graph.resolved_bases(class);     // Vec<ClassId; External bases handled below>
    let mut lists: Vec<Vec<MroNode>> = bases.iter().map(|b| graph.mro(*b).clone()).collect();
    lists.push(bases.iter().map(MroNode::Class).collect());
    let mut result = vec![MroNode::Class(class)];
    loop {
        if lists.iter().all(|l| l.is_empty()) { return (result, true); }
        // Find a candidate head not in the tail of any list.
        let candidate = lists.iter().filter(|l| !l.is_empty())
            .find_map(|l| Some(l[0]))                          // head of some list
            .filter(|head| !lists.iter().any(|other|
                other.iter().skip(1).any(|n| n == head)));      // not in any tail
        match candidate {
            Some(h) => {
                lists.iter_mut().for_each(|l| if l.first() == Some(&h) { l.remove(0); });
                result.push(h);
            }
            None => return (result, false),   // C3 failure — genuine diamond ambiguity
        }
    }
}
```

External bases (`MroNode::External`) are opaque: they participate in ordering but cannot be linearized further. **C3 failure:** the class is marked `EffectiveClass::Abstract` with `mro_error = true`; the partial MRO computed so far is retained so resolution still finds inherited methods where possible.

**Invalidation.** MRO for class `C` is invalidated when:
- One of `C`'s direct bases changes.
- Any transitive ancestor of `C` changes its bases.

Tracked via the `subclasses` reverse index: when class `B`'s bases change, walk `subclasses[B]` transitively (bounded at `max_mro_invalidation_depth`, default 50) and clear each affected class's cached `mro`. **Bounded invalidation safeguard:** if the walk exceeds the depth bound — pathological on deeply nested hierarchies — the entire `method_in_class`
resolution-cache section for all affected classes is flushed rather than clearing entries one at a time, preventing O(n²) blowup. Python inheritance chains rarely exceed 10 levels, so the default is safe.

### 5.4 Resolution Cache

```rust
pub struct ResolutionCache {
    name_in_module: HashMap<(ModuleId, String), Resolution>,
    method_in_class: HashMap<(ClassId, String), FunctionId>,
    import_target: HashMap<(ModuleId, String), ImportResolution>,
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

### 5.5 WAL and Atomicity

**No whole-arena cloning.** Each entity is `Arc<Entity>`. Modifications create new `Arc`s and atomically replace the slot pointer. This is per-entry MVCC.

```rust
pub struct PatchTransaction {
    id: TxId,
    entries: Vec<WalEntry>,
    rollback: Vec<(ArenaKind, SlotKeyRaw, Option<ArcAny>)>,
}

pub enum WalEntry {
    Insert { kind: ArenaKind, key: SlotKeyRaw, entity: ArcAny },
    Modify { kind: ArenaKind, key: SlotKeyRaw, new_entity: ArcAny },
    Remove { kind: ArenaKind, key: SlotKeyRaw },
    IndexInsert { index: IndexKind, key: IndexKey, value: IndexValue },
    IndexRemove { index: IndexKind, key: IndexKey, value: IndexValue },
    TxBegin,
    TxAck,
}

// WAL helper types (previously undefined).
pub type TxId = u64;                       // monotonic per-transaction id
pub enum ArenaKind { Module, Class, Function, Import, Constant, TypeAlias }
pub struct SlotKeyRaw { pub kind: ArenaKind, pub raw: u64 }   // generational key as u64
pub enum ArcAny {                         // type-erased Arc<Entity>
    Module(Arc<Module>), Class(Arc<Class>), Function(Arc<Function>),
    Import(Arc<Import>), Constant(Arc<Constant>), TypeAlias(Arc<TypeAlias>),
}
pub enum IndexKind {
    FileToModules, ModuleByDottedName, Importers,
    CallersByCallee, CalleesByCaller, Subclasses, OverriddenBy,
}
pub enum IndexKey {
    Path(PathBuf),
    DottedName { language: Language, name: String },
    Entity(u64), // generational key
}
pub enum IndexValue {
    Entity(u64), EntityList(Vec<u64>), ModuleList(Vec<ModuleId>),
}
```

**Commit protocol:**
1. **Prepare** — build transaction in memory. Read locks only.
2. **Validate** — re-check preconditions; abort+retry on conflict.
3. **Journal write** (if persistence enabled) — write `WalEntry`s to journal, `fsync()`.
4. **Apply** — walk entries in order; replace `Arc`s atomically. Per-arena write lock (brief).
5. **Journal ack** — write `TxAck` record, `fsync()`.
6. **Bump epoch** — `graph.epoch.fetch_add(1, Ordering::SeqCst)`.
7. **Release locks**.

Crash recovery: replay only journal entries with trailing `TxAck`.

---

## 6. Semantic Resolution Engine

### 6.1 Five-Layer Resolution Cascade

All primary resolution happens in Rust. Layers 4-5 run in Python after the Rust engine returns unresolved references.

```
LAYER 1  Stack Graphs            confidence 0.90 - 1.00   (12 languages, .tsg rules)
LAYER 2  Import Graph + Scope    confidence 0.80 - 0.89   (single match 0.89, ambiguous 0.80)
LAYER 3  Signature Matching      confidence 0.40 - 0.79   (name + arity + proximity)
LAYER 4  Embedding Fallback      confidence 0.20 - 0.39   (Python, threshold 0.85 cosine)
LAYER 5  LSP Override            confidence 1.00          (optional, pool-backed, §14)
```

Bands are **disjoint by construction**: every resolver clamps its output into its band.

### 6.2 Layer 1 — Stack Graphs

Uses the `stack-graphs` crate (0.14+) with language-specific `.tsg` rule files. Production `.tsg` files are derived from the `stack-graphs` project's per-language crates, vendored at build time.

```rust
// DESIGN PSEUDOCODE
pub struct StackGraphResolver {
    graph: StackGraph,
    language_rules: HashMap<Language, TsgRules>,
    file_fragments: LruCache<FilePath, FragmentNodes>,
    spill_dir: PathBuf,
}

impl StackGraphResolver {
    pub fn index_file(&mut self, file_path: &str, source: &str,
                      tree: &Tree, language: Language) -> Result<()>;

    pub fn resolve_reference(&self, file_path: &str,
                             reference: &ParsedReference) -> Option<ResolvedRef>;
}
```

**Incremental indexing.** `index_file` is destructive per-file: it evicts the previous fragment for `file_path` (`evict_fragment`), builds fresh fragment nodes from the current tree, and stores them in the LRU. All other files' fragments stay valid because stack-graph node ids are file-local with a file-handle prefix — removal of one file's fragment never renumbers another's.

**Spill policy.** When the fragments LRU hits the `stack_graph_mb` budget (§15), the least-recently-used fragment is serialized to `.harness/spill/<sha>.zst` and rebuilt lazily on the next `resolve_reference` against that file. The `imports`/`call` cores are never spilled — only the stack-graph fragments, which are the only component that can be reproduced deterministically from source.

**Path scoring.** A resolved `ResolvedRef` carries `confidence` = `score_path(path).clamp(0.90, 1.00)`. `score_path` rewards shorter, less-ambiguous stitching paths:

```text
score_path(path) =
    0.98  - 0.01 * edge_length               // each hop costs 1% confidence
        - 0.01 * num_scopes_crossed          // pushing through nested scopes
        - 0   if exactly one definition reached, else
         (0.04 * num_alternate_definitions)  // multiplicity penalty
```
The single-statement-function reference in a closed module scores ~0.97; a reference requiring 3 scope-pops + 2 import hops scores ~0.91. The clamp keeps every result inside the 0.90–1.00 band.

**Language coverage (Tier 1):** Python (~95%), TypeScript (~93%), JavaScript (~92%), Rust (~90%), Go (~92%), Java (~91%), C (~85%), C++ (~83%), Ruby (~88%), PHP (~87%), C# (~90%), Kotlin (~88%).

Languages without vendored TSG rules fall through to Layers 2–3.

### 6.3 Layer 2 — Import Graph + Scope

Uses the `ImportGraph` (§3.4a) for O(1) file removal. Resolution walks the import graph from the importer `BFS` up to `max_import_depth` (default 3), collecting modules that export the reference name.

```rust
// DESIGN PSEUDOCODE — §3.4a ImportGraph underlies this.
fn resolve_in_imports(&self, file_path, name) -> Vec<ImportMatch> {
    let candidates: Vec<ImportNode> = self.transitive_imports(file_path, max_import_depth);
    candidates.into_iter()
        .filter_map(|n| self.exports.get(&n.path)
            .and_then(|ex| ex.iter().find(|e| e.name == name)
                .map(|e| ImportMatch { module: n, export: e.clone() })))
        .collect()
}
```

Confidence assignment (disjoint from L1):
- Exactly one candidate across the reachable imports → **0.89**.
- Multiple candidates → rank by proximity (same package > same directory > deeper import), choose the best → **0.80** (the band floor) and record an `ambiguous` flag.
- Zero candidates → return `None`; the layer 3 engine picks up.

`include_same_package` (default `true`) lets the importer's own package be searched even without an explicit import (Python package-level resolution).

### 6.4 Layer 3 — Signature Matching

Falls back when neither L1 nor L2 resolves. Matches by name + arity, biased toward proximity. This layer is language-agnostic and works for every Tier 2/3 language.

```rust
// DESIGN PSEUDOCODE
fn signature_match(&self, name, receiver, file_path, definitions) -> Option<Vec<ScoredDef>> {
    let mut scored: Vec<ScoredDef> = definitions.iter()
        .filter(|d| d.name == name)
        .map(|d| ScoredDef {
            def: d,
            score: d.arity_score(receiver) * arity_weight
                 + d.name_exact_score(name)  * name_weight
                 + d.proximity_score(file_path) * proximity_weight,
        })
        .filter(|s| s.score >= min_score)      // default 0.5
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score));
    if scored.is_empty() { None } else { Some(scored) }
}
```

Weights default to name=0.4, arity=0.3, proximity=0.3 (§15). A single strong match's final confidence is `(0.40 + score * 0.39).clamp(0.40, 0.79)` — never overlaps L2's floor (0.80). Below `min_score` the reference falls through to Python Layer 4 (embedding), or reports unresolved if embeddings are disabled.

### 6.5 Layers 4–5 — Python-Side Fallback

After the Rust engine returns unresolved (or below-threshold) references, the Python orchestrator runs two optional fallbacks before persisting edges (see §13.3 and §14).

- **Layer 4 — Embedding Fallback.** Compute a query embedding over the reference's enclosing context, search `func_embedding_idx` (cosine, `top_k`), and attach the top match if `cosine >= 0.85`. Confidence band 0.20–0.39 (linear map: `0.20 + (cosine - 0.85) / (1.0 - 0.85) * 0.19`). Below 0.85 the reference stays `Unresolved::NameNotInScope`.
- **Layer 5 — LSP Override.** If LSP is enabled (§14) and the edge resolved below `override_threshold` (default 0.90), consult `LSPPool.definition(...)`. If the LSP returns a location that maps to a known entity → overwrite with confidence **1.00** and `resolution_method = Lsp`. LSP is authoritative by design; it never lowers a Rust-resolved edge.

Both layers run in the Python commit phase (§6.7 step 2). Failed embedding/LSP lookups never block commit — the edge is persisted at its Rust-layer confidence and a `low_confidence` flag is set for later LSP retry.

### 6.6 `::toplevel` Sentinel

Every edge gets a valid source. References outside any function attach to the file's synthetic `{path}::toplevel` node — no dangling edges.

### 6.7 Staged Two-Phase Commit

```rust
impl SemanticEngine {
    /// Stage all in-memory graph mutations WITHOUT applying them.
    pub fn stage_file(&mut self, parsed: &ParsedFile, source: &str,
                      tree: &Tree) -> StagedChange;

    pub fn commit_staged(&mut self, staged: StagedChange);
    pub fn rollback_staged(&mut self, staged: StagedChange);
}
```

**Python orchestration:**
1. Rust `stage_file()` → `StagedChange` (pure diff, no mutation).
2. Python: embed unresolved references, run LSP fallback, write to LadybugDB.
3. On DB success → Rust `commit_staged()`.
4. On DB failure → Rust `rollback_staged()`.

Idempotency: `stage_file` is a pure diff against current state. Retries are safe.

---

## 7. Query Engine

CodeRadar provides two query interfaces: a **Pest-based query language** for fast in-memory queries, and **Cypher** (delegated to LadybugDB) for persisted queries with vector search.

### 7.1 Pest Grammar (In-Memory)

Operator precedence: `NOT > AND > OR`; aggregations and `derived_call` operands are first-class. The grammar is identical to CodeRadar v2 §6.1; it is reproduced here so this document is self-contained.

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
- **Streaming** (no `group by`, no aggregation): walk the relevant arena, apply `where`, yield lazily. Materialize+sort only if `order by` is given.
- **Aggregated** (any `group by` or aggregation in `select`): materialize groups into `HashMap<GroupKey, GroupAccumulator>`, then iterate (sorted if `order by` references group/agg field).

Both operate on a `QuerySnapshot` (§3.4a) taken at query start, so concurrent updates never perturb the result. Streaming reads need never materialize beyond the current row.

**Cooperative cancellation.** The Python iterator (§7.2a) releases the GIL on the cheap path and polls `py.check_signals()` every `query_check_interval` items (default 64).

### 7.2 Derived Field Catalog

See **Appendix D** for the complete derived field reference with computation cost, caching semantics, and invalidation rules. Derived fields are computed on demand from reverse indexes (part of the snapshot) — there is no separate derived-field cache that could diverge from snapshot state.

### 7.2a Python Query Iterator (FFI)

```rust
#[pyclass]
pub struct QueryIterator {
    inner: Box<dyn Iterator<Item = QueryRow> + Send>,
    cancelled: Arc<AtomicBool>,
    check_interval: usize,           // default 64
    items_since_check: usize,
}

#[pymethods]
impl QueryIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> { slf }
    fn __next__(&mut self, py: Python) -> PyResult<Option<PyObject>> {
        self.items_since_check += 1;
        if self.items_since_check >= self.check_interval {
            self.items_since_check = 0;
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(PyKeyboardInterrupt::new_err("query cancelled"));
            }
            py.check_signals()?;
        }
        Ok(self.inner.next().map(|r| r.into_py(py)))
    }
    fn cancel(&self) { self.cancelled.store(true, Ordering::Relaxed); }
}
```

Query examples (committed parse targets — see Appendix D for the derived fields):

```sql
classes where inherits_from contains "BaseModel"
functions where line_count > 50
functions where caller_count == 0 and not name matches "^test_.*"
classes where method_count > 20 order by method_count desc limit 25
functions where module.name == "app.services" and is_async == true
classes where has_method("__init__") == true and has_method("__eq__") == false
classes select module.name, count(*) as class_count, avg(method_count) as avg_methods
  group by module.name order by class_count desc limit 20
calls where unresolved_reason == "TypeInferenceRequired"
functions where decorators contains "deprecated"
imports where kind == "StarImport"
functions where kind == "Property" and has_setter == false
functions where overrides_of("BaseService.handle") == true
```

### 7.3 Cypher Query Templates (LadybugDB)

For queries requiring vector search or persistent traversal. All relationship-property predicates live in `WHERE` (Kùzu pattern maps support equality only). The full template library is reproduced here.

```cypher
-- Scope Exploration with pre-filtered vector search
MATCH (root:File {path: $root_path})
OPTIONAL MATCH (root)-[:IMPORTS*1..2]->(dep:File)
WITH collect(DISTINCT dep) + [root] AS scope_files
UNWIND scope_files AS sf
MATCH (sf)-[:DECLARES_FUNC]->(target:Function)
WITH collect(DISTINCT target.id) AS candidate_ids
CALL db_similarity_search('func_embedding_idx', $query_embedding, $top_k,
                          {filter: {id: candidate_ids}})
YIELD node AS matched, score
OPTIONAL MATCH (matched)<-[r:CALLS]-(caller:Function)
OPTIONAL MATCH (matched)-[r2:CALLS]->(callee:Function)
WHERE r.confidence > 0.7
RETURN matched.name, matched.signature, matched.body, matched.docstring,
       parent.path AS file_path, score, collect(DISTINCT callee.name) AS calls
ORDER BY score DESC
LIMIT $top_k;
-- Methods are queried analogously against method_embedding_idx and merged
-- in the executor (two searches, one ranked merge).

-- Impact Analysis (reverse dependency)
MATCH (target:Function {id: $target_id})
MATCH (caller:Function)-[:CALLS*1..$depth]->(target)
WITH DISTINCT caller
MATCH (caller)-[r:CALLS]->(other:Function)
RETURN caller.name, caller.signature, caller.body, parent_file.path,
       collect(DISTINCT other.name) AS also_calls
ORDER BY parent_file.path;

-- Call Chain
MATCH path = (src:Function {name: $source_name})-[:CALLS*1..$max_depth]->(tgt:Function {name: $target_name})
WITH path, length(path) AS depth
ORDER BY depth
LIMIT 5
UNWIND nodes(path) AS node
WITH collect(node.name) AS chain, depth
RETURN chain, depth;

-- Global Similarity Search
CALL db_similarity_search('func_embedding_idx', $query_embedding, $top_k)
YIELD node AS matched, score
OPTIONAL MATCH (matched)-[r:CALLS]->(callees:Function)
RETURN matched.name, matched.signature, matched.body, matched.docstring,
       parent.path, score, collect(DISTINCT callees.name) AS calls
ORDER BY score DESC;

-- Dependency Graph
MATCH (root:File {path: $root_path})-[imp:IMPORTS*1..$depth]->(dep:File)
OPTIONAL MATCH (dep)-[:DECLARES_CLASS]->(cls:Class)
OPTIONAL MATCH (dep)-[:DECLARES_FUNC]->(func:Function)
RETURN dep.path, dep.language,
       collect(DISTINCT cls.name) AS classes,
       collect(DISTINCT func.name) AS functions,
       length(imp) AS depth
ORDER BY depth, dep.path;

-- Definition Lookup
MATCH (func:Function)
WHERE func.name = $name OR func.qualified_name = $name
OPTIONAL MATCH (func)-[:HAS_PARAM]->(param:Parameter)
OPTIONAL MATCH (func)-[r:CALLS]->(callees:Function)
OPTIONAL MATCH (callers:Function)-[r2:CALLS]->(func)
RETURN func.name, func.signature, func.body, func.docstring, parent.path,
       collect(DISTINCT param.name) AS parameters,
       collect(DISTINCT callees.name) AS calls,
       collect(DISTINCT callers.name) AS called_by;
```

The query cache is an LRU keyed on `(template_id, params, graph_epoch)`; invalidated on every write. `default_top_k = 10`, `cache_ttl_seconds = 300`, `cache_max_size = 256` (§15).

### 7.4 Rust-Accelerated Traversal

`call_chain` and `impact_analysis` intents are served from the in-memory `CallGraph` (O(1) neighbor access via `StableDiGraph`), then enriched with code bodies from LadybugDB.

---

## 8. Python API

```python
import coderadar

# Initial analysis
graph = coderadar.analyze("src/")

# Update after LLM writes a file
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
for row in rows:
    print(row["module_name"], row["n"])

# Cypher query (delegates to LadybugDB)
results = graph.cypher("""
    MATCH (f:Function)
    WHERE f.name = $name
    RETURN f.name, f.signature, f.body
""", name="validate_email")

# Snapshot export
graph.export_snapshot("./.coderadar/snapshot.bin")

# Watch mode
with coderadar.watch("src/") as w:
    for event in w:
        print(event.affected_files, event.elapsed_ms)

# ID-based access
fn_id = graph.find_function("app.services.UserService.create")
fn = graph.get_function(fn_id)        # None if removed
callers = graph.callers_of(fn_id)

# Mutation (LLM-driven)
plan = graph.plan_body_replacement(
    entity_id="src/auth.py::validate_user",
    new_body="    return bool(re.match(r'^[^@]+@[^@]+$', email))",
    expected_hash="abc123...",
    dry_run=True
)
print(plan.diff_preview)

result = graph.apply(plan)            # atomic, verified
print(result.status, result.files_written)
```

### 8.1 Entity Wrappers

Each entity type has a Python wrapper that lazily fetches fields via PyO3. Wrappers carry `(epoch, SlotMap key)`; calls that find a stale epoch raise `coderadar.StaleHandle` (this is the user's signal that the underlying entity changed and must be re-fetched).

```python
class Class:
    id: int
    name: str
    qualified_name: str
    file: str
    line: int
    bases: list[str]                # raw, unresolved names
    resolved_bases: list[Class]
    mro: list[Class | ExternalRef]
    methods: list[Function]
    fields: list[Field]
    decorators: list[str]
    effective: EffectiveClass       # tagged union
    docstring: str | None
    parse_quality: ParseQuality
    subclasses: list[Class]         # via reverse index (cheap)
    method_count: int
```

### 8.2 `UpdateReport`

`graph.update_file` / `graph.batch` return an `UpdateReport` describing the resulting change:

```python
@dataclass(frozen=True)
class UpdateReport:
    affected_files: list[str]
    changed_symbols: list[SymbolChange]
    new_unresolved_references: list[UnresolvedRef]
    newly_resolved_references: list[ResolvedRef]
    elapsed_ms: float
    parse_quality: ParseQuality      # quality of the *primary* file
    parse_errors: int                # count of ERROR nodes
    fully_applied: bool              # false if rejected (tainted, see §4.5/§19.2)
    epoch_before: int
    epoch_after: int

@dataclass(frozen=True)
class SymbolChange:
    kind: Literal["module", "class", "function", "import",
                  "constant", "type_alias", "field"]
    operation: Literal["added", "removed", "signature_changed",
                       "body_changed", "moved"]
    qualified_name: str
    file: str
    line: int
    id: int                          # SlotMap key (valid only if operation != "removed")
```

---

## 9. Concurrency, Locking, and Snapshot Isolation

### 9.1 Snapshot Isolation via ArcSwap and Epochs

Adopted from CodeRadar v2 §8.1 — the most rigorously specified part of either document.

- Every entity is stored as `Arc<Entity>` in its slot.
- Each `SlotMap` arena is wrapped in `arc_swap::ArcSwap<SlotMapInner>`.
- A query takes a `QuerySnapshot { epoch, arena_refs }` by cloning arena pointers (O(1)).
- A concurrent update:
  1. Clones the `SlotMapInner` (Vec of slots — pointer copy, ~800KB for 100k entities, ~100µs).
  2. Mutates the clone (insert/update/remove `Arc<Entity>` pointers).
  3. `ArcSwap::store` the new inner — one atomic pointer swap.

**Cost summary:**
- Query start: O(1) — one `ArcSwap::load` per arena.
- Per-item iteration: O(1) — one slot index. No locks.
- Update commit: **O(arena_size)** for the inner-vector clone. For 100k entities ~100µs per arena.

### 9.2 Single-Writer, Multiple-Reader

One ingestion worker holds the write lock. Query threads read MVCC snapshots. The MutationEngine (§11) acquires the same writer lock during apply. Embedding pool bounded at 2 workers.

### 9.3 Writer Throughput

Target: 1,000 file changes ingested in < 30 s. Chunked sub-transactions (200 files per chunk) so readers are never blocked > 2 s.

### 9.4 Lock Hierarchy

Arenas locked in fixed order: `modules → classes → functions → imports → indexes`. Resolution cache uses its own `RwLock`, acquired after arena locks. Readers never escalate.

### 9.5 GIL Handling

Long-running Rust methods wrap in `py.allow_threads()`. Query iterator's `__next__` does not release the GIL on the cheap path but calls `py.check_signals()` at configured intervals.

---

## 10. Persistence (LadybugDB)

### 10.1 Database Schema

LadybugDB (Kùzu Cypher dialect) provides ACID persistence. The schema is configuration-driven to avoid hardcoded embedding dimensions (§10.3). The dimension constants below reflect the default 896-d model.

**Node tables (physical structure + code entities + metadata + audit):**

```cypher
-- PHYSICAL STRUCTURE
CREATE NODE TABLE Module (
    id STRING PRIMARY KEY, name STRING, path STRING, language STRING,
    package_type STRING, updated_at TIMESTAMP
);
CREATE NODE TABLE File (
    id STRING PRIMARY KEY, path STRING, language STRING, size_bytes INT64,
    line_count INT64, content_hash STRING, last_modified TIMESTAMP,
    git_blame_author STRING, git_blame_commit STRING, updated_at TIMESTAMP
);

-- CODE ENTITIES (Function shown; Class/Method/Variable reuse the span pattern)
CREATE NODE TABLE Function (
    id STRING PRIMARY KEY, name STRING, qualified_name STRING, signature STRING,
    body STRING, docstring STRING,
    start_line INT64, end_line INT64,
    start_byte INT64, end_byte INT64,                 -- whole definition span
    name_start_byte INT64, name_end_byte INT64,       -- identifier only
    params_start_byte INT64, params_end_byte INT64,   -- "(...)" parameter list
    body_start_byte INT64, body_end_byte INT64,       -- block, signature excluded
    is_async BOOLEAN, is_generator BOOLEAN, is_static BOOLEAN, is_property BOOLEAN,
    is_toplevel BOOLEAN,                              -- synthetic module-level sentinel
    visibility STRING, decorators STRING[], content_hash STRING,
    embedding FLOAT[896], embedding_short FLOAT[64], updated_at TIMESTAMP
);
CREATE NODE TABLE Class    ( /* as Function, minus params_*, plus is_abstract, bases */ );
CREATE NODE TABLE Method   ( /* as Function, plus parent_class, is_class_method, is_abstract */ );
CREATE NODE TABLE Variable ( /* id, name, type_annotation, is_global, is_constant,
                                is_exported, start_line, start_byte, end_byte,
                                name_start_byte, name_end_byte, content_hash */ );
CREATE NODE TABLE Parameter (
    id STRING PRIMARY KEY, name STRING, type_annotation STRING, position INT64,
    has_default BOOLEAN, default_value STRING, is_variadic BOOLEAN, is_keyword_only BOOLEAN
);
CREATE NODE TABLE Import (
    id STRING PRIMARY KEY, module_path STRING, symbol_name STRING, alias STRING,
    is_wildcard BOOLEAN, is_relative BOOLEAN, start_line INT64,
    name_start_byte INT64, name_end_byte INT64        -- symbol slot, for rename rewriting
);

-- METADATA
CREATE NODE TABLE ChangeEvent (
    id STRING PRIMARY KEY, entity_id STRING, entity_type STRING, change_type STRING,
    timestamp TIMESTAMP, old_hash STRING, new_hash STRING, trigger STRING
);
CREATE NODE TABLE IndexMetadata (
    id STRING PRIMARY KEY, schema_version INT64, last_full_index TIMESTAMP,
    last_incremental_index TIMESTAMP, total_files INT64, total_entities INT64,
    embedding_model STRING, embedding_dimension INT64
);

-- MUTATION AUDIT — retention is tiered (§11.8)
CREATE NODE TABLE MutationLog (
    id STRING PRIMARY KEY,                            -- plan ULID
    tool STRING, entity_id STRING, plan_json STRING,  -- nulled after summarize window
    affected_files STRING[], edit_count INT64,
    status STRING,                                    -- applied | rolled_back | rejected_stale
    syntax_errors STRING[], trace_id STRING, timestamp TIMESTAMP
);
```

**Relationship tables:**

```cypher
-- STRUCTURAL (SYNTACTIC) — from Tree-sitter AST
CREATE REL TABLE CONTAINS_MODULE (FROM Module TO Module);
CREATE REL TABLE CONTAINS_FILE (FROM Module TO File);
CREATE REL TABLE DECLARES_CLASS (FROM File TO Class);
CREATE REL TABLE DECLARES_FUNC (FROM File TO Function);
CREATE REL TABLE DECLARES_VAR (FROM File TO Variable);
CREATE REL TABLE HAS_METHOD (FROM Class TO Method);
CREATE REL TABLE HAS_PARAM (FROM Function TO Parameter);
CREATE REL TABLE HAS_PARAM (FROM Method TO Parameter);
CREATE REL TABLE HAS_IMPORT (FROM File TO Import);
CREATE REL TABLE NESTED_IN (FROM Class TO Class);
CREATE REL TABLE NESTED_IN (FROM Function TO Function);

-- SEMANTIC (resolved by the Rust semantic engine); CALLS is a REL GROUP across
-- (Function→Function, Method→Method, Function→Method):
CREATE REL TABLE CALLS (
    FROM Function TO Function,
    confidence FLOAT, resolution_method STRING,
    callsite_line INT64, is_conditional BOOLEAN, resolved_at TIMESTAMP,
    callsite_start_byte INT64, callsite_end_byte INT64,   -- mutation cascade targets
    args_start_byte INT64, args_end_byte INT64
);
CREATE REL TABLE CALLS (FROM Method TO Method,    /* same properties */ );
CREATE REL TABLE CALLS (FROM Function TO Method,   /* same properties */ );
CREATE REL TABLE INSTANTIATES (FROM Function TO Class, confidence FLOAT, resolution_method STRING, site_line INT64);
CREATE REL TABLE IMPORTS (FROM File TO File, module_name STRING, symbols STRING[], is_relative BOOLEAN);
CREATE REL TABLE EXTENDS (FROM Class TO Class, confidence FLOAT);
CREATE REL TABLE IMPLEMENTS (FROM Class TO Class, confidence FLOAT);
CREATE REL TABLE OVERRIDES (FROM Method TO Method, confidence FLOAT);
CREATE REL TABLE READS_FROM (FROM Function TO Variable);
CREATE REL TABLE WRITES_TO (FROM Function TO Variable);
CREATE REL TABLE READS_FROM (FROM Method TO Variable);
CREATE REL TABLE WRITES_TO (FROM Method TO Variable);
CREATE REL TABLE RETURNS_TYPE (FROM Function TO Class);
CREATE REL TABLE RETURNS_TYPE (FROM Method TO Class);
CREATE REL TABLE HAS_TYPE (FROM Variable TO Class);
CREATE REL TABLE HAS_TYPE (FROM Parameter TO Class);
CREATE REL TABLE DECORATED_BY (FROM Function TO Function);
CREATE REL TABLE DECORATED_BY (FROM Class TO Function);
CREATE REL TABLE TRIGGERED_BY (FROM MutationLog TO File);
```

**Vector indexes (HNSW):**

```cypher
CALL create_hnsw_index('func_embedding_idx',       'Function', 'embedding',
    {dimension: 896, metric: 'cosine', ef_construction: 128, m: 16});
CALL create_hnsw_index('func_embedding_short_idx', 'Function', 'embedding_short',
    {dimension:  64, metric: 'cosine', ef_construction:  64, m:  8});
CALL create_hnsw_index('method_embedding_idx',     'Method',   'embedding',
    {dimension: 896, metric: 'cosine', ef_construction: 128, m: 16});
CALL create_hnsw_index('class_embedding_idx',     'Class',    'embedding',
    {dimension: 896, metric: 'cosine', ef_construction: 128, m: 16});
```

**Span validity rule.** All byte columns are trustworthy only while the on-disk file content hashes to the row's `content_hash`. The MutationEngine enforces this (§11.6 step 3); queries treat spans as display hints. Spans refresh on every reindex.

### 10.2 Schema Versioning

```python
SCHEMA_VERSION = 4

MIGRATIONS: dict[int, Migration] = {
    2: Migration(kind="additive", statements=[...]),
    3: Migration(kind="destructive", reason="embedding dimension 768 -> 896",
                 action="background_reindex"),
    4: Migration(kind="additive", statements=[...]),
}
```

Additive migrations apply at startup. Destructive migrations build `semantic.db.next` in background, verify counts, then atomically swap.

### 10.3 Embedding Configuration

```python
class EmbeddingConfig(BaseModel):
    model: str = "jinaai/jina-code-embeddings-0.5b"
    dimension: int = 896
    truncated_dimension: int = 64   # Matryoshka pre-filter
    max_body_tokens: int = 2000
    batch_size: int = 32
```

---

## 11. AST-Aware Mutation Engine

### 11.1 Design

The MutationEngine extends CodeRadar from read-only to read-write. The LLM decides **what** to change semantically; the Rust core computes **where** and **how** at the byte level. No regex, no search-and-replace.

It reuses three existing mechanisms:

| Mechanism | Reused for |
|-----------|------------|
| Staged two-phase commit (§6.7) | Mutation apply/rollback |
| Content-addressed xxHash (§13.1) | Optimistic concurrency control (`expected_hash`) |
| Single-writer lock (§9.2) | Mutations and ingestion never interleave |

### 11.2 Four Refactoring Tools

All four share the `expected_hash` optimistic-concurrency control and the §11.6 apply pipeline. The planner is pure and side-effect-free; only `apply` mutates.

```rust
// DESIGN PSEUDOCODE — core_indexer/src/mutation/mod.rs
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
Replaces the function/method body — signature, docstring, and decorators are untouched. One edit replacing `body_span`; target indentation = leading whitespace of `body_span`'s first line. The `new_body` is indent-normalized (§11.4) before the rope edit.

#### `update_signature` (the flagship — signature cascade)
Rewrites the definition `params_span` **and** every verified call site's argument list so the LLM never sees the 50 files.

1. **Preflight parse.** `new_signature` is parsed standalone. Parse failure → plan rejected with a `SyntaxDiagnostic`. If the embedded name differs from the current name → rejected with guidance to use `rename_symbol`.
2. **Parameter diff.** Old vs new parameter lists → `added[]`, `removed[]`, `renamed[]`, `reordered[]`, `retyped[]`.
3. **Definition edit.** One edit replacing `params_span`.
4. **Call-site enumeration.** `call_graph.find_callers(entity_id, depth=1)` ∪ Stack-Graph references of kind `Call`, filtered to edges with a stored `args_span` and `confidence ≥ 0.8`.
5. **Per-site rewrite rules** (each site parsed with tree-sitter; argument spans located in the AST, never by text search):

   | Parameter change | Positional call site | Keyword call site |
   |---|---|---|
   | Added, has default, `inject_defaults=false` | skip | skip |
   | Added, has default, `inject_defaults=true` | insert `expr` at index | append `name=expr` |
   | Added, required | insert `call_site_values[name]` at index | append `name=` |
   | Removed | delete argument at old index | delete by keyword name |
   | Renamed | untouched (positional semantics unchanged) | rewrite keyword `old=` → `new=` |
   | Reordered | rewrite argument order to new positions | untouched |

6. **Preflight completeness.** If any required parameter lacks a `call_site_values` entry → plan rejected with the full list of affected call sites, so the LLM can supply expressions with full knowledge.
7. **Unverified sites.** Call sites with `confidence < 0.8`, dynamic dispatch, or macro bodies are listed in `unverified_sites` for manual review — never auto-edited.

#### `rename_symbol`
Renames a function, method, class, or variable. Rewrites the definition `name_span`, every Stack-Graph-resolved (L1) and import-constrained (L2) reference's `name_span` (scope-aware by construction, so a shadowing local in another module is untouched), and import statements (replaces the symbol slot, preserving aliases). Qualified usages (`auth.login(...)`) rewrite the attribute identifier node only. String-literal occurrences are rewritten only when `include_strings=true`, and each such site is flagged in `unverified_sites` for human confirmation.

#### `create_entity`
Creation anchored after an existing entity (insertion point = anchor's `span.end` + blank-line normalization) or at file `top`/`end`. The new code is indent-normalized to the anchor's sibling level (§11.4) and parse-checked **in context** (a synthetic file = target + insertion is parsed) before the plan is issued. Symbols the new entity references that are not resolvable are listed in `warnings` — imports are **not** added automatically.

Shared result schemas (`MutationPlan` and `MutationResult`) live in Appendix A.

### 11.3 Rope-Based Multi-Edit Application

```rust
pub fn apply_edits_to_file(source: &str, edits: &[MutationEdit]) -> Result<String> {
    let mut rope = Rope::from_str(source);
    let mut ordered: Vec<&MutationEdit> = edits.iter().collect();
    ordered.sort_by(|a, b| b.span.start.cmp(&a.span.start));   // descending offsets
    for edit in ordered {
        let (s, e) = rope_clamped_char_bounds(&rope, edit.span)?;
        rope.remove(s..e);
        rope.insert(s, &edit.replacement);
    }
    Ok(rope.to_string())
}
```

### 11.4 Indent Normalization

LLMs routinely paste code at column 0. The engine normalizes indentation **before** rope application (§11.3) and parse verification (§11.6 step 6), so repair attempts are reserved for genuinely semantic errors.

```rust
// DESIGN PSEUDOCODE — core_indexer/src/mutation/indent.rs
pub struct IndentStyle { pub unit: char /* ' ' or '\t' */, pub width: usize }

/// Detect the file's dominant convention: tabs if tab-indented lines outnumber
/// space-indented ones, else spaces with the most common leading-run width.
pub fn detect_indent_style(source: &str) -> IndentStyle;

pub fn normalize_indent(new_code: &str, target: &str, style: IndentStyle,
                        verbatim_spans: &[ByteSpan]) -> String {
    // 1. incoming_base = minimum leading whitespace across non-empty lines
    // 2. Per line: target + (line with incoming_base stripped), preserving
    //    relative depth exactly
    // 3. Convert leading whitespace to style (tabs <-> spaces)
    // 4. Lines inside verbatim_spans (multi-line string-literal interiors,
    //    detected by the standalone parse of new_code) are preserved verbatim —
    //    re-indenting them would change runtime string content
    // 5. Empty lines stay empty (no trailing whitespace introduced)
}
```

**Application points.**
- `replace_entity_body`: `target` = leading whitespace of the first line of `body_span`.
- `create_entity`: `target` = leading whitespace of the anchor's first line (sibling level), or the file's base level for `position = "top"`.

Normalization is **total and side-effect-free**: the worst case is the identity (code was already at the correct level). It never fails and never consumes a repair attempt; if the normalized code still does not parse, the error is genuinely semantic and enters the §11.6 repair loop as usual.

### 11.5 WriteGuard — Watcher Self-Write Suppression

```rust
pub struct WriteGuard {
    suppressed: DashMap<PathBuf, (String, Instant)>,  // path -> (expected hash, expiry)
}
```

When the engine writes mutated files, the watcher would otherwise trigger on them. `WriteGuard.suppress()` inserts a 5s TTL entry. The watcher checks `should_drop()` for every debounced event.

### 11.6 Mutation Apply Pipeline

```
LLM tool call (dry_run=false)
  │
  ├─ 1. Policy check: allow/deny globs, budgets, git cleanliness
  ├─ 2. Acquire single-writer lock
  ├─ 3. Hash guard: every file's on-disk xxHash == edit.expected_hash
  │       mismatch → RejectedStale
  ├─ 4. Snapshot originals → .harness/backups/{plan_id}/   (zstd, 24h)
  ├─ 5. Per file: indent normalize → Rope apply → candidate content
  ├─ 6. VERIFY: re-parse every candidate with Tree-sitter
  │       NEW ERROR nodes RELATIVE to the pre-mutation parse → full rollback
  │       from snapshot, return SyntaxDiagnostic[] (Python feeds these verbatim
  │       to the LLM; max_repair_attempts = 3)
  │       NOTE: indentation failures are normalized upfront (§11.4) and never
  │       reach this step, so they never consume a repair attempt. A file that
  │       was already partially broken is NOT rejected unless the mutation made
  │       it worse.
  ├─ 7. Register paths in WriteGuard
  ├─ 8. Atomic write: temp file + rename()
  ├─ 9. Synchronous reindex through §6 pipeline
  ├─ 10. Release writer lock; MutationLog entry; metrics
  └─ 11. Return MutationResult
```

### 11.7 Mutation Policy

```toml
[mutation]
enabled = true
default_dry_run = true
max_files_per_plan = 100
max_edits_per_plan = 500
max_body_tokens = 4000
backup_retention_hours = 24
post_verify = true
max_repair_attempts = 3
require_clean_git = false
allow = ["src/", "lib/", "tests/", "scripts/"]
deny  = [".git/", ".harness/", "/migrations/", "/*.lock", "/generated/"]
```

Hard guarantees (independent of config):
- **Never commits** — mutations only touch git working tree.
- **Never touches deny-listed paths.**
- **Stale contexts always rejected** (`expected_hash`).
- **All-or-nothing across files.**
- **Every mutation audited** — MutationLog + backup + trace_id.

### 11.8 MutationLog Retention (Tiered)

MutationLog is append-only and grows with every tool call; the `plan_json` field (which embeds the full unified diff) dominates row size. Left unbounded it is the only steadily-growing table in the schema. Retention is tiered:

| Age | Retained |
|-----|----------|
| 0 – `audit_summarize_after_days` (7) | Full row including `plan_json` with diff |
| 7 – `audit_retention_days` (30) | Summary only: `tool`, `entity_id`, `affected_files`, `edit_count`, `status`, `syntax_errors`, `trace_id`, `timestamp`; `plan_json` nulled |
| > 30 days | Pruned |

The `audit_max_entries` (10,000) hard cap applies at all times, evicting oldest first regardless of age. Pruning runs at daemon startup and every 24 h, alongside — but independent of — backup expiry: backups serve rollback (`backup_retention_hours`), MutationLog serves audit. The audit-critical fields (`tool`, `entity_id`, `status`, `trace_id`, `timestamp`) survive the full retention window even after the diff payload is dropped.

**Chunked pruning.** Deletes run as chunked transactions (500 rows each) interleaved with reader snapshots, so the §9.3 ≤2 s reader blackout bound is never blown by an audit prune sweeping thousands of rows. Pruning is an additive maintenance operation — no schema migration is required.

### 11.8a Mutation Result Types

```rust
pub enum MutationStatus {
    Applied,                          // files written, reindex succeeded
    RolledBack,                       // step 6 found new ERROR nodes; nothing persisted
    RejectedStale,                    // step 3 hash mismatch; nothing written
}

pub struct ReindexSummary {
    pub files: usize,
    pub entities_updated: usize,
    pub edges_updated: usize,
    pub duration_ms: u64,
}
```

The `status` field's three values map to distinct LLM-recovery contracts: `Applied` requires read-back (`definition_lookup`); `RolledBack` requires repair (`SyntaxDiagnostic[]` fed to the next message); `RejectedStale` requires a fresh context re-fetch (the file changed unexpectedly under the plan).

---

## 12. Git Integration

### 12.1 Branch-Switch Detection

Uses `git2::Diff::foreach` to detect HEAD changes. On branch switch, returns the list of changed files for re-indexing.

### 12.2 .gitignore Integration

Via the `ignore` crate: `.gitignore` + `.harnessignore` + built-in defaults (`node_modules/`, `target/`, `dist/`, `build/`, `__pycache__/`, `.git/`, `*.min.js`, `vendor/`, `.venv/`, `.harness/`).

### 12.3 Blame

Uses `git2::Blame` with `newest_commit(true)`. Refreshes lazily (at most once per file per HEAD change). Populates `File.git_blame_author` / `File.git_blame_commit`.

---

## 13. Embedding & GraphRAG Pipeline

### 13.1 Content-Addressed Deduplication

Before embedding an entity, compute `xxHash` of its body. If the hash matches the `content_hash` already stored on the row → skip the embedding call and reuse the existing vector from LadybugDB. This is the dominant steady-state win: in steady state more than 85% of entity bodies are unchanged between edits, so most updates touch only graph edges, not the model.

```python
# DESIGN PSEUDOCODE — py_agent/src/embedding/dedup.py
def embed_batch(self, to_embed, db) -> list[Vector]:
    out = []
    for e in to_embed:
        cached = db.get_embedding(e.id, e.content_hash)   # (id, hash) check
        if cached is not None:
            self.metrics.inc("embedding.cache_hit")
            out.append(cached); continue
        out.append(None)
    miss_idx = [i for i, v in enumerate(out) if v is None]
    if miss_idx:
        vectors = self.model.embed([to_embed[i].body for i in miss_idx])
        for i, v in zip(miss_idx, vectors):
            out[i] = v
            db.set_embedding(to_embed[i].id, to_embed[i].content_hash, v)
    return out
```

### 13.2 Embedding Generation & Backpressure

Embeddings are produced by `fastembed` (ONNX, ``jinaai/jina-code-embeddings-0.5b``, 896-d, Matryoshka-truncatable to 64-d for the pre-filter index). Bodies are truncated at `max_body_tokens` (2000) before embedding; very short bodies (`< 8` tokens) fall back to the bare signature.

Each batch carries an embedding time budget (`embedding_budget_ms`, default 2000 ms); the pool reports elapsed time per sub-batch. If a batch exceeds its budget it is split: embedded entities proceed to commit; the remainder re-queues as a lower-priority continuation batch. Non-critical embeddings are deferrable: entities only reachable via Layer-3 edges (confidence < `defer_low_priority_below`, default 0.6) queue behind everything else. The outer governor is a hard cap on pending low-priority batches (default 100) — above it, new low-priority embeddings are skipped and recomputed lazily on next query.

### 13.3 GraphRAG Query Execution

1. **Query Planner** classifies natural-language intent into one of six classes and extracts parameters:

   | Intent | Parameters | Primary template (§7.3) |
   |--------|------------|------------------------|
   | `scope_exploration` | `root_path`, `query_text` | Scope Exploration |
   | `impact_analysis` | `target_id`, `depth` | Impact Analysis |
   | `call_chain` | `source_name`, `target_name`, `max_depth` | Call Chain |
   | `similarity_search` | `query_text`, `top_k` | Global Similarity |
   | `dependency_graph` | `root_path`, `depth` | Dependency Graph |
   | `definition_lookup` | `name` (qualified or simple) | Definition Lookup |

2. **Template selection** — parameterized Cypher template from §7.3, with the query text embedded by `fastembed` and passed as `$query_embedding` / `$query_short`.
3. **Execution** — LadybugDB with HNSW vector search (`db_similarity_search`). Two-stage for `similarity_search`/`scope_exploration`: 64-d Matryoshka pre-filter (top 50) → 896-d refinement (top 10) — keeps the expensive full-vector search bounded.
4. **Rust-accelerated traversal** — `call_chain` and `impact_analysis` intents are served from the in-memory `CallGraph` (`find_call_chain` / `find_callers`), then enriched with code bodies from LadybugDB. This is the `use_rust_graph_for_traversal` config flag (default on).
5. **Context Builder** — `grep-ast` structural compression runs over the result set with a token budget (`max_context_tokens`, default 8192). Three strategies, selected per query:

   | Strategy | What's kept | Tokens / entity |
   |----------|-------------|----------------|
   | `signatures_only` | Signatures + docstring + decorators | ~50–150 |
   | `structural` | Above + control-flow skeleton (`if`/`for`/`return` lines, calls collapsed to `name(...)`) | ~200–600 |
   | `full` | Entire bodies | ~400–4000 |

   The builder starts with `signatures_only`, and promotes the top-ranked entities to `structural`/`full` until the budget is exhausted. The query result carries `tokens_used` and `strategy_per_entity` so the LLM can see what it got.

---

## 14. LSP — Optional Persistent Warm Pool

Disabled by default (`[resolution.lsp] enabled = false`). When enabled, LSP servers run as **persistent, warm processes — never spawned per request.**

```python
# DESIGN PSEUDOCODE — py_agent/src/lsp/pool.py
class LSPPool:
    """
    One long-lived server process per enabled language, shared across all files.
    Spawned once on first use; initialized with the workspace root.
    Kept synchronized via textDocument/didOpen + didChange on every ingestion.
    Idle servers shut down after idle_timeout_s (default 600) and re-spawn lazy.
    Definition lookups hit a TTL cache keyed on (path, line, col, content_hash).
    """
    def ensure_server(self, language: str, workspace_root: str) -> ManagedServer: ...

    def sync_file(self, path: str, text: str) -> None:
        """Called by ingestion BEFORE any LSP query for this file."""
        lang = detect_language(path)
        if not self.is_enabled(lang): return
        server = self.ensure_server(lang, workspace_root_of(path))
        if server.is_open(path):
            server.did_change(path, text, version=server.bump_version(path))
        else:
            server.did_open(path, text, lang, version=1)
        self.cache.invalidate_prefix(path)      # stale results die here

    def definition(self, path, line, col, content_hash):
        key = (path, line, col, content_hash)
        if (hit := self.cache.get(key)) is not None: return hit
        server = self.ensure_server(detect_language(path), workspace_root_of(path))
        result = server.request("textDocument/definition",
                                position_params(path, line, col),
                                timeout=self.config.timeout_ms / 1000)
        self.cache.put(key, result)
        return result

    def override_batch(self, low_confidence_edges):
        """Only consulted for edges the Rust engine resolved below 0.90."""
        for edge in low_confidence_edges:
            lsp = self.definition(edge.file, edge.line, edge.column, edge.content_hash)
            if lsp and self.maps_to_known_entity(lsp):
                yield LSPOverride(edge, target=lsp, confidence=1.0)
```

**Per-language server commands** (configurable under `[resolution.lsp.servers]`, §15):

```toml
[resolution.lsp.servers]
python     = "pyright-langserver --stdio"
typescript = "typescript-language-server --stdio"
rust       = "rust-analyzer"
go         = "gopls"
```

**Mitigations against the naive-spawn anti-pattern (CG-ARCH-003-R1 finding 1.3):**

| Concern | Mitigation |
|---------|-----------|
| 1–10 s cold-start per query | Servers spawn **once per language per workspace**; steady-state latency is single-digit ms. |
| Stale results without `didChange` | Ingestion pushes `didOpen`/`didChange` before any query; per-file cache prefix invalidated on every change. |
| Process leak | Idle timeout (600 s) + daemon-shutdown hook. |
| Cost when unused | Pool stays empty until the first LSP-eligible edge; default config keeps it off. |

**Confidence contract.** LSP results carry confidence **1.00** and `resolution_method = Lsp`. They are authoritative — a Rust-resolved edge below `override_threshold` (default 0.90) is *overwritten*, never merged. If the LSP location does not map to a known graph entity, the original Rust-layer confidence is retained and a warning is logged for later reconciliation.

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
extra_known_decorators = [
    { name = "myapp.cache",       effect = "cached_property" },
    { name = "myapp.deprecated",  effect = "warn" },
]

[typescript]
tsconfig = "tsconfig.json"
node_modules = true

[resolution]
min_confidence = 0.3

[resolution.stack_graph]
rules_dir = ""
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

[database]
path = ".harness/semantic.db"
hnsw_ef_construction = 128
hnsw_m = 16
hnsw_ef_search = 64

[ingestion]
batch_chunk_size = 200
embedding_budget_ms = 2000
defer_low_priority_below = 0.6

[memory]
stack_graph_mb = 60
call_graph_mb = 40
resolution_cache_mb = 20
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
deny  = [".git/", ".harness/", "/migrations/", "/*.lock", "/generated/"]
audit_retention_days = 30
audit_max_entries = 10000
audit_summarize_after_days = 7

[query]
max_depth = 5
default_top_k = 10
cache_ttl_seconds = 300
cache_max_size = 256
use_rust_graph_for_traversal = true

[git]
enabled = true
reindex_on_branch_switch = true

[llm]                                             # used by GraphRAG + mutation router
provider = "openai"
model = "gpt-4o"
max_context_tokens = 8192
context_strategy = "structural"                  # signatures_only | structural | full
temperature = 0.1
api_key_env = "OPENAI_API_KEY"

[performance]
worker_threads = 4
debounce_ms = 50
query_check_interval = 64

[output]
snapshot_path = "./.coderadar/snapshot.bin"
journal_path  = "./.coderadar/wal.log"
```

Top-level general options (watch, debounce, file-size limits, excludes) and per-language configuration live in `.harness/config.toml` for parity with the watcher and the `ignore` crate:

```toml
# .harness/config.toml — watcher / project layout
[general]
watch_paths = ["src/", "tests/"]
exclude_patterns = [".generated", ".pb.go", ".g.dart"]
debounce_ms = 500
max_file_size_bytes = 1_048_576                   # files larger than this are skipped
log_level = "info"

# Per-language overrides (see §20.4 for the full add-a-language recipe)
[languages.python]
extensions = [".py", ".pyi"]
parser = "tree-sitter-python"
tags_query = "tags.scm"
import_patterns = ["from {module} import {symbol}",
                   "import {module}",
                   "import {module} as {alias}"]
function_patterns = ["def {name}({params}):", "async def {name}({params}):"]
method_self_param = "self"
lsp_command = "pyright-langserver --stdio"
```

---

## 16. Command-Line Interface

```
coderadar init <path>                  Initial analysis; persists to .harness/
coderadar analyze <path>               One-shot analysis without persistence
coderadar update <file> [--content -]  One-shot update
coderadar watch <path>                 Long-running watcher; JSONL on stdout
coderadar query "<query string>"       Execute Pest query; pretty-print results
coderadar cypher "<cypher>" [--params] Execute Cypher query against LadybugDB
coderadar shell                        REPL with persistent graph in memory
coderadar export <path> [--format f]   Export snapshot (bin, json, yaml)
coderadar load <snapshot>              Load and verify snapshot integrity
coderadar rebuild --full               Full re-index of all files
coderadar stats                        Counts, parse-error summary, memory usage
coderadar warnings [--category c]      List warnings
coderadar resolve <qualified-name>     Show resolution chain (debugging)
coderadar callers <qualified-name>     List callers of a function
coderadar visualize <type> <args>      Run a visualizer
coderadar mutations --last 20          Audit trail from MutationLog
coderadar diagnose --unresolved        Show all unresolved references
coderadar status                       Daemon health check
```

---

## 17. Watch Mode

Uses `notify` (Rust) for cross-platform file events.

**Pipeline:** `fs events → notify → debounce (50ms) → dedupe → parse (rayon) → commit (serial MPSC)`

**Debouncing & coalescing.** Notify events are buffered in a `50 ms` window (configurable `debounce_ms`). Within the window:
1. Multiple writes to the same path collapse to a single update keyed on the latest on-disk hash.
2. Create + modify on a never-seen path collapse to an insert.
3. Modify + delete on a known path collapse to a remove.

**Burst handling.** >10 files within 100 ms are batched into a single transaction with one epoch bump and one `trace_id` (the watchers generates a fresh ULID per debounced batch). Sub-transactions are chunked at 200 files (§9.3) so readers are never blocked >2 s and a mid-batch crash loses at most one chunk — the survivor reconciles from disk hashes on restart.

**External-mutation safety (WriteGuard).** Every mutation path the MutationEngine writes is registered with `WriteGuard` (§11.5); the watcher's `should_drop()` check suppresses the self-write event so it is not double-indexed. The TTL is a safety net: if the synchronous reindex crashes, the guard entry expires and the watcher re-captures the file normally — no event is ever lost.

**Python API:**
```python
# Synchronous iteration
with coderadar.watch("src/") as w:
    for report in w:
        print(report.affected_files)

# Callback
handle = coderadar.watch_async("src/", callback=on_update)
handle.stop()
```

---

## 18. Visualizers

Visualizers live in the Python layer; they consume query results and `reverse-index` reads from a `QuerySnapshot` and emit graph descriptions. Three outputs:

- **Class Hierarchy (Mermaid/Graphviz).** `subclasses[c]` + `Class.mro` walk. Renders the inheritance DAG rooted at a given class (or all roots if none given). MRO errors are rendered with a red `*` annotation so C3-ambiguous diamonds are visible.
- **Module Dependency Graph (Graphviz).** Edges = `importers` reverse index. **Cycle highlighting** via Tarjan strongly-connected-components: modules in a non-trivial SCC share a cluster background; the SCC count and largest SCC size are printed to stdout.
- **Call Graph (Mermaid/Graphviz).** `callees_by_caller` (fan-out) or `callers_by_callee` (fan-in) for a single function up to `max_depth` (default 5). Edges below `min_confidence` (default 0.7) are dashed and labelled with their method+confidence.

**Output adapters.** Each visualizer writes Mermaid source (`.mmd`) or Graphviz DOT (`.dot`), optionally invoking `mmdc`/`dot` to render PNG/SVG when `--output` points at an image path. The pipeline is pure — no mutation, no graph writes. Output via `coderadar visualize <type> <args> [--output <path>] [--format mermaid|dot|svg|png]`.

---

## 19. Error Handling and Fault Tolerance

### 19.1 Error Categories

| Category | Default behavior | `--strict` |
|----------|-----------------|------------|
| Parse error | Mark symbols `Partial`/`Tainted`; continue | Exit 1 |
| Resolution failure | Mark `Unresolved`; continue | Continue (expected) |
| I/O error | Drop file's slice; warn | Exit 1 |
| Grammar mismatch | Refuse to load | Exit 1 |
| Internal invariant | `debug_assert!` panic; log+abort in release | Same |

### 19.2 Tainted Update Policy

When `update_file` rejects a tainted update, the WAL transaction is aborted, the previous file slice is retained, and the returned `UpdateReport` has `fully_applied = false`.

### 19.3 Warnings

Collected on the graph during analysis and updates: `ParseWarning`, `ResolutionWarning`, `DecoratorWarning`, `WildcardImportShadow`. Stored but not printed by default. `--verbose` prints them.

---

## 20. Multi-Language Support

### 20.1 Language Tiers

| Tier | Languages | Resolution | Mutation |
|------|-----------|------------|----------|
| Tier 1 | Python, TypeScript, JavaScript, Rust, Go, Java, C, C++, Ruby, PHP, C#, Kotlin | Stack Graphs → Import → Signature | Full tool suite |
| Tier 2 | Swift, Scala, Lua, Elixir, Erlang, Haskell, OCaml, Zig, Nim, Dart, R, Julia, Perl | Import → Signature | `replace_entity_body`, `create_entity` |
| Tier 3 | Shell, SQL, HTML, CSS, YAML, TOML, JSON, Markdown + 280 more | Signature Match only | `replace_entity_body`, `create_entity` |

### 20.2 Language Tagging

Every entity carries `language: Language`. Queries support filtering: `classes where language == "python"`.

### 20.3 Cross-Language Edges

Cross-language references resolve to `External { distribution: None }` with `cross_language: true` flag. No cross-language type bridging.

### 20.4 Adding a New Language

1. Add `tree-sitter-<lang>` to Cargo.toml
2. Write `queries/<lang>.scm` with standard capture names
3. Implement language-specific walker extensions
4. Write `.tsg` rule file (for Tier 1) or configure import/signature patterns (Tier 2)
5. Define FileType mapping (Impl/Stub extensions)
6. Add fixture files under `tests/fixtures/<lang>/`
7. Add golden resolution tests

---

## 21. Observability & Diagnostics

### 21.1 Structured Logging

All components emit structured JSON via `tracing` (Rust) and `structlog` (Python):

```json
{
  "timestamp": "2026-07-25T14:32:01.123Z",
  "level": "info",
  "component": "ingestion.pipeline",
  "event": "batch_processed",
  "files_count": 3, "entities_created": 12, "entities_modified": 5,
  "embeddings_generated": 8, "embeddings_cached": 9, "edges_created": 23,
  "duration_ms": 342, "trigger": "file_save", "trace_id": "01J9ZK3M..."
}
```

### 21.2 Metrics (.harness/metrics.json)

**Ingestion:** `files_watched`, node/edge totals, `embedding_cache_hit_rate`, `avg_ingestion_latency_ms`, `parse_errors_last_hour`, `unresolved_references_rate`, `ingestion.rollback`.

**Mutation:** `mutations_total{tool,status}`, `mutation_edits_total`, `mutation_rollback_rate`, `mutation_stale_rejection_rate`, `unverified_sites_total`, `repair_attempts_total`, `mutation_log_rows`.

**System:** `memory_rss_mb`, `db_size_mb`, per-component residency vs budget (§22.2), `stack_graph_spill_count`, `call_graph_evictions`.

Metrics are written at a fixed 10 s cadence and on every batch/mutation completion (event-driven counter increments). They are the primary input to the `coderadar diagnose` and `coderadar status` CLI commands.

### 21.3 Health Check & Daemon Endpoint

When running as a daemon (long-lived `watch`), CodeRadar listens on `.harness/codegraph.sock` for `ping` and `stats` requests. The CLI commands `coderadar status` and `coderadar diagnose` reuse this socket when present, falling back to one-shot snapshot reads otherwise. `diagnose --unresolved` enumerates every `Unresolved` reason bucket; `diagnose --low-confidence` lists edges below `min_confidence` (the queue LSP/embedding would consider).

### 21.4 Cross-Boundary Trace Correlation

ULID `trace_id` generated by the watcher, carried through PyO3 into Python's `structlog.bind(trace_id=...)` and back. One ingestion or mutation flow is greppable end-to-end with a single ID.

---

## 22. Performance Targets & Benchmarking

### 22.1 Targets

| Metric | Target | Measurement conditions |
|--------|--------|------------------------|
| Initial analysis (cold) | < 30s | 8-core workstation, 5k files / ~1M LOC |
| Single-file update, file save → graph update | < 500 ms (p95) | After `didChange` debounced |
| Single-file update p50 / p95 / p99 | < 30 / 100 / 250 ms | Re-index only; embeddings cached |
| Idle CPU usage | < 1% | Watcher running, no file changes |
| Batch write throughput | > 200 entities/s | batch_size 32, CPU embedding |
| Steady-state dedup hit rate | > 85% | After initial full index |
| Mutation plan generation (≤100 files, dry run) | < 100 ms | Planning only (no apply) |
| Mutation apply ≤100 files | < 200 ms | Excluding post-verify-reindex |
| Query latency (streaming, simple `where`) | < 5 ms to first result | Snapshot-cold; no `order by` |
| Query latency (aggregated, full scan) | < 100 ms | `group by` + `order by` |
| Memory (steady-state, 5k files) | < 500 MB | All in-memory components |
| Stack-graph spill rate | < 5% of resolutions | At 5k-file scale |

### 22.1a Performance Impact of Rust-Native Resolution

| Operation | Python-side (legacy) | Rust-native | Speedup |
|-----------|----------------------|------------|---------|
| Resolve 100 references | ~45 ms | ~2 ms | ~22× |
| Import traversal (depth 3) | ~12 ms | ~0.3 ms | ~40× |
| Call chain (depth 5) | ~25 ms | ~0.5 ms | ~50× |
| PyO3 crossings per file | 5–8 | 1 | 5–8× fewer |

### 22.2 Memory Budgets

| Component | 10K files | 100K files | Eviction |
|-----------|-----------|------------|----------|
| Stack-graph fragments | 60 MB | 400 MB | LRU; cold spill to `.harness/spill/` (zstd) |
| Import graph | 15 MB | 120 MB | Never evicted (core substrate) |
| Call graph | 40 MB | 300 MB | LRU; edges < 0.60 confidence evicted |
| In-memory arenas (entities) | 60 MB | 400 MB | Never evicted |
| Reverse indexes | 40 MB | 200 MB | Never evicted |
| Resolution cache | 40 MB | 200 MB | LRU |

**Eviction.** Only stack-graph fragments, call-graph edges (below 0.60 confidence), and the resolution cache are evictable. Import graph, in-memory entity arenas, and reverse indexes are never evicted — they are the core substrate. Fragment spills are compressed with `zstd` (§15) and rebuilt on demand. The `max_file_size_bytes` cap (§15 `general`) prevents a single oversized file from blowing the stack-graph budget.

### 22.3 Reference Codebases & Methodology

Reference corpora for regression benchmarks: (a) a ~5k-file Python monorepo instrumented with the `incremental_matches_full` property harness; (b) the CPython stdlib (~4k files, ~1.4M LOC); (c) a TypeScript test corpus (~3k files). Benchmarks run under `cargo bench` (criterion) with the `--bench` profile and deterministic `codebase-snapshot` fixtures stored under `tests/fixtures/`. Every perf-regression CI run uploads a `metrics.json` diff against the previous tagged release; a >1.15× regression on any p95 latency stalls the release. Benchmarks run cold (fresh process, empty caches) and warm (after a full reindex) and report both.

---

## 23. Testing Strategy

### 23.1 Test Pyramid

Unit 70% / Integration 25% / E2E 5%.

### 23.2 Critical Property Test

The gold-standard invariant: an arbitrarily-long edit sequence applied incrementally must converge to the same graph as a single full re-analysis at the end. Drift in either direction (missing entities, stale edges, diverged reverse indexes) fails this test.

```rust
proptest! {
    #[test]
    fn incremental_matches_full(edits in arbitrary_edit_sequence(1..100)) {
        let mut graph_inc = analyze(initial_fixture());
        let mut fs = initial_fixture();
        for edit in &edits {
            apply_edit_to_fs(&mut fs, edit);            // also drops/creates files
            graph_inc.update_file(&edit.path).unwrap();
            // every boundary: no half-applied state observable
            assert_eq!(graph_inc.epoch(), 1 + graph_inc.snapshot().epoch_at_start());
        }
        let graph_full = analyze_in_memory(&fs);
        assert_graphs_equivalent(&graph_inc, &graph_full);  // entities, edges, reverse indexes
    }
}
```

`arbitrary_edit_sequence` produces realistic edits — single-token renames, body rewrites, body-only changes leaving signatures intact (exercising the body-change fast path), file splits, file moves (qualified-name change → remove+insert), and corrupted-syntax saves (exercising the tainted-update rejection path). The harness recomputes graph identity by *qualified_name content* rather than SlotMap keys, so key reuse does not mask drift.

### 23.3 Rust Core Tests

**Semantic engine (golden resolution):** per-language resolution fixtures (`tests/fixtures/{lang}_project/`) with a `≈`-expected resolution manifest. `test_python_import_resolution` proves L1 produces `confidence ≥ 0.90` resolved CALLS edges with `method = StackGraph`. `test_cyclic_call_graph_terminates` feeds `def a(): b()` / `def b(): a()` and asserts `find_callers("a", 10)` returns rather than looping. `test_remove_file_is_o1_and_stable` indexes 1000 files, removes the 500th, asserts `node_count == before-1` and that `f999.py` is still findable (surviving NodeIndex valid). `test_toplevel_sentinel_for_module_level_refs` proves every edge's `source_id` is non-empty (module-level refs attach to `{path}::toplevel`).

**Diff / patch / MRO:** diff-patch round-trip (proptest: apply then reverse must restore), MRO consistency (proptest: C3 output is a true linearization of the bases; failure path sets `mro_error` rather than panicking), snapshot round-trip (`export` → `load` → assert entity/edge counts equal), extraction outputs against fixture files (insta snapshots).

**Fuzzing:** Pest grammar fuzzing (proptest), tree-sitter input fuzzing (cargo-fuzz targets feeding arbitrary bytes to the parser to assert no panic on byte-span slicing).

**Import/Call graph:** Import graph O(1) removal + index stability (above), cycle-safe traversal termination on mutually-recursive graphs.

**Mutation engine:** `body_replacement_preserves_signature_docstring_decorators`, `indent_normalization_rebases_column_zero_paste_to_entity_level`, `indent_normalization_preserves_triple_quoted_string_interiors`, `signature_cascade_rewrites_all_verified_call_sites`, `required_param_without_callsite_value_fails_preflight`, `rename_skips_shadowed_same_name_symbols` (stack-graph scope proof), `stale_expected_hash_rejects_mutation`, `syntax_error_triggers_full_multi_file_rollback`, `crlf_file_mutation_is_byte_accurate`, `multi_byte_identifier_offsets_never_panic`, `descending_offset_application_keeps_edits_stable` (200 edits, one file), `mutation_log_pruning_respects_retention_and_cap`.

### 23.4 Python Tests

- Full ingestion pipeline (LadybugDB): run `process_batch` over a sample project, assert `MATCH (f:File) RETURN count(f)` and that all `CALLS` edges have `0.0 ≤ confidence ≤ 1.0`.
- Embedding deduplication: unchanged bodies between two runs produce zero `embeddings_generated` increments; cache-hit rate recorded.
- Query execution (Pest + Cypher): the §7.2a example queries parse and return non-empty iterators; Cypher templates return parameter-bound results.
- Mutation tool routing (`tool_router.py`): each of the four schemas routes to the correct planner; `dry_run=false` calls `engine.apply` and writes a MutationLog row.
- Policy enforcement: deny-listed paths reject with `PolicyViolation`; `max_files_per_plan` exceeded rejects with the offending count; `require_clean_git=true` rejects on a dirty worktree.
- LSP pool lifecycle (when enabled): `ensure_server` is called at most once per language per workspace; `idle_timeout_s<600` teardown re-spawns lazily; `didChange` invalidates the cache prefix.

---

## 24. Build and Distribution

- **Build tool:** `maturin` (PEP 517)
- **Rust crates:** `pyo3`, `tree-sitter` + language packs, `stack-graphs`, `tree-sitter-graph`, `petgraph`, `ropey`, `similar`, `dashmap`, `lru`, `rayon`, `xxhash-rust`, `git2`, `zstd`, `slotmap`, `pest`, `pest_derive`, `parking_lot`, `arc-swap`, `notify`, `ignore`, `crossbeam-channel`, `tracing`, `serde`, `smol_str`, `ulid`, `postcard`
- **Python deps:** `pydantic>=2.0`, `click`, `rich`, `ladybug`, `fastembed`, `litellm`, `grep-ast`, `structlog`
- **Wheel matrix:** Linux x86_64, Linux aarch64, macOS x86_64, macOS arm64, Windows x86_64. Python 3.9–3.13 (abi3).
- **CI:** `cibuildwheel`; publish to PyPI on tagged releases.

### 24.1 Implementation Validation Gate

Before Python integration, the Rust core must pass:

```bash
cargo check --package core_indexer
cargo clippy --all-targets -- -D warnings
cargo test --package core_indexer
```

---

## 25. Out of Scope

| Out of scope | Alternative |
|---|---|
| Type inference | Consume annotations as strings |
| Runtime behavior | No execution |
| IDE / LSP integration (primary) | LSP is optional fallback only (§14) |
| Semantic refactoring | MutationEngine handles structural rewriting |
| Cross-language type bridges | Explicitly unsupported |
| Build-system parsing | Consume config, not build logic |
| Git history / blame (as primary feature) | Blame is best-effort convenience |
| Code style / linting | Not a linter |
| Security analysis | Not a SAST tool |
| Code metrics beyond counts | `line_count` exposed; users compute their own |
| Plugin API for decorators | Deferred to post-v1 |
| Distributed snapshots | Deferred to post-v1 |

---

## 26. Open Questions — Resolved (v3.3.1)

The five questions carried over from v2/v3.2.1 have been resolved in this review pass. Rationales and decision text are consolidated in [Appendix F](#appendix-f-open-question-decisions); the summary:

1. **Wildcard import multi-hop precision.** **DECISION: yes, follow up to 3 hops** when every intermediate module has a statically-determinable `__all__`. The cap is a structural-cycle guard, not a precision trade-off. (§5.3.1)
2. **Plugin API.** **DECISION: deferred to post-v1, with a concrete design.** A Python entry-point group `coderadar.extractors` will let third parties register `.scm` tag queries and known-decorator handlers (Django, SQLAlchemy) without recompiling the Rust core; decorators contribute `FunctionKind`/`EffectiveClass` effects through a marshalled effect table, not arbitrary Rust callbacks. The design is in Appendix F.2.
3. **Distributed snapshots.** **DECISION: deferred to post-v1.** The §9 single-writer model plus chunked sub-transactions already meets the 5k-file / 1M-LOC target; sharding adds an inter-process commit protocol (2PC) that is not warranted until a single-writer hits its throughput ceiling. Appendix F.3 sketches the v4-shaped protocol for when that day comes.
4. **Stack Graphs vs. hand-rolled resolution.** **DECISION: keep the hand-rolled resolver as an L0-style fallback for ~5% Stack-Graphs edge cases only,** gated by a per-language config flag `use_stack_graphs_fallback` (default on for Python). The hand-rolled path kicks in for decorator-synthesized methods (`@dataclass`), metaclass `__all__` mutation, and dynamic `__all__` patterns that Stack Graphs' `.tsg` cannot express. Resolution from this path is clamped into the **0.90–1.00 band** to keep the confidence contract intact. (§6)
5. **LadybugDB vs. embedded (SQLite + vector).** **DECISION: ship LadybugDB as the primary store; prototype an embedded option post-Phase-1.** LadybugDB preserves the Kùzu dialect and existing DDL/Cypher, and removing it now would discard the GraphRAG vector-search templates. An embedded SQLite-vss adapter is a deployment-simplification, evaluated against the embedded perf targets and only swapped when its capability parity is proven. (§10)

The remaining open question — whether `__all__ += [...]` should be statically analyzed by AST pattern-special-casing (current behavior: non-determinable fallback) — remains intentionally unresolved in v3.3.1; see Appendix F.4.

---

## Appendix A: PyO3 Data Structures (FFI Boundary)

```rust
#[pyclass] #[derive(Clone)]
pub struct ParsedFile {
    #[pyo3(get)] pub path: String,
    #[pyo3(get)] pub language: String,
    #[pyo3(get)] pub content_hash: String,
    #[pyo3(get)] pub functions: Vec<ParsedFunction>,
    #[pyo3(get)] pub methods: Vec<ParsedMethod>,
    #[pyo3(get)] pub classes: Vec<ParsedClass>,
    #[pyo3(get)] pub variables: Vec<ParsedVariable>,
    #[pyo3(get)] pub imports: Vec<ParsedImport>,
    #[pyo3(get)] pub references: Vec<ParsedReference>,
    #[pyo3(get)] pub error_ratio: f32,
    #[pyo3(get)] pub line_count: u32,
    #[pyo3(get)] pub size_bytes: u64,
}

#[pyclass] #[derive(Clone)]
pub struct ParsedFunction {
    #[pyo3(get)] pub name: String,
    #[pyo3(get)] pub qualified_name: String,
    #[pyo3(get)] pub signature: String,
    #[pyo3(get)] pub body: String,
    #[pyo3(get)] pub docstring: Option<String>,
    #[pyo3(get)] pub start_line: u32,
    #[pyo3(get)] pub end_line: u32,
    #[pyo3(get)] pub span: ByteSpan,
    #[pyo3(get)] pub name_span: ByteSpan,
    #[pyo3(get)] pub params_span: ByteSpan,
    #[pyo3(get)] pub body_span: ByteSpan,
    #[pyo3(get)] pub decorators_span: Option<ByteSpan>,
    #[pyo3(get)] pub is_async: bool,
    #[pyo3(get)] pub is_generator: bool,
    #[pyo3(get)] pub is_static: bool,
    #[pyo3(get)] pub visibility: String,
    #[pyo3(get)] pub decorators: Vec<String>,
    #[pyo3(get)] pub parameters: Vec<ParsedParameter>,
    #[pyo3(get)] pub content_hash: String,
}
// ParsedClass, ParsedMethod, ParsedVariable follow the same pattern

#[pyclass] #[derive(Clone)]
pub struct ParsedParameter {
    #[pyo3(get)] pub name: String,
    #[pyo3(get)] pub type_annotation: Option<String>,
    #[pyo3(get)] pub position: u32,
    #[pyo3(get)] pub has_default: bool,
    #[pyo3(get)] pub default_value: Option<String>,
    #[pyo3(get)] pub is_variadic: bool,
    #[pyo3(get)] pub is_keyword_only: bool,
}

#[pyclass] #[derive(Clone)]
pub struct ParsedImport {
    #[pyo3(get)] pub module_path: String,
    #[pyo3(get)] pub symbol_name: Option<String>,
    #[pyo3(get)] pub alias: Option<String>,
    #[pyo3(get)] pub is_wildcard: bool,
    #[pyo3(get)] pub is_relative: bool,
    #[pyo3(get)] pub start_line: u32,
    #[pyo3(get)] pub name_span: ByteSpan,
}

#[pyclass] #[derive(Clone)]
pub struct ParsedReference {
    #[pyo3(get)] pub name: String,
    #[pyo3(get)] pub kind: ReferenceKind,
    #[pyo3(get)] pub line: u32,
    #[pyo3(get)] pub column: u32,
    #[pyo3(get)] pub enclosing_function: Option<String>,
    #[pyo3(get)] pub receiver: Option<String>,
    #[pyo3(get)] pub name_span: ByteSpan,
    #[pyo3(get)] pub args_span: Option<ByteSpan>,
}

#[pyclass] #[derive(Clone, PartialEq)]
pub enum ReferenceKind { Call, Instantiation, Inheritance, TypeAnnotation, AttributeAccess, Import }

// Watcher types
#[pyclass] #[derive(Clone)]
pub struct FileChange {
    #[pyo3(get)] pub path: String,
    #[pyo3(get)] pub kind: FileChangeKind,  // Create | Modify | Delete
    #[pyo3(get)] pub content_hash: Option<String>, // None for Delete
    #[pyo3(get)] pub trace_id: String,      // ULID shared with the parent BatchEvent
}
#[pyclass] #[derive(Clone, PartialEq)]
pub enum FileChangeKind { Create, Modify, Delete }

#[pyclass]
pub struct BatchEvent {
    #[pyo3(get)] pub trace_id: String,        // ULID from watcher
    #[pyo3(get)] pub changes: Vec<FileChange>,
    #[pyo3(get)] pub trigger: String,        // "file_save" | "branch_switch" | "bulk_rebuild"
    #[pyo3(get)] pub timestamp: u64,
}

// Staged-change types (opaque to Python; only metadata fields are #[pyo3(get)])
#[pyclass]
pub struct StagedChange {
    #[pyo3(get)] pub path: String,
    #[pyo3(get)] pub entities: Vec<ParsedEntity>,
    #[pyo3(get)] pub edges: Vec<ResolvedEdge>,
    #[pyo3(get)] pub unresolved: Vec<ParsedReference>,
    #[pyo3(get)] pub language: String,
}

#[pyclass] #[derive(Clone)]
pub struct ParsedEntity {
    #[pyo3(get)] pub kind: ParsedEntityKind, // Function | Class | Method | Variable | Import
    #[pyo3(get)] pub id: String,            // stable dotted id ("src/auth.py::validate_user")
    #[pyo3(get)] pub name: String,
    #[pyo3(get)] pub content_hash: String,  // xxHash of body bytes — dedup + expected_hash
    #[pyo3(get)] pub body: String,
}
#[pyclass] #[derive(Clone, PartialEq)]
pub enum ParsedEntityKind { Function, Class, Method, Variable, Import }

#[pyclass] #[derive(Clone)]
pub struct ResolvedEdge {
    #[pyo3(get)] pub source_id: String,
    #[pyo3(get)] pub target_id: String,
    #[pyo3(get)] pub confidence: f32,
    #[pyo3(get)] pub method: String,        // "StackGraph" | "ImportConstrained" | ...
    #[pyo3(get)] pub edge_type: String,     // "CALLS" | "EXTENDS" | "IMPORTS" | ...
    #[pyo3(get)] pub call_site_span: ByteSpan,
    #[pyo3(get)] pub args_span: Option<ByteSpan>,
}

// Mutation types
#[pyclass] #[derive(Clone)]
pub struct MutationEdit {
    #[pyo3(get)] pub file: String,
    #[pyo3(get)] pub span: ByteSpan,
    #[pyo3(get)] pub replacement: String,
    #[pyo3(get)] pub expected_hash: String,
}

#[pyclass]
pub struct MutationPlan {
    #[pyo3(get)] pub id: String,
    #[pyo3(get)] pub tool: String,
    #[pyo3(get)] pub edits: Vec<MutationEdit>,
    #[pyo3(get)] pub affected_files: Vec<String>,
    #[pyo3(get)] pub diff_preview: String,
    #[pyo3(get)] pub unverified_sites: Vec<UnverifiedSite>,
    #[pyo3(get)] pub warnings: Vec<String>,
}

#[pyclass] #[derive(Clone)]
pub struct UnverifiedSite {
    #[pyo3(get)] pub file: String,
    #[pyo3(get)] pub line: u32,
    #[pyo3(get)] pub snippet: String,
    #[pyo3(get)] pub reason: String,
}

#[pyclass]
pub struct MutationResult {
    #[pyo3(get)] pub status: MutationStatus,
    #[pyo3(get)] pub files_written: Vec<String>,
    #[pyo3(get)] pub syntax_errors: Vec<SyntaxDiagnostic>,
    #[pyo3(get)] pub reindex: ReindexSummary,
    #[pyo3(get)] pub backup_path: Option<String>,
}

#[pyclass] #[derive(Clone)]
pub struct SyntaxDiagnostic {
    #[pyo3(get)] pub file: String,
    #[pyo3(get)] pub line: u32,
    #[pyo3(get)] pub column: u32,
    #[pyo3(get)] pub message: String,
    #[pyo3(get)] pub offending_span: ByteSpan,
}
```

---

## Appendix B: LadybugDB Vector Search API

```cypher
CALL create_hnsw_index('index_name', 'NodeTable', 'embedding_property',
    { dimension: 896, metric: 'cosine', ef_construction: 128, m: 16 });

-- Pre-filtered search
CALL db_similarity_search('index_name', $query_vector, $top_k,
    {filter: {property: $value}})
YIELD node, score;

-- Two-stage: 64-d Matryoshka pre-filter, then 896-d refinement
CALL db_similarity_search('func_embedding_short_idx', $query_short, 50)
YIELD node AS candidate
WITH collect(candidate.id) AS candidates
CALL db_similarity_search('func_embedding_idx', $query_full, 10,
    {filter: {id: candidates}})
YIELD node, score
RETURN node.name, node.body, score;
```

---

## Appendix C: Review Response Register

All findings from the CodeGraph Engine v3.1 review and the v3.2.1 amendment review are resolved in this consolidated document.

### From CG-ARCH-003-R1 (18 findings):

| # | Finding | Resolution |
|---|---------|------------|
| 1.1 | Rust listings not compilable | Labeled design pseudocode; cargo check gate (§24.1) |
| 1.2 | TSG rule files invalid | Production rules from reference stack-graphs crates |
| 1.3 | LSP on-demand spawning anti-pattern | Persistent warm pool (§14) |
| 1.4 | ImportGraph::remove_file O(N) | StableDiGraph + bidirectional maps, O(1) |
| 1.5 | Traversals lack cycle detection | Explicit visited set + depth cap on all traversals |
| 2.1 | LadybugDB vs KùzuDB ambiguity | Community successor, Kùzu dialect |
| 2.2 | Broken Cypher templates | Rewritten; predicates in WHERE |
| 2.3 | Empty source_id dangling edges | `::toplevel` sentinel |
| 2.4 | No rollback on embedding failure | Staged two-phase commit (§6.7) |
| 2.5 | Backpressure missing on embedding pool | Per-batch budget + splitting |
| 3.1 | Overlapping confidence ranges | Disjoint bands (§6.1) |
| 3.2 | "cache-bypass via xxHash" misnomer | Renamed to content-addressed embedding dedup |
| 3.3 | Git blame stubbed | Real git2 implementation (§12.3) |
| 3.4 | Python test syntax error | Fixed |
| 4.1 | In-memory graph memory unbounded | Per-component budgets, LRU, disk spill |
| 4.2 | Single-writer throughput unquantified | 1,000 files < 30 s; chunked sub-transactions |
| 4.3 | No schema versioning/migration | schema_version + migration policy |
| 4.4 | No cross-boundary trace correlation | trace_id (ULID) across PyO3 |

### From v3.2.1 Amendment:

| Finding | Resolution |
|---------|------------|
| Patch 3.1: MutationLog retention | Tiered retention adopted (§11.8) |
| Patch 3.2: Indent normalization | Adopted (§11.4) |
| Document truncation (§15.4–§16, Appendix A) | Repaired; content integrated |

### Concepts merged from CodeRadar v2:

| Concept | Section |
|---------|---------|
| ArcSwap snapshot isolation | §9.1 |
| Tiered diff algorithm | §5.2 |
| Resolution cache + invalidation | §5.4 |
| Pest query grammar + derived fields | §7.1, Appendix D |
| Decorator semantics (known-decorator table) | §4.3 |
| Stub/Impl merging with provenance | §5.3.2 |
| ParseQuality classification + tainted rejection | §4.5, §19.2 |
| WAL with TxAck for crash recovery | §5.5 |
| MRO computation (C3 linearization) | §5.3.4 |
| Visualizers (Mermaid/Graphviz) | §18 |
| Property testing (incremental_matches_full) | §23.2 |
| TYPE_CHECKING handling | §3.2, §5.3.1 |
| Derived field reference (Appendix D) | Appendix D |

---

## Appendix D: Derived Field Reference

### D.1 Functions Entity

| Derived Field | Return | Cost | Cached | Source |
|---------------|--------|------|--------|--------|
| `caller_count` | i64 | O(1) | No | `callers_by_callee[function_id].len()` |
| `line_count` | i64 | O(1) | No | Extracted at parse time |
| `module.name` | String | O(1) | No | `Function.parent_module → Module.name` |
| `is_async` | bool | O(1) | No | Extracted metadata |
| `has_method(name)` | bool | O(k) | No (per-query memo) | Linear scan of `Class.methods` |
| `has_setter` | bool | O(1) | No | Decorator analysis |
| `decorators` | List\<String\> | O(1) | No | Extracted metadata |
| `overrides_of(target)` | bool | O(m) | Partial (MRO cached) | MRO walk |
| `parse_quality` | Enum | O(f) | Yes (on Module) | Aggregate across files |
| `unresolved_reason` | Enum | O(1) | No | Resolution results |

### D.2 Classes Entity

| Derived Field | Return | Cost | Cached | Source |
|---------------|--------|------|--------|--------|
| `inherits_from(name)` | bool | O(m) | Partial (MRO cached) | MRO linear scan |
| `mro_names` | List\<String\> | O(n) first, O(1) after | Yes (on Class.mro) | C3 linearization |
| `method_count` | i64 | O(1) | No | `Class.methods.len()` |
| `subclasses` | List\<Class\> | O(d) | No | `subclasses[class_id]` reverse index |
| `decorators` | List\<String\> | O(1) | No | Extracted metadata |
| `parse_quality` | Enum | O(f) | Yes (on Module) | Aggregate across files |

### D.3 Modules Entity

| Derived Field | Return | Cost | Cached | Source |
|---------------|--------|------|--------|--------|
| `name` | String | O(1) | No | Native field |
| `parse_quality` | Enum | O(f) | Yes | Aggregate across files |

### D.4 Imports Entity

| Derived Field | Return | Cost | Cached | Source |
|---------------|--------|------|--------|--------|
| `kind` | Enum | O(1) | No | Extracted metadata |

### D.5 Calls Entity

| Derived Field | Return | Cost | Cached | Source |
|---------------|--------|------|--------|--------|
| `unresolved_reason` | Enum | O(1) | No | Resolution results |

### D.6 Caching and Invalidation

| Change Event | Derived Field Impact | Invalidation Action |
|-------------|---------------------|---------------------|
| New caller for F | `caller_count` for F +1 | No invalidation (live index) |
| Function removed | All derived fields referencing it | Entity no longer exists |
| Class bases change | `inherits_from`, `mro_names`, `subclasses` for C + transitive subclasses | Clear cached MROs (bounded depth 50) |
| File parse quality changes | `parse_quality` for affected modules | Recompute module-level aggregate |

**Interaction with snapshot isolation:** Derived fields computed on-demand from reverse indexes (part of the snapshot). No separate cache can diverge from snapshot state.

---

## Appendix E: Type Glossary & Module Layout

### E.1 Crate/Package Layout

`py_agent/src/` (Python orchestrator), `core_indexer/src/` (Rust core). The Python import surface is a single `coderadar` package re-exporting from `py_agent/`. Every Rust type crossing the boundary is in Appendix A.

```
core_indexer/src/
  types.rs                 # §3 entities + ByteSpan + enums
  extract/
    tagger.rs               # Pass 1 .scm queries (§4.2)
    walker.rs               # Pass 2 hierarchy walker (§4.2)
    decorators.rs           # known-decorator table (§4.3)
    spans.rs                # byte-span extraction + slice_span helper
  update/
    diff.rs                 # §5.2 tiered diff
    patch.rs                # §5.1 apply flow
    wal.rs                  # PatchTransaction + TxBegin/TxAck (§5.5)
  resolve/
    stack_graph.rs          # Layer 1 + LRU spill (§6.2)
    import_graph.rs         # Layer 2 (§6.3)
    signature.rs            # Layer 3 (§6.4)
    cache.rs                # ResolutionCache (§5.4)
    orchestrator.rs         # cascade + ::toplevel sentinel (§6.6)
  query/
    grammar.rs              # Pest (separate grammar.pest)
    exec.rs                 # streaming + aggregated (§7.1)
  mutation/
    mod.rs                  # MutationEngine (§11.2)
    edit.rs                 # rope apply (§11.3)
    indent.rs               # §11.4 normalize
    write_guard.rs          # §11.5
  fs/
    watcher.rs              # notify + debounce + WriteGuard check (§17)
    git.rs                  # §12
  graph.rs                  # CodeGraph (§3.4), arc-swap arenas, reverse indexes

py_agent/src/
  pipeline.py              # ingestionorchestration: stage -> commit (§6.7)
  embedding/dedup.py       # §13.1 content-addressed dedup
  agent/graphrag.py        # §13.3 query planner + context builder
  lsp/pool.py              # §14 LSPPool
  mutation/tool_router.py  # §11.2 tool schema routing
  query/{planner,templates,executor,cache}.py
  config.py                 # Pydantic models for .coderadar.toml + .harness/config.toml
```

### E.2 Type Glossary (cross-reference)

| Type | Defined in | Consumed by |
|------|-----------|-------------|
| `Module`/`Class`/`Function`/`Import` | §3.2 | walker, diff, query |
| `ByteSpan` | §3.3 | mutation spans |
| `ExtractedUnit`/`TaggedTree`/`TagInfo`/`FrameKind` | §3.3a | walker, diff |
| `ImportGraph`/`CallGraph` | §3.4a | Layers 1-2, traversal |
| `QuerySnapshot` | §3.4a | query exec |
| `ResolvedEdge`/`ResolutionMethod` | §3.4a | semantic engine, FFI |
| `GraphConfig` | §3.4a | graph build |
| `ResolutionCache`/`Resolution` | §5.4 | orchestrator |
| `PatchTransaction` + WAL helper types | §5.5 | update apply |
| `MutationEdit`/`MutationPlan`/`MutationResult`/`UnverifiedSite`/`SyntaxDiagnostic` | Appendix A | mutation |
| `MutationStatus`/`ReindexSummary` | §11.8a | mutation result |
| `FileChange`/`BatchEvent`/`ParsedEntity` | Appendix A | watcher, FFI |
| `UpdateReport`/`SymbolChange` | §8.2 | Python API |

### E.3 Confidence-Band Reference

| Band | Layer | Method | When |
|------|------|--------|------|
| 0.90–1.00 | L1 | StackGraph (or L0 hand-rolled fallback, §26 Q4) | vendored `.tsg` rules exist |
| 0.80–0.89 | L2 | ImportConstrained | Stack Graphs returned None |
| 0.40–0.79 | L3 | SignatureMatch | L1+L2 returned None |
| 0.20–0.39 | L4 | Embedding (Python) | cosine ≥ 0.85; the LSP override candidate pool |
| 1.00 | L5 | Lsp (Python) | LSP maps to a known entity; overrides L1-L4 |
| unset | — | Unresolved | no layer matched; reason recorded |

---

## Appendix F: Open-Question Decisions

### F.1  Wildcard import multi-hop (Q1)

**Decision.** Follow `from x import *` chains up to **3 hops** when every intermediate module's `__all__` is statically determinable (a top-level string-list literal). The hop cap is a structural-cycle guard against cyclic `__all__`-gated re-exports; it does not trade precision for capped cases. Beyond 3 hops the resolution keeps the first-hop module and sets `ImportResolution::Wildcard { module, exposed: <first hop __all__> }` with a `WildcardImportShadow` warning at the importer.

**Implementation note.** This adds a `max_wildcard_hops: u8 = 3` field to `ImportGraphConfig`; the BFS in `ImportGraph::resolve_in_imports` enforces it independently of `max_import_depth`.

### F.2  Plugin API recipe (Q2)

A Python entry-point group `coderadar.extractors` will be discovered at startup by the Python orchestrator and a marshalled effect table forwarded to the Rust core. Each plugin registers:

```toml
# inside an installed distribution's pyproject.toml entry_points
[project.entry-points.coderadar_extractors]
my_framework = "myapp.coderadar_plugin:register"
```

```python
# myapp/coderadar_plugin.py
def register():
    return ExtractorPlugin(
        name="django",
        decorators={
            "@property":                       Effect.property(),
            "@functools.cached_property":      Effect.cached_property(),
            "@django.views.decorators.csrf.csrf_exempt": Effect.class_effect("DjangoView"),
        },
        tail_tags="queries/django.scm",       # additional .scm captures only
        # Plugins cannot extend language tags.scm extraction itself (that lives in Rust);
        # they augment the known-decorator table and emit extra derived-field helpers.
        derived_fields={
            "is_django_view": lambda ctx: "DjangoView" in ctx.decorators,
        },
    )
```

The `decorators` map is marshalled at startup into the Rust `known_decorator` table; `derived_fields` are registered in the Python query-extension hook so `functions where is_django_view == true` resolves. **Constraints:** plugins cannot run arbitrary Rust callbacks (they contribute effects through the table, not free functions), cannot add new languages, and cannot mutate the graph. They are confined to extraction augmentation and query extension. This confinement scoping is why the mechanism remains post-v1 — its API surface needs a stability commitment we won't make blindly.

### F.3  Distributed-snapshot protocol sketch (Q3)

A single-writer-per-shard model with a two-phase commit across shards. Each shard owns a disjoint subtree of the import graph (rooted at a top-level package). Cross-shard `IMPORTS` edges are committed by a lightweight 2PC: shard A (importer) prepares, shard B (imported) acknowledges the target exists, then both commit. Read fan-out across shards is handled by a query coordinator that issues snapshot reads against each shard and merges. This is sketched, not specified, because single-writer throughput has not yet approached a shard split threshold (§9.3: 5k files < 30 s). The protocol is revisited when a single-writer exceeds 2 s p95 on its chunked sub-transaction budget.

### F.4  `__all__` static-analysis extension

The one open question intentionally left for v3.4: whether `__all__ += ["x"]` and `__all__.extend([...])` should be AST-pattern-special-cased into "append to known list" rather than unconditionally falling back to non-determinable. Current behavior loses precision when `__all__` is built incrementally. The attractively-cheap fix is to recognize a small set of mutation patterns (`+=`, `.extend`, `.append` with a literal-string arg) against a uniquely-assigned `__all__` name at module top-level only. The risk is that partial static analysis invites users to expect full static analysis — a slippery slope toward the §1.3 non-goal of build-system parsing. v3.4 will decide this against evidence from real-world fixtures.

---

*End of consolidated specification — CodeRadar v3.3 (v3.3.1 review pass: all referenced content inlined, undefined types defined, open questions resolved)*
