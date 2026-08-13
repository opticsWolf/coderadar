// AST-side metric computation (Phase 4.1).
//
// Computed during single-pass extraction (`extract/single_pass.rs`) where the
// tree-sitter node + source are in hand, then carried on `Function.metrics`
// (`types::FunctionMetrics`). The smell engine therefore needs no source
// re-parse.
//
// The decision-point set below is language-agnostic: it matches tree-sitter
// node kinds by name across Python, JS/TS, Rust, Go, Java, C#, Kotlin, etc.
// It is a deterministic approximation of McCabe cyclomatic complexity
// (1 + structural branch points), not an exact per-language count. Thresholds
// are coarse enough (10/20/47/50) that minor over/under-counting of exotic
// node kinds is immaterial.

use tree_sitter::Node;

use crate::types::FunctionMetrics;

/// Compute the three AST-derived function metrics in one walk.
pub fn compute_function_metrics(node: Node, source: &str) -> FunctionMetrics {
    FunctionMetrics {
        cyclomatic: cyclomatic_complexity(node),
        nesting_depth: nesting_depth(node),
        return_count: return_count(node, source),
    }
}

/// Cyclomatic complexity: 1 + number of control-flow decision points.
pub fn cyclomatic_complexity(node: Node) -> usize {
    1 + count_decision_points(node)
}

fn count_decision_points(node: Node) -> usize {
    let mut count = if is_decision_point(node.kind()) { 1 } else { 0 };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_decision_points(child);
    }
    count
}

/// Maximum control-flow nesting depth within the subtree (0 at the root).
pub fn nesting_depth(node: Node) -> usize {
    nesting_depth_inner(node, 0).0
}

fn nesting_depth_inner(node: Node, current: usize) -> (usize, usize) {
    let this = if is_nesting_node(node.kind()) { current + 1 } else { current };
    let mut max = this;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let (child_max, _) = nesting_depth_inner(child, this);
        if child_max > max {
            max = child_max;
        }
    }
    (max, this)
}

/// Number of `return` statements in the subtree.
pub fn return_count(node: Node, source: &str) -> usize {
    let mut count = 0;
    let kind = node.kind();
    if node.is_named()
        && (kind == "return_statement" || kind == "return_expression" || kind == "return")
    {
        count += 1;
    } else if kind == "control_transfer_statement" {
        // Swift represents `return`/`break`/`continue` uniformly; count only
        // the actual returns.
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            if text.trim_start().starts_with("return") {
                count += 1;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += return_count(child, source);
    }
    count
}

/// A decision point adds a linearly independent path (McCabe).
fn is_decision_point(kind: &str) -> bool {
    matches!(
        kind,
        // if / else-if chains
        "if_statement" | "if_expression" | "if_let_expression"
            | "elif_clause" | "else_if_clause" | "elseif_clause"
        // ternary / conditional
            | "ternary_expression" | "conditional_expression"
        // loops
            | "for_statement" | "for_in_statement" | "for_expression"
            | "for_range_expression" | "enhanced_for_statement"
            | "while_statement" | "while_expression" | "do_while_statement"
            | "loop_expression" | "loop_statement"
        // exception branches
            | "catch_clause" | "catch_expression" | "except_clause"
            | "rescue_clause" | "rescue" | "handler_clause"
        // match/switch/when arms (one branch each; the container is not counted)
            | "match_arm" | "switch_case" | "case_statement" | "case_clause"
            | "case_item" | "when_entry" | "when_clause"
    )
}

/// A nesting node adds a control-flow level (decision points + block scopes).
fn is_nesting_node(kind: &str) -> bool {
    is_decision_point(kind)
        || matches!(
            kind,
            "try_statement" | "try_expression" | "try_block"
                | "with_statement" | "with_clause"
                | "do_statement" | "do_block"
                | "else_clause" | "else_block" | "else_branch"
                | "finally_clause" | "finally_block"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::CodeGraph;
    use crate::types::Language;

    fn find_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_python_function_metrics() {
        let src = "def analyze(x):\n    if x > 0:\n        for i in range(x):\n            if i % 2 == 0:\n                print(i)\n    else:\n        return 0\n    return 1\n";
        let lang = CodeGraph::ts_language(&Language::Python).expect("python grammar available");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();
        let fnode = find_kind(root, "function_definition").expect("function node");
        let m = compute_function_metrics(fnode, src);
        assert_eq!(m.cyclomatic, 4, "if + for + if = 3 decision points → 1+3");
        assert_eq!(m.nesting_depth, 3, "if → for → if = 3 levels");
        assert_eq!(m.return_count, 2, "two return statements");
    }
}
