// CodeRadar v3.3 — Extraction: Tagger Pass 1 (§4.2)
// Tree-sitter .scm queries tag nodes with coarse classifications.

use std::collections::HashMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

use crate::types::{Language, Tag, TagInfo, TaggedTree};

/// Run tree-sitter queries against a parsed source file to produce a TaggedTree.
/// The caller handles parsing — tag_tree only runs queries.
pub fn tag_tree<'a>(
    source: &'a str,
    root_node: tree_sitter::Node,
    language: Language,
    ts_lang: tree_sitter::Language,
) -> TaggedTree<'a> {
    let query_source = get_query_for_language_src(language);
    let query = Query::new(&ts_lang, query_source)
        .unwrap_or_else(|e| {
            eprintln!("Tree-sitter query compile error for {:?}: {} — using fallback", language, e);
            // Universal fallback: matches nothing gracefully
            let fallback = "(comment) @docstring\n";
            Query::new(&ts_lang, fallback).unwrap_or_else(|_| {
                // Truly empty query — some grammars don't even have comment nodes
                Query::new(&ts_lang, "(ERROR) @_none").unwrap()
            })
        });

    let mut cursor = QueryCursor::new();
    let mut tags: HashMap<usize, TagInfo> = HashMap::new();

    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);
    while let Some((qm, _idx)) = captures.next() {
        for capture in qm.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if let Some(tag) = capture_name_to_tag(capture_name) {
                tags.insert(
                    capture.node.id() as usize,
                    TagInfo {
                        tag,
                        capture_name: capture_name.to_string(),
                    },
                );
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
        Language::OtherTen => r#"(identifier) @id"#, // fallback
    }
}
