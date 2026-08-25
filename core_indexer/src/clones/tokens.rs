// CodeRadar Stage 2.1 — normalized token streams for clone comparison.
//
// Fossil reference: `src/clones/ir_tokenizer.rs` (replaces identifiers and
// literals with kind placeholders so `getUserEmail(u)` ≡ `fetchPhoneNumber(x)`).
//
// Two streams come out of one tree walk:
//   * RAW  — leaves hashed as (kind, text): comments dropped. Equal RAW
//            streams ⇒ Type-1 clone (identical up to layout/comments).
//   * NORM — identifiers → ID, literals → LIT: equal NORM streams ⇒ Type-2
//            clone (systematic renames).
//
// Vocabulary: small const sentinels for placeholder classes; everything else
// is xxh3 of `(kind, text)` folded to u32 — no string allocations.

use crate::types::Language;

pub const TOK_IDENT: u32 = 1;
pub const TOK_STR: u32 = 2;
pub const TOK_NUM: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Type-1 comparison: keep identifier/literal text.
    Raw,
    /// Type-2 comparison: replace identifiers/literals with placeholders.
    Normalized,
}

fn fold64(h: u64) -> u32 {
    ((h >> 32) ^ h) as u32
}

fn hash_leaf(kind: &str, text: &str) -> u32 {
    let mut buf = Vec::with_capacity(kind.len() + text.len() + 1);
    buf.extend_from_slice(kind.as_bytes());
    buf.push(0xFF);
    buf.extend_from_slice(text.as_bytes());
    fold64(xxhash_rust::xxh3::xxh3_64(&buf))
}

/// Normalize a function body into clone-comparable tokens.
pub fn tokenize_body(body: &str, lang: Language, mode: Mode) -> Vec<u32> {
    let ts_lang = match crate::graph::CodeGraph::ts_language(&lang) {
        Some(l) => l,
        None => return Vec::new(),
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(body, None) else {
        return Vec::new();
    };
    let src = body.as_bytes();
    let mut out = Vec::with_capacity(body.len() / 4);
    walk(tree.root_node(), src, mode, &mut out);
    out
}

fn walk(node: tree_sitter::Node, src: &[u8], mode: Mode, out: &mut Vec<u32>) {
    let kind = node.kind();
    // Comments never participate in clone identity — and they can be leaves.
    if kind.contains("comment") {
        return;
    }
    if node.child_count() == 0 {
        let text = node.utf8_text(src).unwrap_or("");
        let tok = match mode {
            Mode::Raw => hash_leaf(kind, text),
            Mode::Normalized => {
                if kind.contains("identifier") {
                    TOK_IDENT
                } else if kind.contains("string") {
                    TOK_STR
                } else if kind.contains("number") || kind == "integer" || kind == "float" {
                    TOK_NUM
                } else {
                    hash_leaf(kind, text)
                }
            }
        };
        out.push(tok);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, mode, out);
    }
}

/// Pack a token window into one u64 for k-shingling (k ≤ 8).
fn pack_window(w: &[u32]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for (i, t) in w.iter().enumerate() {
        h ^= (*t as u64).wrapping_mul(0x100000001b3).rotate_left(i as u32 * 7);
    }
    h
}

/// k-token shingles over a token stream, hashed to u64s.
pub fn shingles(tokens: &[u32], k: usize) -> impl Iterator<Item = u64> + '_ {
    tokens.windows(k.max(1)).map(pack_window)
}
