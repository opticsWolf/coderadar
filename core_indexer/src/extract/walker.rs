// CodeRadar v3.3 — Extraction: Hierarchy Walker Pass 2 (§4.2)
// Typed stack-frame walker that traverses the tagged tree and emits ExtractedUnits.

use tree_sitter::Node;

use crate::types::*;

use super::spans::extract_byte_spans;
use super::spans::SpanExtractor;

/// Walk the tagged tree and produce a list of ExtractedUnits.
/// The root_node comes from a tree-sitter Tree already parsed by the caller.
pub fn walk_and_extract<'a>(tagged: &'a TaggedTree<'a>, root_node: Node<'a>) -> Vec<ExtractedUnit> {
    let mut ctx = WalkContext {
        tags: tagged,
        units: Vec::new(),
        stack: Vec::new(),
    };

    // Begin with a module-level frame
    ctx.stack.push(WalkFrame {
        qualified: String::new(),
        kind: FrameKind::Module,
    });
    walk_node(root_node, &mut ctx);

    ctx.units
}

/// Recursively walk a tree-sitter node, emitting units for tagged nodes.
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

    // Pop ONLY frames this invocation pushed — fixes the v1 over-pop bug.
    if let Some(frame_kind) = pushed {
        let popped = ctx.stack.pop();
        debug_assert!(
            popped
                .map(|f| std::mem::discriminant(&f.kind)
                    == std::mem::discriminant(&frame_kind))
                .unwrap_or(true),
            "Frame stack mismatch"
        );
    }
}

/// Emit the appropriate ExtractedUnit for a tagged node.
fn emit_for_node(node: Node, info: &TagInfo, ctx: &mut WalkContext) -> Option<FrameKind> {
    let source = ctx.tags.source;

    match info.tag {
        Tag::Class => {
            let name_node = node.child_by_field_name("name");
            let name = name_node
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();

            let line = node.start_position().row + 1;
            let exit_line = node.end_position().row + 1;
            let spans = extract_byte_spans(node);

            let qualified_name =
                build_qualified_name(&ctx.stack, &name);

            ctx.units.push(ExtractedUnit::Class(ExtractedClass {
                name: name.clone(),
                qualified_name: qualified_name.clone(),
                parent_module: None,
                parent_class: None,
                bases: Vec::new(),
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
                kind: FrameKind::Class(ClassId::null()),
            });
            Some(FrameKind::Class(ClassId::null()))
        }

        Tag::Function => {
            let name_node = node.child_by_field_name("name");
            let name = name_node
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();

            // is_method tests the IMMEDIATE parent frame's kind — not stack depth
            let is_method = matches!(
                ctx.stack.last(),
                Some(WalkFrame {
                    kind: FrameKind::Class(_),
                    ..
                })
            );

            let kind = derive_function_kind(&[], is_method);

            let line = node.start_position().row + 1;
            let exit_line = node.end_position().row + 1;
            let spans = extract_byte_spans(node);
            let params = extract_parameters(node, source);

            let qualified_name = build_qualified_name(&ctx.stack, &name);

            ctx.units.push(ExtractedUnit::Function(ExtractedFunction {
                name: name.clone(),
                qualified_name: qualified_name.clone(),
                parent_module: None,
                parent_class: None,
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

            let kind = if text.starts_with("from ") {
                ImportKind::FromImport {
                    module: String::new(),
                    names: Vec::new(),
                }
            } else if text.contains("import *") {
                ImportKind::StarImport {
                    module: String::new(),
                }
            } else {
                ImportKind::ModuleImport {
                    module: String::new(),
                    alias: None,
                }
            };

            ctx.units.push(ExtractedUnit::Import(ExtractedImport {
                raw: text,
                kind,
                line,
                is_type_only: false,
                name_span,
            }));
            None
        }

        Tag::Call
        | Tag::Docstring
        | Tag::Field
        | Tag::ClassBase
        | Tag::FunctionParam
        | Tag::FunctionReturn
        | Tag::Decorator
        | Tag::CallReceiver
        | Tag::ImportFromClause
        | Tag::ImportSpecifier => None,
    }
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
