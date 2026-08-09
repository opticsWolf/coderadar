// CodeRadar v3.5 — Extraction: Hierarchy Walker Pass 2 (§4.2)
// Typed stack-frame walker that traverses the tagged tree and emits ExtractedUnits.
// v3.5: EntityId-based identities; file_path required for building entity IDs.
// v3.5a: Call capture — Tag::Call/Tag::CallReceiver attach calls to current function.

use tree_sitter::Node;

use crate::types::*;

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
    };

    ctx.stack.push(WalkFrame {
        qualified: String::new(),
        kind: FrameKind::Module,
    });
    walk_node(root_node, &mut ctx);

    ctx.units
}

fn walk_node(node: Node, ctx: &mut WalkContext) {
    let pushed = if let Some(info) = ctx.tags.tags.get(&(node.id() as usize)) {
        emit_for_node(node, info, ctx)
    } else {
        None
    };

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

fn emit_for_node(node: Node, info: &TagInfo, ctx: &mut WalkContext) -> Option<FrameKind> {
    let source = ctx.tags.source;

    match info.tag {
        Tag::Class => {
            let name_node = node.child_by_field_name("name");
            // Go: type_declaration has (type_spec name: (type_identifier)) child
            let name = if name_node.is_some() && name_node.unwrap().kind() != "" {
                name_node
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .to_string()
            } else if node.kind() == "type_declaration" {
                // Go: walk into type_spec → name
                node.child_by_field_name("type")
                    .and_then(|ts| ts.child_by_field_name("name"))
                    .or_else(|| {
                        // Try direct child
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
                    .to_string()
            } else if node.kind() == "class_declaration" || node.kind() == "object_declaration" {
                // Kotlin: name child is type_identifier or simple_identifier
                let mut cursor = node.walk();
                let children: Vec<_> = node.children(&mut cursor).collect();
                children.iter().find(|ch|
                    ch.kind() == "type_identifier" || ch.kind() == "simple_identifier"
                )
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("")
                .to_string()
            } else {
                "".to_string()
            };

            let line = node.start_position().row + 1;
            let exit_line = node.end_position().row + 1;
            let spans = extract_byte_spans(node);
            let qualified_name = build_qualified_name(&ctx.stack, &name);
            let entity_id = make_entity_id(&ctx.file_path, &qualified_name);

            // Extract base classes (Python/Ruby/Java/C#/etc.)
            let bases: Vec<UnresolvedRef> = node
                .child_by_field_name("superclasses")  // Python
                .or_else(|| node.child_by_field_name("superclass"))  // Java-style
                .or_else(|| node.child_by_field_name("bases"))  // C#
                .map(|sc| {
                    let mut cursor = sc.walk();
                    sc.children(&mut cursor)
                        .filter(|child| child.kind() == "identifier"
                            || child.kind() == "type_identifier"
                            || child.kind() == "constant"  // Ruby
                            || child.kind() == "name")  // PHP
                        .filter_map(|id| {
                            id.utf8_text(source.as_bytes()).ok().map(|txt| UnresolvedRef {
                                name: txt.to_string(),
                                path: vec![],
                                line: id.start_position().row + 1,
                                col: id.start_position().column as usize,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            ctx.units.push(ExtractedUnit::Class(ExtractedClass {
                id: entity_id.clone(),
                name: name.clone(),
                qualified_name: qualified_name.clone(),
                parent_module: entity_id.clone(), // patched later
                parent_class: None,
                bases,
                decorators: Vec::new(),
                docstring: None,
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
            let name_node = node.child_by_field_name("name");
            // C++: function_definition has declarator → function_declarator → identifier
            let name = if name_node.is_some() {
                name_node
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .to_string()
            } else if node.kind() == "function_definition" {
                // Walk declarator chain to find the identifier
                node.child_by_field_name("declarator")
                    .and_then(|d| d.child_by_field_name("declarator"))
                    .and_then(|id| id.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .to_string()
            } else if node.kind() == "function_declaration" {
                // Kotlin: name is simple_identifier
                let mut cursor = node.walk();
                let children: Vec<_> = node.children(&mut cursor).collect();
                children.iter().find(|ch|
                    ch.kind() == "simple_identifier" || ch.kind() == "identifier"
                )
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("")
                .to_string()
            } else {
                "".to_string()
            };

            // Go: method_declaration has a receiver (e.g., `func (d *Dog) Bark()`)
            // Extract receiver type as the parent_class
            let go_receiver_type: Option<String> = if node.kind() == "method_declaration" {
                node.child_by_field_name("receiver")
                    .and_then(|recv| {
                        // receiver is a parameter_list → parameter_declaration
                        let mut cursor = recv.walk();
                        for child in recv.children(&mut cursor) {
                            if child.kind() == "parameter_declaration" {
                                // Get the type from: pointer_type → type_identifier,
                                // or type_identifier directly
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
            } else {
                None
            };

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

            // Track this function for call capture
            ctx.current_function_idx = Some(ctx.units.len());

            ctx.units.push(ExtractedUnit::Function(ExtractedFunction {
                id: entity_id.clone(),
                name: name.clone(),
                qualified_name: qualified_name.clone(),
                parent_module: entity_id.clone(), // patched later
                parent_class: parent_class.clone().map(|q| make_entity_id(&ctx.file_path, &q)),
                parameters: params,
                return_type: None,
                calls: Vec::new(),
                decorators: Vec::new(),
                docstring: None,
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
            // The call tag fires on (call) nodes. For simple calls like `foo()`,
            // the function child is an identifier with the name.
            // For dotted calls like `obj.method()`, the function child is an
            // attribute node — the CallReceiver tag fires first on `obj`,
            // then Call fires on `obj.method`, and the name is the method part.
            if let Some(idx) = ctx.current_function_idx {
                if let Some(ExtractedUnit::Function(ref mut func)) = ctx.units.get_mut(idx) {
                    let name_node = node.child_by_field_name("function")
                        .or_else(|| node.child_by_field_name("method"))
                        .or_else(|| node.child_by_field_name("name"))
                        .or_else(|| node.child_by_field_name("callee")); // Kotlin
                    let line = node.start_position().row + 1;
                    let col = node.start_position().column as u32;

                    // Java: method_invocation — name/object are direct children
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
                        func.calls.push(UnresolvedRef {
                            name: method_name,
                            path,
                            line,
                            col: col as usize,
                        });
                        return None;
                    }

                    match name_node {
                        // Simple call: `foo(x)` — name_node is (identifier)
                        Some(n) if n.kind() == "identifier" => {
                            let name = n.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                            func.calls.push(UnresolvedRef {
                                name,
                                path: vec![],
                                line,
                                col: col as usize,
                            });
                        }
                        // Dotted call: `obj.method(x)` — name_node is (attribute) [Python]
                        // or `obj.method(x)` — name_node is (field_expression) [Rust/C++]
                        // or `obj.method(x)` — name_node is (member_expression) [TypeScript]
                        // or `obj.method(x)` — name_node is (selector_expression) [Go]
                        // or `obj.method(x)` — name_node is (method_invocation) [Java]
                        Some(n) if n.kind() == "attribute"
                            || n.kind() == "field_expression"
                            || n.kind() == "member_expression"
                            || n.kind() == "selector_expression"
                            || n.kind() == "member_access_expression"
                            || n.kind() == "chained_method_call"
                            || n.kind() == "call" => {
                            // Ruby: chained method call or call with explicit receiver
                            // C#: member_access_expression (obj.Method())
                            // PHP: member_call_expression is handled by method= field above
                            let method_field = if n.kind() == "attribute" { "attribute" }
                                else if n.kind() == "field_expression" { "field" }
                                else if n.kind() == "member_expression" { "property" }
                                else if n.kind() == "call" { "method" }
                                else if n.kind() == "chained_method_call" { "method" }
                                else if n.kind() == "member_access_expression" { "name" }
                                else { "field" }; // selector_expression [Go]
                            let object_field = if n.kind() == "attribute" { "object" }
                                else if n.kind() == "field_expression" { "value" }
                                else if n.kind() == "member_expression" { "object" }
                                else if n.kind() == "call" { "receiver" }
                                else if n.kind() == "chained_method_call" { "receiver" }
                                else if n.kind() == "member_access_expression" { "expression" }
                                else { "operand" }; // selector_expression [Go]

                            let method = n
                                .child_by_field_name(method_field)
                                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                                .unwrap_or("")
                                .to_string();
                            let object = n
                                .child_by_field_name(object_field)
                                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                                .unwrap_or("")
                                .to_string();

                            let path = if object.is_empty() { vec![] } else { vec![object] };
                            func.calls.push(UnresolvedRef {
                                name: method,
                                path,
                                line,
                                col: col as usize,
                            });
                        }
                        _ => {}
                    }
                }
            }
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
fn parse_import_statement(node: Node, source: &str) -> ImportKind {
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
fn parse_import_from_statement(node: Node, source: &str) -> ImportKind {
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
fn extract_parameters(node: Node, source: &str) -> Vec<Parameter> {
    let params_node = node.child_by_field_name("parameters");
    let mut params = Vec::new();

    if let Some(p_node) = params_node {
        let mut cursor = p_node.walk();
        for child in p_node.children(&mut cursor) {
            let kind = child.kind();
            if kind == "identifier" || kind == "typed_parameter" || kind == "default_parameter" {
                let name = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                if name != "," && name != "(" && name != ")" {
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
            }
        }
    }

    params
}
