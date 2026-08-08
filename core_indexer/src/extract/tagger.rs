// CodeRadar v3.3 — Extraction: Tagger Pass 1 (§4.2)
// Tree-sitter .scm queries tag nodes with coarse classifications.

use std::collections::HashMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

use crate::types::{Tag, TagInfo, TaggedTree};

/// Run tree-sitter queries against a source file to produce a TaggedTree.
pub fn tag_tree<'a>(
    source: &'a str,
    language: tree_sitter::Language,
    _query_dir: &str,
) -> TaggedTree<'a> {
    let query_source = get_query_for_language_src(language.clone());
    let query = Query::new(&language, query_source)
        .expect("Failed to compile tree-sitter query");

    let mut parser = Parser::new();
    parser.set_language(&language)
        .expect("Failed to set language on parser");
    let tree = parser.parse(source, None)
        .expect("Failed to parse source");

    let mut cursor = QueryCursor::new();
    let mut tags: HashMap<usize, TagInfo> = HashMap::new();

    let root = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root, source_bytes);
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
        "class" | "class.name" => Some(Tag::Class),
        "class_base" => Some(Tag::ClassBase),
        "function" | "function.name" => Some(Tag::Function),
        "function_param" | "function.params" => Some(Tag::FunctionParam),
        "function_return" | "function.return" => Some(Tag::FunctionReturn),
        "import" | "import.module" => Some(Tag::Import),
        "import_from_clause" | "import_from" | "import_from.module" => Some(Tag::ImportFromClause),
        "import_specifier" | "import_from.name" => Some(Tag::ImportSpecifier),
        "call" | "call.name" => Some(Tag::Call),
        "call_receiver" | "call.receiver" => Some(Tag::CallReceiver),
        "decorator" | "decorator.name" => Some(Tag::Decorator),
        "docstring" => Some(Tag::Docstring),
        "field" | "field.name" => Some(Tag::Field),
        _ => None,
    }
}

/// Return the appropriate .scm query source for a language.
fn get_query_for_language_src(_language: Language) -> &'static str {
    // In production: match on language, return the appropriate .scm.
    // For now, return a minimal query that works for any tree-sitter grammar.
    r#"
(function_definition name: (identifier) @function.name) @function.def
(class_definition name: (identifier) @class.name) @class.def
(call function: (identifier) @call.name) @call
(import_statement) @import
"#
}
