// CodeRadar v0.5.3 — Single-Pass Cursor-Driven Extraction
// Replaces the two-pass tag_tree + walk_and_extract pipeline with a single
// cursor-driven pass. The QueryCursor visits every tagged node in document
// order; we emit entities directly from the cursor, using a byte-range
// frame stack + parent-chain walk for context resolution.

use std::collections::{HashMap, HashSet};

use streaming_iterator::StreamingIterator;
use tree_sitter::Node;

use crate::extract::docstring::preceding_docstring;
use crate::extract::spans::extract_byte_spans;
use crate::extract::tagger::CompiledQuery;
use crate::extract::walker::{
    classify_class_like, derive_function_kind,
    emit_call_for_node, extract_base_classes, extract_class_name,
    extract_function_name, extract_go_receiver_type, extract_parameters,
    make_entity_id, parse_import_from_statement, parse_import_statement,
};
use crate::types::*;

/// An emitted entity's metadata, stored for parent-chain context resolution.
#[derive(Clone)]
struct Emitted {
    unit_idx: usize,
    qualified_name: String,
    kind: EmittedKind,
    end_byte: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EmittedKind {
    Module,
    Class,
    Function,
}

/// Byte-range stack frame for O(1) context in sequential captures.
struct Frame {
    end_byte: usize,
    qualified_name: String,
    kind: EmittedKind,
}

/// Single-pass cursor-driven extractor.
pub struct CursorExtractor<'a> {
    source: &'a str,
    file_path: &'a str,
    units: Vec<ExtractedUnit>,
    /// node_id → emitted metadata
    emitted: HashMap<usize, Emitted>,
    /// byte-range stack for nesting context
    frames: Vec<Frame>,
    /// fn-ref candidates: (func_unit_idx, name, line, col)
    fn_ref_candidates: Vec<(usize, String, usize, usize)>,
    /// Track current function index for call attribution
    current_function_idx: Option<usize>,
    /// Docstring info for attachment
    pending_docstring: Option<(usize, String, usize)>,
    /// All function names for fn_ref resolution
    fn_names: HashSet<String>,
}

impl<'a> CursorExtractor<'a> {
    pub fn new(source: &'a str, file_path: &'a str) -> Self {
        CursorExtractor {
            source,
            file_path,
            units: Vec::new(),
            emitted: HashMap::new(),
            frames: Vec::new(),
            fn_ref_candidates: Vec::new(),
            current_function_idx: None,
            pending_docstring: None,
            fn_names: HashSet::new(),
        }
    }

    /// Run the single-pass extraction: cursor drives emission, parent chain + frame
    /// stack resolves context, then targeted fn_ref scan and resolution.
    pub fn extract(mut self, root_node: Node, compiled: &CompiledQuery) -> Vec<ExtractedUnit> {
        // Phase 1: Emit file-level module frame
        self.frames.push(Frame {
            end_byte: root_node.end_byte(),
            qualified_name: String::new(),
            kind: EmittedKind::Module,
        });

        // Phase 2: Direct cursor-driven dispatch.
        // Dedup by node ID — same node can match multiple patterns (e.g., Elixir
        // call nodes match both class.def and function.def when predicates fail).
        // The capture order in the .scm file determines priority: function.def
        // patterns appear BEFORE class.def in language queries to ensure
        // `def greet` inside a module dispatches as Function, not Class.
        let source_bytes = self.source.as_bytes();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = cursor.captures(&compiled.query, root_node, source_bytes);
        let mut seen: HashSet<usize> = HashSet::new();

        while let Some((qm, _idx)) = captures.next() {
            for capture in qm.captures {
                let idx = capture.index as usize;
                if idx >= compiled.capture_tags.len() {
                    continue;
                }
                let tag = match &compiled.capture_tags[idx] {
                    Some(t) => *t,
                    None => continue,
                };
                let node = capture.node;
                let node_id = node.id() as usize;

                if !seen.insert(node_id) {
                    continue;
                }

                self.pop_frames(node.start_byte());
                self.dispatch(node, tag);
            }
        }

        // Phase 3: Resolve fn_ref candidates (inline scan happened during emit_function)
        self.resolve_fn_refs();

        self.units
    }

    /// Pop frames whose end byte is before the given position.
    fn pop_frames(&mut self, byte_pos: usize) {
        while self.frames.last().map_or(false, |f| byte_pos >= f.end_byte) {
            let popped = self.frames.pop().unwrap();
            // Restore current_function_idx when leaving a function
            if popped.kind == EmittedKind::Function {
                self.current_function_idx = None;
                // Walk up to find enclosing function
                for frame in self.frames.iter().rev() {
                    if frame.kind == EmittedKind::Function {
                        self.current_function_idx = self.units.iter().enumerate()
                            .rev()
                            .find(|(_, u)| matches!(u, ExtractedUnit::Function(f) if f.qualified_name == frame.qualified_name))
                            .map(|(i, _)| i);
                        break;
                    }
                }
            }
        }
    }

    /// Resolve the parent qualified name from the frame stack.
    /// Returns (parent_qname, parent_class_option).
    fn resolve_context(&self) -> (String, Option<String>) {
        let mut parent_qname = String::new();
        let mut parent_class: Option<String> = None;

        for frame in self.frames.iter().rev() {
            match frame.kind {
                EmittedKind::Class => {
                    if parent_qname.is_empty() {
                        parent_qname = frame.qualified_name.clone();
                    }
                    if parent_class.is_none() {
                        parent_class = Some(frame.qualified_name.clone());
                    }
                }
                EmittedKind::Function | EmittedKind::Module => {
                    if parent_qname.is_empty() {
                        parent_qname = frame.qualified_name.clone();
                    }
                }
            }
        }

        (parent_qname, parent_class)
    }

    /// Register an emitted entity for context resolution.
    fn record_emitted(&mut self, node_id: usize, unit_idx: usize,
                       qualified_name: &str, kind: EmittedKind, end_byte: usize) {
        self.emitted.insert(node_id, Emitted {
            unit_idx,
            qualified_name: qualified_name.to_string(),
            kind,
            end_byte,
        });
    }

    // ── Tag dispatch ────────────────────────────────────────────────────

    fn dispatch(&mut self, node: Node, tag: Tag) {
        match tag {
            Tag::Class => self.emit_class(node),
            Tag::Function => self.emit_function(node),
            Tag::Import => self.emit_import(node),
            Tag::Call => {
                emit_call_for_node(node, self.source, &mut self.units, self.current_function_idx);
            }
            Tag::Impl => self.emit_impl(node),
            Tag::Docstring => {
                self.pending_docstring = emit_docstring_node(node, self.source);
            }
            Tag::Decorator | Tag::Field | Tag::ClassBase
            | Tag::FunctionParam | Tag::FunctionReturn
            | Tag::CallReceiver | Tag::ImportFromClause
            | Tag::ImportSpecifier | Tag::Export => {
                // Silent tags — no entity emitted directly
            }
        }
    }

    fn emit_class(&mut self, node: Node) {
        let name = extract_class_name(node, self.source);
        let line = node.start_position().row + 1;
        let exit_line = node.end_position().row + 1;
        let spans = extract_byte_spans(node);
        let (parent_qname, _parent_class) = self.resolve_context();
        let qualified_name = build_qualified_name_simple(&parent_qname, &name);
        let entity_id = make_entity_id(self.file_path, &qualified_name);
        let bases = extract_base_classes(node, self.source);
        let docstring = preceding_docstring(node, self.source);
        let grammar_kind = if node.kind() == "class_declaration" {
            let sub = classify_class_like(node);
            if sub != "class" {
                format!("class_declaration/{}", sub)
            } else {
                "class_declaration".to_string()
            }
        } else {
            node.kind().to_string()
        };

        let unit_idx = self.units.len();
        self.units.push(ExtractedUnit::Class(ExtractedClass {
            id: entity_id.clone(),
            name: name.clone(),
            qualified_name: qualified_name.clone(),
            grammar_kind,
            parent_module: entity_id.clone(),
            parent_class: None,
            bases,
            decorators: Vec::new(),
            docstring,
            fields: Vec::new(),
            line,
            exit_line,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            span: spans.full_span,
            name_span: spans.name_span,
            body_span: spans.body_span,
            decorators_span: spans.decorators_span,
        }));

        self.record_emitted(node.id() as usize, unit_idx, &qualified_name,
                            EmittedKind::Class, node.end_byte());
        self.frames.push(Frame {
            end_byte: node.end_byte(),
            qualified_name,
            kind: EmittedKind::Class,
        });
    }

    fn emit_function(&mut self, node: Node) {
        let name = extract_function_name(node, self.source);
        // Skip purely anonymous functions (JS/TS arrow callbacks, R/Ex/Lua anon
        // fns, ...). They have no name node and would otherwise collapse to a
        // single empty-name entity per file ("file::") — mis-attributing every
        // callback's calls — or, if synthesized, flood the graph with thousands
        // of <anonymous:L:C> callbacks. A navigation/search graph tracks NAMED
        // declarations; direct calls inside a skipped callback are still
        // captured by dispatch and attributed to the enclosing function via its
        // frame, so the call graph stays accurate.
        if name.is_empty() {
            return;
        }
        let go_receiver_type = extract_go_receiver_type(node, self.source);
        let (parent_qname, parent_class_from_frame) = self.resolve_context();
        let is_method = self.frames.iter().rev().any(|f| f.kind == EmittedKind::Class);
        let parent_class = if is_method {
            parent_class_from_frame.or_else(|| go_receiver_type.clone())
        } else if let Some(ref recv) = go_receiver_type {
            Some(recv.clone())
        } else {
            None
        };

        let kind = derive_function_kind(&[], is_method);
        let line = node.start_position().row + 1;
        let exit_line = node.end_position().row + 1;
        let spans = extract_byte_spans(node);
        let params = extract_parameters(node, self.source);
        let qualified_name = build_qualified_name_simple(&parent_qname, &name);
        let entity_id = make_entity_id(self.file_path, &qualified_name);

        let return_type = {
            let rt_node = node.child_by_field_name("return_type")
                .or_else(|| node.child_by_field_name("returns"));
            rt_node.and_then(|rt| {
                let rt_text = rt.utf8_text(self.source.as_bytes()).unwrap_or("");
                if !rt_text.is_empty() && !is_builtin_type(rt_text) {
                    Some(rt_text.to_string())
                } else {
                    None
                }
            })
        };

        let docstring = preceding_docstring(node, self.source);

        // Track this function for call attribution
        self.current_function_idx = Some(self.units.len());

        // Record function name for fn_ref resolution
        self.fn_names.insert(name.clone());

        let unit_idx = self.units.len();
        self.units.push(ExtractedUnit::Function(ExtractedFunction {
            id: entity_id.clone(),
            name: name.clone(),
            qualified_name: qualified_name.clone(),
            parent_module: entity_id.clone(),
            parent_class: parent_class.map(|q| make_entity_id(self.file_path, &q)),
            parameters: params,
            return_type,
            calls: Vec::new(),
            decorators: Vec::new(),
            docstring,
            kind,
            is_async: false,
            is_generator: false,
            line,
            exit_line,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            signature_hash: 0,
            body_hash: 0,
            span: spans.full_span,
            name_span: spans.name_span,
            params_span: spans.params_span,
            body_span: spans.body_span,
            decorators_span: spans.decorators_span,
        }));

        self.record_emitted(node.id() as usize, unit_idx, &qualified_name,
                            EmittedKind::Function, node.end_byte());

        // Scan this function's body subtree for fn-ref patterns inline
        scan_subtree_for_fn_ref(node, self.source, unit_idx, &mut self.fn_ref_candidates);

        self.frames.push(Frame {
            end_byte: node.end_byte(),
            qualified_name,
            kind: EmittedKind::Function,
        });
    }

    fn emit_import(&mut self, node: Node) {
        let text = node.utf8_text(self.source.as_bytes()).unwrap_or("").to_string();
        let line = node.start_position().row + 1;
        let name_span = ByteSpan {
            start: node.start_byte(),
            end: node.end_byte(),
        };

        let kind = match node.kind() {
            "import_statement" => parse_import_statement(node, self.source),
            "import_from_statement" => parse_import_from_statement(node, self.source),
            _ => ImportKind::ModuleImport {
                module: text.clone(),
                alias: None,
            },
        };

        // Collect imported names for fn_ref resolution
        match &kind {
            ImportKind::FromImport { names, .. } | ImportKind::RelativeImport { names, .. } => {
                for (name, alias) in names {
                    self.fn_names.insert(name.clone());
                    if let Some(a) = alias {
                        self.fn_names.insert(a.clone());
                    }
                }
            }
            ImportKind::ModuleImport { alias, module, .. } => {
                self.fn_names.insert(module.clone());
                if let Some(a) = alias {
                    self.fn_names.insert(a.clone());
                }
            }
            _ => {}
        }

        let entity_id = make_entity_id(self.file_path, &format!("import@{}", line));

        self.units.push(ExtractedUnit::Import(ExtractedImport {
            id: entity_id,
            raw: text,
            kind,
            line,
            is_type_only: false,
            name_span,
        }));
    }

    fn emit_impl(&mut self, node: Node) {
        let type_name = node
            .child_by_field_name("type")
            .and_then(|n| n.utf8_text(self.source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();

        let (parent_qname, _) = self.resolve_context();
        let qualified = build_qualified_name_simple(&parent_qname, &type_name);

        self.frames.push(Frame {
            end_byte: node.end_byte(),
            qualified_name: qualified,
            kind: EmittedKind::Class,
        });
    }

    // ── Targeted fn_ref scan ────────────────────────────────────────────

    /// Resolve fn_ref candidates against extracted function names.
    fn resolve_fn_refs(&mut self) {
        if self.fn_ref_candidates.is_empty() || self.fn_names.is_empty() {
            return;
        }

        for (func_idx, name, line, col) in &self.fn_ref_candidates {
            if self.fn_names.contains(name) {
                if let Some(ExtractedUnit::Function(ref mut func)) = self.units.get_mut(*func_idx) {
                    let already_present = func.calls.iter().any(|c| {
                        c.name == *name && c.line == *line && c.col == *col
                    });
                    if !already_present {
                        func.calls.push(UnresolvedRef {
                            name: name.clone(),
                            path: vec![],
                            line: *line,
                            col: *col,
                        });
                    }
                }
            }
        }
    }
}

/// Convenience function — single-pass extraction.
pub fn extract_single_pass(source: &str, root_node: Node, compiled: &CompiledQuery, file_path: &str) -> Vec<ExtractedUnit> {
    CursorExtractor::new(source, file_path).extract(root_node, compiled)
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Build qualified name from parent context and child name.
fn build_qualified_name_simple(parent_qname: &str, name: &str) -> String {
    if parent_qname.is_empty() {
        name.to_string()
    } else if name.is_empty() {
        parent_qname.to_string()
    } else {
        format!("{}.{}", parent_qname, name)
    }
}

/// Emit a docstring tag.
fn emit_docstring_node(node: Node, source: &str) -> Option<(usize, String, usize)> {
    let line = node.start_position().row + 1;
    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
    if text.is_empty() {
        return None;
    }
    // Clean comment markers
    let cleaned = text.trim_start_matches('#')
        .trim_start_matches("//")
        .trim_start_matches("///")
        .trim_start_matches("/*")
        .trim_end_matches("*/")
        .trim();
    if cleaned.is_empty() {
        None
    } else {
        let end_line = node.end_position().row + 1;
        Some((line, cleaned.to_string(), end_line))
    }
}

/// Scan a function's subtree for fn-ref patterns.
fn scan_subtree_for_fn_ref(node: Node, source: &str, func_idx: usize,
                           candidates: &mut Vec<(usize, String, usize, usize)>) {
    let kind = node.kind();

    // Assignment RHS: `x = handler`
    if matches!(kind, "assignment" | "assignment_expression" | "variable_declarator" | "let_declaration") {
        let rhs = node.child_by_field_name("right")
            .or_else(|| node.child_by_field_name("value"))
            .or_else(|| node.child_by_field_name("init"));
        if let Some(rhs_node) = rhs {
            if let Some(name) = extract_ref_name(rhs_node, source) {
                if !name.is_empty() && !is_stoplisted(&name) {
                    let line = rhs_node.start_position().row + 1;
                    let col = rhs_node.start_position().column as usize;
                    candidates.push((func_idx, name, line, col));
                }
            }
        }
    }

    // Return value: `return handler`
    if matches!(kind, "return_statement" | "return" | "return_expression" | "control_transfer_statement") {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if is_identifier_kind(child.kind()) {
                    if let Ok(name) = child.utf8_text(source.as_bytes()) {
                        if !name.is_empty() && !is_stoplisted(name) {
                            let line = child.start_position().row + 1;
                            let col = child.start_position().column as usize;
                            candidates.push((func_idx, name.to_string(), line, col));
                        }
                    }
                }
            }
        }
    }

    // Keyword argument: `on=handler`
    if kind == "keyword_argument" || kind == "pair" {
        let val = node.child_by_field_name("value");
        if let Some(val_node) = val {
            if is_identifier_kind(val_node.kind()) {
                if let Ok(name) = val_node.utf8_text(source.as_bytes()) {
                    if !name.is_empty() && !is_stoplisted(name) {
                        let line = val_node.start_position().row + 1;
                        let col = val_node.start_position().column as usize;
                        candidates.push((func_idx, name.to_string(), line, col));
                    }
                }
            }
        }
    }

    // Argument list identifiers
    if kind == "argument_list" || kind == "arguments" || kind == "call_suffix" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if is_identifier_kind(child.kind()) {
                    if let Ok(name) = child.utf8_text(source.as_bytes()) {
                        if !name.is_empty() && !is_stoplisted(name) {
                            let line = child.start_position().row + 1;
                            let col = child.start_position().column as usize;
                            candidates.push((func_idx, name.to_string(), line, col));
                        }
                    }
                }
            }
        }
    }

    // Dict/list literal values
    if kind == "dictionary" || kind == "dict" || kind == "list" || kind == "list_literal" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if child.kind() == "pair" {
                    if let Some(val) = child.child_by_field_name("value") {
                        if is_identifier_kind(val.kind()) {
                            if let Ok(name) = val.utf8_text(source.as_bytes()) {
                                if !name.is_empty() && !is_stoplisted(name) {
                                    let line = val.start_position().row + 1;
                                    let col = val.start_position().column as usize;
                                    candidates.push((func_idx, name.to_string(), line, col));
                                }
                            }
                        }
                    }
                } else if is_identifier_kind(child.kind()) {
                    if let Ok(name) = child.utf8_text(source.as_bytes()) {
                        if !name.is_empty() && !is_stoplisted(name) {
                            let line = child.start_position().row + 1;
                            let col = child.start_position().column as usize;
                            candidates.push((func_idx, name.to_string(), line, col));
                        }
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_subtree_for_fn_ref(child, source, func_idx, candidates);
    }
}

/// Extract a reference name from a node (identifier or dotted access).
fn extract_ref_name(node: Node, source: &str) -> Option<String> {
    let kind = node.kind();
    if is_identifier_kind(kind) {
        node.utf8_text(source.as_bytes()).ok().map(|s| s.to_string())
    } else if kind == "attribute" {
        node.child_by_field_name("attribute")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string())
    } else if kind == "field_expression" {
        node.child_by_field_name("field")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string())
    } else if kind == "member_expression" {
        node.child_by_field_name("property")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string())
    } else {
        None
    }
}

/// Check if a node kind is an identifier-like node.
fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier" | "IDENTIFIER" | "simple_identifier"
        | "type_identifier" | "property_identifier"
        | "field_identifier" | "shorthand_property_identifier"
    )
}
