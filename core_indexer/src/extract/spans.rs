// CodeRadar v3.6 — Extraction: Byte-Span Extraction (§4.6)
// Extract byte-accurate spans from tree-sitter nodes for mutation targeting.

use tree_sitter::Node;

use crate::types::ByteSpan;

/// Result of extracting all spans for a definition node.
#[derive(Clone, Debug)]
pub struct ExtractedSpans {
    pub full_span: ByteSpan,
    pub name_span: ByteSpan,
    pub body_span: ByteSpan,
    pub params_span: ByteSpan,
    pub decorators_span: Option<ByteSpan>,
}

/// Helper for constructing ByteSpans from tree-sitter nodes.
pub struct SpanExtractor<'a> {
    _source: &'a str,
}

impl<'a> SpanExtractor<'a> {
    pub fn new(source: &'a str) -> Self {
        Self { _source: source }
    }

    pub fn node_span(&self, node: Node) -> ByteSpan {
        ByteSpan {
            start: node.start_byte(),
            end: node.end_byte(),
        }
    }

    pub fn safe_span(&self, node: Option<Node>) -> ByteSpan {
        node.map(|n| self.node_span(n))
            .unwrap_or(ByteSpan { start: 0, end: 0 })
    }
}

/// Extract all byte spans from a function or class definition node.
pub fn extract_byte_spans(node: Node) -> ExtractedSpans {
    let name_node = node.child_by_field_name("name");
    let body_node = node.child_by_field_name("body");
    let params_node = node.child_by_field_name("parameters");

    ExtractedSpans {
        full_span: ByteSpan {
            start: node.start_byte(),
            end: node.end_byte(),
        },
        name_span: ByteSpan {
            start: name_node.map(|n| n.start_byte()).unwrap_or(node.start_byte()),
            end: name_node.map(|n| n.end_byte()).unwrap_or(node.start_byte()),
        },
        body_span: ByteSpan {
            start: body_node.map(|n| n.start_byte()).unwrap_or(node.end_byte()),
            end: body_node.map(|n| n.end_byte()).unwrap_or(node.end_byte()),
        },
        params_span: ByteSpan {
            start: params_node.map(|n| n.start_byte()).unwrap_or(node.start_byte()),
            end: params_node.map(|n| n.end_byte()).unwrap_or(node.start_byte()),
        },
        decorators_span: None,
    }
}
