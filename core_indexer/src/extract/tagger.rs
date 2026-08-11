// CodeRadar v3.5 — Extraction: Tagger Pass 1 (§4.2)
// Tree-sitter .scm queries tag nodes with coarse classifications.
// v3.5a: Pre-compiled query + pre-indexed capture names.

use std::collections::HashMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

use crate::types::{Language, Tag, TagInfo, TaggedTree};

/// Compiled query + pre-indexed capture name → Tag mapping for reuse across files.
pub struct CompiledQuery {
    pub query: Query,
    /// capture_index → Option<Tag>, precomputed once.
    pub capture_tags: Vec<Option<Tag>>,
}

impl CompiledQuery {
    pub fn new(language: Language, ts_lang: &tree_sitter::Language) -> Option<Self> {
        let query_source = get_query_for_language_src(language);
        let query = match Query::new(ts_lang, query_source) {
            Ok(q) => q,
            Err(e) => {
                eprintln!("Tree-sitter query compile error for {:?}: {} — using fallback", language, e);
                let fallback = "(comment) @docstring\n";
                Query::new(ts_lang, fallback)
                    .unwrap_or_else(|_| Query::new(ts_lang, "(ERROR) @_none").unwrap())
            }
        };
        let capture_count = query.capture_names().len();
        let mut capture_tags: Vec<Option<Tag>> = Vec::with_capacity(capture_count);
        for i in 0..capture_count {
            let name = query.capture_names()[i];
            capture_tags.push(capture_name_to_tag(name));
        }
        Some(CompiledQuery { query, capture_tags })
    }
}

/// Run tree-sitter queries against a parsed source file to produce a TaggedTree.
/// Accepts a pre-compiled query — callers should compile once per language.
pub fn tag_tree<'a>(
    source: &'a str,
    root_node: tree_sitter::Node,
    compiled: &CompiledQuery,
) -> TaggedTree<'a> {
    let mut cursor = QueryCursor::new();
    let mut tags: HashMap<usize, TagInfo> = HashMap::new();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&compiled.query, root_node, source_bytes);
    while let Some((qm, _idx)) = captures.next() {
        for capture in qm.captures {
            let idx = capture.index as usize;
            if idx < compiled.capture_tags.len() {
                if let Some(ref tag) = compiled.capture_tags[idx] {
                    tags.insert(
                        capture.node.id() as usize,
                        TagInfo { tag: *tag },
                    );
                }
            }
        }
    }

    TaggedTree { source, tags }
}

/// Map capture name from .scm file to Tag enum.
fn capture_name_to_tag(name: &str) -> Option<Tag> {
    match name {
        "class" | "class.def" => Some(Tag::Class),
        "class_base" => Some(Tag::ClassBase),
        "function" | "function.def" | "function.arrow" => Some(Tag::Function),
        "function_param" | "function.params" => Some(Tag::FunctionParam),
        "function_return" | "function.return" => Some(Tag::FunctionReturn),
        "import" | "import.module" | "import_from" => Some(Tag::Import),
        "import_specifier" | "import_from.alias" => Some(Tag::ImportSpecifier),
        "impl" => Some(Tag::Impl),
        "call" => Some(Tag::Call),
        "call_receiver" | "call.receiver" | "call.method" => Some(Tag::CallReceiver),
        "decorator" => Some(Tag::Decorator),
        "docstring" => Some(Tag::Docstring),
        "field" => Some(Tag::Field),
        "export" | "export.function" | "export.class" | "export.module" => Some(Tag::Export),
        _ => None,
    }
}

/// Return the appropriate .scm query source for a language.
pub fn get_query_for_language_src(language: Language) -> &'static str {
    match language {
        Language::Python => include_str!("../../queries/python.scm"),
        Language::TypeScript => include_str!("../../queries/typescript.scm"),
        Language::JavaScript => include_str!("../../queries/javascript.scm"),
        Language::Rust => include_str!("../../queries/rust.scm"),
        Language::Go => include_str!("../../queries/go.scm"),
        Language::Java => include_str!("../../queries/java.scm"),
        Language::C => include_str!("../../queries/c.scm"),
        Language::Cpp => include_str!("../../queries/cpp.scm"),
        Language::Ruby => include_str!("../../queries/ruby.scm"),
        Language::Php => include_str!("../../queries/php.scm"),
        Language::CSharp => include_str!("../../queries/csharp.scm"),
        Language::Kotlin => include_str!("../../queries/kotlin.scm"),
        Language::Swift => include_str!("../../queries/swift.scm"),
        Language::Scala => include_str!("../../queries/scala.scm"),
        Language::Lua => include_str!("../../queries/lua.scm"),
        Language::Elixir => include_str!("../../queries/elixir.scm"),
        Language::Zig => include_str!("../../queries/zig.scm"),
        Language::R => include_str!("../../queries/r.scm"),
        Language::Bash => include_str!("../../queries/bash.scm"),
        Language::Dart => include_str!("../../queries/dart.scm"),
        Language::Protobuf => include_str!("../../queries/protobuf.scm"),
        Language::Dockerfile => include_str!("../../queries/dockerfile.scm"),
        Language::Sql => include_str!("../../queries/sql.scm"),
        Language::Hcl => include_str!("../../queries/hcl.scm"),
        Language::Cmake => include_str!("../../queries/cmake.scm"),
        Language::Graphql => include_str!("../../queries/graphql.scm"),
        Language::Erlang => include_str!("../../queries/erlang.scm"),
        Language::Haskell => include_str!("../../queries/haskell.scm"),
        Language::Nix => include_str!("../../queries/nix.scm"),
        Language::Shell => include_str!("../../queries/shell.scm"),
        Language::Groovy => include_str!("../../queries/groovy.scm"),
        Language::Perl => include_str!("../../queries/perl.scm"),
        Language::SystemVerilog => include_str!("../../queries/systemverilog.scm"),
        Language::Ocaml => include_str!("../../queries/ocaml.scm"),
        Language::Clojure => include_str!("../../queries/clojure.scm"),
        Language::Fsharp => include_str!("../../queries/fsharp.scm"),
        Language::Verilog => include_str!("../../queries/verilog.scm"),
        Language::Julia => include_str!("../../queries/julia.scm"),
        Language::Powershell => include_str!("../../queries/powershell.scm"),
        Language::EmacsLisp => include_str!("../../queries/elisp.scm"),
        Language::Objc => include_str!("../../queries/objc.scm"),
        Language::OtherTen => r#"(identifier) @id"#, // fallback
    }
}
