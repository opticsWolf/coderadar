// CodeRadar v3.3 — Core Types & Enums
// §3 Data Models, §3.2 Core Entities, §3.3 Supporting Types, §3.3a Extraction Intermediates

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use slotmap::{new_key_type, SlotMap};

// ── SlotMap Key Types (§3.1) ────────────────────────────────────────────────

new_key_type! { pub struct ModuleId; }
new_key_type! { pub struct ClassId; }

impl ClassId {
    /// Placeholder ClassId for the walker's FrameKind — patched after arena insert.
    pub const fn null() -> Self {
        ClassId(slotmap::KeyData::from_ffi(0))
    }
}
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

// ── Quality / FileType / SourceType ─────────────────────────────────────────

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ParseQuality {
    Clean,
    Partial,
    Tainted,
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
    DataclassSynthesized { from_class: ClassId },
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
    Class(ClassId),
    External { name: String },
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
        from: ModuleId,
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
    Module(ModuleId),
    Symbol(SymbolId),
    Wildcard {
        module: ModuleId,
        exposed: Vec<String>,
    },
    Dynamic,
    External {
        distribution: Option<String>,
    },
}

// ── UnresolvedRef / ResolvedCall (§3.3) ─────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct UnresolvedRef {
    pub name: String,
    pub path: Vec<String>,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ResolvedCall {
    Function(FunctionId),
    Method {
        receiver: ReceiverShape,
        method: FunctionId,
    },
    Constructor(ClassId),
    Builtin(String),
    Unresolved {
        reason: UnresolvedReason,
        raw: UnresolvedRef,
    },
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ReceiverShape {
    SelfRef,
    ClassRef(ClassId),
    ModuleRef(ModuleId),
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
}

// ── Core Entities (§3.2) ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Module {
    pub name: String,
    pub path: PathBuf,
    pub language: Language,
    pub package: Option<ModuleId>,
    pub exports: Vec<Export>,
    pub star_exports: Option<Vec<String>>,
    pub classes: Vec<ClassId>,
    pub functions: Vec<FunctionId>,
    pub imports: Vec<ImportId>,
    pub constants: Vec<ConstantId>,
    pub type_aliases: Vec<TypeAliasId>,
    pub parse_quality: ParseQuality,
    pub file_version: u64,
    pub content_hash: u64,
}

#[derive(Clone, Debug)]
pub struct Class {
    pub name: String,
    pub parent_module: ModuleId,
    pub parent_class: Option<ClassId>,
    pub bases: Vec<UnresolvedRef>,
    pub resolved_bases: Vec<ClassId>,
    pub mro: Vec<MroNode>,
    pub mro_error: bool,
    pub methods: Vec<FunctionId>,
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
    pub name: String,
    pub parent_module: ModuleId,
    pub parent_class: Option<ClassId>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub calls: Vec<UnresolvedRef>,
    pub resolved_calls: Vec<ResolvedCall>,
    pub decorators: Vec<String>,
    pub setter_of: Option<FunctionId>,
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
    pub raw: String,
    pub kind: ImportKind,
    pub resolution: ImportResolution,
    pub line: usize,
    pub is_type_only: bool,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct Constant {
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct TypeAlias {
    pub name: String,
    pub target: String,
    pub source: SourceType,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

// ── Extraction Intermediate Types (§3.3a) ───────────────────────────────────

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
    pub name: String,
    pub path: PathBuf,
    pub language: Language,
    pub parse_quality: ParseQuality,
    pub content_hash: u64,
}

#[derive(Clone, Debug)]
pub struct ExtractedClass {
    pub name: String,
    pub qualified_name: String,
    pub parent_module: Option<ModuleId>,
    pub parent_class: Option<ClassId>,
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
    pub name: String,
    pub qualified_name: String,
    pub parent_module: Option<ModuleId>,
    pub parent_class: Option<ClassId>,
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
    pub name: String,
    pub annotation: Option<String>,
    pub source: SourceType,
    pub default_value: Option<String>,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct ExtractedTypeAlias {
    pub name: String,
    pub target: String,
    pub source: SourceType,
    pub span: ByteSpan,
    pub name_span: ByteSpan,
}

// ── TaggedTree / TagInfo (§3.3a, §4.1) ──────────────────────────────────────

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

// ── Walker Frame (§3.3a) ────────────────────────────────────────────────────

pub enum FrameKind {
    Module,
    Class(ClassId),
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
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ByteSpan ──────────────────────────────────────────────────────

    #[test]
    fn test_byte_span_len() {
        let span = ByteSpan { start: 10, end: 25 };
        assert_eq!(span.len(), 15);
        assert!(!span.is_empty());
    }

    #[test]
    fn test_byte_span_empty() {
        let span = ByteSpan { start: 0, end: 0 };
        assert!(span.is_empty());
        assert_eq!(span.len(), 0);
    }

    #[test]
    fn test_slice_span_valid() {
        let src = "hello world";
        let span = ByteSpan { start: 0, end: 5 };
        let result = slice_span(src, span).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_slice_span_out_of_bounds() {
        let src = "hi";
        let span = ByteSpan { start: 0, end: 10 };
        assert!(slice_span(src, span).is_err());
    }

    // ── Language ──────────────────────────────────────────────────────

    #[test]
    fn test_from_extension_python() {
        assert_eq!(Language::from_extension("py"), Language::Python);
        assert_eq!(Language::from_extension("pyi"), Language::Python);
    }

    #[test]
    fn test_from_extension_typescript() {
        assert_eq!(Language::from_extension("ts"), Language::TypeScript);
        assert_eq!(Language::from_extension("tsx"), Language::TypeScript);
    }

    #[test]
    fn test_from_extension_unknown() {
        assert_eq!(Language::from_extension("exoticlang"), Language::OtherTen);
    }

    #[test]
    fn test_language_tier() {
        assert_eq!(Language::Python.tier(), 1);
        assert_eq!(Language::Rust.tier(), 1);
        assert_eq!(Language::OtherTen.tier(), 2);
    }

    #[test]
    fn test_language_as_str() {
        assert_eq!(Language::Python.as_str(), "python");
        assert_eq!(Language::Go.as_str(), "go");
        assert_eq!(Language::OtherTen.as_str(), "other");
    }

    // ── ClassId null ──────────────────────────────────────────────────

    #[test]
    fn test_classid_null() {
        let null_id = ClassId::null();
        // null is a sentinel placeholder — two nulls are equal (same zero key)
        assert_eq!(null_id, ClassId::null());
    }

    // ── FunctionKind derivation ───────────────────────────────────────

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
    fn test_derive_function_kind_property() {
        let kind = crate::extract::walker::derive_function_kind(
            &["@property".into()], true);
        assert_eq!(kind, FunctionKind::Property);
    }

    // ── Decorator classification ──────────────────────────────────────

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

    #[test]
    fn test_is_abstract_decorator() {
        assert!(crate::extract::decorators::is_abstract_decorator("@abstractmethod"));
        assert!(!crate::extract::decorators::is_abstract_decorator("@property"));
    }

    #[test]
    fn test_is_dataclass_decorator() {
        assert!(crate::extract::decorators::is_dataclass_decorator("@dataclass"));
        assert!(crate::extract::decorators::is_dataclass_decorator("@dataclass()"));
        assert!(!crate::extract::decorators::is_dataclass_decorator("@property"));
    }
}
