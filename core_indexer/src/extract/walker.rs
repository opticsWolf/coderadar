// CodeRadar — Extraction: node-level helpers.
//
// This was "Hierarchy Walker Pass 2": `walk_and_extract` plus the private
// walk tree it drove. single_pass.rs superseded that in v0.5.3 and imports
// only the leaf helpers, so the entry point and everything reachable only
// from it are gone. What remains parses one node at a time — import
// statements, class and function names, base classes, parameters — and has
// no traversal of its own.

use tree_sitter::Node;

use crate::types::*;


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

/// Parse an `import_statement` node: `import os`, `import os.path as p`
/// Parse an `import_statement` node. Handles bare `import foo` /
/// `import foo as bar` plus the TS/JS forms `import { ... } from '...'`,
/// `import Foo from '...'`, and `import '...'`.
pub fn parse_import_statement(node: Node, source: &str) -> ImportKind {
    // TS/JS from-import: the module source is a `string` child.
    let mut find_cur = node.walk();
    if let Some(string_node) = node.children(&mut find_cur).find(|c| c.kind() == "string") {
        let module = strip_import_quotes(
            string_node.utf8_text(source.as_bytes()).unwrap_or(""),
        )
        .to_string();
        let names = collect_ts_import_names(node, source);
        let level = count_leading_dots(&module);
        if level > 0 {
            let module_after = if level < module.len() {
                module[level..].trim_start_matches('/').to_string()
            } else {
                String::new()
            };
            return ImportKind::RelativeImport { level, module: Some(module_after), names };
        }
        if names.is_empty() {
            // `import 'x'` — side-effect import, no symbols.
            return ImportKind::Side { module };
        }
        return ImportKind::FromImport { module, names };
    }

    // Bare `import foo` / `import foo as bar`.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "dotted_name" | "identifier" => {
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

/// Strip surrounding quotes from a `string` node's text (`'x'` → `x`).
fn strip_import_quotes(s: &str) -> &str {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 {
        let (first, last) = (b[0] as char, b[b.len() - 1] as char);
        if (first == '\'' && last == '\'') || (first == '"' && last == '"') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Collect `import { A, B as C, type D } from '...'` names (TS/JS).
fn collect_ts_import_names(node: Node, source: &str) -> Vec<(String, Option<String>)> {
    let mut names = Vec::new();
    collect_ts_import_names_rec(node, source, &mut names);
    names
}

fn collect_ts_import_names_rec(
    node: Node, source: &str, out: &mut Vec<(String, Option<String>)>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "import_specifier" => {
                let (name, alias) = import_specifier_name_alias(child, source);
                if !name.is_empty() {
                    out.push((name, alias));
                }
            }
            "named_imports" | "import_clause" => {
                collect_ts_import_names_rec(child, source, out);
            }
            "identifier" => {
                // Default import: `import Foo from 'x'`.
                let text = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                if !text.is_empty() {
                    out.push((text, None));
                }
            }
            _ => {}
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

/// Extract `name` + optional `alias` from an `import_specifier` node
/// (`Foo`, `Foo as Bar`, `type Foo`).
fn import_specifier_name_alias(node: Node, source: &str) -> (String, Option<String>) {
    let mut name = String::new();
    let mut alias = None;
    let mut seen_as = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "as" => seen_as = true,
            "type" => {} // type-only import keyword — ignore
            k if is_identifier_kind(k) => {
                let text = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                if seen_as {
                    alias = Some(text);
                } else if name.is_empty() {
                    name = text;
                }
            }
            _ => {}
        }
    }
    (name, alias)
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
