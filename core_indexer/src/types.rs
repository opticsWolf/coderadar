// CodeRadar v3.5 — Core Types & Enums
// §3 Data Models — EntityId-based identity, Macrame-backed persistence, ProjectedGraph.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

// ── EntityId (v3.5 §3.1) ────────────────────────────────────────────────────
// Stable dotted-path identity, e.g. "src/auth.py::UserService.create".
// Used as Macrame concept IDs and ProjectedGraph hashmap keys.

pub type EntityId = String;

/// Typed wrappers for compile-time safety within Rust — all backed by EntityId.
#[derive(Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct ModuleKey(pub EntityId);

#[derive(Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct ClassKey(pub EntityId);

impl ClassKey {
    /// Placeholder for walker FrameKind — patched after arena insert.
    pub fn null() -> Self {
        ClassKey(String::new())
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct FunctionKey(pub EntityId);

#[derive(Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct ImportKey(pub EntityId);

#[derive(Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct ConstantKey(pub EntityId);

#[derive(Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct TypeAliasKey(pub EntityId);

// Convenience conversions
impl From<EntityId> for ModuleKey     { fn from(id: EntityId) -> Self { ModuleKey(id) } }
impl From<EntityId> for ClassKey      { fn from(id: EntityId) -> Self { ClassKey(id) } }
impl From<EntityId> for FunctionKey   { fn from(id: EntityId) -> Self { FunctionKey(id) } }
impl From<EntityId> for ImportKey     { fn from(id: EntityId) -> Self { ImportKey(id) } }
impl From<EntityId> for ConstantKey   { fn from(id: EntityId) -> Self { ConstantKey(id) } }
impl From<EntityId> for TypeAliasKey  { fn from(id: EntityId) -> Self { TypeAliasKey(id) } }

impl ModuleKey    { pub fn as_str(&self) -> &str { &self.0 } }
impl ClassKey     { pub fn as_str(&self) -> &str { &self.0 } }
impl FunctionKey  { pub fn as_str(&self) -> &str { &self.0 } }
impl ImportKey    { pub fn as_str(&self) -> &str { &self.0 } }
impl ConstantKey  { pub fn as_str(&self) -> &str { &self.0 } }
impl TypeAliasKey { pub fn as_str(&self) -> &str { &self.0 } }

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum SymbolId {
    Module(EntityId),
    Class(EntityId),
    Function(EntityId),
    Import(EntityId),
}

// ── ByteSpan (§3.3) ─────────────────────────────────────────────────────────

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize, // exclusive
}

impl ByteSpan {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Slice a source string by a ByteSpan, verifying UTF-8 char boundaries.
pub fn slice_span<'a>(source: &'a str, span: ByteSpan) -> Result<&'a str, SpanError> {
    if span.start > source.len() || span.end > source.len() {
        return Err(SpanError::OutOfBounds);
    }
    if !source.is_char_boundary(span.start) || !source.is_char_boundary(span.end) {
        return Err(SpanError::CharBoundary);
    }
    Ok(&source[span.start..span.end])
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpanError {
    OutOfBounds,
    CharBoundary,
}

// ── Language Enum (§3.3) ─────────────────────────────────────────────────────

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub enum Language {
    Python,
    TypeScript,
    JavaScript,
    Go,
    Rust,
    Java,
    C,
    Cpp,
    Ruby,
    Php,
    CSharp,
    Kotlin,
    OtherTen, // canonical lowercase name for Tier 2/3 languages
}

impl Language {
    pub fn from_extension(ext: &str) -> Language {
        match ext {
            "py" | "pyi" => Language::Python,
            "ts" | "tsx" => Language::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "go" => Language::Go,
            "rs" => Language::Rust,
            "java" => Language::Java,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Language::Cpp,
            "rb" => Language::Ruby,
            "php" => Language::Php,
            "cs" => Language::CSharp,
            "kt" | "kts" => Language::Kotlin,
            _ => Language::OtherTen,
        }
    }

    pub fn tier(&self) -> u8 {
        match self {
            Language::Python
            | Language::TypeScript
            | Language::JavaScript
            | Language::Rust
            | Language::Go
            | Language::Java
            | Language::C
            | Language::Cpp
            | Language::Ruby
            | Language::Php
            | Language::CSharp
            | Language::Kotlin => 1,
            Language::OtherTen => 2,
        }
    }

    pub fn parser_crate(&self) -> &'static str {
        match self {
            Language::Python => "tree-sitter-python",
            Language::TypeScript => "tree-sitter-typescript",
            Language::JavaScript => "tree-sitter-javascript",
            Language::Rust => "tree-sitter-rust",
            Language::Go => "tree-sitter-go",
            Language::Java => "tree-sitter-java",
            Language::C => "tree-sitter-c",
            Language::Cpp => "tree-sitter-cpp",
            Language::Ruby => "tree-sitter-ruby",
            Language::Php => "tree-sitter-php",
            Language::CSharp => "tree-sitter-c-sharp",
            Language::Kotlin => "tree-sitter-kotlin",
            Language::OtherTen => "",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Ruby => "ruby",
            Language::Php => "php",
            Language::CSharp => "csharp",
            Language::Kotlin => "kotlin",
            Language::OtherTen => "other",
        }
    }
}

// ── Quality / FileType / SourceType (§4.5, §4.5a) ──────────────────────────

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ParseQuality {
    Clean,
    Partial,
    Tainted,
    Deferred,   // v3.5 §4.5a: routed to recovery extractor
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum FileType {
    Impl,
    Stub,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum SourceType {
    Impl,
    Stub,
}

// ── FunctionKind (§3.3) ─────────────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
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
    DataclassSynthesized { from_class: EntityId },
}

// ── EffectiveClass (§3.3) ───────────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum EffectiveClass {
    Plain,
    Dataclass {
        frozen: bool,
        eq: bool,
        order: bool,
    },
    NamedTuple,
    TypedDict {
        total: bool,
    },
    Protocol,
    Enum {
        variant: EnumVariant,
    },
    Abstract,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum EnumVariant {
    Plain,
    IntEnum,
    Flag,
    StrEnum,
    Other(String),
}

// ── MroNode ─────────────────────────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum MroNode {
    Class(EntityId),
    External { name: String },
}

// ── TargetKind (§6.1a) ─────────────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum TargetKind {
    Internal,
    External(String),   // standard library, third-party; always emitted
}

// ── Parameter & Field (§3.3) ────────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Parameter {
    pub name: String,
    pub annotation: Option<String>,
    pub default_value: Option<String>,
    pub is_varargs: bool,
    pub is_kwargs: bool,
    pub is_positional_only: bool,
    pub is_keyword_only: bool,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Field {
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub is_class_var: bool,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

// ── Export ──────────────────────────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Export {
    pub name: String,
    pub source: ExportSource,
    pub file_type: FileType,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ExportSource {
    Local,
    ReExport {
        from: EntityId,
        original_name: String,
    },
}

// ── Import Types (§3.3) ─────────────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ImportKind {
    ModuleImport {
        module: String,
        alias: Option<String>,
    },
    FromImport {
        module: String,
        names: Vec<(String, Option<String>)>,
    },
    RelativeImport {
        level: usize,
        module: Option<String>,
        names: Vec<(String, Option<String>)>,
    },
    StarImport {
        module: String,
    },
    Side {
        module: String,
    },
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ImportResolution {
    Unresolved,
    Module(EntityId),
    Symbol(SymbolId),
    Wildcard {
        module: EntityId,
        exposed: Vec<String>,
    },
    Dynamic,
    External {
        distribution: Option<String>,
    },
}

// ── UnresolvedRef / ResolvedCall (§3.3, §6.1a) ─────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct UnresolvedRef {
    pub name: String,
    pub path: Vec<String>,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ResolvedCall {
    Function(EntityId),
    Method {
        receiver: ReceiverShape,
        method: EntityId,
    },
    Constructor(EntityId),
    Builtin(String),
    External(String),          // v3.5 §6.1a: library / third-party call
    Unresolved {
        reason: UnresolvedReason,
        raw: UnresolvedRef,
    },
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ReceiverShape {
    SelfRef,
    ClassRef(EntityId),
    ModuleRef(EntityId),
    LocalVar,
    Unknown,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum UnresolvedReason {
    NameNotInScope,
    TypeInferenceRequired,
    DynamicImport,
    WildcardImportShadow,
    ParseError,
    IncompleteFlow,            // v3.5 §6.1a: internal dead-end suppressed
}

// ── Core Entities (§3.2) — EntityId-based identity ──────────────────────────

#[derive(Clone, Debug)]
pub struct Module {
    pub id: EntityId,
    pub name: String,
    pub path: PathBuf,
    pub language: Language,
    pub package: Option<EntityId>,
    pub exports: Vec<Export>,
    pub star_exports: Option<Vec<String>>,
    pub classes: Vec<EntityId>,
    pub functions: Vec<EntityId>,
    pub imports: Vec<EntityId>,
    pub constants: Vec<EntityId>,
    pub type_aliases: Vec<EntityId>,
    pub parse_quality: ParseQuality,
    pub file_version: u64,
    pub content_hash: u64,
}

#[derive(Clone, Debug)]
pub struct Class {
    pub id: EntityId,
    pub name: String,
    pub parent_module: EntityId,
    pub parent_class: Option<EntityId>,
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

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct Import {
    pub id: EntityId,
    pub raw: String,
    pub kind: ImportKind,
    pub resolution: ImportResolution,
    pub line: usize,
    pub is_type_only: bool,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct Constant {
    pub id: EntityId,
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct TypeAlias {
    pub id: EntityId,
    pub name: String,
    pub target: String,
    pub source: SourceType,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

// ── Extraction Intermediate Types (§3.3a) — EntityId-based ──────────────────

#[derive(Clone, Debug)]
pub enum ExtractedUnit {
    Module(ExtractedModule),
    Class(ExtractedClass),
    Function(ExtractedFunction),
    Import(ExtractedImport),
    Field(ExtractedField),
    Constant(ExtractedConstant),
    TypeAlias(ExtractedTypeAlias),
}

#[derive(Clone, Debug)]
pub struct ExtractedModule {
    pub id: EntityId,
    pub name: String,
    pub path: PathBuf,
    pub language: Language,
    pub parse_quality: ParseQuality,
    pub content_hash: u64,
}

#[derive(Clone, Debug)]
pub struct ExtractedClass {
    pub id: EntityId,
    pub name: String,
    pub qualified_name: String,
    pub parent_module: EntityId,
    pub parent_class: Option<EntityId>,
    pub bases: Vec<UnresolvedRef>,
    pub decorators: Vec<String>,
    pub docstring: Option<String>,
    pub fields: Vec<ExtractedField>,
    pub line: usize,
    pub exit_line: usize,
    pub source: SourceType,
    pub is_type_checking_only: bool,
    pub parse_quality: ParseQuality,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
    pub body_span: ByteSpan,
    pub decorators_span: Option<ByteSpan>,
}

#[derive(Clone, Debug)]
pub struct ExtractedFunction {
    pub id: EntityId,
    pub name: String,
    pub qualified_name: String,
    pub parent_module: EntityId,
    pub parent_class: Option<EntityId>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub calls: Vec<UnresolvedRef>,
    pub decorators: Vec<String>,
    pub docstring: Option<String>,
    pub kind: FunctionKind,
    pub is_async: bool,
    pub is_generator: bool,
    pub line: usize,
    pub exit_line: usize,
    pub source: SourceType,
    pub is_type_checking_only: bool,
    pub parse_quality: ParseQuality,
    pub signature_hash: u64,
    pub body_hash: u64,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
    pub params_span: ByteSpan,
    pub body_span: ByteSpan,
    pub decorators_span: Option<ByteSpan>,
}

#[derive(Clone, Debug)]
pub struct ExtractedImport {
    pub id: EntityId,
    pub raw: String,
    pub kind: ImportKind,
    pub line: usize,
    pub is_type_only: bool,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct ExtractedField {
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub is_class_var: bool,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct ExtractedConstant {
    pub id: EntityId,
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct ExtractedTypeAlias {
    pub id: EntityId,
    pub name: String,
    pub target: String,
    pub source: SourceType,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

// ── TaggedTree / TagInfo (§4.1) ─────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

pub struct TaggedTree<'a> {
    pub source: &'a str,
    pub tags: HashMap<usize, TagInfo>,
}

pub struct TagInfo {
    pub tag: Tag,
    pub capture_name: String,
}

// ── Walker Frame (§3.3a) — EntityId-based ──────────────────────────────────

pub enum FrameKind {
    Module,
    Class(EntityId),
    Function,
}

impl std::fmt::Debug for FrameKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameKind::Module => write!(f, "Module"),
            FrameKind::Class(_) => write!(f, "Class(...)"),
            FrameKind::Function => write!(f, "Function"),
        }
    }
}

pub struct WalkFrame {
    pub qualified: String,
    pub kind: FrameKind,
}

pub struct WalkContext<'a> {
    pub tags: &'a TaggedTree<'a>,
    pub units: Vec<ExtractedUnit>,
    pub stack: Vec<WalkFrame>,
    pub file_path: String,
}

// ── ProjectedGraph types (§3.4) — used by graph.rs ──────────────────────────

#[derive(Clone)]
pub struct ProjectedGraph {
    pub modules: HashMap<EntityId, Arc<Module>>,
    pub classes: HashMap<EntityId, Arc<Class>>,
    pub functions: HashMap<EntityId, Arc<Function>>,
    pub imports: HashMap<EntityId, Arc<Import>>,
    pub constants: HashMap<EntityId, Arc<Constant>>,
    pub type_aliases: HashMap<EntityId, Arc<TypeAlias>>,

    pub file_to_modules: HashMap<PathBuf, Vec<EntityId>>,
    pub module_by_dotted_name: HashMap<(Language, String), EntityId>,

    pub importers: HashMap<EntityId, BTreeSet<EntityId>>,
    pub callers_by_callee: HashMap<EntityId, BTreeSet<EntityId>>,
    pub callees_by_caller: HashMap<EntityId, BTreeSet<EntityId>>,
    pub subclasses: HashMap<EntityId, BTreeSet<EntityId>>,
    pub overridden_by: HashMap<EntityId, BTreeSet<EntityId>>,
}

// ── ResolvedEdge (§3.4a, §6.1a) — used by resolution engine ─────────────────

// v3.5: Clone and Debug derives needed for resolution engine usage.
#[derive(Clone, Debug)]
pub struct ResolvedEdge {
    pub source_id: EntityId,
    pub target_id: EntityId,
    pub confidence: f32,
    pub method: ResolutionMethod,
    pub kind: ReferenceKind,
    pub line: usize,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
    pub target_kind: TargetKind,
}

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum ResolutionMethod {
    StackGraph,
    ImportConstrained,
    SignatureMatch,
    Embedding,
    Lsp,
}

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum ReferenceKind {
    Call,
    Instantiation,
    Inheritance,
    TypeAnnotation,
    AttributeAccess,
    Import,
}

// ── StagedChange (§6.7) ─────────────────────────────────────────────────────

pub struct StagedChange {
    pub path: String,
    pub entities: Vec<ExtractedUnit>,
    pub edges: Vec<ResolvedEdge>,
    pub unresolved: Vec<(UnresolvedRef, usize)>,
    pub language: Language,
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_span_len() {
        let span = ByteSpan { start: 10, end: 25 };
        assert_eq!(span.len(), 15);
    }

    #[test]
    fn test_byte_span_empty() {
        let span = ByteSpan { start: 0, end: 0 };
        assert!(span.is_empty());
    }

    #[test]
    fn test_slice_span_valid() {
        let src = "hello world";
        let span = ByteSpan { start: 0, end: 5 };
        assert_eq!(slice_span(src, span).unwrap(), "hello");
    }

    #[test]
    fn test_slice_span_out_of_bounds() {
        let span = ByteSpan { start: 0, end: 10 };
        assert!(slice_span("hi", span).is_err());
    }

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("py"), Language::Python);
        assert_eq!(Language::from_extension("ts"), Language::TypeScript);
        assert_eq!(Language::from_extension("exotic"), Language::OtherTen);
    }

    #[test]
    fn test_language_tier() {
        assert_eq!(Language::Python.tier(), 1);
        assert_eq!(Language::OtherTen.tier(), 2);
    }

    #[test]
    fn test_entity_id_wrappers() {
        let id = "src/main.py::main".to_string();
        let mk: ModuleKey = id.clone().into();
        let ck: ClassKey = id.clone().into();
        let fk: FunctionKey = id.clone().into();
        assert_eq!(mk.as_str(), "src/main.py::main");
        assert_eq!(ck.as_str(), "src/main.py::main");
        assert_eq!(fk.as_str(), "src/main.py::main");
    }

    #[test]
    fn test_classkey_null() {
        let null = ClassKey::null();
        assert_eq!(null.0, "");
    }

    #[test]
    fn test_parse_quality_deferred() {
        // v3.5: Deferred is a valid ParseQuality variant
        assert!(matches!(ParseQuality::Deferred, ParseQuality::Deferred));
    }

    #[test]
    fn test_unresolved_reason_incomplete_flow() {
        // v3.5 §6.1a: internal dead-ends suppressed
        assert!(matches!(
            UnresolvedReason::IncompleteFlow,
            UnresolvedReason::IncompleteFlow
        ));
    }

    #[test]
    fn test_resolved_call_external() {
        // v3.5 §6.1a: library calls always emitted
        let call = ResolvedCall::External("requests.post".to_string());
        assert!(matches!(call, ResolvedCall::External(_)));
    }

    #[test]
    fn test_target_kind() {
        assert!(matches!(TargetKind::Internal, TargetKind::Internal));
        let ext = TargetKind::External("numpy.array".into());
        assert!(matches!(ext, TargetKind::External(ref s) if s == "numpy.array"));
    }

    #[test]
    fn test_derive_function_kind_free() {
        let kind = crate::extract::walker::derive_function_kind(&[], false);
        assert_eq!(kind, FunctionKind::Free);
    }

    #[test]
    fn test_derive_function_kind_method() {
        let kind = crate::extract::walker::derive_function_kind(&[], true);
        assert_eq!(kind, FunctionKind::Method);
    }

    #[test]
    fn test_derive_function_kind_static() {
        let kind = crate::extract::walker::derive_function_kind(
            &["@staticmethod".into()], true);
        assert_eq!(kind, FunctionKind::StaticMethod);
    }

    #[test]
    fn test_known_decorator_staticmethod() {
        let effect = crate::extract::decorators::known_decorator_effects("@staticmethod");
        assert!(effect.is_some());
    }

    #[test]
    fn test_known_decorator_unknown() {
        let effect = crate::extract::decorators::known_decorator_effects("@myapp.custom");
        assert!(effect.is_none());
    }
}
