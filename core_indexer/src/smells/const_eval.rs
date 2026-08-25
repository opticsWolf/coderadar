// CodeRadar Stage 6.2 — statically-decided conditions (dead branches).
//
// Fossil reference: `src/graph/constant_prop.rs` (1,924 LOC) +
// `expr_evaluator.rs` (1,075 LOC), MIT OR Apache-2.0 — interprocedural
// constant propagation with environments. Deliberately NOT ported (plan
// §11.2 scope-down): we evaluate only boolean literals and literal-vs-
// literal comparisons, optionally under not/!/and/or, directly on condition
// expressions. Everything else evaluates to None = "unknown", and unknowns
// never produce findings.
//
// Short-circuit awareness is kept where it is free: `True or <anything>` is
// decided even when the other operand is unknown.

use tree_sitter::Node;

/// Max parser recursion depth. Conditions are shallow; this only guards
/// against pathological nesting.
const MAX_DEPTH: u32 = 32;

#[derive(Debug, Clone, PartialEq)]
enum Lit {
    Bool(bool),
    Int(i128),
    Str(String),
}

impl Lit {
    fn as_bool(&self) -> Option<bool> {
        match self {
            Lit::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Evaluate a condition expression's source text to a definite truth value.
pub(crate) fn eval_constant(raw: &str) -> Option<bool> {
    let tokens = lex(raw.trim());
    let mut pos = 0usize;
    let v = parse_or(&tokens, &mut pos, 0)?;
    // Fully consumed? Trailing garbage means we mis-parsed; decline.
    if pos != tokens.len() {
        return None;
    }
    v
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Not,
    And,
    Or,
    Cmp(&'static str),
    Lit(Lit),
    /// An operand whose value cannot be known from source text alone
    /// (identifier, call, attribute...).
    Unknown,
}

fn lex(src: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'!' if i + 1 < b.len() && b[i + 1] == b'=' => {
                out.push(Tok::Cmp("!="));
                i += 2;
            }
            b'!' => {
                out.push(Tok::Not);
                i += 1;
            }
            b'&' if i + 1 < b.len() && b[i + 1] == b'&' => {
                out.push(Tok::And);
                i += 2;
            }
            b'|' if i + 1 < b.len() && b[i + 1] == b'|' => {
                out.push(Tok::Or);
                i += 2;
            }
            b'=' if i + 1 < b.len() && b[i + 1] == b'=' => {
                // === folds into == after one extra char.
                if i + 2 < b.len() && b[i + 2] == b'=' {
                    out.push(Tok::Cmp("=="));
                    i += 3;
                } else {
                    out.push(Tok::Cmp("=="));
                    i += 2;
                }
            }
            b'<' | b'>' => {
                let op = if i + 1 < b.len() && b[i + 1] == b'=' {
                    if b[i] == b'<' {
                        "<="
                    } else {
                        ">="
                    }
                } else if b[i] == b'<' {
                    "<"
                } else {
                    ">"
                };
                out.push(Tok::Cmp(op));
                i += if src[i..].starts_with("<=") || src[i..].starts_with(">=") {
                    2
                } else {
                    1
                };
            }
            _ => {
                let rest = &src[i..];
                let word_len = rest
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                let word = &rest[..word_len];
                if word.is_empty() {
                    // String literal?
                    let q = b[i];
                    if q == b'"' || q == b'\'' {
                        if let Some(close) = src[i + 1..].find(q as char) {
                            out.push(Tok::Lit(Lit::Str(src[i + 1..i + 1 + close].into())));
                            i += close + 2;
                            continue;
                        }
                    }
                    out.push(Tok::Unknown);
                    i += 1;
                    continue;
                }
                match word {
                    "true" | "True" => out.push(Tok::Lit(Lit::Bool(true))),
                    "false" | "False" => out.push(Tok::Lit(Lit::Bool(false))),
                    "not" => out.push(Tok::Not),
                    "and" => out.push(Tok::And),
                    "or" => out.push(Tok::Or),
                    "!=" => out.push(Tok::Cmp("!=")),
                    w if w.chars().all(|c| c.is_ascii_digit() || c == '_') => {
                        let cleaned = w.replace('_', "");
                        let val = if let Some(hex) = cleaned.strip_prefix("0x") {
                            i128::from_str_radix(hex, 16).ok()
                        } else {
                            cleaned.parse::<i128>().ok()
                        };
                        match val {
                            Some(v) => out.push(Tok::Lit(Lit::Int(v))),
                            None => out.push(Tok::Unknown),
                        }
                    }
                    _ => out.push(Tok::Unknown),
                }
                i += word_len;
            }
        }
    }
    out
}

/// Parse levels return Option<Option<bool>>: outer None = parse failure
/// (decline entirely), inner None = a syntactically valid operand whose
/// value is unknowable from literals (identifier, call, anything opaque).
/// Unknown propagates honestly; a decided operand short-circuits it.
fn parse_or(toks: &[Tok], pos: &mut usize, depth: u32) -> Option<Option<bool>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut left = parse_and(toks, pos, depth + 1)?;
    while matches!(toks.get(*pos), Some(Tok::Or)) {
        *pos += 1;
        let right = parse_and(toks, pos, depth + 1)?;
        left = match (left, right) {
            // `True or x` is decided regardless of x.
            (Some(false), r) => r,
            (l, Some(false)) => l,
            _ => Some(true),
        };
    }
    Some(left)
}

fn parse_and(toks: &[Tok], pos: &mut usize, depth: u32) -> Option<Option<bool>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut left = parse_not(toks, pos, depth + 1)?;
    while matches!(toks.get(*pos), Some(Tok::And)) {
        *pos += 1;
        let right = parse_not(toks, pos, depth + 1)?;
        left = match (left, right) {
            // `False and x` is decided regardless of x.
            (Some(true), r) => r,
            (l, Some(true)) => l,
            _ => Some(false),
        };
    }
    Some(left)
}

fn parse_not(toks: &[Tok], pos: &mut usize, depth: u32) -> Option<Option<bool>> {
    if depth > MAX_DEPTH {
        return None;
    }
    if matches!(toks.get(*pos), Some(Tok::Not)) {
        *pos += 1;
        return parse_not(toks, pos, depth + 1).map(|v| v.map(|b| !b));
    }
    parse_cmp(toks, pos, depth + 1)
}

fn parse_cmp(toks: &[Tok], pos: &mut usize, depth: u32) -> Option<Option<bool>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let left = parse_primary(toks, pos, depth + 1)?;
    if let Some(Tok::Cmp(op)) = toks.get(*pos) {
        let op = *op;
        *pos += 1;
        let right = parse_primary(toks, pos, depth + 1)?;
        return match (left, right) {
            (Some(l), Some(r)) => Some(compare(l, r, op)),
            // Comparing anything opaque keeps the condition undecided.
            _ => Some(None),
        };
    }
    Some(left.and_then(|l| l.as_bool()))
}

/// An operand resolves to `Some(Some(Lit))` (a literal), `Some(None)` (an
/// opaque token — identifier, call, attribute — consumed so the surrounding
/// structure still parses, value unknown), or `None` (lex poison / parse
/// garbage — declines the whole expression).
fn parse_primary(toks: &[Tok], pos: &mut usize, depth: u32) -> Option<Option<Lit>> {
    if depth > MAX_DEPTH {
        return None;
    }
    match toks.get(*pos)? {
        Tok::LParen => {
            *pos += 1;
            let v = parse_or(toks, pos, depth + 1)?;
            if !matches!(toks.get(*pos), Some(Tok::RParen)) {
                return None;
            }
            *pos += 1;
            Some(v.map(Lit::Bool))
        }
        Tok::Lit(l) => {
            *pos += 1;
            Some(Some(l.clone()))
        }
        // Identifier, call, attribute...: consume so surrounding structure
        // parses; value unknown.
        Tok::Unknown => {
            *pos += 1;
            Some(None)
        }
        _ => {
            *pos += 1;
            Some(None)
        }
    }
}

fn compare(a: Lit, b: Lit, op: &str) -> Option<bool> {
    if std::mem::discriminant(&a) != std::mem::discriminant(&b) {
        // Cross-type comparisons are never decided (Python's `1 == True`
        // is True, JS loose equality disagrees — no safe answer).
        return None;
    }
    let ord = match (&a, &b) {
        (Lit::Int(x), Lit::Int(y)) => x.partial_cmp(y),
        (Lit::Str(x), Lit::Str(y)) => x.partial_cmp(y),
        (Lit::Bool(x), Lit::Bool(y)) => x.partial_cmp(y),
        _ => None,
    };
    ord.map(|ord| match op {
        "==" | "===" => ord == std::cmp::Ordering::Equal,
        "!=" | "!==" => ord != std::cmp::Ordering::Equal,
        "<" => ord == std::cmp::Ordering::Less,
        "<=" => ord != std::cmp::Ordering::Greater,
        ">" => ord == std::cmp::Ordering::Greater,
        ">=" => ord != std::cmp::Ordering::Less,
        _ => false,
    })
}
/// Count conditions in `body` that are statically decided (always-true or
/// always-false), walking decision nodes iteratively. Returns `None` when
/// nothing is decidable — callers must degrade honestly rather than guess.
pub(crate) fn count_decided_conditions(body: Node, source: &str) -> Option<usize> {
    const CONDITION_KINDS: &[&str] = &[
        "if_statement",
        "if_expression",
        "conditional_expression",
        "ternary_expression",
        "while_statement",
        "while_expression",
    ];

    let mut count = 0usize;
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if CONDITION_KINDS.contains(&node.kind()) {
            // The condition is the first named child across every grammar we
            // support (Python: expression before block; JS: parenthesized
            // expression; Rust/Go/Java: expression before block).
            if let Some(cond) = node.children(&mut node.walk()).find(|c| c.is_named()) {
                let text = &source[cond.start_byte()..cond.end_byte()];
                if eval_constant(text).is_some() {
                    count += 1;
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    if count == 0 {
        None
    } else {
        Some(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Language;

    fn eval(s: &str) -> Option<bool> {
        eval_constant(s)
    }

    #[test]
    fn boolean_literals() {
        assert_eq!(eval("True"), Some(true));
        assert_eq!(eval("False"), Some(false));
        assert_eq!(eval("true"), Some(true));
        assert_eq!(eval("false"), Some(false));
    }

    #[test]
    fn literal_comparisons() {
        assert_eq!(eval("1 > 2"), Some(false));
        assert_eq!(eval("2 >= 2"), Some(true));
        assert_eq!(eval("'a' == 'a'"), Some(true));
        assert_eq!(eval("\"x\" != \"y\""), Some(true));
        assert_eq!(eval("1 == True"), None, "cross-type stays unknown");
    }

    #[test]
    fn negation_and_parens() {
        assert_eq!(eval("not True"), Some(false));
        assert_eq!(eval("!false"), Some(true));
        assert_eq!(eval("(1 > 2)"), Some(false));
        assert_eq!(eval("not (2 == 2)"), Some(false));
    }

    #[test]
    fn short_circuit_decides_unknown_operand() {
        // The free wins: one decided operand settles the whole condition.
        assert_eq!(eval("True or flag"), Some(true));
        assert_eq!(eval("False and flag"), Some(false));
        assert_eq!(eval("flag or False"), None);
        assert_eq!(eval("flag and True"), None);
    }

    #[test]
    fn unknowns_stay_unknown() {
        assert_eq!(eval("x"), None);
        assert_eq!(eval("n > 0"), None);
        assert_eq!(eval("__debug__"), None);
        assert_eq!(eval("len(items) == 0"), None);
    }

    #[test]
    fn counts_python_conditions_in_body() {
        use crate::types::Language;

        fn body_block<'a>(tree: &'a tree_sitter::Tree) -> tree_sitter::Node<'a> {
            let root = tree.root_node();
            let func = root
                .children(&mut root.walk())
                .find(|n| n.kind() == "function_definition")
                .expect("function");
            func.children(&mut func.walk())
                .find(|n| n.kind() == "block")
                .expect("block")
        }

        let ts_lang = crate::graph::CodeGraph::ts_language(&Language::Python).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();

        // Two decided conditions (`if True`, `while 1 > 2`); `if x` unknown.
        let src = "def f():
    if True:
        return 1
    while 1 > 2:
        break
    if x:
        return 3
";
        let tree = parser.parse(src, None).unwrap();
        assert_eq!(count_decided_conditions(body_block(&tree), src), Some(2));

        // A body with no decidable conditions reports honest absence.
        let src2 = "def g(x):
    if x:
        return 1
";
        let tree2 = parser.parse(src2, None).unwrap();
        assert_eq!(count_decided_conditions(body_block(&tree2), src2), None);
    }

}
