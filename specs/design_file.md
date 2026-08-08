# CodeRadar — Specification, v2

> **Status:** Draft for implementation.
> **Scope of this revision:** Integrates the review of v1: corrects bugs in the extraction walker and tree-sitter queries, replaces the ambiguous WAL/COW design, specifies cross-file resolution semantics, rewrites the query grammar with proper precedence, commits to a snapshot isolation model, and fills in the previously-absent CLI, watch mode, configuration, persistence, testing, and out-of-scope sections.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Architecture](#2-architecture)
3. [Data Models](#3-data-models)
4. [Tree-Sitter Extraction Layer](#4-tree-sitter-extraction-layer)
5. [Incremental Update Algorithm](#5-incremental-update-algorithm)
6. [Query Engine](#6-query-engine)
7. [Python API](#7-python-api)
8. [Concurrency, Locking, and Snapshot Isolation](#8-concurrency-locking-and-snapshot-isolation)
9. [Error Handling and Fault Tolerance](#9-error-handling-and-fault-tolerance)
10. [Command-Line Interface](#10-command-line-interface)
11. [Configuration](#11-configuration)
12. [Watch Mode](#12-watch-mode)
13. [Persistence and Snapshots](#13-persistence-and-snapshots)
14. [Visualizers](#14-visualizers)
15. [Build and Distribution](#15-build-and-distribution)
16. [Performance Targets and Benchmarking](#16-performance-targets-and-benchmarking)
17. [Testing Strategy](#17-testing-strategy)
18. [Multi-Language Considerations](#18-multi-language-considerations)
19. [Out of Scope](#19-out-of-scope)
20. [Open Questions](#20-open-questions)

---

## 1. Overview

CodeRadar is a hybrid Python/Rust tool that maintains a live, incrementally updatable map of a source codebase's logical structure. It targets continuous refactoring scenarios — most notably, an LLM editing files one at a time — where the map must stay consistent after each small change without re-analyzing the entire codebase.

### 1.1 Design Pillars

- **Rust core.** Mutable graph storage, tree-sitter parsing, differential updates, query execution, cross-file resolution.
- **Python shell.** Thin wrapper (PyO3) providing CLI, visualizers, snapshot serialization, and a high-level API. The live graph lives in Rust; Python queries it.
- **Incremental by design.** After a file change, only affected symbols and their dependents are recomputed.
- **Name-based, not type-based.** CodeRadar performs static, lexical, name-based resolution. It does not infer types, evaluate metaclasses, or execute code. Symbols requiring type information to resolve are explicitly marked as unresolved (see [§5.3](#53-cross-file-resolution)).
- **Resilient to broken code.** Parse failures produce tainted symbols rather than aborts. The graph never enters an inconsistent state, even mid-update.

### 1.2 Phased Language Support

| Phase | Languages | Notes |
|-------|-----------|-------|
| 1     | Python    | First-class support; resolution semantics fully implemented |
| 2     | TypeScript / JavaScript | Class/function/import extraction; resolution best-effort |
| 3     | Go, Rust  | Package- and module-aware |
| 4     | (Cross-cutting) Rename detection via similarity hashing |

### 1.3 Non-Goals

- Type inference, type checking, or any form of abstract interpretation.
- Runtime behavior analysis (no execution).
- IDE / LSP integration (out of scope; the API is sufficient for an external LSP to be built on top).
- Semantic equivalence checking of refactored code.
- Build-system integration (no understanding of `setup.py`, `pyproject.toml` build configs, `tsconfig`, etc., beyond reading them for source-root configuration).

See [§19](#19-out-of-scope) for the complete out-of-scope list.

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                       Python Layer (thin)                        │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────────────┐    │
│  │ CLI      │  │ Visual-  │  │ Snapshot │  │ High-level API │    │
│  │ (click)  │  │ izers    │  │ Export   │  │ (wraps Rust)   │    │
│  │          │  │ (mermaid,│  │ (JSON/   │  │                │    │
│  │          │  │ graphviz)│  │  bin)    │  └───────┬────────┘    │
│  └──────────┘  └──────────┘  └──────────┘          │             │
│                                                    │             │
│                                          PyO3      │             │
│                                    (calls,         │             │
│                                     iterators,     │             │
│                                     snapshots)     │             │
└────────────────────────────────────────────────────┼─────────────┘
                                                     │
┌────────────────────────────────────────────────────┼─────────────┐
│                       Rust Core                    │             │
│  ┌─────────────────────────────────────────────────┴───────────┐ │
│  │ • CodeGraph (mutable, indexed, MVCC-ish epochs)             │ │
│  │ • Tree-sitter parsing + two-pass extraction                 │ │
│  │ • Incremental update engine (diff + patch + WAL)            │ │
│  │ • Reverse index maintenance                                 │ │
│  │ • Cross-file resolution (imports, calls, MRO)               │ │
│  │ • Resolution cache with invalidation                        │ │
│  │ • Query engine (pest + evaluation on graph)                 │ │
│  │ • File watcher (notify, debounced)                          │ │
│  └─────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
```

### 2.1 Layering Rules

1. The Python layer never holds Rust references across `await` boundaries (none are async anyway) or across GIL releases.
2. Every `#[pyfunction]` and `#[pymethod]` releases the GIL during long Rust operations (`py.allow_threads`).
3. Rust types crossing the FFI boundary are either `Copy` IDs, owned strings/bytes, or `PyObject`s built inside the Rust call.
4. The query engine and update engine share read access via the snapshot isolation mechanism in [§8](#8-concurrency-locking-and-snapshot-isolation); they never hold each other's locks.

---

## 3. Data Models

### 3.0 Identity Model

This section was missing from v1 and is the source of several downstream design decisions.

**Stable identity is the `SlotMap` key, not the qualified name.**

- `ModuleId`, `ClassId`, `FunctionId`, `ImportId` are stable across the lifetime of the entity. They persist through "modify" operations: a function whose body changes keeps its `FunctionId`.
- A key is invalidated only when the entity is removed (e.g., the symbol disappears from the source file). After removal, the slot may be reused; SlotMap's generational keys prevent ABA confusion.
- Qualified names are *labels*. They are used for diff matching and human display. They are not unique — overloads via `@typing.overload`, conditional definitions, and (in tests) fixture re-definitions can produce multiple entities with the same qualified name in the same scope.
- The Python wrappers expose IDs as opaque integers (the bit-packed SlotMap key). Code that stores these IDs across updates must be prepared for `None` lookups (entity may have been removed).

**Diff matching for incremental updates uses a tiered key**, not the qualified name alone — see [§5.2](#52-the-diff-algorithm).

### 3.1 Unique Identifiers (SlotMap)

```rust
use slotmap::{SlotMap, new_key_type};

new_key_type! { pub struct ModuleId; }
new_key_type! { pub struct ClassId; }
new_key_type! { pub struct FunctionId; }
new_key_type! { pub struct ImportId; }
new_key_type! { pub struct ConstantId; }
new_key_type! { pub struct TypeAliasId; }

/// Convenience union for "any symbol" references in edges and queries.
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
    pub path: PathBuf,                // absolute path on disk
    pub language: Language,
    pub package: Option<ModuleId>,    // parent package's __init__ module
    pub exports: Vec<Export>,         // full explicit-import set (union with provenance)
    pub star_exports: Option<Vec<String>>,  // subset visible via `from x import *` (impl precedence)
    pub constants: Vec<ConstantId>,   // module-level constants defined in this module
    pub type_aliases: Vec<TypeAliasId>, // module-level type aliases defined in this module
    pub parse_quality: ParseQuality,
    pub file_version: u64,            // monotonic per file; bumps on every update
}

pub struct Class {
    pub name: String,
    pub bases: Vec<UnresolvedRef>,    // unresolved base class names
    pub resolved_bases: Vec<ClassId>, // populated by resolver; opaque externs become MroNode::External
    pub mro: Vec<MroNode>,            // C3 linearization, computed lazily
    pub methods: Vec<FunctionId>,
    pub fields: Vec<Field>,
    pub source: SourceType, // Impl (from .py) | Stub (from .pyi); default Impl for pre-stub entities
    pub decorators: Vec<String>,      // raw decorator expressions
    pub effective: EffectiveClass,    // see §4.5
    pub is_type_checking_only: bool,  // true if extracted from inside `if TYPE_CHECKING:` block (§20.3)
    pub line: usize,
    pub docstring: Option<String>,
    pub parse_quality: ParseQuality,
}

pub struct Function {
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub calls: Vec<UnresolvedRef>,    // every name-based call site, pre-resolution
    pub resolved_calls: Vec<ResolvedCall>, // see §5.3
    pub decorators: Vec<String>,
    pub line: usize,
    pub docstring: Option<String>,
    pub kind: FunctionKind,           // Free | Method | StaticMethod | ClassMethod | Property | ...
    pub is_async: bool,
    pub is_generator: bool,
    pub source: SourceType, // Impl (from .py) | Stub (from .pyi); default Impl for pre-stub entities
    pub signature_hash: u64,          // FNV-1a hash of (params, return, decorators, kind, async)
    pub body_hash: u64,               // hash of the body text; used to skip caller-rebuild on body-only changes
    pub is_type_checking_only: bool,  // true if extracted from inside `if TYPE_CHECKING:` block (§20.3)
    pub parse_quality: ParseQuality,
}

pub struct Import {
    pub raw: String,                  // original source text of the import statement
    pub kind: ImportKind,
    pub resolution: ImportResolution, // see §5.3
    pub line: usize,
}
```

### 3.3 Supporting Types

```rust
pub struct Parameter {
    pub name: String,
    pub annotation: Option<String>,
    pub default_value: Option<String>,
    pub is_varargs: bool,             // *args
    pub is_kwargs: bool,              // **kwargs
    pub is_positional_only: bool,     // before /
    pub is_keyword_only: bool,        // after *
}

/// Module-level or class-level field/constant.
/// For stub files, annotations may be present in the stub but NOT backfilled from stubs
/// (see Annotation Provenance Rule below).
pub struct Field {
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType, // Impl | Stub; default Impl for pre-stub entities
    pub default_value: Option<String>,
    pub is_class_var: bool,
    // NOTE: `is_type_checking_only` is NOT added to Field in v1.
    // TYPE_CHECKING blocks inside class bodies (e.g., `if TYPE_CHECKING: x: int`) are
    // extracted without a type-checking flag. This is an accepted limitation — such patterns
    // are rare, and the Class/Function-level flag covers the common cases (§20.3).
}

/// Module-level constant or variable declaration.
/// Extracted from both implementation and stub files.
pub struct Constant {
    pub name: String,
    pub annotation: Option<String>,  // e.g., "int", "str" — NOT backfilled from stubs (see Annotation Provenance Rule)
    pub source: SourceType,          // Impl | Stub; default Impl for pre-stub entities
    pub default_value: Option<String>, // e.g., "30.0" if present in source
}

/// Module-level type alias declaration.
/// Extracted from both implementation and stub files.
pub struct TypeAlias {
    pub name: String,
    pub target: String,              // the aliased type as a string, e.g., "dict[str, int]"
    pub source: SourceType,          // Impl | Stub; default Impl for pre-stub entities
}

/// Represents a single export entry in a module's `__all__` or implicit public namespace.
/// Carries provenance for both the origin of the symbol (Local vs. ReExport) and
/// which source file contributed this export entry (Impl vs. Stub).
pub struct Export {
    pub name: String,
    pub source: ExportSource,         // Local | ReExport { from: ModuleId, original_name: String }
    pub file_type: FileType,          // Impl | Stub — provenance for import resolution
}

/// How a symbol enters this module's namespace.
pub enum ExportSource {
    Local,
    ReExport { from: ModuleId, original_name: String },
}

pub enum ImportKind {
    ModuleImport { module: String, alias: Option<String> },
    FromImport   { module: String, names: Vec<(String, Option<String>)> }, // (name, alias)
    RelativeImport { level: usize, module: Option<String>, names: Vec<(String, Option<String>)> },
    StarImport   { module: String },
    Side         { module: String }, // `import "y";` in TS
}

pub enum ImportResolution {
    Unresolved,
    Module(ModuleId),
    Symbol(SymbolId),
    Wildcard { module: ModuleId, exposed: Vec<String> }, // resolved if __all__ is known
    Dynamic,                                              // `importlib.import_module(...)`
    External { distribution: Option<String> },            // third-party
}

pub struct UnresolvedRef {
    pub name: String,                 // identifier as written
    pub path: Vec<String>,            // for `a.b.c`: ["a","b","c"]
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
    SelfRef,                          // self.foo() inside a method
    ClassRef(ClassId),                // Foo.bar() where Foo is a known class
    ModuleRef(ModuleId),              // mod.foo()
    LocalVar,                         // x.foo() where x is a local — TYPE-INFERENCE-NEEDED
    Unknown,
}

pub enum UnresolvedReason {
    NameNotInScope,
    TypeInferenceRequired,            // x.foo() with x being a local
    DynamicImport,                    // came from importlib
    WildcardImportShadow,             // multiple wildcard imports; ambiguous
    ParseError,                       // call site was in tainted code
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
    DataclassSynthesized { from_class: ClassId }, // __init__, __repr__, __eq__ created by @dataclass
}

pub enum EffectiveClass {
    Plain,
    Dataclass { frozen: bool, eq: bool, order: bool },
    NamedTuple,
    TypedDict { total: bool },
    Protocol,
    Enum { variant: EnumVariant },    // Enum, IntEnum, StrEnum, Flag, IntFlag
    Abstract,
}

/// **Annotation Provenance Rule.** Type annotations are extracted only from implementation files (.py, .ts, etc.).
/// Stub files (.pyi, .d.ts) do not contribute annotation data to the merged entity model.
/// This is intentional: stub annotations may be outdated relative to runtime behavior,
/// and CodeRadar prioritizes correctness over completeness for type metadata.
/// Users who need stub-provided type information should query `source == "Stub"` members directly.
pub enum ParseQuality {
    Clean,
    Partial,                          // some ERROR nodes; symbol still extracted
    Tainted,                          // symbol may be wrong; downstream consumers should warn
}

pub enum FileType { Impl, Stub }

/// Tracks whether an entity was extracted from an implementation file (.py, .ts) or a stub/declaration file (.pyi, .d.ts).
pub enum SourceType { Impl, Stub }

pub enum Language { Python, TypeScript, JavaScript, Go, Rust }

pub enum MroNode {
    Class(ClassId),
    External { name: String },        // unknown base; kept as opaque marker so MRO chains don't break
}
```

### 3.4 Graph Container with Reverse Indexes

```rust
pub struct CodeGraph {
    // -------- Primary storage (one arena per kind) --------
    modules:      SlotMap<ModuleId,   ModuleEntry>,
    classes:      SlotMap<ClassId,    ClassEntry>,
    functions:    SlotMap<FunctionId, FunctionEntry>,
    imports:      SlotMap<ImportId,   ImportEntry>,
    constants:    SlotMap<ConstantId, ConstantEntry>,       // module-level constants
    type_aliases: SlotMap<TypeAliasId, TypeAliasEntry>,     // module-level type aliases

    // -------- File-level structure --------
    file_to_modules: HashMap<PathBuf, Vec<ModuleId>>,
    /// Composite key: (language of the module, dotted import path).
    /// Language qualification prevents cross-language name collisions (§18.1).
    /// In Phase 1 this is always `(Language::Python, name)`.
    module_by_dotted_name: HashMap<(Language, String), ModuleId>,

    // -------- Reverse indexes --------
    importers:       HashMap<ModuleId, BTreeSet<ModuleId>>,   // who imports this module
    callers_by_callee: HashMap<FunctionId, BTreeSet<FunctionId>>, // primary direction
    callees_by_caller: HashMap<FunctionId, BTreeSet<FunctionId>>, // for symmetry; rebuilt from Function.resolved_calls
    subclasses:      HashMap<ClassId, BTreeSet<ClassId>>,
    overridden_by:   HashMap<FunctionId, BTreeSet<FunctionId>>, // key is base method; values are overrides

    // -------- Resolution cache (see §5.4) --------
    resolution_cache: ResolutionCache,

    // -------- Concurrency / versioning --------
    epoch: AtomicU64,                                          // bumps on every commit
    config: GraphConfig,
}

/// Each entry is `Arc<...>` so readers holding old snapshots don't block writers.
pub struct ModuleEntry    { inner: Arc<Module> }
pub struct ClassEntry     { inner: Arc<Class> }
pub struct FunctionEntry  { inner: Arc<Function> }
pub struct ImportEntry    { inner: Arc<Import> }
pub struct ConstantEntry  { inner: Arc<Constant> }
pub struct TypeAliasEntry { inner: Arc<TypeAlias> }
```

**Why `Arc` per entry?** Snapshot isolation (see [§8.1](#81-snapshot-isolation-via-arc-swap-and-epochs)) requires readers to safely observe a consistent past state without blocking writers. Wrapping each entity in `Arc` lets the writer install a new `Arc` in the slot atomically while readers continue to hold the old one.

**Why `BTreeSet` rather than `Vec` for reverse indexes?** Updates frequently insert and remove edges. `BTreeSet` gives O(log n) operations with deterministic iteration order (helps reproducible snapshots) and avoids duplicate edges.

---

## 4. Tree-Sitter Extraction Layer

The extraction layer is the only part of CodeRadar with language-specific code paths. Everything below the extractor operates on language-neutral `ExtractedUnit`s.

### 4.1 Tag Enum

```rust
pub enum Tag {
    Class,
    ClassBase,
    Function,                         // detailed flags set by decorator-pass, not by .scm
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

The `.scm` file does coarse classification only. Refinement (e.g., "is this function actually a `@staticmethod`?") happens in a post-tagging decorator pass — see [§4.5](#45-decorator-semantics).

### 4.2 `.scm` Query Files

#### 4.2.1 Python — `queries/python.scm`

```scheme
;; ---------- Classes ----------
(class_definition
  name: (identifier) @class.name) @class

(class_definition
  superclasses: (argument_list
                  (identifier)        @class.base))

;; Keyword args in class bases (metaclass=, total=, etc.) — captured for completeness
(class_definition
  superclasses: (argument_list
                  (keyword_argument
                    name:  (identifier)        @class.base_kw_name
                    value: (_)                 @class.base_kw_value)))

;; ---------- Functions and methods ----------
(function_definition
  name: (identifier) @function.name) @function

(decorated_definition
  (decorator (_) @function.decorator_expr)
  definition: (function_definition) @function.decorated)

(decorated_definition
  (decorator (_) @class.decorator_expr)
  definition: (class_definition) @class.decorated)

;; Parameters
(function_definition
  parameters: (parameters
                [(identifier)                          @param.name
                 (typed_parameter
                   (identifier)                        @param.name
                   type: (type)                        @param.type)
                 (default_parameter
                   name: (identifier)                  @param.name
                   value: (_)                          @param.default)
                 (typed_default_parameter
                   name: (identifier)                  @param.name
                   type: (type)                        @param.type
                   value: (_)                          @param.default)
                 (list_splat_pattern (identifier)      @param.varargs)
                 (dictionary_splat_pattern (identifier) @param.kwargs)]))

;; Return type
(function_definition
  return_type: (type) @function.return_type)

;; ---------- Imports ----------
(import_statement
  name: (dotted_name) @import.module) @import.module_form

(import_statement
  name: (aliased_import
          name:  (dotted_name) @import.module
          alias: (identifier)  @import.alias)) @import.module_form

(import_from_statement
  module_name: [(dotted_name)            @import.from
                (relative_import)        @import.from_relative]
  name: (dotted_name)                    @import.name) @import.from_form

(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.from_any
  name: (aliased_import
          name:  (dotted_name) @import.name
          alias: (identifier)  @import.alias)) @import.from_form

(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.from_star
  (wildcard_import)) @import.star_form

;; ---------- Calls ----------
(call function: (identifier) @call.function) @call.simple
(call function: (attribute
                  object:    (identifier)        @call.receiver
                  attribute: (identifier)        @call.method)) @call.attr
(call function: (attribute
                  object:    (attribute) @call.chain
                  attribute: (identifier) @call.method)) @call.deep
(call function: (attribute
                  object:    (call)               @call.nested
                  attribute: (identifier)         @call.method)) @call.chained
;; `Type(arg)` constructor-shaped call (same shape as simple call;
;; treated as constructor only if the resolver determines the name is a class)

;; ---------- Docstrings (first statement only, via anchoring `.`) ----------
(module . (expression_statement (string) @docstring))
(class_definition body: (block . (expression_statement (string) @docstring)))
(function_definition body: (block . (expression_statement (string) @docstring)))

;; ---------- Class-body field assignments ----------
(class_definition
  body: (block
          (expression_statement
            (assignment
              left: (identifier) @field.name
              right: (_) @field.value))))

(class_definition
  body: (block
          (expression_statement
            (assignment
              left: (identifier) @field.name
              type: (type) @field.type
              right: (_)? @field.value))))
```

**Notes:**
- `import_from_star` is intentionally a distinct form so the walker can flag wildcard imports for the resolver.
- The `field.*` captures only catch direct class-body assignments. Fields assigned in `__init__` (`self.x = ...`) are caught by a separate AST visit during method extraction; they are stored on the class, not the method.
- The `.` anchor on docstring queries is what guarantees we only capture the *first* statement of a body. This fixes the v1 bug where every bare string expression was tagged as a docstring.

#### 4.2.2 TypeScript / JavaScript — `queries/typescript.scm`

```scheme
;; ---------- Classes ----------
(class_declaration
  name: (type_identifier) @class.name) @class

(class_declaration
  (class_heritage
    (extends_clause value: (_) @class.base)))

(class_declaration
  (class_heritage
    (implements_clause (type_identifier) @class.implements)))

;; ---------- Methods ----------
(method_definition
  name: (property_identifier) @function.name) @function

;; Constructor — distinguished by name (TS grammar tags it like a method)
(method_definition
  name: (property_identifier) @function.name
  (#eq? @function.name "constructor")) @function.constructor

;; ---------- Top-level functions ----------
(function_declaration
  name: (identifier) @function.name) @function
(generator_function_declaration
  name: (identifier) @function.name) @function

;; Arrow / function-expression assigned to a const
(variable_declarator
  name: (identifier) @function.name
  value: [(arrow_function) (function_expression)]) @function

;; Exported declarations — captured but treated as transparent wrappers
(export_statement (function_declaration name: (identifier) @function.name)) @function
(export_statement (class_declaration    name: (type_identifier) @class.name)) @class

;; ---------- Parameters (TS) ----------
(formal_parameters
  [(required_parameter pattern: (identifier) @param.name
                       type: (type_annotation (_) @param.type)?)
   (optional_parameter pattern: (identifier) @param.name
                       type: (type_annotation (_) @param.type)?)
   (rest_pattern (identifier) @param.varargs)])

;; ---------- Return type (TS) ----------
(function_declaration
  return_type: (type_annotation (_) @function.return_type))
(method_definition
  return_type: (type_annotation (_) @function.return_type))
(arrow_function
  return_type: (type_annotation (_) @function.return_type))

;; ---------- Imports ----------
;; default:       import x from "y"
(import_statement
  (import_clause (identifier) @import.default)
  source: (string (string_fragment) @import.from)) @import

;; named:         import { a, b as c } from "y"
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name:  (identifier) @import.named
        alias: (identifier)? @import.named_alias)))
  source: (string (string_fragment) @import.from)) @import

;; namespace:     import * as x from "y"
(import_statement
  (import_clause
    (namespace_import (identifier) @import.namespace))
  source: (string (string_fragment) @import.from)) @import

;; side-effect:   import "y"
(import_statement
  source: (string (string_fragment) @import.from)
  !import_clause) @import.side_effect

;; type-only:     import type { X } from "y"
(import_statement
  "type"
  (import_clause (named_imports (import_specifier (identifier) @import.type_named)))
  source: (string (string_fragment) @import.from)) @import.type_only

;; re-exports:    export { X } from "y"
(export_statement
  (export_clause (export_specifier name: (identifier) @export.named))
  source: (string (string_fragment) @export.from)) @export.reexport

;; dynamic:       import("y")
(call_expression
  function: (import)
  arguments: (arguments (string (string_fragment) @import.dynamic_path))) @import.dynamic

;; ---------- Calls ----------
(call_expression
  function: (identifier) @call.function) @call.simple
(call_expression
  function: (member_expression
              object:   (identifier)         @call.receiver
              property: (property_identifier) @call.method)) @call.attr
(call_expression
  function: (member_expression
              object:   (call_expression)    @call.nested
              property: (property_identifier) @call.method)) @call.chained
(new_expression
  constructor: (identifier) @call.constructor) @call.new

;; ---------- JSDoc-style docstrings ----------
;; Captured by walker only when the comment immediately precedes a function/class.
(comment) @docstring.candidate
```

**Notes:**
- The original spec's `public_field_definition` is not a TypeScript tree-sitter node and is removed.
- `function_declaration` replaces the Python-only `function_definition`. Arrow functions assigned to `const`/`let`/`var` are caught via `variable_declarator`.
- JSDoc handling is done in the walker because tree-sitter has no notion of "comment immediately preceding a node"; the walker scans for the closest preceding `comment.docstring_candidate` whose end-row is exactly `target.start_row - 1`.

### 4.3 Tagging Phase

```rust
pub struct TaggedTree<'tree> {
    pub tree: &'tree Tree,
    pub source: &'tree [u8],
    pub tags: HashMap<usize, TagInfo>,    // keyed by node id
    pub by_kind: HashMap<Tag, Vec<usize>>, // node ids grouped by tag for fast scanning
}

pub struct TagInfo {
    pub tag: Tag,
    pub capture_name: &'static str,       // exact .scm capture name; useful for the walker
}

pub fn tag_nodes<'t>(
    source: &'t str,
    tree: &'t Tree,
    query: &Query,
) -> TaggedTree<'t> {
    let mut cursor = QueryCursor::new();
    let mut tags = HashMap::new();
    let mut by_kind: HashMap<Tag, Vec<usize>> = HashMap::new();

    for m in cursor.matches(query, tree.root_node(), source.as_bytes()) {
        for cap in m.captures {
            let cap_name = query.capture_names()[cap.index as usize];
            let tag = classify(cap_name);
            if let Some(t) = tag {
                tags.entry(cap.node.id()).or_insert(TagInfo { tag: t, capture_name: cap_name });
                by_kind.entry(t).or_default().push(cap.node.id());
            }
        }
    }

    TaggedTree { tree, source: source.as_bytes(), tags, by_kind }
}

fn classify(cap: &str) -> Option<Tag> {
    match cap {
        "class" | "class.decorated"           => Some(Tag::Class),
        "class.base" | "class.implements"     => Some(Tag::ClassBase),
        "function" | "function.decorated" |
        "function.constructor"                => Some(Tag::Function),
        "function.decorator_expr" |
        "class.decorator_expr"                => Some(Tag::Decorator),
        "param.name" | "param.varargs" | "param.kwargs"
                                              => Some(Tag::FunctionParam),
        "function.return_type"                => Some(Tag::FunctionReturn),
        "import" | "import.module_form" |
        "import.from_form" | "import.star_form" |
        "import.dynamic" | "import.side_effect" |
        "import.type_only"                    => Some(Tag::Import),
        "call.simple" | "call.attr" |
        "call.chained" | "call.deep" |
        "call.new"                            => Some(Tag::Call),
        "docstring"                           => Some(Tag::Docstring),
        "field.name"                          => Some(Tag::Field),
        _ => None,
    }
}
```

### 4.4 Hierarchy Walker (corrected)

The v1 walker had two bugs: it popped the context stack for any tagged node (including calls and imports, which never push), and it used `context_stack.len() > 1` as the test for "is method," which misclassifies nested functions as methods.

The corrected walker tracks **typed stack frames** and pops only frames it pushed.

```rust
struct Frame {
    qualified: String,
    kind: FrameKind,
}

enum FrameKind {
    Module,
    Class(ClassId),       // set after the Class entity is created
    Function,             // a function frame; nested functions inside are closures, not methods
}

struct WalkContext<'a> {
    file_path: &'a Path,
    language: Language,
    tags: &'a TaggedTree<'a>,
    units: &'a mut Vec<ExtractedUnit>,
    stack: Vec<Frame>,
}

fn walk_and_extract(node: Node, ctx: &mut WalkContext) {
    let pushed = if let Some(info) = ctx.tags.tags.get(&node.id()) {
        emit_for_node(node, info, ctx)
    } else {
        None
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_and_extract(child, ctx);
    }

    if let Some(frame_kind) = pushed {
        let popped = ctx.stack.pop();
        debug_assert!(matches!(popped, Some(f) if std::mem::discriminant(&f.kind) == std::mem::discriminant(&frame_kind)));
    }
}

/// Returns the kind of frame pushed (if any). The walker uses this to decide whether
/// to pop after recursing.
fn emit_for_node(node: Node, info: &TagInfo, ctx: &mut WalkContext) -> Option<FrameKind> {
    match info.tag {
        Tag::Class => {
            let name        = extract_text(&node.child_by_field_name("name")?, ctx.tags.source);
            let bases       = extract_class_bases(node, ctx);
            let decorators  = extract_decorators_for(node, ctx); // looks at @decorator siblings
            let docstring   = extract_docstring(node, ctx);
            let fields      = extract_class_body_fields(node, ctx);
            let parent      = ctx.stack.last().map(|f| f.qualified.clone());
            let qualified   = qualify(&parent, &name);

            ctx.units.push(ExtractedUnit::Class(ExtractedClass {
                name, qualified_name: qualified.clone(),
                bases, decorators, docstring, fields,
                line: node.start_position().row + 1,
                parent_qualified_name: parent,
                file_path: ctx.file_path.to_path_buf(),
                parse_quality: tree_quality_at(node),
            }));

            ctx.stack.push(Frame { qualified, kind: FrameKind::Class(ClassId::null()) });
            //                                            ^ placeholder; patched up after arena insert
            Some(FrameKind::Class(ClassId::null()))
        }

        Tag::Function => {
            let name       = extract_text(&node.child_by_field_name("name")?, ctx.tags.source);
            let params     = extract_parameters(node, ctx);
            let return_t   = extract_return_type(node, ctx);
            let decorators = extract_decorators_for(node, ctx);
            let docstring  = extract_docstring(node, ctx);
            let parent     = ctx.stack.last().map(|f| f.qualified.clone());
            let qualified  = qualify(&parent, &name);

            // is_method depends on the *immediate* parent frame's kind, not stack depth
            let is_method = matches!(ctx.stack.last(), Some(Frame { kind: FrameKind::Class(_), .. }));
            let kind = derive_function_kind(&decorators, is_method, /*is_async*/ is_async(node));

            ctx.units.push(ExtractedUnit::Function(ExtractedFunction {
                name, qualified_name: qualified.clone(),
                params, return_type: return_t, decorators, docstring, kind,
                is_async: is_async(node),
                is_generator: contains_yield(node),
                line: node.start_position().row + 1,
                parent_qualified_name: parent,
                file_path: ctx.file_path.to_path_buf(),
                calls: extract_call_sites(node, ctx),
                signature_hash: hash_signature(&params, &return_t, &decorators, &kind, is_async(node)),
                body_hash:      hash_body(node, ctx.tags.source),
                parse_quality:  tree_quality_at(node),
            }));

            // Push a Function frame so nested defs are treated as closures, not methods
            ctx.stack.push(Frame { qualified, kind: FrameKind::Function });
            Some(FrameKind::Function)
        }

        Tag::Import   => { emit_import(node, info, ctx);   None }
        Tag::Call     => { /* calls are emitted into the *enclosing* function's `calls` */
                           None }
        Tag::Docstring => None,   // already handled by extract_docstring at class/function emit time
        Tag::Field     => None,   // handled inside extract_class_body_fields

        Tag::ClassBase | Tag::FunctionParam | Tag::FunctionReturn |
        Tag::Decorator | Tag::CallReceiver |
        Tag::ImportFromClause | Tag::ImportSpecifier => None,
    }
}
```

After emission, the runner walks `ctx.units` once more in declaration order and back-patches `Frame::Class(ClassId::null())` placeholders with the real `ClassId` returned from the arena insert. This avoids a forward-reference problem during the initial walk.

### 4.5 Decorator Semantics

v1 said "decorator handling is simplified." This section makes it concrete.

Decorators affect three observable things about a function or class:
1. The `FunctionKind` (e.g., `@staticmethod` → `StaticMethod`).
2. The `EffectiveClass` (e.g., `@dataclass` → `Dataclass { ... }`).
3. The set of synthesized methods (e.g., `@dataclass` adds `__init__`, `__repr__`, `__eq__`).

**Known-decorator table.** A user-configurable map drives behavior:

| Decorator (Python) | Effect |
|---|---|
| `@staticmethod` | `FunctionKind::StaticMethod` |
| `@classmethod`  | `FunctionKind::ClassMethod` |
| `@property`     | `FunctionKind::Property` |
| `@<prop>.setter` | `FunctionKind::PropertySetter`; linked to the getter |
| `@<prop>.deleter` | `FunctionKind::PropertyDeleter` |
| `@functools.cached_property`, `@cached_property` | `FunctionKind::CachedProperty` |
| `@abstractmethod`, `@abc.abstractmethod` | `FunctionKind::AbstractMethod`; class becomes `EffectiveClass::Abstract` if any method is abstract |
| `@dataclass`, `@dataclasses.dataclass` | `EffectiveClass::Dataclass`; synthesizes `__init__`, `__repr__`, `__eq__` (and `__hash__`, `__lt__`, etc. depending on args) |
| `@dataclass(frozen=True)` | as above, `frozen: true` |
| Any unknown decorator | `decorators` field records the raw expression; no semantic effect |

**Decorator argument parsing.** For `@dataclass(frozen=True, eq=False)`, the extractor parses the keyword arguments and stores them on `EffectiveClass::Dataclass`. The grammar handles only literal arguments; complex expressions are conservatively ignored (e.g., `@dataclass(frozen=SOME_FLAG)` is treated as `@dataclass()`).

**Synthesized methods** are added to the class's `methods` list with `FunctionKind::DataclassSynthesized { from_class }`. They have no source line (set to the class's line) and a synthetic `signature_hash` derived from the field list. They are recomputed whenever the class's fields change.

**Property pairing.** For `@x.setter`, the extractor records a `setter_of: FunctionId` field on the function, looking up the getter by name within the same class. If the getter is missing (e.g., user wrote `@x.setter` before `@property def x`), the setter is still extracted but `setter_of` is `None` and a warning is recorded.

**TypeScript / JavaScript decorators.** Stage-3 decorators are recognized syntactically and stored on the target, but no semantic effects are applied in Phase 2; users get the raw decorator strings.

### 4.6 Docstring Extraction

A docstring is the first statement of a module, class, or function body, and only if it is a string literal. The `.scm` queries in [§4.2.1](#421-python--queriespythonscm) enforce this via the `.` anchor. The walker performs no additional filtering for Python.

For TypeScript, JSDoc comments are extracted in the walker: for each function/class node, look at the immediately preceding sibling. If it is a `comment` whose end-row is exactly `target.start_row - 1` and whose text begins with `/**`, attach it as the docstring.

### 4.7 Parse Quality and Tainted Symbols

Tree-sitter produces a partial tree with `ERROR` and `MISSING` nodes when it encounters syntax problems. CodeRadar extracts from these partial trees rather than skipping the file.

- `ParseQuality::Clean` — the subtree containing this symbol has no errors.
- `ParseQuality::Partial` — the subtree contains errors but the symbol's identifying fields (name, position) are intact.
- `ParseQuality::Tainted` — the subtree contains errors that affect the symbol's identifying fields; extraction is best-effort and the symbol may be wrong.

Determined by walking the symbol's subtree once and checking for any `ERROR` or `MISSING` nodes. The walk is O(subtree size) and runs only once per symbol.

**Update behavior.** If a file's new version is tainted (any tainted symbol or >5% of the file is `ERROR` nodes), `update_file` returns `fully_applied: false` and **does not commit**: the old graph slice for that file is retained. The user can override via `update_file(..., force=True)`. This protects against the LLM mid-edit case where the user reads the half-edited file.

---

## 5. Incremental Update Algorithm

This is the core of CodeRadar. Most v1 hand-waving lived here.

### 5.1 Update Flow

When a file changes (content, deletion, or creation):

1. **Parse** with the appropriate tree-sitter backend → produce a new tree, then a new set of raw `ExtractedUnit`s via the tagger and walker.
2. **Retrieve previous slice** for the file from `file_to_modules` and the dependent indexes.
3. **Diff** old vs new units (see [§5.2](#52-the-diff-algorithm)) → produce a `Patch` of `Add | Remove | Modify` operations on the four arenas.
4. **Compute affected dependents** using the reverse indexes:
   - Changed/removed class → all subclasses (transitively) and all references to the class name from importers.
   - Changed function signature → all callers (via `callers_by_callee`).
   - Changed function body only (signature unchanged) → no caller rebuild; just update `body_hash`.
   - Changed module → all modules in `importers[module]`.
   - New/changed wildcard import (`from x import *`) → all symbols using unqualified names from `x`.
5. **Apply patch** under a WAL transaction (see [§5.5](#55-wal-and-atomicity)). The WAL records intended mutations; if anything fails, the transaction is rolled back without exposing partial state to readers.
6. **Re-resolve only affected symbols** (see [§5.3](#53-cross-file-resolution)):
   - Import targets in the changed file.
   - Call sites in the changed file.
   - Call sites in affected callers (because signatures may have changed; resolved targets may have moved).
   - MRO of affected classes (those whose bases changed, or whose ancestors changed).
7. **Update reverse indexes** to reflect new edges.
8. **Bump file version** for the changed file; bump graph epoch.
9. **Invalidate stale resolution cache entries** ([§5.4](#54-resolution-cache)).
10. **Return `UpdateReport`** describing what changed.

### 5.2 The Diff Algorithm

v1 said "diff old vs new units (by qualified name and signature)." This is the elaboration.

**Goal.** Given the previous set of `ExtractedUnit`s for a file and the new set, produce a minimal `Patch` such that applying it preserves SlotMap keys for unchanged-or-merely-modified entities.

**Match key, in order of preference:**

1. **Exact match**: `(kind, qualified_name, signature_hash, body_hash)` identical → no-op (entity unchanged).
2. **Same identity, body changed**: `(kind, qualified_name, signature_hash)` match, `body_hash` differs → emit `Modify { id, new_body_hash }`. No caller rebuild needed.
3. **Same identity, signature changed**: `(kind, qualified_name)` match, `signature_hash` differs → emit `Modify { id, full_fields }`. Affected callers must re-resolve.
4. **Unmatched old**: no new unit matches → emit `Remove { id }`.
5. **Unmatched new**: no old unit matches → emit `Insert { unit }`.

Renames across versions appear as `Remove + Insert` (different qualified names). This is acceptable for Phase 1; Phase 4 will add similarity-based rename detection (Myers diff on body hashes, edit distance on parameter lists).

**Identity collision edge case:** When two overloads (or other duplicate entities) have identical `(kind, qualified_name, signature_hash)`, the diff algorithm has no deterministic way to choose which old entity matches which new entity. In this case, the algorithm falls back to position-based matching (earliest declaration line number). This is a known limitation; the correct fix is Phase 4's similarity-based rename detection.

**Ordering of operations within a patch.**

A naive "remove first, then insert" approach can break referential integrity transiently. Order:
1. Insert new modules.
2. Insert new classes (using forward-reference placeholders for bases that resolve to not-yet-inserted classes).
3. Insert new functions.
4. Insert new imports.
5. Modify existing entities (signature, body, decorators).
6. Resolve all forward references.
7. Remove obsolete entities (in reverse dependency order: imports first, then functions, then classes, then modules).

Step 6 may discover that a removal would invalidate a still-referenced edge. In that case, the edge is rewritten to point to `External { name }` (an opaque marker), and a warning is recorded.

### 5.3 Cross-File Resolution

This section was almost entirely missing from v1.

CodeRadar performs **static, name-based resolution**. It does not infer types. The output of resolution is one of: a concrete `SymbolId`, an `External` marker for known-external dependencies, or `Unresolved` with a reason from `UnresolvedReason`.

#### 5.3.1 Import Resolution

A module is identified by a dotted name (`foo.bar.baz`) which is mapped to a file on disk via the configured source roots (see [§11](#11-configuration)).

**Resolution rules for Python:**

| Form | Resolution |
|---|---|
| `import foo.bar` | Lookup `module_by_dotted_name[(lang_of_importer, "foo.bar")]`. If not found, search source roots for `foo/bar.py` or `foo/bar/__init__.py`. If still not found, mark as `External { distribution: lookup_distribution("foo") }`. |



**Language-qualified lookup.** The resolver always uses the importing module's language as the first component of the composite key (§3.4). This means a Python importer searching for `"foo.bar"` will only find Python modules with that dotted name, and a TypeScript importer will only find TypeScript modules. Cross-language lookups are explicitly unsupported — they resolve to `External { distribution: None }` per §18.2.

| `import foo.bar as fb` | As above; record alias `fb` in the module's local namespace. |
| `from foo.bar import baz` | Resolve `foo.bar`. If resolved to module `M`, look for symbol `baz` in `M`'s `exports`. If `baz` is a re-export, follow the chain. Resolution result is the original definition's `SymbolId`. |
| `from foo.bar import baz as b` | As above; alias `b`. |
| `from . import x` | Resolve `current_module.package` for `level=1`; for `level=k`, walk up `k` packages. Then `from <pkg> import x`. |
| `from .foo import bar` | Walk up one level to package, then resolve `<pkg>.foo`, then look up `bar`. |
| `from foo import *` | Resolve `foo`. If `foo.__all__` is statically determinable (a literal list of strings at module top level), expose those names. Otherwise, the import is `ImportResolution::Wildcard { module: foo, exposed: foo.public_top_level_names() }` where "public" means "does not start with underscore." Mark the importer's resolution scope as "shadowed by wildcard" so unresolved name lookups can attribute to this wildcard. |
| `if TYPE_CHECKING: import foo` | Recognized as type-checking-only. Resolved normally, but stored as `Import { type_only: true }`. Callers that respect `treat_type_only_as_runtime = false` ignore these for call resolution. |
| `importlib.import_module(name)` | `ImportResolution::Dynamic` if `name` is not a string literal; if it is a literal, resolve as a normal import. |

**TypeScript:** module resolution requires honoring `tsconfig.json` `paths` and `baseUrl`, plus `node_modules` lookup. Phase 2 implements a configurable resolver:
- Source roots from config.
- `paths` from `tsconfig.json` if present.
- `node_modules`-style lookup (walking up from the importer).
- Bare imports without a match → `External`.

`import type` is recognized and produces `Import { type_only: true }`.

### 5.3.2 `__all__` and Re-Export Tracking

A `Module`'s `exports: Vec<Export>` is populated during extraction:

1. If `__all__ = ["a", "b", "c"]` is present as a top-level assignment with a string-list literal, only those names are exported. Each becomes an `Export { name, source: Local | ReExport { from, original_name } }` depending on whether the name was defined locally or imported.
2. If no `__all__` exists, all top-level names not starting with underscore are exported as `Local`.

Re-exports are detected when an imported name appears in `__all__`. The chain is followed during resolution so `from a import X` where `a.__all__ = ["X"]` and `from .core import X` in `a/__init__.py` resolves to the `X` in `a.core`.

**Limits.** `__all__ += [...]`, `__all__.extend([...])`, conditional `__all__`, and string-formatted entries are treated as "non-statically-determinable" → fall back to "public top-level names."

#### 5.3.4 `__all__` Merge Rules for Stub/Impl Pairs

When a module has both an implementation file (`.py`) and a stub file (`.pyi`), the merged exports are computed as follows:

- **Only stub has `__all__`:** Use the stub export list. All entries carry `file_type = Stub`. Set `star_exports` to this list (no impl overrides it).
- **Only impl has `__all__`:** Use the impl export list. All entries carry `file_type = Impl`. Set `star_exports` to this list.
- **Both have `__all__`:**
  - For `exports`: take the union of both lists. Names present in both appear once with `file_type = Impl` (impl takes precedence for identity). Names unique to stub carry `file_type = Stub`. Names unique to impl carry `file_type = Impl`.
  - For `star_exports`: use impl's `__all__` only (Python runtime semantics).

**Explicit import resolution.** When resolving `from foo import X`, check if `X` exists anywhere in the merged module namespace (regardless of `file_type` or presence in any `__all__`). This matches Python behavior where explicit imports bypass `__all__`.

**Star import resolution.** When resolving `from foo import *`, use `star_exports` only. If `star_exports` is present, expose exactly those names. If absent (non-statically-determinable), fall back to public top-level names not starting with underscore.

### 5.3.3 Call Resolution

For each `UnresolvedRef` in a function's `calls`:

**Step 1 — classify the call shape:**

| Shape | Example |
|---|---|
| `Name(...)` | `foo(x)` |
| `self.method(...)` | `self.bar()` |
| `cls.method(...)` | `cls.create()` (in a `@classmethod`) |
| `Class.method(...)` | `MyClass.bar()` |
| `module.name(...)` | `os.path.join(...)` |
| `obj.method(...)` | `x.foo()` where `x` is a local |
| `chain().method(...)` | `get_thing().do_it()` |

**Step 2 — resolve based on shape:**

- **`Name(...)`** — scope chain lookup:
  1. Function's own locals (parameters, local defs).
  2. Enclosing function locals (for closures).
  3. Module's top-level names.
  4. Module's imported names (from `Import` entities).
  5. Builtins (`builtins` module's names; hardcoded list for Python 3.x).

  If found and the binding is a function → `ResolvedCall::Function(FunctionId)`. If a class → `ResolvedCall::Constructor(ClassId)`. If a builtin → `ResolvedCall::Builtin(name)`. If shadowed by a wildcard import → `ResolvedCall::Unresolved { reason: WildcardImportShadow }`. Otherwise `NameNotInScope`.

- **`self.method(...)`** — find the enclosing class via the walker context, then walk the class's MRO looking for a method named `method`. First match wins. If no match, `NameNotInScope`. Records as `ResolvedCall::Method { receiver: SelfRef, method }`.

- **`cls.method(...)`** in a `@classmethod` — same as `self.method`, but `receiver: ClassRef(class_id)`.

- **`Class.method(...)`** — resolve `Class` as a name first (step 1). If it resolves to a class, look up `method` in its MRO.

- **`module.name(...)`** — resolve `module` as a name. If it resolves to an `Import` with `ImportResolution::Module(m)`, look up `name` in `m`'s exports.

- **`obj.method(...)`** where `obj` is a local with no available type — `ResolvedCall::Unresolved { reason: TypeInferenceRequired }`. We do not guess.

- **`chain().method(...)`** — same as the above; chained calls require return-type inference.

**Step 3 — record the resolution.**

The result goes into `Function.resolved_calls`. The reverse index `callers_by_callee` is updated: for each `ResolvedCall::Function(callee)` or `ResolvedCall::Method { method: callee, .. }`, insert `(callee → caller)`.

**Important:** the resolver runs *per file*, not globally. It uses `resolution_cache` ([§5.4](#54-resolution-cache)) to amortize repeated lookups across calls within the same file and same module's importers.

### 5.3.4 MRO Computation (C3 Linearization)

For each class, the MRO is computed lazily on first access (and cached on `Class.mro`) using the standard C3 algorithm:

```
L[C] = C + merge(L[B1], L[B2], ..., [B1, B2, ...])
```

where `merge` takes the head of the first list whose head does not appear in the tail of any other list.

**External bases** (`MroNode::External`) are treated as opaque: they participate in the MRO order but cannot be linearized further. If C3 fails to produce a consistent MRO (genuine diamond ambiguity), the class is marked `EffectiveClass::Abstract` with a `mro_error` flag and the partial MRO is retained.

**Invalidation.** MRO is invalidated for class `C` whenever:
- One of `C`'s bases changes.
- Any class transitively above `C` in the inheritance graph changes its bases.

Tracked via `subclasses`: when class `B`'s bases change, walk `subclasses[B]` transitively and clear their cached MROs.

**Bounded invalidation.** The transitive walk is bounded by a configurable maximum depth (default: 50). If the walk exceeds this depth, the entire `method_in_class` cache for all affected classes is flushed (rather than clearing individual entries one at a time). This prevents pathological O(n²) invalidation on deeply nested inheritance hierarchies. In practice, Python inheritance chains rarely exceed 10 levels, so the default bound is safe.

### 5.4 Resolution Cache

To avoid re-resolving every name on every update, the resolver maintains a cache:

```rust
pub struct ResolutionCache {
    /// (module_id, name_in_scope) -> resolved symbol
    name_in_module: HashMap<(ModuleId, String), Resolution>,
    /// (class_id, method_name) -> FunctionId via MRO lookup
    method_in_class: HashMap<(ClassId, String), FunctionId>,
    /// (importer_module, dotted_path) -> resolution
    import_target: HashMap<(ModuleId, String), ImportResolution>,
}

pub enum Resolution {
    Symbol(SymbolId),
    External,
    Unresolved(UnresolvedReason),
}
```

**Invalidation rules** (run during step 9 of [§5.1](#51-update-flow)):

| Change | Invalidate |
|---|---|
| Module `M` added/removed/renamed | All `name_in_module[(_, name)]` where name resolves through M; all `import_target[(_, path)]` where path starts with M's dotted name |
| Class `C` added/removed | All `method_in_class[(C, _)]`; all `name_in_module[(_, C.name)]` in C's module |

| Class `C` bases changed | All `method_in_class[(C, _)]`; all `m

**Memory cost:** `module_epochs` adds ~8 bytes per module (u64 epoch counter). For 5k modules: ~40KB — negligible within budget.

### 5.5 WAL and Atomicity

The v1 spec described arena cloning as "copy-on-write," which is contradictory. This section commits to a concrete design that is both correct and meets the p95 < 100ms target.

**No whole-arena cloning.** Cloning an arena of 100k functions on every update is fatal. Instead:

1. Each entity is stored as `Arc<Entity>` in its slot (see [§3.4](#34-graph-container-with-reverse-indexes)).
2. Modifications create a new `Arc<Entity>` and atomically replace the slot's pointer.
3. Readers that obtained an `Arc` clone of the old entity continue to see it; the new pointer is observed by readers that read the slot after the swap.

This is per-entry MVCC — like RCU, scoped to each SlotMap slot.

**Write-Ahead Log structure:**

```rust
pub struct PatchTransaction {
    id: TxId,
    entries: Vec<WalEntry>,
    /// Snapshot of (slot_key, old_arc) for every slot the transaction touched.
    /// Used to roll back if commit fails midway.
    rollback: Vec<(ArenaKind, SlotKeyRaw, Option<ArcAny>)>,
}

pub enum WalEntry {
    Insert { kind: ArenaKind, key: SlotKeyRaw, entity: ArcAny },
    Modify { kind: ArenaKind, key: SlotKeyRaw, new_entity: ArcAny },
    Remove { kind: ArenaKind, key: SlotKeyRaw },
    /// Reverse-index edge mutations are recorded for rollback completeness.
    IndexInsert { index: IndexKind, key: IndexKey, value: IndexValue },
    IndexRemove { index: IndexKind, key: IndexKey, value: IndexValue },
}
```

**Commit protocol:**

1. **Prepare (no global lock).** The transaction is built up in memory; new `Arc<Entity>` values are constructed. Reading existing slots requires only a read lock (cheap with `parking_lot::RwLock`).
2. **Validate.** Re-check preconditions (no concurrent transaction modified the same slots since the prepare started). If a conflict is found, abort and retry from step 1.
3. **Apply (per-arena write lock, brief).** Walk `entries` in order; replace `Arc`s atomically. Reverse-index mutations are applied in the same critical section. Total work: O(|entries|) pointer writes plus index updates.
4. **Bump epoch.** `graph.epoch.fetch_add(1, Ordering::SeqCst)`.
5. **Release locks.** Drop write locks; readers may observe the new state.

**Rollback.** If step 3 fails (it should not, given step 2), undo by walking `rollback` in reverse and restoring the captured `Arc`s. The graph never exposes a half-applied state because step 3 holds the write lock from start to finish.

**Persistence (optional).** The WAL is in-memory by default. With `--journal /path/to/wal` it is also written to disk and synced before step 3 begins. On startup with a journal file present, replay the journal against the loaded snapshot before serving queries.

**Crash recovery protocol.** The journal must use a two-phase commit:
1. **Phase 1 — Journal write.** Write all `WalEntry` records to the journal file and call `fsync()`.
2. **Phase 2 — Commit.** Apply entries to the arenas (step 3 above).
3. **Phase 3 — Ack.** Write a `TxAck` record to the journal (after the entries) and `fsync()`.

On startup, replay only journal entries whose `TxAck` record is present (fully committed transactions). Incomplete transactions (entries without a trailing `TxAck`) are ignored — the arenas already contain their effects (since the journal is written before commit), so the graph is already in a consistent state.

**This is a mandatory requirement.** The current description of "write journal before step 3" without an ack record is insufficient for crash safety: if the process crashes between journal write and arena commit, the journal would replay those entries against the old snapshot, producing a graph that includes mutations that were never applied. The ack record distinguishes "fully committed" from "in-flight" transactions.

---

## 6. Query Engine

### 6.1 Pest Grammar (revised)

The v1 grammar had no operator precedence, no parenthesization, no `NOT`, and ambiguous comma usage between `group by` lists and aggregation lists.

```pest
WHITESPACE = _{ " " | "\t" | "\n" | "\r" }
COMMENT    = _{ "--" ~ (!"\n" ~ ANY)* }

// Reserved keywords — exclude from `identifier`.
keyword = { "where" | "group" | "by" | "order" | "asc" | "desc" | "limit"
          | "and" | "or" | "not" | "true" | "false" | "null"
          | "select" | "as" | "count" | "sum" | "avg" | "min" | "max"
          | "contains" | "matches" | "in" }

identifier = @{ !keyword ~ (ASCII_ALPHA | "_") ~ (ASCII_ALPHANUMERIC | "_")* }
path       = @{ identifier ~ ("." ~ identifier)* }

string = @{ "\"" ~ ("\\\"" | (!"\"" ~ ANY))* ~ "\"" }
number = @{ "-"? ~ ASCII_DIGIT+ ~ ("." ~ ASCII_DIGIT+)? }
bool   = { "true" | "false" }
null   = { "null" }
list   = { "[" ~ value ~ ("," ~ value)* ~ "]" }
value  = { string | number | bool | null | list }

// Function-call-style derived field operand. E.g., has_method("__init__"), inherits_from("BaseModel").
derived_call = { identifier ~ "(" ~ value ~ ("," ~ value)* ~ ")" }

// Operands of comparisons: a path (entity field), a literal, an aggregation result, or a derived call.
operand = { path | value | derived_call }

comp_op = { "==" | "!=" | "<=" | ">=" | "<" | ">" | "contains" | "matches" | "in" }

predicate = { operand ~ comp_op ~ operand }

// Boolean expression with explicit precedence (NOT > AND > OR) and grouping.
atom      = { "(" ~ or_expr ~ ")" | "not" ~ atom | predicate }
and_expr  = { atom    ~ ("and" ~ atom)* }
or_expr   = { and_expr ~ ("or"  ~ and_expr)* }

where_clause = { "where" ~ or_expr }

agg_func = { "count" | "sum" | "avg" | "min" | "max" }
agg_expr = { agg_func ~ "(" ~ (path | "*") ~ ")" ~ "as" ~ identifier }

group_by_clause = { "group" ~ "by" ~ path ~ ("," ~ path)* }

// SELECT-style projection. For non-aggregated queries, defaults to selecting the entity itself.
select_item    = { path | agg_expr }
select_clause  = { "select" ~ select_item ~ ("," ~ select_item)* }

order_by_clause = { "order" ~ "by" ~ path ~ ("asc" | "desc")? }
limit_clause    = { "limit" ~ number }

entity = { "modules" | "classes" | "functions" | "imports" | "calls" | "fields" }

query = {
    SOI
    ~ entity
    ~ select_clause?
    ~ where_clause?
    ~ group_by_clause?
    ~ order_by_clause?
    ~ limit_clause?
    ~ EOI
}
```

### 6.2 AST Representation

```rust
pub struct Query {
    pub entity: EntityType,
    pub select: Vec<SelectItem>,            // empty = select entity itself
    pub where_clause: Option<BoolExpr>,
    pub group_by: Vec<FieldPath>,
    pub order_by: Option<(FieldPath, OrderDirection)>,
    pub limit: Option<usize>,
}

pub enum BoolExpr {
    Predicate(Predicate),
    Not(Box<BoolExpr>),
    And(Vec<BoolExpr>),
    Or(Vec<BoolExpr>),
}

pub struct Predicate {
    pub left: Operand,
    pub op: CompOp,
    pub right: Operand,
}

pub enum Operand {
    Path(FieldPath),                       // e.g., ["module", "name"]
    Literal(LiteralValue),
}

pub enum SelectItem {
    Field(FieldPath),
    Aggregation { func: AggFunc, target: AggTarget, alias: String },
}

pub enum AggTarget { Field(FieldPath), Star }
```

### 6.3 Execution Modes

Two modes, but both expose an iterator interface so the Python API is uniform.

- **Streaming mode** (no `group by`, no aggregations in `select`). The executor walks the relevant arena, applying the `where` filter and yielding wrapped Python entities lazily. Order: source-file order unless `order by` is specified, in which case the executor materializes IDs into a `Vec`, sorts, then streams from the sorted vec.
- **Aggregated mode** (any `group by` or any aggregation in `select`). The executor materializes groups into a `HashMap<GroupKey, GroupAccumulator>`, then iterates the map (sorted by the group keys if `order by` references a group field, by the aggregation result if it references one).

Both modes operate on a **snapshot** of the graph taken at query start (see [§8.1](#81-snapshot-isolation-via-arc-swap-and-epochs)), so concurrent updates do not perturb the result.

### 6.4 Python Iterator

```rust
#[pyclass]
pub struct QueryIterator {
    inner: Box<dyn Iterator<Item = QueryRow> + Send>,
    cancelled: Arc<AtomicBool>,
    check_interval: usize,
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
        match self.inner.next() {
            Some(row) => Ok(Some(row.into_py(py))),
            None => Ok(None),
        }
    }

    fn cancel(&self) { self.cancelled.store(true, Ordering::Relaxed); }
}
```

Cancellation is cooperative; the executor checks every `check_interval` items. Default 64; tunable via config.

### 6.5 Query Examples

The grammar is best validated by the queries it must support. The following are committed targets:

```sql
-- Simple: all classes inheriting from BaseModel
classes where inherits_from contains "BaseModel"

-- All functions longer than 50 lines
functions where line_count > 50

-- Functions never called from elsewhere in the codebase (potential dead code)
functions where caller_count == 0 and not name matches "^test_.*"

-- "God classes": classes with more than 20 methods
classes where method_count > 20 order by method_count desc limit 25

-- All async functions in a module
functions where module.name == "app.services" and is_async == true

-- All classes with a custom __init__ but no __eq__
classes where has_method("__init__") == true and has_method("__eq__") == false

-- Group-by aggregation: method count per module
classes
  select module.name, count(*) as class_count, avg(method_count) as avg_methods
  group by module.name
  order by class_count desc
  limit 20

-- Import-cycle detector helper: modules and their importer count
modules
  select name, count(importers) as fan_in
  group by name
  order by fan_in desc
  limit 50

-- All call sites that remain unresolved due to needing type inference
calls where unresolved_reason == "TypeInferenceRequired"

-- Find subclasses of a specific class anywhere in the codebase
classes where "MyBaseClass" in mro_names

-- Functions decorated with a known-deprecated decorator
functions where decorators contains "deprecated"

-- Modules with parse errors
modules where parse_quality != "Clean"

-- Wildcard-import modules (often a smell)
imports where kind == "StarImport"

-- Property/setter pairs missing setters
functions where kind == "Property" and has_setter == false

-- All overrides of a particular method, transitively
functions where overrides_of("BaseService.handle") == true
```

The `inherits_from`, `caller_count`, `mro_names`, `has_method`, `overrides_of`, etc. are **derived fields** exposed by the executor. They are computed from the reverse indexes on demand (with caching).

### 6.6 Derived Field Reference

This section provides the authoritative schema for all derived fields used in query predicates (§6.5). The supplementary file `docs/query-fields.md` mirrors this reference and is intended for quick lookup during development/debugging; in case of discrepancy, §6.6 takes precedence.

#### 6.6.1 Functions Entity — Derived Fields

| Derived Field | Return Type | Scope | Semantics | Source Index |
|---|---|---|---|---|
| `caller_count` | i64 (non-negative) | All modules in the graph | Count of distinct call sites that resolve to this function. Computed as `callers_by_callee[function_id].len()`. Includes callers from all languages present in the codebase. Zero means no resolved callers exist. | `callers_by_callee` reverse index (§3.4) |
| `line_count` | i64 (non-negative) | Single function body | Number of source lines in the function body (excluding the def line itself, including blank lines and comments within the body). Computed during extraction from tree-sitter node span: `body_end_line - body_start_line + 1`. | Extracted unit metadata (§4.2) |
| `module.name` | String | Single function to parent module | Dotted name of the module containing this function (e.g., "app.services"). This is a native field but appears in derived-field examples because it requires cross-entity traversal through the `parent_module` link. | Function.parent_module → Module.dotted_name (§3.4) |
| `is_async` | bool | Single function | Whether the function is declared with `async def` (Python) or `async function` / async arrow (TypeScript/JS). True if the Async flag is set on the extracted unit. | Extracted unit metadata (§4.2) |
| `has_method(name: String)` | bool | Class to its method table | Returns true if the class direct methods (not inherited) include a method with the given name. Case-sensitive match on method name. Implemented as a `DerivedCall` operand — see §6.1 grammar extension. | Class.methods field (§7.2) |
| `has_setter` | bool | Single function | Returns true if this function is decorated as a property setter (e.g., `@name.setter`). Only meaningful when `kind == "Property"`. | Extracted unit metadata: decorator analysis (§4.3) |
| `decorators` | List<String> | Single function or class | List of all decorator names applied to the entity, in source order. Each entry is the fully qualified name if resolvable, otherwise the raw identifier as written. E.g., `["cached_property", "deprecated"]`. | Extracted unit metadata (§4.3) |
| `overrides_of(target: String)` | bool | Function to MRO chain | Returns true if this function overrides a method named by target (dotted, e.g., "BaseService.handle"). The executor resolves the target name to a method ID, then checks whether this function appears in any class's direct methods that shadow the target via MRO. Case-sensitive. **Complexity:** O(m) where m is the MRO depth of the enclosing class (with cached MRO). | MRO lookup (§5.3.4) + Class.methods |
| `parse_quality` | Enum: Clean / Partial / Tainted | Single module | The aggregate parse quality of all files in the module. Clean if zero ERROR nodes across all files; Partial if some files have errors but others parsed cleanly; Tainted if any file exceeds the error threshold (see §4.7). Cached on Module entity; invalidated during update step 7 (§5.1). | Parse results (§4.7) |
| `unresolved_reason` | Enum: NameNotInScope / WildcardImportShadow / TypeInferenceRequired / DynamicImport / ParseError | Single call site | The reason a call site could not be resolved. Maps directly to UnresolvedReason (§5.3). Only meaningful for calls entity type. | Resolution results (§5.3) |

#### 6.6.2 Classes Entity — Derived Fields

| Derived Field | Return Type | Scope | Semantics | Source Index |
|---|---|---|---|---|
| `inherits_from(name: String)` | bool | Single class to its MRO | Returns true if the string name appears anywhere in the class C3-linearized MRO (including the class itself). Case-sensitive, full-name match — "BaseModel" matches a base named exactly "BaseModel", not "MyBaseModel". If the base resolves to an external module, the dotted name is used for matching. Implemented as a `DerivedCall` operand — see §6.1 grammar extension. | Class.mro (§5.3.4) |
| `mro_names` | List<String> | Single class to its MRO | List of all class names in the C3-linearized MRO, from most-derived to least-derived (including object). Each entry is a string: for local classes, the qualified name; for external bases, "External.dotted.name". Supports containment via `in` operator: `"MyBaseClass" in mro_names`. Cached on Class.mro; invalidated when class bases change (§5.3.4). | Class.mro (§5.3.4) |
| `method_count` | i64 (non-negative) | Single class | Count of methods defined directly on this class (not inherited). Computed as `Class.methods.len()`. Does not include properties counted separately — a property setter is a separate function entry in methods. | Class.methods (§7.2) |
| `subclasses` | List<Class> | Single class to subclass index | List of all classes that directly or transitively inherit from this class. This is the forward direction of `inherits_from`. Computed by walking `subclasses[class_id]` (the reverse index). | subclasses reverse index (§3.4) |
| `decorators` | List<String> | Single class | Same semantics as for functions — list of decorator names in source order. E.g., `["dataclass", "slotted"]`. | Extracted unit metadata (§4.3) |
| `parse_quality` | Enum: Clean / Partial / Tainted | Single class | Quality of the file containing this class, mapped to the module-level quality. If the class file has parse errors, it inherits the file quality; otherwise it inherits the module aggregate quality. | Parse results (§4.7) |

#### 6.6.3 Modules Entity — Derived Fields

| Derived Field | Return Type | Scope | Semantics | Source Index |
|---|---|---|---|---|
| `name` | String | Single module | Dotted name of the module (e.g., "app.services"). Native field, not derived. | Module.dotted_name (§3.4) |
| `parse_quality` | Enum: Clean / Partial / Tainted | Single module | Aggregate parse quality across all files in the module. See §6.6.1 for classification rules. Cached on Module entity; invalidated during update step 7 (§5.1). | Parse results (§4.7) |

#### 6.6.4 Imports Entity — Derived Fields

| Derived Field | Return Type | Scope | Semantics | Source Index |
|---|---|---|---|---|
| `kind` | Enum: NormalImport / StarImport / FromImport / ReExport | Single import | Classification of the import statement. StarImport for "from x import *". Used in queries like `imports where kind == "StarImport"`. | Extracted unit metadata (§4.3) |

#### 6.6.5 Calls Entity — Derived Fields

| Derived Field | Return Type | Scope | Semantics | Source Index |
|---|---|---|---|---|
| `unresolved_reason` | Enum (see §6.6.1) | Single call site | The reason a call site could not be resolved. Maps directly to UnresolvedReason (§5.3). Only meaningful for calls entity type. | Resolution results (§5.3) |

#### 6.6.6 Grammar Extension: DerivedCall Operand

The grammar production `derived_call` (added to §6.1) enables function-call-style operands:

```
derived_call = { identifier ~ "(" ~ value ~ ("," ~ value)* ~ ")" }
operand = { path | value | derived_call }
```

This makes `has_method("__init__")`, `inherits_from("BaseModel")`, and `overrides_of("BaseService.handle")` first-class operands that can appear on either side of a comparison operator.

The executor maps these to the Rust enum:
```rust
pub enum Operand {
    Path(FieldPath),
    Literal(LiteralValue),
    DerivedCall { name: String, args: Vec<LiteralValue> },
}
```

#### 6.6.7 Caching and Invalidation for Derived Fields

**Two-tier cache architecture:**

| Tier | Lifetime | Scope | Purpose |
|---|---|---|---|
| Per-query memo | Query lifetime | Single query execution | Avoid recomputing the same derived field multiple times within a single query (e.g., caller_count used in both where and order by). Implemented as HashMap<EntityId, CachedDerivedValue> on the executor. |
| Persistent index cache | Until next update commit | Entire graph | Fields requiring computation (MRO linearization, parse_quality aggregation) cache their results on the entity itself. Most derived fields are O(1) from reverse indexes and need no separate caching. |

**Invalidation rules tied to incremental updates (§5):**

| Change Event | Derived Field Impact | Invalidation Action |
|---|---|---|
| New caller added for function F | `caller_count` for F increases by 1 | No invalidation needed — `callers_by_callee[F]` updated atomically during patch step 7. Count computed on-demand from live index. |
| Function removed | All derived fields referencing that function become stale | Affected entities per-query caches are irrelevant (entity no longer exists). Persistent caches on surviving entities unaffected unless transitively dependent. |
| Class bases change | `inherits_from`, `mro_names`, `subclasses` for affected classes and transitive subclasses | Walk `subclasses[C]` transitively (bounded by depth 50 per §5.3.4) and clear cached MROs. `inherits_from` recomputes from updated MRO on next access. |
| File parse quality changes | `parse_quality` for affected modules/classes/functions | Recompute module-level aggregate during step 7 of §5.1; propagate to dependent entities. |

**Performance characteristics:**

| Derived Field | Computation Cost | Cached? | Reason |
|---|---|---|---|
| `caller_count` | O(1) — index lookup + len() | No | Direct from callers_by_callee |
| `method_count` | O(1) — vector len() | No | Direct from Class.methods |
| `line_count` | O(1) — stored in extracted unit | No | Computed at extraction time |
| `inherits_from(name)` | O(m) where m = MRO depth | Partial (MRO cached) | Depends on mro_names; MRO memoized per class |
| `mro_names` | O(n) for linearization, O(1) after first | Yes — on Class.mro | C3 linearization is expensive; invalidated when bases change (§5.3.4) |
| `has_method(name)` | O(k) where k = methods count | No (per-query memo only) | Linear scan of Class.methods; typically small (< 50). For classes with >100 methods, consider adding persistent cache in Phase 2. |
| `overrides_of(target)` | O(m) where m = MRO depth | Partial (MRO cached) | Walks current class's MRO chain; benefits from cached MRO |
| `decorators` | O(1) — stored in extracted unit | No | Computed at extraction time |
| `parse_quality` | O(f) where f = files per module | Yes — on Module | Aggregate across files; recomputed during update step 7 |

**Interaction with snapshot isolation (§8.1):**
- Queries running before an update commits see pre-update values of all derived fields, because they operate on a snapshot taken at their start epoch.
- Queries running after an update commits see post-update values. The new epoch means they load fresh arena references and compute derived fields from updated indexes.
- No stale reads: derived fields like `caller_count` are computed on-demand from reverse indexes (which are part of the snapshot), so there is no separate cache that could diverge from the snapshot.
- Per-query memo caches are scoped to a single query execution and do not cross epoch boundaries.

---

## 7. Python API

```python
import coderadar

# ---------- Initial analysis ----------
graph = coderadar.analyze("src/")
# or load a previously-exported snapshot
graph = coderadar.load("./.coderadar/snapshot.bin")

# ---------- Update after an LLM writes a file ----------
report = graph.update_file("src/core/engine.py")        # reads from disk
# or pass content directly:
report = graph.update_file("src/core/engine.py", content=new_content)
print(f"{report.elapsed_ms:.1f}ms; "
      f"affected {len(report.affected_files)} files, "
      f"{len(report.changed_symbols)} symbols")
if not report.fully_applied:
    print("Tainted update; old slice retained")

# ---------- Batch updates ----------
with graph.batch() as b:
    b.update_file("src/a.py")
    b.update_file("src/b.py")
    b.update_file("src/c.py")
# single epoch bump on exit; readers see all-or-none

# ---------- Streaming query ----------
for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
    print(cls.name, [m.name for m in cls.methods])

# ---------- Aggregated query (still iterable) ----------
rows = graph.query(
    "classes "
    "select module.name, count(*) as n "
    "group by module.name "
    "order by n desc limit 10"
)
for row in rows:
    print(row["module_name"], row["n"])

# ---------- Snapshot export ----------
graph.export_snapshot("./.coderadar/snapshot.bin")      # binary, fast
graph.export_snapshot("./snapshot.json", format="json") # human-readable

# ---------- Watch mode ----------
import coderadar
with coderadar.watch("src/") as w:
    for event in w:                                     # iterator of UpdateReport
        print(event.affected_files, event.elapsed_ms)

# ---------- ID-based access ----------
fn_id = graph.find_function("app.services.UserService.create")
fn = graph.get_function(fn_id)        # None if removed
callers = graph.callers_of(fn_id)     # list[FunctionId]
```

### 7.1 `UpdateReport`

```python
@dataclass(frozen=True)
class UpdateReport:
    affected_files: list[str]
    changed_symbols: list[SymbolChange]                 # see below
    new_unresolved_references: list[UnresolvedRef]
    newly_resolved_references: list[ResolvedRef]
    elapsed_ms: float
    parse_quality: ParseQuality                          # quality of the *primary* file
    parse_errors: int                                    # count of ERROR nodes
    fully_applied: bool                                  # false if rejected (tainted, see §4.7)
    epoch_before: int
    epoch_after: int

@dataclass(frozen=True)
class SymbolChange:
    kind: Literal["module", "class", "function", "import"]
    operation: Literal["added", "removed", "signature_changed", "body_changed", "moved"]
    qualified_name: str
    file: str
    line: int
    id: int                                              # SlotMap key (valid only if not "removed")
```

### 7.2 Entity Wrappers

Each entity type has a Python wrapper that lazily fetches fields via PyO3 calls. Wrappers carry the `(epoch, SlotMap key)` pair; calls that find a stale epoch raise `coderadar.StaleHandle`. This is the user's signal that the underlying entity has changed and should be re-fetched.

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
    # Derived (cheap via index lookup):
    subclasses: list[Class]
    method_count: int
```

---

## 8. Concurrency, Locking, and Snapshot Isolation

### 8.1 Snapshot Isolation via `Arc`-swap and Epochs

The v1 spec said "QueryIterator holds the lock only while fetching the next item." This is unsafe: between yielding item N and N+1, an update can remove the entity at the key the iterator was about to dereference.

**Replacement design.**

- Every entity is stored as `Arc<Entity>` in its slot.
- The graph has a monotonic `epoch: AtomicU64`. Each successful commit bumps the epoch.
- A query starts by taking a `QuerySnapshot { epoch, arena_refs }`. The arena refs are obtained by cloning the underlying `Vec<Slot<Arc<Entity>>>` *pointers* (cheap; one `Arc` per arena, not per entity). This works because internally each `SlotMap` arena is itself wrapped in `arc-swap::ArcSwap<SlotMapInner>`.
- During iteration, the query reads `Arc`s out of its snapshot. It never observes a mid-commit state.
- A concurrent update commits by:
  1. Cloning the current `SlotMapInner` (the *outer* structure: a Vec of slots), but **not** the `Arc<Entity>` values inside.
  2. Mutating the clone: insert/update/remove `Arc<Entity>` pointers.
  3. `ArcSwap::store` the new `SlotMapInner`. Cheap: one atomic pointer swap.

The clone in step 1 is O(slot count) but only copies pointers, not entities. For 100k entities, that is ~800KB of contiguous memory, taking under 100µs.

**Cost summary.**
- Query start: one `ArcSwap::load` per arena touched. O(1).
- Per-item iteration: one slot index. O(1). No locks.
- Update commit: one inner-vector clone per touched arena + one pointer swap. **O(arena_size)** — this is the dominant cost of the write path. For 100k entities, ~800KB of contiguous memory copy, taking ~100µs on modern hardware. This is acceptable for the p95 < 100ms target, but it is NOT O(1).

**IMPORTANT:** The O(arena_size) clone cost must be accounted for in the update budget. For the worst case (all four arenas modified), the total clone cost is ~3.2MB, taking ~400µs. This leaves ~99.6ms for parsing, diffing, resolution, and index updates — which is tight for large files.

**Optimization path:** If the clone cost becomes a bottleneck, consider a fine-grained copy-on-write arena (e.g., `bumpalo`-style bump allocation with arena snapshots) where only modified slots are copied. This is deferred to Phase 2.

**Memory.** Long-lived queries hold old inner-vectors alive. For interactive use, queries complete in milliseconds; this is fine. For pathological cases (a Python script iterates a query slowly while many updates land), old snapshots accumulate; they are released as the iterator advances and the user drops the reference.

### 8.2 Lock Hierarchy

To avoid deadlocks during the commit path:

1. Arenas are locked in fixed order: `modules → classes → functions → imports → indexes`.
2. The resolution cache uses its own `RwLock`, acquired *after* any arena locks.
3. Readers never escalate from read to write.
4. The watcher thread and the API thread share a single committer task via an MPSC channel; only the committer holds write locks.

### 8.3 GIL Handling

- Every long-running Rust method (`analyze`, `update_file`, query iteration) wraps its body in `py.allow_threads(|| { ... })`.
- The query iterator's `__next__` does *not* release the GIL during the cheap path (advancing one item), but does call `py.check_signals()` at the configured interval to give Python a chance to handle `KeyboardInterrupt`.

---

## 9. Error Handling and Fault Tolerance

### 9.1 Error Categories

| Category | Examples | Default behavior | `--strict` |
|---|---|---|---|
| Parse error | Tree-sitter ERROR/MISSING nodes | Mark symbols as `Partial`/`Tainted`; continue | Exit 1 on any |
| Resolution failure | Unresolved name, missing import target | Mark as `Unresolved`; continue | Continue (these are expected) |
| I/O error | File disappeared mid-update | Drop file's slice; warn | Exit 1 |
| Grammar mismatch | Snapshot from incompatible CodeRadar version | Refuse to load | Exit 1 |
| Internal invariant | (Bug.) `debug_assert!` fires | Panic in debug; log + abort in release | Same |

### 9.2 Warnings

Warnings are collected on the graph during analysis and updates. Categories:

- `ParseWarning { file, line, message }`
- `ResolutionWarning { symbol_id, message }`
- `DecoratorWarning { function_id, decorator, message }` (e.g., orphan `@x.setter`)
- `WildcardImportShadow { module, name }`

By default, warnings are stored but not printed. `--verbose` prints them. `coderadar warnings` lists them.

### 9.3 Tainted Update Policy

See [§4.7](#47-parse-quality-and-tainted-symbols). When `update_file` rejects a tainted update, the WAL transaction is aborted and the previous file slice is retained. The returned `UpdateReport` has `fully_applied = false` and includes a `rejection_reason`.

---

## 10. Command-Line Interface

```
coderadar init <path>                  Initial analysis; writes .coderadar/snapshot.bin
coderadar analyze <path>               Same as init but does not persist
coderadar update <file> [--content -]  One-shot update; reads stdin if --content -
coderadar watch <path>                 Long-running watcher; emits JSONL events on stdout
coderadar query "<query string>"       Execute query; pretty-prints results
coderadar shell                        REPL with persistent graph in memory
coderadar export <path> [--format f]   Export snapshot; f ∈ {bin, json, yaml}
coderadar load <snapshot>              Load a snapshot and verify integrity
coderadar stats                        Counts, parse-error summary, memory usage
coderadar warnings [--category c]      List warnings
coderadar resolve <qualified-name>     Show how a name resolves (debugging)
coderadar callers <qualified-name>     List callers of a function
coderadar visualize <type> <args>      Run a visualizer (see §14)
```

Global flags:
- `--config <path>` (default: `./.coderadar.toml`)
- `--strict`
- `--verbose`
- `--quiet`
- `--language <py|ts|js|...>` (force; otherwise inferred from file extensions)
- `--exclude <glob>` (repeatable)
- `--journal <path>` (enable WAL persistence)
- `--no-color`

Exit codes:
- `0` success
- `1` user error (bad query, missing file)
- `2` parse error (only in `--strict`)
- `3` internal error
- `130` interrupted (SIGINT)

---

## 11. Configuration

A `.coderadar.toml` in the project root (or the path given to `--config`):

```toml
[project]
languages = ["python"]
roots     = ["src/", "tests/"]
exclude   = ["**/migrations/**", "**/__pycache__/**", "**/.venv/**"]

[python]
# Source roots for module resolution (analog to sys.path entries)
sys_path = ["src/"]
# Honor `if TYPE_CHECKING:` imports during call resolution
follow_type_checking_imports = false
# Treat string-literal `from x import *` exposing only __all__ explicitly
strict_wildcard_imports = true
# Recognize these as known decorators (in addition to the builtin table)
extra_known_decorators = [
    { name = "myapp.cache",       effect = "cached_property" },
    { name = "myapp.deprecated",  effect = "warn" },
]

[typescript]
tsconfig = "tsconfig.json"               # if present, used for paths/baseUrl
node_modules = true                      # walk node_modules for resolution

[resolution]
treat_unresolved_calls_as_errors = false # in strict mode

[performance]
worker_threads        = 4                # parse parallelism for batch updates
debounce_ms           = 50               # watcher debounce
query_check_interval  = 64               # signal/cancel check frequency

[output]
snapshot_path = "./.coderadar/snapshot.bin"
journal_path  = "./.coderadar/wal.log"   # absent → no journal
```

All values have defaults; the file is optional. Defaults are loaded from `src/config/defaults.rs`.

---

## 12. Watch Mode

The watcher uses `notify` (Rust crate) for cross-platform file events.

### 12.1 Event Pipeline

```
fs events ──► notify ──► debounce (50ms window) ──► dedupe ──► dispatch
                                                              │
                            ┌─────────────────────────────────┘
                            │
                            ▼
                  parse (parallel, rayon) ──► commit (serial, MPSC to committer)
                            │
                            ▼
                       UpdateReport ──► subscribers
```

### 12.2 Subscription API

```python
import coderadar

# Synchronous iteration
with coderadar.watch("src/") as w:
    for report in w:
        print(report.affected_files)

# Callback
def on_update(report):
    print("Got update:", report.elapsed_ms)
handle = coderadar.watch_async("src/", callback=on_update)
# ...
handle.stop()
```

CLI form emits JSONL on stdout:

```
$ coderadar watch src/
{"event":"update","file":"src/a.py","elapsed_ms":12.4,"affected":3,"epoch":42}
{"event":"update","file":"src/b.py","elapsed_ms":8.1,"affected":1,"epoch":43}
{"event":"taint_rejected","file":"src/c.py","reason":"syntax errors > 5%","epoch":43}
```

### 12.3 Debouncing and Coalescing

- Events for the same file within `debounce_ms` are coalesced into a single update.
- Bursts (e.g., a `git checkout`) that touch many files are batched into a single `Batch` transaction with one epoch bump. The threshold is 10 files within 100ms; this is configurable.

### 12.4 External Mutation Safety

The watcher and the API can both trigger updates. The committer is single-threaded (MPSC consumer), so updates are serialized. There is no "mid-update read" hazard because the snapshot isolation in [§8.1](#81-snapshot-isolation-via-arc-swap-and-epochs) is independent of who is committing.

If the LLM writes a file while the watcher is reading it, the watcher may parse a partial file. This is handled by [§4.7](#47-parse-quality-and-tainted-symbols): the update is likely tainted and rejected. The next file event triggers a fresh re-parse.

---

## 13. Persistence and Snapshots

### 13.1 Formats

| Format | Use case | Speed | Size |
|---|---|---|---|
| `bin`  | Default; fast load/save | ~10× faster than JSON | Smallest |
| `json` | Human-readable, debugging | Slow | ~3× binary |
| `yaml` | As JSON but YAML for tooling | Slowest | ~4× binary |

Binary format: `postcard` (compact, schema-driven). JSON/YAML: serde with a stable representation.

### 13.2 Snapshot Contents

```rust
struct Snapshot {
    schema_version: u32,                 // bumps on incompatible changes
    coderadar_version: String,
    grammar_versions: HashMap<Language, String>, // tree-sitter grammar hash
    config: GraphConfig,
    arenas: ArenaSnapshot,
    indexes: IndexSnapshot,
    resolution_cache: Option<ResolutionCache>, // omitted in minimal snapshots
    file_versions: HashMap<PathBuf, u64>,
    file_mtimes:   HashMap<PathBuf, SystemTime>, // for change detection on load
    epoch: u64,
}
```

### 13.3 Load Behavior

1. Verify `schema_version`. Mismatch → refuse and suggest re-running `init`.
2. Verify `grammar_versions`. Mismatch on any language → refuse for that language's files; re-parse them.
3. For each file in `file_mtimes`: if the file's current mtime differs, mark for re-analysis.
4. After loading, run incremental update for marked files. Report this in `LoadReport`.

### 13.4 Snapshot Memory Cost

Snapshots roughly double memory transiently (in-memory graph + snapshot being serialized). The binary path can be made streaming (write arena-by-arena) at the cost of complexity; deferred to a later phase.

---

## 14. Visualizers

Visualizers consume query results and emit graph descriptions. They live in the Python layer.

### 14.1 Class Hierarchy (Mermaid)

```python
coderadar.visualize.class_hierarchy(
    graph,
    root="myapp.models.Base",
    max_depth=3,
    output="mermaid",   # or "graphviz"
)
```

Output (Mermaid):
```
classDiagram
    Base <|-- User
    Base <|-- Product
    User <|-- AdminUser
```

Driving query: `classes where "Base" in mro_names limit N`, then a BFS from `Base` over `subclasses`.

### 14.2 Module Dependency Graph (Graphviz)

```python
coderadar.visualize.module_dependencies(
    graph,
    scope="myapp.services",       # restrict to a subtree of modules
    output="graphviz",
    layout="dot",
    highlight_cycles=True,
)
```

Driving data: `importers` and the dual `imports` table on each module. Cycle detection via Tarjan SCC.

### 14.3 Call Graph for a Function

```python
coderadar.visualize.call_graph(
    graph,
    root="myapp.services.UserService.create",
    direction="callees",          # or "callers"
    max_depth=4,
    output="mermaid",
)
```

Driving data: BFS over `callees_by_caller` (or `callers_by_callee` for the inverse direction).

### 14.4 Output Adapters

Each visualizer returns a `VisualizerResult` containing both the source (Mermaid/DOT) and a renderable representation. CLI form (`coderadar visualize ...`) writes to stdout or `--output <path>`.

---

## 15. Build and Distribution

- **Build tool:** `maturin` (PEP 517 backend).
- **Rust crates:**
  - `tree-sitter`, `tree-sitter-python`, `tree-sitter-typescript`
  - `slotmap`, `petgraph`
  - `pest`, `pest_derive`
  - `rayon`, `parking_lot`, `arc-swap`
  - `notify` (watcher)
  - `pyo3` with `extension-module` and `abi3-py39`
  - `postcard`, `serde`, `serde_json`, `serde_yaml`
  - `criterion` (dev-only)
  - `proptest`, `insta` (dev-only)
- **Python deps:** `pydantic>=2.0`, `click`, `rich` (for pretty printing in CLI).
- **Wheel matrix:** Linux x86_64, Linux aarch64, macOS x86_64, macOS arm64, Windows x86_64. Python 3.9–3.13 (abi3, so one wheel per platform).

CI builds wheels via `cibuildwheel`; publishes to PyPI on tagged releases.

---

## 16. Performance Targets and Benchmarking

### 16.1 Targets

| Metric | Target |
|---|---|
| Codebase scale | 5,000 files / ~1M LOC |
| Initial analysis (cold) | < 30s on 8-core machine |
| Single-file update p50 | < 30ms |
| Single-file update p95 | < 100ms |
| Single-file update p99 | < 250ms |
| Query latency (streaming, simple where) | < 5ms to first result |
| Query latency (aggregated, full scan) | < 100ms |
| Memory (steady-state) | < 500MB |
| Memory during snapshot save | < 1GB |

### 16.2 Memory Budget Breakdown (5k files, 1M LOC)

| Component | Estimate |
|---|---|
| Source text (kept for re-parse) | 40 MB |
| Tree-sitter trees | 120 MB (cached) or 0 (dropped after extraction) |
| Entity `Arc` arenas | 60 MB |
| Reverse indexes | 40 MB |
| Resolution cache | 40 MB |
| Slack / allocator overhead | 30 MB |
| **Total (trees kept)** | **~330 MB** |
| **Total (trees dropped)** | **~210 MB** |

Phase 1 default: **drop trees after extraction**; re-parse on update. Trees can be retained via `[performance] keep_trees = true` for environments that prioritize update latency over memory.

### 16.3 Reference Codebases for Benchmarking

Pinned to specific commits for reproducibility:

| Codebase | Language | LOC | Files | Pin |
|---|---|---|---|---|
| Django | Python | 460k | 2,400 | `4.2.7` |
| FastAPI | Python | 60k | 350 | `0.110.0` |
| Pydantic | Python | 80k | 250 | `2.6.0` |
| TypeScript compiler self-host | TS | 2.5M | 1,500 | `5.4.0` |
| (Internal repo placeholder) | Python | ~600k | ~3,000 | — |

### 16.4 Methodology

- **Rust micro-benchmarks** via `criterion`. Each commit runs benches; CI fails on >5% regression vs. main.
- **End-to-end benches** in Python (`pytest-benchmark`):
  - Initial analysis of each reference codebase.
  - 1000 single-file updates randomly sampled from the codebase (warm-up: 50 dummy updates).
  - 100 aggregated queries spanning all entities.
- Results published to a static benchmarks site on each release.

---

## 17. Testing Strategy

### 17.1 Test Levels

| Level | Framework | Coverage target |
|---|---|---|
| Unit tests (Rust) | `cargo test` | ≥85% line coverage |
| Integration tests (Rust) | `cargo test --test integration_*` | All extraction queries, all resolution rules |
| Property tests | `proptest` | Diff/patch round-trip, MRO consistency, snapshot round-trip |
| Snapshot tests | `insta` | Extraction outputs on fixture files |
| Fuzz tests | `cargo-fuzz` (libfuzzer) | Pest grammar parsing, tree-sitter inputs |
| Python tests | `pytest` | All public API surface |
| End-to-end | `pytest` against reference codebases | Initial analysis correctness, watch-mode events |

### 17.2 Property Tests (Critical)

The most important correctness property: **incremental updates produce the same graph as a full re-analysis.**

```rust
proptest! {
    #[test]
    fn incremental_matches_full(edits in arbitrary_edit_sequence(...)) {
        let mut graph_incremental = analyze(initial_fixture());
        let mut current_fs = initial_fixture();

        for edit in &edits {
            apply_edit_to_fs(&mut current_fs, edit);
            graph_incremental.update_file(&edit.path).unwrap();
        }

        let graph_full = analyze_in_memory(&current_fs);
        assert_graphs_equivalent(&graph_incremental, &graph_full);
    }
}
```

Two graphs are equivalent iff their canonicalized snapshots are byte-equal. Canonicalization sorts entries by `(file, line, qualified_name)` and remaps SlotMap keys to consecutive integers.

### 17.3 Snapshot Tests for Extraction

For each language, a directory of fixture files; each file's expected `ExtractedUnit`s are stored in `__snapshots__/`. Reviewers approve changes via `cargo insta review`.

### 17.4 Fuzzing Targets

- `fuzz_pest_grammar` — random query strings; must not panic, must produce either an `Ok(Query)` or a structured `Err`.
- `fuzz_extraction_python` — random Python-shaped bytes; must not panic during tagging or walking.
- `fuzz_diff` — random pairs of `Vec<ExtractedUnit>`; must produce a valid patch that, when applied, yields the second from the first.

### 17.5 Performance Regression Tests

CI runs the reference-codebase benches on every PR; results compared against the base branch. Regressions >5% on any p50/p95 metric block merge until acknowledged.

---

## 18. Multi-Language Considerations

Even though Phase 1 ships only Python, the API and data model must already accommodate a multi-language graph.

### 18.1 Language Tagging

Every `Module`, `Class`, `Function`, `Import` carries `language: Language`. Queries support filtering: `classes where language == "python"`.

### 18.2 Cross-Language Edges

When a TypeScript file imports a generated `.d.ts` from a Python pipeline, or when a Python file `subprocess`-calls a JS tool, the resolver does **not** attempt to cross language boundaries. Such references resolve to `External { distribution: None }` with a `cross_language: true` flag.

### 18.3 Language-Specific Fields

Some attributes are language-specific:
- Python: `decorators`, `is_async`, `is_generator`, `kind ∈ {Method, StaticMethod, ClassMethod, Property, ...}`.
- TypeScript: `is_abstract`, `is_readonly`, `access_modifier ∈ {public, private, protected}`.

These are stored on the entity. The Python wrapper exposes them via `entity.lang_fields` (a dict) when not part of the base interface, so code that only cares about cross-language properties stays clean.

### 18.4 Adding a New Language

Checklist:
1. Add `tree-sitter-<lang>` to Cargo.toml.
2. Write `queries/<lang>.scm` with the standard capture names.
3. Implement `language/<lang>/walker.rs` (extends the generic walker with lang-specific decorator/import handling).
4. Implement `language/<lang>/resolver.rs` with the lang's name-resolution rules.
5. Define FileType mapping (Impl/Stub extensions) for this language — e.g., Python: Impl=.py, Stub=.pyi; TypeScript: Impl=.ts/.js, Stub=.d.ts.
6. Add fixture files under `tests/fixtures/<lang>/`.
7. Add a section to `docs/query-fields.md` documenting any language-specific fields.

---

## 19. Out of Scope

This list is explicit so users and contributors know what CodeRadar will not do.

| Out of scope | Why / What to use instead |
|---|---|
| Type inference | A full type inferencer is a separate project (jedi, pyright, ty). CodeRadar consumes type *annotations* as strings but does not infer types. |
| Runtime behavior | We don't execute code. `eval`, `exec`, metaclasses with `__init_subclass__` side effects: not modeled. |
| Language Server Protocol | Out of scope; a separate project can build an LSP on top of the API. |
| Semantic refactoring | We map structure; we don't refactor. Refactoring engines can consume our output. |
| Cross-language type bridges | TS↔Python type sharing: not attempted. |
| Build-system parsing | We read `[project] roots` from our own config or `tsconfig.paths`. We don't parse `setup.py`, `Bazel BUILD`, etc. |
| Git history / blame | The graph is a snapshot of the current tree. Git integration could be a downstream tool. |
| Code style / linting | Not a linter. |
| Security analysis | Not a SAST tool. Symbols are not classified by taint. |
| Code metrics beyond counts | Complexity metrics (cyclomatic, cognitive) are not computed. Function `line_count` is exposed; users can compute their own metrics from queries. |

---

## 20. Open Questions

Items left intentionally undecided, to be resolved before/during implementation:

1. **Wildcard import precision.** Should we attempt to follow `from x import *` through multiple hops when intermediate `__all__`s are all present? Performance vs. completeness trade-off.
2. **Stub file support.** Python `.pyi` files contain typing-only declarations. Core merge strategy accepted (stub-first, overlay impl; single namespace with FileType provenance). All follow-up issues resolved:
   - 2a. `__all__` conflict resolution: RESOLVED — Two-field approach (`exports` union-with-provenance + `star_exports` impl-precedence) correctly handles explicit vs. star import semantics (§5.3.4).
   - 2b. Module-level constructs in stubs: RESOLVED — Added `Constant` and `TypeAlias` entities with source provenance (§3.2).
3. **Conditional definitions.** RESOLVED (§20.3):
   - **TYPE_CHECKING blocks** (Pattern B): Extract with `is_type_checking_only: bool` flag on Class/Function entities, defaulting to `false`. Mirrors the existing `Import { type_only: true }` pattern (§5.3.1). Consumers filter via this flag for precise queries.
   - **Version-gated / platform-specific conditionals** (Patterns A & C): Extract all branches unconditionally in v1 — CodeRadar performs static, name-based analysis with no runtime context (§1.1). Produces duplicate entries when different code paths define the same symbol; diff uses position-based matching for identity collisions (§5.2).
   - **Overload chains** (Pattern D): Handled by signature hash disambiguation (§5.2). Identical-signature overloads fall back to position-based matching — a known limitation resolved by Phase 4's similarity-based rename detection.
   - **Phase 4 upgrade path:** Similarity-based rename detection (Myers diff on body hashes) will correctly identify version-gated duplicates as renames rather than remove+insert pairs. Decorator list can be part of the similarity scoring in Phase 4 without requiring v1 complexity.
4. **Plugin API.** Should third parties be able to register extraction passes or known-decorator handlers via a Python entry-point? Useful for frameworks (Django, SQLAlchemy) that synthesize members.
5. **Distributed snapshots.** For mono-repos, sharding the graph across processes would allow more parallelism. Out of scope for v1; needs a separate design.

---

*End of specification.*
