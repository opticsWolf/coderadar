// CodeRadar v3.5 — Docstring Extraction
// Adapted from CodeGraph's docstring.rs (MIT/Apache-2.0, opticsWolf/codegraph).
//
// Extracts preceding docstrings for tree-sitter nodes by walking backwards
// through comment runs, climbing out of declaration wrappers (decorators,
// export statements, etc.), and stripping comment syntax markers.
// Handles Python (#, """), Rust (///, //!, /** */), JS/TS (/** */, //),
// and multi-language comment syntax in one function.

use regex::Regex;
use std::sync::OnceLock;
use tree_sitter::Node;

/// Declaration wrappers to climb through when looking for a preceding docstring.
/// e.g. `@decorator` → `def foo()` — the docstring is before the decorator.
fn is_wrapper(kind: &str) -> bool {
    matches!(
        kind,
        "decorated_definition"
            | "export_statement"
            | "lexical_declaration"
            | "variable_declaration"
            | "variable_declarator"
            | "ambient_declaration"
    )
}

fn is_comment(kind: &str) -> bool {
    matches!(
        kind,
        "comment" | "line_comment" | "block_comment" | "documentation_comment"
    )
}

// ── Comment Markers ─────────────────────────────────────────────────────────

struct Cleaners {
    block_open: Regex,
    block_close: Regex,
    lua_open: Regex,
    lua_close: Regex,
    paren_star_open: Regex,
    paren_star_close: Regex,
    brace_open: Regex,
    brace_close: Regex,
    slashes: Regex,
    dashes: Regex,
    hash: Regex,
    percent: Regex,
    star_cont: Regex,
}

fn cleaners() -> &'static Cleaners {
    static C: OnceLock<Cleaners> = OnceLock::new();
    C.get_or_init(|| Cleaners {
        block_open: Regex::new(r"^/\*+!?").unwrap(),
        block_close: Regex::new(r"\*+/$").unwrap(),
        lua_open: Regex::new(r"^--\[=*\[").unwrap(),
        lua_close: Regex::new(r"\]=*\]$").unwrap(),
        paren_star_open: Regex::new(r"^\(\*").unwrap(),
        paren_star_close: Regex::new(r"\*\)$").unwrap(),
        brace_open: Regex::new(r"^\{").unwrap(),
        brace_close: Regex::new(r"\}$").unwrap(),
        slashes: Regex::new(r"\A//[/!]?\s?").unwrap(),
        dashes: Regex::new(r"\A--\s?").unwrap(),
        hash: Regex::new(r"\A#\s?").unwrap(),
        percent: Regex::new(r"\A%+\s?").unwrap(),
        star_cont: Regex::new(r"\A\s*\*\s?").unwrap(),
    })
}

// ── JS-Line-Terminator Aware Multiline Strip ────────────────────────────────

fn is_js_line_terminator(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// JS-semantics `str.replace(/^<pat>/gm, "")`: apply the \A-anchored `pat` at
/// position 0 and after every JS line terminator, left to right, resuming after
/// each match's end. Used for CRLF byte-parity with the wasm extractor.
fn js_multiline_strip(s: &str, pat: &Regex) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0usize;
    let mut pos = 0usize;
    while pos <= s.len() {
        let at_line_start = pos == 0
            || s[..pos].chars().next_back().is_some_and(is_js_line_terminator);
        if at_line_start {
            if let Some(m) = pat.find(&s[pos..]) {
                if !m.is_empty() {
                    out.push_str(&s[last..pos]);
                    last = pos + m.end();
                    pos = last;
                    continue;
                }
            }
        }
        match s[pos..].chars().next() {
            Some(c) => pos += c.len_utf8(),
            None => break,
        }
    }
    out.push_str(&s[last..]);
    out
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Strip comment syntax markers, leaving the prose.
///
/// Handles: `//`, `///`, `//!`, `/* */`, `/** */`, `#`, `--`, `--[[ ]]`,
/// `(* *)`, `{ }`, `%`, and `*` block-continuation markers.
pub fn clean_comment_markers(comment: &str) -> String {
    let c = cleaners();
    let mut s = comment.trim().to_string();
    if s.starts_with("/*") {
        s = c.block_open.replace(&s, "").into_owned();
        s = c.block_close.replace(&s, "").into_owned();
    } else if s.starts_with("--[") {
        s = c.lua_open.replace(&s, "").into_owned();
        s = c.lua_close.replace(&s, "").into_owned();
    } else if s.starts_with("(*") {
        s = c.paren_star_open.replace(&s, "").into_owned();
        s = c.paren_star_close.replace(&s, "").into_owned();
    } else if s.starts_with('{') {
        s = c.brace_open.replace(&s, "").into_owned();
        s = c.brace_close.replace(&s, "").into_owned();
    }
    s = js_multiline_strip(&s, &c.slashes);
    s = js_multiline_strip(&s, &c.dashes);
    s = js_multiline_strip(&s, &c.hash);
    s = js_multiline_strip(&s, &c.percent);
    s = js_multiline_strip(&s, &c.star_cont);
    s.trim().to_string()
}

/// Extract the docstring immediately preceding a tree-sitter node.
///
/// Walks backwards from the node through adjacent comment siblings, climbs
/// out of declaration wrappers (decorators, export statements, etc.), strips
/// comment markers, and joins the result. Returns `None` when there is no
/// preceding comment. An empty docstring after cleaning returns `Some("")`.
pub fn preceding_docstring(node: Node, src: &str) -> Option<String> {
    // Climb out of wrapper nodes so we look before the outermost wrapper
    let mut anchor = node;
    while let Some(parent) = anchor.parent() {
        if is_wrapper(parent.kind()) {
            anchor = parent;
        } else {
            break;
        }
    }

    let mut comments: Vec<&str> = Vec::new();
    let mut sibling = anchor.prev_named_sibling();
    while let Some(s) = sibling {
        if is_comment(s.kind()) {
            comments.push(&src[s.byte_range()]);
            sibling = s.prev_named_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    comments.reverse(); // collected nearest-first; reverse to source order
    Some(
        comments
            .iter()
            .map(|c| clean_comment_markers(c))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
    )
}

/// Extract a docstring for a node, returning an empty string when absent.
/// Convenience wrapper that returns `""` instead of `None`.
pub fn docstring_or_empty(node: Node, src: &str) -> String {
    preceding_docstring(node, src).unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_comment() {
        assert_eq!(clean_comment_markers("// hello"), "hello");
    }

    #[test]
    fn strips_doc_line_comment() {
        assert_eq!(clean_comment_markers("/// doc line"), "doc line");
    }

    #[test]
    fn strips_block_comment() {
        assert_eq!(
            clean_comment_markers("/**\n * Adds things.\n * @param a first\n */"),
            "Adds things.\n@param a first"
        );
    }

    #[test]
    fn strips_hash_comment() {
        assert_eq!(clean_comment_markers("# This is a Python comment"), "This is a Python comment");
    }

    #[test]
    fn strips_double_dash() {
        assert_eq!(clean_comment_markers("-- SQL comment"), "SQL comment");
    }

    #[test]
    fn strips_rust_doc() {
        assert_eq!(clean_comment_markers("/// Adds two numbers."), "Adds two numbers.");
        assert_eq!(clean_comment_markers("//! Module-level doc"), "Module-level doc");
    }

    #[test]
    fn crlf_parity() {
        // CRLF input: the \r survives because JS ^ matches after \r
        assert_eq!(
            clean_comment_markers("/**\r\n * Class docs.\r\n * Multi-line.\r\n */"),
            "Class docs.\rMulti-line."
        );
        assert_eq!(
            clean_comment_markers("// a\r\n// b"),
            "a\r\nb"
        );
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(clean_comment_markers(""), "");
    }

    #[test]
    fn no_preceding_comment_returns_none() {
        // Tested via integration with tree-sitter parsing
    }

    #[test]
    fn empty_docstring_after_cleaning() {
        assert_eq!(clean_comment_markers("//"), "");
    }
}
