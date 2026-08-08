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
            eprintln!("Tree-sitter query compile error for {:?}: {:?}", language, e);
            panic!("Query compilation failed");
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
        "function" | "function.def" => Some(Tag::Function),
        "function_param" | "function.params" => Some(Tag::FunctionParam),
        "function_return" | "function.return" => Some(Tag::FunctionReturn),
        "import" | "import.module" | "import.name" | "import_from" => Some(Tag::Import),
        "import_specifier" | "import_from.alias" => Some(Tag::ImportSpecifier),
        "impl" => Some(Tag::Impl),
        "call" | "call.name" => Some(Tag::Call),
        "call_receiver" | "call.receiver" | "call.method" => Some(Tag::CallReceiver),
        "decorator" | "decorator.name" => Some(Tag::Decorator),
        "docstring" => Some(Tag::Docstring),
        "field" | "field.name" => Some(Tag::Field),
        _ => None,
    }
}

/// Return the appropriate .scm query source for a language.
fn get_query_for_language_src(language: Language) -> &'static str {
    match language {
        Language::Python => include_str!("../../queries/python.scm"),
        Language::TypeScript | Language::JavaScript => include_str!("../../queries/typescript.scm"),
        Language::Rust => include_str!("../../queries/rust.scm"),
        _ => "(identifier) @id", // fallback for unsupported languages
    }
}
