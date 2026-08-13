// CodeRadar v3.6 — Extraction: Hierarchy Walker Pass 2 (§4.2)
// Typed stack-frame walker that traverses the tagged tree and emits ExtractedUnits.
// v3.6: EntityId-based identities; file_path required for building entity IDs.
// v3.6a: Call capture — Tag::Call/Tag::CallReceiver attach calls to current function.

use tree_sitter::Node;

use crate::types::*;

use super::docstring::preceding_docstring;
use super::spans::extract_byte_spans;

/// Walk the tagged tree and produce a list of ExtractedUnits.
pub fn walk_and_extract<'a>(
    tagged: &'a TaggedTree<'a>,
    root_node: Node<'a>,
    file_path: &str,
) -> Vec<ExtractedUnit> {
    let mut ctx = WalkContext {
        tags: tagged,
        units: Vec::new(),
        stack: Vec::new(),
        file_path: file_path.to_string(),
        current_function_idx: None,
        fn_ref_candidates: Vec::new(),
    };

    ctx.stack.push(WalkFrame {
        qualified: String::new(),
        kind: FrameKind::Module,
    });
    walk_node(root_node, &mut ctx);

    // v3.6: Resolve function-as-value reference candidates.
    // Match identifiers found in assignment RHS, return values, and
    // keyword argument values against known function names.
    resolve_fn_ref_candidates(&mut ctx);

    ctx.units
}

fn walk_node(node: Node, ctx: &mut WalkContext) {
    let pushed = if let Some(info) = ctx.tags.tags.get(&(node.id() as usize)) {
        emit_for_node(node, info, ctx)
    } else {
        None
    };

    // v3.6: Scan for function-as-value references when inside a function.
    // Patterns: `x = handler`, `return handler`, `callback=handler`
    // Based on CodeGraph's maybe_capture_fn_refs (python.rs, swift.rs)
    // Copyright (c) 2024 Colby McHenry — MIT License
    if ctx.current_function_idx.is_some() {
        scan_for_fn_ref(node, ctx);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(child, ctx);
    }

    if let Some(frame_kind) = pushed {
        let popped = ctx.stack.pop();
        debug_assert!(
            popped
                .map(|f| std::mem::discriminant(&f.kind)
                    == std::mem::discriminant(&frame_kind))
                .unwrap_or(true),
            "Frame stack mismatch"
        );
        // Restore outer function index when leaving a function
        if matches!(frame_kind, FrameKind::Function) {
            // Walk up the stack to find the enclosing function, if any
            ctx.current_function_idx = None;
            for frame in ctx.stack.iter().rev() {
                if let FrameKind::Function = frame.kind {
                    // We need to find the function unit index. Walk units backward.
                    ctx.current_function_idx = ctx.units.iter().enumerate()
                        .rev()
                        .find(|(_, u)| matches!(u, ExtractedUnit::Function(f) if f.qualified_name == frame.qualified))
                        .map(|(i, _)| i);
                    break;
                }
            }
        }
    }
}

/// Scan a node for function-as-value reference patterns when inside a function.
/// Detects: `x = handler`, `return handler`, `config = handler` in dicts.
///
/// Based on CodeGraph's maybe_capture_fn_refs (python.rs, swift.rs)
/// Copyright (c) 2024 Colby McHenry — MIT License
/// <https://github.com/colbymchenry/codegraph>
fn scan_for_fn_ref(node: Node, ctx: &mut WalkContext) {
    let source = ctx.tags.source;
    let kind = node.kind();
    let func_idx = match ctx.current_function_idx {
        Some(i) => i,
        None => return,
    };

    // Assignment RHS: `x = handler` (but not `x = handler()`)
    if matches!(kind, "assignment" | "assignment_expression" | "variable_declarator" | "let_declaration") {
        let rhs = node.child_by_field_name("right")
            .or_else(|| node.child_by_field_name("value"))
            .or_else(|| node.child_by_field_name("init"));
        if let Some(rhs_node) = rhs {
            let rhs_kind = rhs_node.kind();
            // Extract name from simple identifier or attribute (obj.method → method)
            let rhs_name = if is_identifier_kind(rhs_kind) {
                rhs_node.utf8_text(source.as_bytes()).ok().map(|s| s.to_string())
            } else if rhs_kind == "attribute" {
                // Python/JS: obj.method → extract "method" part
                rhs_node.child_by_field_name("attribute")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.to_string())
            } else if rhs_kind == "field_expression" {
                // Rust: obj.field → extract "field" part
                rhs_node.child_by_field_name("field")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.to_string())
            } else if rhs_kind == "member_expression" {
                // JS/TS: obj.property → extract "property" part
                rhs_node.child_by_field_name("property")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.to_string())
            } else {
                None
            };
            if let Some(name) = rhs_name {
                if !name.is_empty() && !is_stoplisted(name.as_str()) {
                    let line = rhs_node.start_position().row + 1;
                    let col = rhs_node.start_position().column as usize;
                    ctx.fn_ref_candidates.push((func_idx, name, line, col));
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
                            ctx.fn_ref_candidates.push((func_idx, name.to_string(), line, col));
                        }
                    }
                }
            }
        }
    }

    // Keyword argument value: `on=handler` in function call
    if kind == "keyword_argument" || kind == "pair" {
        let val = node.child_by_field_name("value");
        if let Some(val_node) = val {
            if is_identifier_kind(val_node.kind()) {
                if let Ok(name) = val_node.utf8_text(source.as_bytes()) {
                    if !name.is_empty() && !is_stoplisted(name) {
                        let line = val_node.start_position().row + 1;
                        let col = val_node.start_position().column as usize;
                        ctx.fn_ref_candidates.push((func_idx, name.to_string(), line, col));
                    }
                }
            }
        }
    }

    // Argument list: `register_callback(handler)` — identifiers in arg position
    // that are NOT the callee are fn-ref candidates
    if kind == "argument_list" || kind == "arguments" || kind == "call_suffix" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if is_identifier_kind(child.kind()) {
                    if let Ok(name) = child.utf8_text(source.as_bytes()) {
                        if !name.is_empty() && !is_stoplisted(name) {
                            let line = child.start_position().row + 1;
                            let col = child.start_position().column as usize;
                            ctx.fn_ref_candidates.push((func_idx, name.to_string(), line, col));
                        }
                    }
                }
            }
        }
    }

    // Dict/list literal values: `{"key": handler}`, `[handler1, handler2]`
    if kind == "dictionary" || kind == "dict" || kind == "list" || kind == "list_literal" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                // Dict: value identifiers in key-value pairs
                // List: direct identifier elements
                if child.kind() == "pair" {
                    if let Some(val) = child.child_by_field_name("value") {
                        if is_identifier_kind(val.kind()) {
                            if let Ok(name) = val.utf8_text(source.as_bytes()) {
                                if !name.is_empty() && !is_stoplisted(name) {
                                    let line = val.start_position().row + 1;
                                    let col = val.start_position().column as usize;
                                    ctx.fn_ref_candidates.push((func_idx, name.to_string(), line, col));
                                }
                            }
                        }
                    }
                } else if is_identifier_kind(child.kind()) {
                    if let Ok(name) = child.utf8_text(source.as_bytes()) {
                        if !name.is_empty() && !is_stoplisted(name) {
                            let line = child.start_position().row + 1;
                            let col = child.start_position().column as usize;
                            ctx.fn_ref_candidates.push((func_idx, name.to_string(), line, col));
                        }
                    }
                }
            }
        }
    }
}

/// Check if a node kind is an identifier-like node (not a keyword).
fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier" | "IDENTIFIER" | "simple_identifier"
        | "type_identifier" | "property_identifier"
        | "field_identifier" | "shorthand_property_identifier"
    )
}

/// Resolve function-as-value reference candidates against extracted functions.
/// For each fn-ref candidate (identifier found in assignment RHS, return value,
/// or keyword argument), if the name matches a known function defined in the
/// same file, add it as an unresolved call reference.
///
/// Based on CodeGraph's flush_fn_ref_candidates pattern.
/// Copyright (c) 2024 Colby McHenry — MIT License
fn resolve_fn_ref_candidates(ctx: &mut WalkContext) {
    if ctx.fn_ref_candidates.is_empty() {
        return;
    }

    // Collect all function names from extracted units (owned, to avoid borrow conflicts)
    let mut fn_names: std::collections::HashSet<String> = ctx
        .units
        .iter()
        .filter_map(|u| match u {
            ExtractedUnit::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();

    // v3.6: Also collect imported names for cross-file fn-ref resolution.
    // `from .handlers import handle_click` → "handle_click" is a candidate.
    for unit in &ctx.units {
        if let ExtractedUnit::Import(imp) = unit {
            match &imp.kind {
                ImportKind::FromImport { names, .. } | ImportKind::RelativeImport { names, .. } => {
                    for (name, alias) in names {
                        fn_names.insert(name.clone());
                        if let Some(a) = alias {
                            fn_names.insert(a.clone());
                        }
                    }
                }
                ImportKind::ModuleImport { alias, .. } => {
                    if let Some(a) = alias {
                        fn_names.insert(a.clone());
                    }
                }
                _ => {}
            }
        }
    }

    if fn_names.is_empty() {
        return;
    }

    // For each candidate, if the name matches a known function, add to that function's calls
    for (func_idx, name, line, col) in &ctx.fn_ref_candidates {
        if fn_names.contains(name) {
            if let Some(ExtractedUnit::Function(ref mut func)) = ctx.units.get_mut(*func_idx) {
                // Avoid duplicates
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

/// For grammars that use a single node kind for multiple class-like
/// For grammars that use a single node kind for multiple class-like
/// declarations (Swift), scan keyword children to classify.
///
/// Based on CodeGraph's classifyClassNode pattern (swift.rs, kotlin.rs)
/// Copyright (c) 2024 Colby McHenry — MIT License
/// <https://github.com/colbymchenry/codegraph>
pub fn classify_class_like(node: Node) -> &'static str {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            match child.kind() {
                "struct" => return "struct",
                "enum" => return "enum",
                "protocol" => return "interface",
                "actor" => return "class",
                _ => {}
            }
        }
    }
    "class"
}

fn emit_for_node(node: Node, info: &TagInfo, ctx: &mut WalkContext) -> Option<FrameKind> {
    let source = ctx.tags.source;

    match info.tag {
        Tag::Class => {
            let name = extract_class_name(node, source);
            let line = node.start_position().row + 1;
            let exit_line = node.end_position().row + 1;
            let spans = extract_byte_spans(node);
            let qualified_name = build_qualified_name(&ctx.stack, &name);
            let entity_id = make_entity_id(&ctx.file_path, &qualified_name);
            let bases = extract_base_classes(node, source);
            let docstring = preceding_docstring(node, source);
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

            ctx.units.push(ExtractedUnit::Class(ExtractedClass {
                id: entity_id.clone(),
                name: name.clone(),
                qualified_name: qualified_name.clone(),
                grammar_kind,
                parent_module: entity_id.clone(), // patched later
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

            ctx.stack.push(WalkFrame {
                qualified: qualified_name,
                kind: FrameKind::Class(String::new()),
            });
            Some(FrameKind::Class(String::new()))
        }

        Tag::Function => {
            let name = extract_function_name(node, source);
            let go_receiver_type = extract_go_receiver_type(node, source);

            let is_method = matches!(
                ctx.stack.last(),
                Some(WalkFrame {
                    kind: FrameKind::Class(_),
                    ..
                })
            );

            // Extract parent class name from the stack (Rust impl_item or Python class),
            // or from Go receiver type
            let parent_class = if is_method {
                ctx.stack.last().and_then(|f| {
                    if let FrameKind::Class(_) = &f.kind {
                        if !f.qualified.is_empty() {
                            Some(f.qualified.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
            } else if let Some(ref recv) = go_receiver_type {
                Some(recv.clone())
            } else {
                None
            };

            let kind = derive_function_kind(&[], is_method);
            let line = node.start_position().row + 1;
            let exit_line = node.end_position().row + 1;
            let spans = extract_byte_spans(node);
            let params = extract_parameters(node, source);
            let qualified_name = build_qualified_name(&ctx.stack, &name);
            let entity_id = make_entity_id(&ctx.file_path, &qualified_name);

            // v3.6: Extract return type annotation
            let return_type = {
                let rt_node = node.child_by_field_name("return_type")
                    .or_else(|| node.child_by_field_name("returns"));
                rt_node.and_then(|rt| {
                    let rt_text = rt.utf8_text(source.as_bytes()).unwrap_or("");
                    if !rt_text.is_empty() && !is_builtin_type(rt_text) {
                        Some(rt_text.to_string())
                    } else {
                        None
                    }
                })
            };

            // v3.6: Extract preceding docstring
            let docstring = preceding_docstring(node, source);

            // Track this function for call capture
            ctx.current_function_idx = Some(ctx.units.len());

            ctx.units.push(ExtractedUnit::Function(ExtractedFunction {
                id: entity_id.clone(),
                name: name.clone(),
                qualified_name: qualified_name.clone(),
                parent_module: entity_id.clone(), // patched later
                parent_class: parent_class.clone().map(|q| make_entity_id(&ctx.file_path, &q)),
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
                metrics: crate::types::FunctionMetrics::default(),
                span: spans.full_span,
                name_span: spans.name_span,
                params_span: spans.params_span,
                body_span: spans.body_span,
                decorators_span: spans.decorators_span,
            }));

            ctx.stack.push(WalkFrame {
                qualified: qualified_name,
                kind: FrameKind::Function,
            });
            Some(FrameKind::Function)
        }

        Tag::Import => {
            let text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            let line = node.start_position().row + 1;
            let name_span = ByteSpan {
                start: node.start_byte(),
                end: node.end_byte(),
            };

            let kind = match node.kind() {
                "import_statement" => parse_import_statement(node, source),
                "import_from_statement" => parse_import_from_statement(node, source),
                _ => ImportKind::ModuleImport {
                    module: text.clone(),
                    alias: None,
                },
            };

            let entity_id = make_entity_id(&ctx.file_path, &format!("import@{}", line));

            ctx.units.push(ExtractedUnit::Import(ExtractedImport {
                id: entity_id,
                raw: text,
                kind,
                line,
                is_type_only: false,
                name_span,
            }));
            None
        }

        // ── Call Capture (§5.3) ──────────────────────────────────────

        Tag::Call => {
            emit_call_for_node(node, source, &mut ctx.units, ctx.current_function_idx);
            None
        }

        Tag::CallReceiver => {
            // The CallReceiver tag fires on the object part BEFORE the
            // Call tag fires on the attribute. Store the receiver name
            // for the next Call tag to pick up.
            // (Handled above in Call::attribute path — CallReceiver is
            //  a signal but the actual capture happens in Call.)
            None
        }

        // ── Silent Tags (no entities emitted) ──────────────────────

        Tag::Impl => {
            // impl_item for Rust — push a class-like frame so methods
            // inside get parent_class set, but don't emit a Class entity.
            let type_name = node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();

            let qualified = build_qualified_name(&ctx.stack, &type_name);
            ctx.stack.push(WalkFrame {
                qualified,
                kind: FrameKind::Class(String::new()),
            });
            Some(FrameKind::Class(String::new()))
        }

        Tag::Docstring
        | Tag::Field
        | Tag::ClassBase
        | Tag::FunctionParam
        | Tag::FunctionReturn
        | Tag::Decorator
        | Tag::ImportFromClause
        | Tag::ImportSpecifier
        | Tag::Export => None,
    }
}

/// Parse an `import_statement` node: `import os`, `import os.path as p`
pub fn parse_import_statement(node: Node, source: &str) -> ImportKind {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "dotted_name" => {
                let module = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                return ImportKind::ModuleImport {
                    module,
                    alias: None,
                };
            }
            "aliased_import" => {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .to_string();
                let alias = child
                    .child_by_field_name("alias")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.to_string());
                return ImportKind::ModuleImport { module: name, alias };
            }
            _ => {}
        }
    }
    ImportKind::ModuleImport { module: String::new(), alias: None }
}

/// Parse an `import_from_statement` node: `from foo import bar`, `from . import x`
pub fn parse_import_from_statement(node: Node, source: &str) -> ImportKind {
    // Extract the module_name (dotted_name or relative_import)
    let module_name = node
        .child_by_field_name("module_name")
        .map(|mn| {
            let text = mn.utf8_text(source.as_bytes()).unwrap_or("");
            (mn.kind().to_string(), text.to_string())
        });

    // Check for wildcard import
    let has_star = node
        .children(&mut node.walk())
        .any(|c| c.kind() == "wildcard_import");

    if has_star {
        if let Some((kind, text)) = module_name {
            if kind == "relative_import" {
                let level = count_leading_dots(&text);
                return ImportKind::StarImport {
                    module: if level > text.len() { String::new() } else { text[level..].to_string() },
                };
            }
            return ImportKind::StarImport { module: text };
        }
        return ImportKind::StarImport { module: String::new() };
    }

    // Collect imported names (skip the module_name child)
    let mut names: Vec<(String, Option<String>)> = Vec::new();
    let mut child_cursor = node.walk();
    for child in node.children(&mut child_cursor) {
        // Skip the module_name and wildcard_import children
        if child.kind() == "wildcard_import" {
            continue;
        }
        // Check if this child is the module_name field
        if node.child_by_field_name("module_name").map(|mn| mn.id()) == Some(child.id()) {
            continue;
        }
        match child.kind() {
            "dotted_name" => {
                let name = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                names.push((name, None));
            }
            "aliased_import" => {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .to_string();
                let alias = child
                    .child_by_field_name("alias")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.to_string());
                names.push((name, alias));
            }
            _ => {}
        }
    }

    if let Some((kind, text)) = module_name {
        if kind == "relative_import" {
            let level = count_leading_dots(&text);
            let module = if level < text.len() {
                Some(text[level..].to_string())
            } else {
                None
            };
            return ImportKind::RelativeImport { level, module, names };
        }
        return ImportKind::FromImport { module: text, names };
    }

    ImportKind::FromImport { module: String::new(), names }
}

/// Count leading dots in a relative import string like "..." or "...utils"
fn count_leading_dots(s: &str) -> usize {
    s.chars().take_while(|&c| c == '.').count()
}

// ── Step 1: Extracted emit_* functions for single-pass migration ──────────

/// Extract the name from a class-like AST node.
/// Handles: Python class, Go type_declaration, Rust struct/enum/trait,
/// Swift/Kotlin class_declaration, Zig VarDecl, Elixir defmodule, Lua table_constructor,
/// R setClass_expression, PHP class_definition, etc.
pub fn extract_class_name(node: Node, source: &str) -> String {
    let name_node = node.child_by_field_name("name");
    if name_node.is_some() && name_node.unwrap().kind() != "" {
        return name_node
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();
    }
    if node.kind() == "type_declaration" {
        return node.child_by_field_name("type")
            .and_then(|ts| ts.child_by_field_name("name"))
            .or_else(|| {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "type_spec" {
                        return child.child_by_field_name("name");
                    }
                }
                None
            })
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();
    }
    // Elixir defmodule
    if node.kind() == "call" && node.child_by_field_name("target")
        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
        .map(|s| s == "defmodule")
        .unwrap_or(false)
    {
        return node.children(&mut node.walk())
            .find(|c| c.kind() == "arguments")
            .and_then(|args| {
                args.children(&mut args.walk())
                    .find(|c| c.kind() == "alias")
                    .and_then(|a| a.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
    }
    if node.kind() == "VarDecl" {
        if let Some(name) = node.child_by_field_name("variable_type_function")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        {
            return name.to_string();
        }
    }
    // General: find identifier child
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    children.iter().find(|ch|
        ch.kind() == "type_identifier" || ch.kind() == "simple_identifier"
        || ch.kind() == "identifier"
    )
    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
    .unwrap_or("")
    .to_string()
}

/// Extract the name from a function-like AST node.
/// Handles: Python/JS function_definition, C++ function_definition,
/// Kotlin function_declaration, Zig FnProto, Elixir def/defp, R function_definition.
pub fn extract_function_name(node: Node, source: &str) -> String {
    let name_node = node.child_by_field_name("name");
    if node.kind() == "function_definition" {
        // R: function_definition is child of binary_operator; name is on lhs
        if let Some(rn) = node.parent().and_then(|p| {
            if p.kind() == "binary_operator" {
                p.child_by_field_name("lhs")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            } else {
                None
            }
        }) {
            return rn.to_string();
        }
        if name_node.is_some() {
            return name_node
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();
        }
        // C++/C: walk declarator chain
        return node.child_by_field_name("declarator")
            .and_then(|d| d.child_by_field_name("declarator"))
            .and_then(|id| id.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();
    }
    if name_node.is_some() {
        return name_node
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();
    }
    if node.kind() == "function_declaration" {
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        return children.iter().find(|ch|
            ch.kind() == "simple_identifier" || ch.kind() == "identifier"
        )
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("")
        .to_string();
    }
    if node.kind() == "FnProto" {
        return node.child_by_field_name("function")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();
    }
    // Elixir: (call target: (identifier "def"/"defp") ...)
    if node.is_named() && node.kind() == "call" {
        return node.child_by_field_name("target")
            .and_then(|t| t.utf8_text(source.as_bytes()).ok())
            .and_then(|s| {
                if matches!(s, "def" | "defp") {
                    node.children(&mut node.walk())
                        .find(|c| c.kind() == "arguments")
                        .and_then(|args| {
                            args.children(&mut args.walk())
                                .find(|c| c.kind() == "call")
                                .and_then(|call| call.child_by_field_name("target"))
                                .and_then(|id| id.utf8_text(source.as_bytes()).ok())
                                .map(|s| s.to_string())
                        })
                } else {
                    None
                }
            })
            .unwrap_or_default();
    }
    "".to_string()
}

/// Extract base classes from a class node's superclass/heritage fields.
pub fn extract_base_classes(node: Node, source: &str) -> Vec<UnresolvedRef> {
    fn is_base_node_kind(k: &str) -> bool {
        matches!(k, "identifier" | "type_identifier" | "constant" | "name"
            // Qualified bases (`extends React.Component`, `implements Models.Base`).
            // Previously dropped — capped extends coverage at the base-name
            // heuristic (matrix §0). Captured here as a dotted name.
            | "member_expression" | "qualified_type" | "scoped_type_id"
            | "qualified_identifier" | "nested_type_identifier" | "generic_type")
    }

    // Grammars expose base classes very differently:
    //   Python:  class_definition → `superclasses:` → argument_list of identifiers
    //   TS/JS:   class_declaration → class_heritage → `extends_clause value:`
    //            + `implements_clause` (comma-separated children)
    //   Java/Kt: `superclass:` field directly on the class node
    // So walk the node's descendants and pull base nodes from every source.
    let mut base_nodes: Vec<Node> = Vec::new();
    let mut dfs: Vec<Node> = vec![node];
    while let Some(n) = dfs.pop() {
        // (a) Direct base fields — Python `superclasses`, Java/Kotlin `superclass`.
        let fields: [Option<Node>; 3] = [
            n.child_by_field_name("superclasses"),
            n.child_by_field_name("superclass"),
            n.child_by_field_name("bases"),
        ];
        for f in fields.into_iter().flatten() {
            let mut c = f.walk();
            let kids: Vec<Node> = f.children(&mut c).collect();
            let base_kids: Vec<Node> = kids.iter().copied()
                .filter(|k| is_base_node_kind(k.kind())).collect();
            if !base_kids.is_empty() {
                base_nodes.extend(base_kids);
            } else if is_base_node_kind(f.kind()) {
                base_nodes.push(f);
            }
        }
        // (b) TS/JS heritage clauses.
        match n.kind() {
            "extends_clause" => {
                if let Some(v) = n.child_by_field_name("value") {
                    base_nodes.push(v);
                }
            }
            "implements_clause" => {
                let mut c = n.walk();
                base_nodes.extend(n.children(&mut c).filter(|k| is_base_node_kind(k.kind())));
            }
            _ => {}
        }
        // Descend.
        let mut c = n.walk();
        let kids: Vec<Node> = n.children(&mut c).collect();
        for k in kids.into_iter().rev() { dfs.push(k); }
    }

    // De-duplicate by source byte range (a base may be seen via overlapping
    // field checks) and emit one UnresolvedRef per base.
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    base_nodes
        .into_iter()
        .filter_map(|id| {
            if !seen.insert(id.id()) { return None; }
            let dotted = dotted_name_of(&id, source.as_bytes());
            let leaf = dotted.rsplit('.').next().unwrap_or(&dotted);
            if dotted.is_empty() || is_builtin_type(leaf) { return None; }
            Some(UnresolvedRef {
                name: dotted,
                path: vec![],
                line: id.start_position().row + 1,
                col: id.start_position().column as usize,
            })
        })
        .collect()
}

/// Join the leaf identifiers of a (possibly qualified) base node into a
/// dotted name (`React.Component`, `module.Foo`). Falls back to the node's
/// raw text if no identifier children are found. Used by
/// `extract_base_classes` for member_expression/qualified_type bases.
fn dotted_name_of(node: &Node, source_bytes: &[u8]) -> String {
    let mut leaves: Vec<String> = Vec::new();
    // Pre-order DFS collecting identifier-like leaves.
    let mut stack: Vec<Node> = vec![*node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "identifier" | "type_identifier" | "name" | "property_identifier") {
            if let Ok(t) = n.utf8_text(source_bytes) {
                leaves.push(t.to_string());
            }
        }
        let mut c = n.walk();
        let children: Vec<Node> = n.children(&mut c).collect();
        // push in reverse so left-to-right order is preserved on pop
        for ch in children.into_iter().rev() { stack.push(ch); }
    }
    if leaves.is_empty() {
        node.utf8_text(source_bytes).map(|s| s.split_whitespace().collect::<Vec<_>>().join("")).unwrap_or_default()
    } else {
        leaves.join(".")
    }
}

/// Extract Go receiver type from method_declaration.
pub fn extract_go_receiver_type(node: Node, source: &str) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    node.child_by_field_name("receiver")
        .and_then(|recv| {
            let mut cursor = recv.walk();
            for child in recv.children(&mut cursor) {
                if child.kind() == "parameter_declaration" {
                    if let Some(typ) = child.child_by_field_name("type") {
                        if typ.kind() == "pointer_type" {
                            return typ.child_by_field_name("name")
                                .or_else(|| typ.child(0))
                                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                                .map(|s| s.to_string());
                        }
                        return typ.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
                    }
                }
            }
            None
        })
}

/// Emit a captured call node, recording it in the current function's call list.
pub fn emit_call_for_node(node: Node, source: &str, units: &mut [ExtractedUnit],
                          current_function_idx: Option<usize>) {
    let idx = match current_function_idx {
        Some(i) => i,
        None => return,
    };
    let func = match units.get_mut(idx) {
        Some(ExtractedUnit::Function(ref mut f)) => f,
        _ => return,
    };
    let line = node.start_position().row + 1;
    let col = node.start_position().column as u32;

    if node.kind() == "method_invocation" {
        let method_name = node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();
        let object_name = node.child_by_field_name("object")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .to_string();
        let path = if object_name.is_empty() { vec![] } else { vec![object_name] };
        func.calls.push(UnresolvedRef { name: method_name, path, line, col: col as usize });
        return;
    }

    let name_node = node.child_by_field_name("function")
        .or_else(|| node.child_by_field_name("method"))
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("callee"));

    match name_node {
        Some(n) if n.kind() == "identifier" => {
            let name = n.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            if !is_stoplisted(&name) {
                func.calls.push(UnresolvedRef { name, path: vec![], line, col: col as usize });
            }
        }
        Some(n) if n.kind() == "attribute"
            || n.kind() == "field_expression"
            || n.kind() == "member_expression"
            || n.kind() == "selector_expression"
            || n.kind() == "member_access_expression"
            || n.kind() == "chained_method_call"
            || n.kind() == "call" => {
            let method_field = if n.kind() == "attribute" { "attribute" }
                else if n.kind() == "field_expression" { "field" }
                else if n.kind() == "member_expression" { "property" }
                else if n.kind() == "call" { "method" }
                else if n.kind() == "chained_method_call" { "method" }
                else if n.kind() == "member_access_expression" { "name" }
                else { "field" };
            let object_field = if n.kind() == "attribute" { "object" }
                else if n.kind() == "field_expression" { "value" }
                else if n.kind() == "member_expression" { "object" }
                else if n.kind() == "call" { "receiver" }
                else if n.kind() == "chained_method_call" { "receiver" }
                else if n.kind() == "member_access_expression" { "expression" }
                else { "operand" };
            let method = n.child_by_field_name(method_field)
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();
            let object = n.child_by_field_name(object_field)
                .and_then(|c| {
                    if is_literal_receiver(c.kind()) { return None; }
                    c.utf8_text(source.as_bytes()).ok()
                })
                .unwrap_or("")
                .to_string();
            if !is_stoplisted(&method) {
                let path = if object.is_empty() { vec![] } else { vec![object] };
                func.calls.push(UnresolvedRef { name: method, path, line, col: col as usize });
            }
        }
        _ => {
            if node.kind() == "FnCallArguments" {
                if let Some(parent) = node.parent() {
                    if parent.kind() == "SuffixExpr" {
                        let zig_name = parent
                            .child_by_field_name("variable_type_function")
                            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                            .unwrap_or("")
                            .to_string();
                        if !zig_name.is_empty() && !is_stoplisted(&zig_name) {
                            func.calls.push(UnresolvedRef { name: zig_name, path: vec![], line, col: col as usize });
                        }
                    }
                }
            }
        }
    }
}

// ── End extracted emit_* helpers ───────────────────────────────────────────

/// Determine the FunctionKind from decorators and method status.
pub fn derive_function_kind(decorators: &[String], is_method: bool) -> FunctionKind {
    if !is_method {
        return FunctionKind::Free;
    }
    for dec in decorators {
        match dec.as_str() {
            "@staticmethod" => return FunctionKind::StaticMethod,
            "@classmethod" => return FunctionKind::ClassMethod,
            "@property" => return FunctionKind::Property,
            "@abstractmethod" => return FunctionKind::AbstractMethod,
            "@functools.cached_property" => return FunctionKind::CachedProperty,
            other if other.ends_with(".setter") => return FunctionKind::PropertySetter,
            other if other.ends_with(".deleter") => return FunctionKind::PropertyDeleter,
            _ => {}
        }
    }
    FunctionKind::Method
}

/// Build a dotted qualified name from the stack context.
fn build_qualified_name(stack: &[WalkFrame], name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for frame in stack {
        if !frame.qualified.is_empty() {
            parts.push(&frame.qualified);
        }
    }
    parts.push(name);
    parts.join(".")
}

/// Build a CodeRadar entity ID: "file_path::qualified_name"
pub fn make_entity_id(file_path: &str, qualified_name: &str) -> String {
    if qualified_name.is_empty() {
        file_path.to_string()
    } else {
        format!("{}::{}", file_path, qualified_name)
    }
}

/// Extract parameters from a function definition node.
///
/// Handles typed_parameter (x: int), default_parameter (x: int = 5),
/// and simple identifier parameters. Extracts name, type annotation,
/// and default value independently.
pub fn extract_parameters(node: Node, source: &str) -> Vec<Parameter> {
    let params_node = node.child_by_field_name("parameters");
    let mut params = Vec::new();

    if let Some(p_node) = params_node {
        let mut cursor = p_node.walk();
        for child in p_node.children(&mut cursor) {
            let kind = child.kind();
            if kind == "identifier" {
                let name = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                if !name.is_empty() && name != "," && name != "(" && name != ")" {
                    params.push(Parameter {
                        name,
                        annotation: None,
                        default_value: None,
                        is_varargs: false,
                        is_kwargs: false,
                        is_positional_only: false,
                        is_keyword_only: false,
                    });
                }
            } else if kind == "typed_parameter" || kind == "default_parameter"
                || kind == "typed_default_parameter" || kind == "keyword_argument"
            {
                let name = child
                    .child_by_field_name("name")
                    .or_else(|| {
                        // Some grammars use (identifier) as first child
                        child.children(&mut child.walk())
                            .find(|ch| ch.kind() == "identifier")
                    })
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .to_string();

                // Extract type annotation from the "type" field
                let annotation = child
                    .child_by_field_name("type")
                    .and_then(|t| {
                        let type_text = t.utf8_text(source.as_bytes()).unwrap_or("");
                        if !type_text.is_empty() && !is_builtin_type(type_text) {
                            Some(type_text.to_string())
                        } else {
                            None
                        }
                    });

                // Extract default value
                let default_value = child
                    .child_by_field_name("value")
                    .or_else(|| child.child_by_field_name("default"))
                    .and_then(|v| {
                        let val = v.utf8_text(source.as_bytes()).unwrap_or("");
                        if !val.is_empty() {
                            Some(val.to_string())
                        } else {
                            None
                        }
                    });

                if !name.is_empty() && name != "," && name != "(" && name != ")" {
                    params.push(Parameter {
                        name,
                        annotation,
                        default_value,
                        is_varargs: false,
                        is_kwargs: false,
                        is_positional_only: false,
                        is_keyword_only: false,
                    });
                }
            }
        }
    }

    params
}
