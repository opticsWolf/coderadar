// CodeRadar v3.6 — Extraction Module
pub mod decorators;
pub mod docstring;   // v3.6: ported from CodeGraph's docstring.rs
pub mod single_pass;  // v0.5.3: single-pass cursor-driven extraction
pub mod spans;
pub mod tagger;
pub mod walker;

use crate::types::ParseQuality;

/// Content hash of `source[start..end]`, or 0 for an empty or invalid range.
///
/// `apply_diff_update` compares these to decide whether an entity changed. They
/// were hardcoded to 0, so the comparison was `0 != 0` and update_file never
/// re-inserted a function that already existed — edits to existing functions
/// never reached the graph in watch mode.
pub fn hash_span(source: &str, start: usize, end: usize) -> u64 {
    if end <= start {
        return 0;
    }
    source
        .as_bytes()
        .get(start..end)
        .map(xxhash_rust::xxh3::xxh3_64)
        .unwrap_or(0)
}

/// Parse quality of a single entity, read off its own subtree.
///
/// tree-sitter recovers from a syntax error by planting an ERROR or MISSING
/// node and carrying on, so entities are still extracted from files that do
/// not parse. Every construction site hardcoded `Clean`, which meant the
/// recovery was invisible: `plan_body_replacement` refuses to rewrite a body
/// it could not parse cleanly, and that guard could never fire.
pub fn node_quality(node: tree_sitter::Node) -> ParseQuality {
    if node.has_error() {
        ParseQuality::Partial
    } else {
        ParseQuality::Clean
    }
}

/// Number of ERROR / MISSING nodes in a subtree.
///
/// Descent stops at each error node — the recovery region below one is not
/// separately meaningful — so this counts recovery points, not broken tokens.
pub fn count_parse_errors(node: tree_sitter::Node) -> usize {
    if node.is_error() || node.is_missing() {
        return 1;
    }
    if !node.has_error() {
        return 0;
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    children.into_iter().map(count_parse_errors).sum()
}
