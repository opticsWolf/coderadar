// CodeRadar v0.5.3 — Single-Pass Cursor-Driven Extraction
// Replaced the two-pass tag_tree + walk_and_extract pipeline with a single
// cursor-driven pass. The QueryCursor visits every tagged node in document
// order; we emit entities directly from the cursor, using a byte-range
// frame stack + parent-chain walk for context resolution.

use std::collections::HashSet;

use streaming_iterator::StreamingIterator;
use tree_sitter::Node;

use crate::extract::docstring::preceding_docstring;
use crate::extract::spans::extract_byte_spans;
use crate::extract::tagger::CompiledQuery;
use crate::extract::walker::{
    classify_class_like, derive_function_kind, detect_async, detect_generator, emit_call_for_node,
    extract_base_classes, extract_class_name, extract_decorators, extract_function_name,
    extract_go_receiver_type, extract_parameters, make_entity_id, parse_import_from_statement,
    parse_import_statement,
};
use crate::types::*;

use super::{hash_span, node_quality};

/// The callee of a Python `call` node as an `UnresolvedRef` (`Foo()`,
/// `mod.make()`); `None` for any other expression.
fn call_ref(node: Node, src: &str) -> Option<UnresolvedRef> {
    if node.kind() != "call" {
        return None;
    }
    let text = |n: Node| n.utf8_text(src.as_bytes()).unwrap_or("").to_string();
    let f = node.child_by_field_name("function")?;
    let (name, path, name_node) = match f.kind() {
        "identifier" => (text(f), vec![], f),
        "attribute" => (
            text(f.child_by_field_name("attribute")?),
            crate::extract::walker::receiver_segments(f.child_by_field_name("object"), src),
            f.child_by_field_name("attribute")?,
        ),
        _ => return None,
    };
    Some(UnresolvedRef {
        name,
        path,
        line: node.start_position().row + 1,
        col: node.start_position().column,
        name_span: node_span(name_node),
    })
}

/// A bare name or attribute chain (`desk`, `desk.manager`, `make().win`) as
/// receiver segments; `None` for any other expression.
fn expr_path(node: Node, src: &str) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![node.utf8_text(src.as_bytes()).ok()?.to_string()]),
        "attribute" => {
            let segs = crate::extract::walker::receiver_segments(Some(node), src);
            let clean = |s: &String| {
                (s.starts_with("<call:") && s.ends_with('>'))
                    || s.chars().all(|c| c.is_alphanumeric() || c == '_')
            };
            segs.iter().all(clean).then_some(segs)
        }
        _ => None,
    }
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
    /// byte-range stack for nesting context
    frames: Vec<Frame>,
    /// fn-ref candidates: (func_unit_idx, reference)
    fn_ref_candidates: Vec<(usize, UnresolvedRef)>,
    /// Track current function index for call attribution
    current_function_idx: Option<usize>,
    /// Track current class index for field attribution
    current_class_idx: Option<usize>,
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
            frames: Vec::new(),
            fn_ref_candidates: Vec::new(),
            current_function_idx: None,
            current_class_idx: None,
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
                let node_id = node.id();

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
        while self.frames.last().is_some_and(|f| byte_pos >= f.end_byte) {
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
            } else if popped.kind == EmittedKind::Class {
                // Restore current_class_idx when leaving a class
                self.current_class_idx = None;
                for frame in self.frames.iter().rev() {
                    if frame.kind == EmittedKind::Class {
                        self.current_class_idx = self.units.iter().enumerate()
                            .rev()
                            .find(|(_, u)| matches!(u, ExtractedUnit::Class(c) if c.qualified_name == frame.qualified_name))
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

    // ── Tag dispatch ────────────────────────────────────────────────────

    fn dispatch(&mut self, node: Node, tag: Tag) {
        match tag {
            Tag::Class => self.emit_class(node),
            Tag::Function => self.emit_function(node),
            Tag::Import => self.emit_import(node),
            Tag::Call => {
                emit_call_for_node(
                    node,
                    self.source,
                    &mut self.units,
                    self.current_function_idx,
                );
            }
            Tag::Impl => self.emit_impl(node),
            Tag::Docstring => {
                if let Some((line, text, end_line)) = emit_docstring_node(node, self.source) {
                    self.pending_docstring = Some((line, text.clone(), end_line));
                    // In-body docstrings (Python) fire *after* their unit was
                    // emitted — the cursor is preorder, so the function node
                    // is visited before the docstring statement in its body.
                    // `preceding_docstring` only reaches text *before* the
                    // declaration (Rust `///` comments), so without this
                    // backfill a Python function's `Function.docstring` was
                    // always None and the search scorer could never see it.
                    // The line-range guard keeps a stray string statement
                    // from leaking onto an unrelated function.
                    // The function block runs first and consumes `text` (a
                    // method docstring belongs to the method). The class block
                    // gets its own copy so the two backfills do not fight.
                    let class_text = text.clone();
                    if let Some(idx) = self.current_function_idx {
                        if let Some(ExtractedUnit::Function(f)) = self.units.get_mut(idx) {
                            if f.docstring.is_none() && f.line < line && end_line <= f.exit_line {
                                f.docstring = Some(text);
                            }
                        }
                    }
                    // Class docstrings use the same in-body backfill as
                    // functions: a Python class body fires the string
                    // *after* `emit_class` set `current_class_idx`, so the
                    // class docstring was otherwise always None. The
                    // `current_function_idx.is_none()` guard keeps a method
                    // docstring from leaking onto its enclosing class, and the
                    // line guard keeps a stray string outside the class body
                    // from landing on it (`is_none()` stops a later stray
                    // string from overwriting a real docstring).
                    if let Some(idx) = self.current_class_idx {
                        if self.current_function_idx.is_none() {
                            if let Some(ExtractedUnit::Class(c)) = self.units.get_mut(idx) {
                                if c.docstring.is_none() && c.line < line && end_line <= c.exit_line
                                {
                                    c.docstring = Some(class_text);
                                }
                            }
                        }
                    }
                }
            }
            Tag::Field => self.emit_field(node),
            Tag::Return => self.emit_return_binding(node),
            Tag::Decorator
            | Tag::ClassBase
            | Tag::FunctionParam
            | Tag::FunctionReturn
            | Tag::CallReceiver
            | Tag::ImportFromClause
            | Tag::ImportSpecifier
            | Tag::Export => {
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
            parse_quality: node_quality(node),
            content_hash: hash_span(self.source, spans.full_span.start, spans.full_span.end),
            span: spans.full_span,
            name_span: spans.name_span,
            body_span: spans.body_span,
            decorators_span: spans.decorators_span,
        }));

        self.current_class_idx = Some(unit_idx);
        self.frames.push(Frame {
            end_byte: node.end_byte(),
            qualified_name,
            kind: EmittedKind::Class,
        });
    }

    /// Record type evidence from an assignment inside a function body:
    /// `m = Manager()`, `m: Manager = ...`, `self.ser = Serializer()`.
    fn emit_binding(&mut self, node: Node) {
        let Some(idx) = self.current_function_idx else {
            return;
        };
        if node.kind() != "assignment" {
            return;
        }
        let src = self.source;
        let text = |n: Node| n.utf8_text(src.as_bytes()).unwrap_or("").to_string();
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let target = match left.kind() {
            "identifier" => vec![text(left)],
            "attribute" => {
                let segs = crate::extract::walker::receiver_segments(Some(left), src);
                if segs.len() == 2 && segs[0] == "self" {
                    segs
                } else {
                    return;
                }
            }
            _ => return,
        };
        let annotation = node.child_by_field_name("type").map(text);
        let right = node.child_by_field_name("right");
        let rhs = right.and_then(|r| call_ref(r, src));
        let expr = right.and_then(|r| expr_path(r, src));
        if rhs.is_none() && expr.is_none() && annotation.is_none() {
            return;
        }
        if let Some(ExtractedUnit::Function(f)) = self.units.get_mut(idx) {
            f.bindings.push(Binding {
                target,
                rhs,
                expr,
                annotation,
            });
        }
    }

    /// `return Foo()` / `return local.attr` / `yield value`: evidence for what
    /// the function evaluates to (a pytest fixture's value, a factory's result).
    fn emit_return_binding(&mut self, node: Node) {
        let Some(idx) = self.current_function_idx else {
            return;
        };
        let src = self.source;
        let Some(value) = node.named_child(0) else {
            return;
        };
        let rhs = call_ref(value, src);
        let expr = expr_path(value, src);
        // `return None` is "no value", not a competing type; any other
        // expression the typer cannot read is recorded as unknown evidence.
        if value.kind() == "none" {
            return;
        }
        let target = if node.kind() == "yield" {
            "<yield>"
        } else {
            "<return>"
        };
        if let Some(ExtractedUnit::Function(f)) = self.units.get_mut(idx) {
            f.bindings.push(Binding {
                target: vec![target.to_string()],
                rhs,
                expr,
                annotation: None,
            });
        }
    }

    /// A module-level constant: a top-level `NAME = ...` or `name: T = ...`.
    /// Only `UPPER_CASE` names or annotated ones count, so loop variables and
    /// scratch module code stay out of the entity set.
    fn emit_constant(&mut self, node: Node) {
        if node.kind() != "assignment" {
            return;
        }
        // Direct child of the module (`x = 1` statement), not nested in a
        // block, comprehension or class.
        let top_level = node.parent().is_some_and(|p| {
            p.kind() == "module"
                || (p.kind() == "expression_statement"
                    && p.parent().is_some_and(|m| m.kind() == "module"))
        });
        if !top_level {
            return;
        }
        let Some(name_node) = node.child_by_field_name("left") else {
            return;
        };
        if name_node.kind() != "identifier" {
            return;
        }
        let src = self.source;
        let text = |n: Node| n.utf8_text(src.as_bytes()).unwrap_or("").to_string();
        let name = text(name_node);
        let annotation = node.child_by_field_name("type").map(text);
        let upper = name.chars().any(|c| c.is_ascii_uppercase())
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if name.is_empty() || !(upper || annotation.is_some()) {
            return;
        }
        let default_value = node.child_by_field_name("right").map(|r| {
            let t = text(r);
            if t.len() > 200 {
                let cut = (0..=200)
                    .rev()
                    .find(|&i| t.is_char_boundary(i))
                    .unwrap_or(0);
                format!("{}…", &t[..cut])
            } else {
                t
            }
        });
        self.units.push(ExtractedUnit::Constant(ExtractedConstant {
            id: make_entity_id(self.file_path, &name),
            name,
            annotation,
            source: SourceType::Impl,
            default_value,
            span: ByteSpan {
                start: node.start_byte(),
                end: node.end_byte(),
            },
            name_span: ByteSpan {
                start: name_node.start_byte(),
                end: name_node.end_byte(),
            },
        }));
    }

    /// Capture a class-level field (e.g. `x = 1` or `x: int = 1` in a class
    /// body). Module-level assignments (no enclosing class) and local
    /// assignments inside methods (enclosing function frame) are skipped.
    fn emit_field(&mut self, node: Node) {
        if self.current_function_idx.is_some() {
            self.emit_binding(node);
            return;
        }
        if self.current_class_idx.is_none() {
            self.emit_constant(node);
            return;
        }
        let Some(name_node) = node
            .child_by_field_name("left")
            .or_else(|| node.child_by_field_name("name"))
        else {
            return;
        };
        // `a.b = ...` in a class body is not a field of the class.
        if name_node.kind() == "attribute" {
            return;
        }
        let name = name_node
            .utf8_text(self.source.as_bytes())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            return;
        }
        let annotation = node
            .child_by_field_name("type")
            .and_then(|t| t.utf8_text(self.source.as_bytes()).ok())
            .map(|s| s.to_string());
        let span = ByteSpan {
            start: node.start_byte(),
            end: node.end_byte(),
        };
        let name_span = ByteSpan {
            start: name_node.start_byte(),
            end: name_node.end_byte(),
        };

        let default_value = node
            .child_by_field_name("right")
            .or_else(|| node.child_by_field_name("value"))
            .and_then(|v| v.utf8_text(self.source.as_bytes()).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let idx = self.current_class_idx.unwrap();
        if let Some(ExtractedUnit::Class(ref mut class)) = self.units.get_mut(idx) {
            class.fields.push(ExtractedField {
                name,
                annotation,
                source: SourceType::Impl,
                default_value,
                is_class_var: true,
                span,
                name_span,
            });
        }
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
        let is_method = self
            .frames
            .iter()
            .rev()
            .any(|f| f.kind == EmittedKind::Class);
        let parent_class = if is_method {
            parent_class_from_frame.or_else(|| go_receiver_type.clone())
        } else {
            go_receiver_type.clone()
        };

        let decorators = extract_decorators(node, self.source);
        let kind = derive_function_kind(&decorators, is_method);
        let line = node.start_position().row + 1;
        let exit_line = node.end_position().row + 1;
        let spans = extract_byte_spans(node);
        let params = extract_parameters(node, self.source);
        let qualified_name = build_qualified_name_simple(&parent_qname, &name);
        let entity_id = make_entity_id(self.file_path, &qualified_name);

        let return_type = {
            let rt_node = node
                .child_by_field_name("return_type")
                .or_else(|| node.child_by_field_name("returns"));
            rt_node.and_then(|rt| {
                // TypeScript's return node is a `type_annotation`, whose text
                // includes the leading colon — rendering as `-> : string`.
                let rt_text = rt
                    .utf8_text(self.source.as_bytes())
                    .unwrap_or("")
                    .trim()
                    .trim_start_matches(':')
                    .trim();
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
            bindings: Vec::new(),
            refs: Vec::new(),
            decorators,
            docstring,
            kind,
            is_async: detect_async(node),
            is_generator: detect_generator(node),
            line,
            exit_line,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: node_quality(node),
            content_hash: hash_span(self.source, spans.full_span.start, spans.full_span.end),
            signature_hash: hash_span(self.source, spans.full_span.start, spans.body_span.start),
            body_hash: hash_span(self.source, spans.body_span.start, spans.body_span.end),
            metrics: crate::smells::metrics::compute_function_metrics(node, self.source),
            span: spans.full_span,
            name_span: spans.name_span,
            params_span: spans.params_span,
            body_span: spans.body_span,
            decorators_span: spans.decorators_span,
        }));

        // Scan this function's body subtree for fn-ref patterns inline
        scan_subtree_for_fn_ref(node, self.source, unit_idx, &mut self.fn_ref_candidates);

        self.frames.push(Frame {
            end_byte: node.end_byte(),
            qualified_name,
            kind: EmittedKind::Function,
        });
    }

    fn emit_import(&mut self, node: Node) {
        let text = node
            .utf8_text(self.source.as_bytes())
            .unwrap_or("")
            .to_string();
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

        for (func_idx, r) in &self.fn_ref_candidates {
            // A bare name must be something this file defines or imports;
            // an attribute reference is left to the resolver to type.
            if r.path.is_empty() && !self.fn_names.contains(&r.name) {
                continue;
            }
            if let Some(ExtractedUnit::Function(ref mut func)) = self.units.get_mut(*func_idx) {
                let seen = func
                    .refs
                    .iter()
                    .any(|x| x.name == r.name && x.line == r.line && x.col == r.col);
                if !seen {
                    func.refs.push(r.clone());
                }
            }
        }
    }
}

/// Convenience function — single-pass extraction.
pub fn extract_single_pass(
    source: &str,
    root_node: Node,
    compiled: &CompiledQuery,
    file_path: &str,
) -> Vec<ExtractedUnit> {
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
    // Clean comment markers and Python string delimiters — the tagged node
    // may be a `///` comment or a `"""…"""` literal; either way the
    // projection wants the prose, not the syntax.
    let cleaned = text
        .trim_start_matches('#')
        .trim_start_matches("//")
        .trim_start_matches("///")
        .trim_start_matches("/*")
        .trim_end_matches("*/")
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim();
    if cleaned.is_empty() {
        None
    } else {
        let end_line = node.end_position().row + 1;
        Some((line, cleaned.to_string(), end_line))
    }
}

/// A function-valued reference: a bare name or (Python) a dotted receiver
/// path ending in a name. `None` for any other expression.
fn ref_of(node: Node, source: &str) -> Option<UnresolvedRef> {
    let (name, path, name_node) = if is_identifier_kind(node.kind()) {
        (
            node.utf8_text(source.as_bytes()).ok()?.to_string(),
            vec![],
            node,
        )
    } else if node.kind() == "attribute" {
        let name_node = node.child_by_field_name("attribute")?;
        let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
        let path =
            crate::extract::walker::receiver_segments(node.child_by_field_name("object"), source);
        (name, path, name_node)
    } else {
        return None;
    };
    if name.is_empty() || is_stoplisted(&name) {
        return None;
    }
    Some(UnresolvedRef {
        name,
        path,
        line: node.start_position().row + 1,
        col: node.start_position().column,
        name_span: node_span(name_node),
    })
}

/// Byte range of a tree-sitter node.
fn node_span(node: Node) -> ByteSpan {
    ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

/// Scan a function's subtree for function-as-value sites: call arguments,
/// keyword arguments, dict/list/tuple/set elements, assignment right-hand
/// sides and returned values.
fn scan_subtree_for_fn_ref(
    node: Node,
    source: &str,
    func_idx: usize,
    candidates: &mut Vec<(usize, UnresolvedRef)>,
) {
    let mut found = Vec::new();
    value_refs(node, source, &mut found);
    candidates.extend(found.into_iter().map(|r| (func_idx, r)));

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_subtree_for_fn_ref(child, source, func_idx, candidates);
    }
}

/// Calls, function-valued references and attribute reads of Python
/// module-level code: statements, class bodies and decorators, which run at
/// import. Function bodies are left to their own functions; their attribute
/// reads are still collected (a `@property` is used by reading it anywhere).
/// Returns (uses, sorted attribute names).
pub(crate) fn module_scope_uses(root: Node, source: &str) -> (Vec<UnresolvedRef>, Vec<String>) {
    fn walk(
        node: Node,
        source: &str,
        in_fn: bool,
        uses: &mut Vec<UnresolvedRef>,
        attrs: &mut HashSet<String>,
    ) {
        if node.kind() == "attribute" {
            if let Some(a) = node
                .child_by_field_name("attribute")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            {
                attrs.insert(a.to_string());
            }
        }
        let in_fn = in_fn || node.kind() == "function_definition";
        if !in_fn {
            uses.extend(call_ref(node, source));
            value_refs(node, source, uses);
            // `@register` (bare decorator): the decorator itself is applied.
            if node.kind() == "decorator" {
                uses.extend(node.named_child(0).and_then(|n| ref_of(n, source)));
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, source, in_fn, uses, attrs);
        }
    }
    let mut uses = Vec::new();
    let mut attrs = HashSet::new();
    walk(root, source, false, &mut uses, &mut attrs);
    uses.retain(|r| !r.name.is_empty());
    let mut attrs: Vec<String> = attrs.into_iter().collect();
    attrs.sort();
    (uses, attrs)
}

/// Function-as-value sites directly at `node` (not recursive): call
/// arguments, keyword arguments, dict/list/tuple/set elements, assignment
/// right-hand sides and returned values.
fn value_refs(node: Node, source: &str, out: &mut Vec<UnresolvedRef>) {
    let kind = node.kind();
    let mut take = |n: Option<Node>| {
        if let Some(r) = n.and_then(|n| ref_of(n, source)) {
            out.push(r);
        }
    };

    match kind {
        // `x = handler`
        "assignment" | "assignment_expression" | "variable_declarator" | "let_declaration" => {
            take(
                node.child_by_field_name("right")
                    .or_else(|| node.child_by_field_name("value"))
                    .or_else(|| node.child_by_field_name("init")),
            );
        }
        // `return handler`
        "return_statement" | "return" | "return_expression" | "control_transfer_statement" => {
            for i in 0..node.child_count() {
                take(node.child(i as u32));
            }
        }
        // `on=handler`, `{"k": handler}`
        "keyword_argument" | "pair" => take(node.child_by_field_name("value")),
        // `def f(on_orphan=_leave)`: a default value is a value binding.
        "default_parameter" | "typed_default_parameter" => take(node.child_by_field_name("value")),
        // `alive or _parent_is_alive`: both operands are values.
        "boolean_operator" => {
            for i in 0..node.named_child_count() {
                take(node.named_child(i as u32));
            }
        }
        // `a if cond else b`: consequence and alternative are values, the
        // condition is not.
        "conditional_expression" => {
            take(node.child_by_field_name("consequence"));
            take(node.child_by_field_name("alternative"));
        }
        // `f(handler, self.method)`, `[a, b]`, `{a, b}`, `(a, b)`
        "argument_list" | "arguments" | "call_suffix" | "list" | "list_literal" | "set"
        | "tuple" | "expression_list" => {
            for i in 0..node.named_child_count() {
                take(node.named_child(i as u32));
            }
        }
        _ => {}
    }
}

/// Check if a node kind is an identifier-like node.
fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "IDENTIFIER"
            | "simple_identifier"
            | "type_identifier"
            | "property_identifier"
            | "field_identifier"
            | "shorthand_property_identifier"
    )
}

#[cfg(test)]
mod call_name_span_tests {
    use super::*;

    fn python_tree(src: &str) -> tree_sitter::Tree {
        let lang = crate::graph::CodeGraph::ts_language(&crate::types::Language::Python)
            .expect("python grammar");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).expect("parse")
    }

    fn first_node_of_kind<'t>(
        node: tree_sitter::Node<'t>,
        kind: &str,
    ) -> Option<tree_sitter::Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = first_node_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    /// The plan §4.1 blocker: `col` points at the receiver (`self.`), so a
    /// rename has no way to address just the method name. `name_span` is that
    /// address — verified against the source here, because an off-by-N span
    /// would rewrite the wrong bytes.
    #[test]
    fn attribute_call_records_the_method_name_span() {
        let src = "def f(self):\n    return self.save_state(1)\n";
        let tree = python_tree(src);
        let call = first_node_of_kind(tree.root_node(), "call").expect("call");
        let reference = call_ref(call, src).expect("attribute calls are extracted");

        assert_eq!(reference.name, "save_state");
        assert_eq!(reference.path, vec!["self".to_string()]);
        assert_eq!(
            &src.as_bytes()[reference.name_span.start..reference.name_span.end],
            b"save_state"
        );
        // The whole-call position is a different place in the line.
        assert!(
            reference.name_span.start > reference.col,
            "the call column points at the receiver, not the name"
        );
    }

    #[test]
    fn bare_call_name_span_covers_the_name() {
        let src = "def f():\n    return greet(1)\n";
        let tree = python_tree(src);
        let call = first_node_of_kind(tree.root_node(), "call").expect("call");
        let reference = call_ref(call, src).expect("bare calls are extracted");

        assert_eq!(reference.name, "greet");
        assert!(reference.path.is_empty());
        assert_eq!(
            &src.as_bytes()[reference.name_span.start..reference.name_span.end],
            b"greet"
        );
    }

    /// Function-as-value references (`map(save_state, xs)`) are renamed too,
    /// so they need the same address.
    #[test]
    fn function_as_value_records_the_name_span() {
        let src = "def f(xs):\n    return map(save_state, xs)\n";
        let tree = python_tree(src);
        let attribute = first_node_of_kind(tree.root_node(), "identifier").expect("identifier");
        let _ = attribute; // the first identifier is the function name `f`
        let reference = ref_of(
            first_node_of_kind(tree.root_node(), "argument_list")
                .expect("argument_list")
                .named_child(0)
                .expect("first argument"),
            src,
        )
        .expect("a bare name is a function-valued reference");

        assert_eq!(reference.name, "save_state");
        assert_eq!(
            &src.as_bytes()[reference.name_span.start..reference.name_span.end],
            b"save_state"
        );
    }
}
