// CodeRadar Stage 6.1 — exact tree-edit-distance verification for clone
// candidates.
//
// Fossil reference: `src/clones/apted.rs` (+ their Zhang-Shasha baseline in
// `tree_edit_distance.rs`, MIT OR Apache-2.0). We adopt their discipline —
// dual implementation with cross-validation tests — but not their structure:
// fossil's APTED path decomposition (~1,200 LOC) buys speed only on trees far
// larger than our verification cap, so we implement the classic exact
// Zhang-Shasha algorithm directly behind the `apted` name for continuity with
// the plan. Indexing is iterative (explicit stack, no recursion) — fossil's
// recursive filler overflowed on deep ASTs; ours can't.
//
// Role in the funnel (plan §11.1): shingle-Jaccard candidates ≥ 0.85 get a
// second opinion from ordered structural TED, which sees statement ORDER that
// bag-of-shingles misses. Pairs below the similarity floor after TED are
// rejected; surviving groups report TED-derived similarity.

use crate::types::Language;

/// Hard cap on nodes per side. Beyond this the verifier declines and the
/// caller falls back to shingle Jaccard — O(n²m²) is fine precisely because
/// of the cap.
pub const MAX_TREE_NODES: usize = 512;

/// A labeled ordered tree over tree-sitter node KINDS. Every named node
/// participates (leaves included), which makes the view structural yet
/// rename-blind: two identifiers are both just `identifier`, two literals
/// differ only by kind (`integer` vs `string`), so Type-2 renames cost
/// nothing — while statement ORDER and control-flow shape are fully visible,
/// which is exactly what bag-of-shingles cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledTree {
    pub label: String,
    pub children: Vec<LabeledTree>,
}

impl LabeledTree {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            children: Vec::new(),
        }
    }

    pub fn with_children(label: impl Into<String>, children: Vec<LabeledTree>) -> Self {
        Self {
            label: label.into(),
            children,
        }
    }

    pub fn size(&self) -> usize {
        1 + self.children.iter().map(|c| c.size()).sum::<usize>()
    }
}

/// Exact ordered tree edit distance (Zhang-Shasha). Unit costs: insert 1,
/// delete 1, relabel 0/1.
pub fn apted_distance(a: &LabeledTree, b: &LabeledTree) -> usize {
    let ta = Indexed::of(a);
    let tb = Indexed::of(b);
    if ta.n == 0 || tb.n == 0 {
        return ta.n + tb.n;
    }

    // td[i][j] = distance between subtrees rooted at postorder i / j.
    let mut td = vec![vec![0usize; tb.n + 1]; ta.n + 1];

    for &x in &ta.key_roots {
        for &y in &tb.key_roots {
            let lx = ta.leftmost_leaf[x];
            let ly = tb.leftmost_leaf[y];
            let rows = x - lx + 2;
            let cols = y - ly + 2;
            let mut fd = vec![vec![0usize; cols]; rows];
            for i in 1..rows {
                fd[i][0] = fd[i - 1][0] + 1;
            }
            for j in 1..cols {
                fd[0][j] = fd[0][j - 1] + 1;
            }

            for i in 1..rows {
                for j in 1..cols {
                    let na = lx + i - 1;
                    let nb = ly + j - 1;
                    let cost = usize::from(ta.labels[na] != tb.labels[nb]);
                    if ta.leftmost_leaf[na] == lx && tb.leftmost_leaf[nb] == ly {
                        fd[i][j] = (fd[i - 1][j] + 1)
                            .min(fd[i][j - 1] + 1)
                            .min(fd[i - 1][j - 1] + cost);
                        td[na][nb] = fd[i][j];
                    } else {
                        let p = ta.leftmost_leaf[na] - lx;
                        let q = tb.leftmost_leaf[nb] - ly;
                        fd[i][j] = (fd[i - 1][j] + 1)
                            .min(fd[i][j - 1] + 1)
                            .min(fd[p][q] + td[na][nb]);
                    }
                }
            }
        }
    }
    td[ta.n][tb.n]
}

/// Similarity in 0..=1 derived from normalized edit distance
/// (1 − dist / max(size)).
pub fn apted_similarity(a: &LabeledTree, b: &LabeledTree) -> f64 {
    let denom = a.size().max(b.size());
    if denom == 0 {
        return 1.0;
    }
    1.0 - apted_distance(a, b) as f64 / denom as f64
}

/// Postorder index of a tree: kind labels, leftmost-leaf table, key roots.
/// Built with an explicit frame stack — no recursion.
struct Indexed {
    labels: Vec<String>,
    leftmost_leaf: Vec<usize>,
    key_roots: Vec<usize>,
    n: usize,
}

/// One pending subtree in the explicit postorder stack. The frame owns its
/// child iterator plus the slots of children already emitted — a child can
/// always hand its slot straight to the frame DIRECTLY BELOW it, because
/// that frame is guaranteed to be its parent's.
struct Frame<'a> {
    node: &'a LabeledTree,
    children: std::vec::IntoIter<&'a LabeledTree>,
    finished: Vec<usize>,
}

impl<'a> Frame<'a> {
    fn enter(node: &'a LabeledTree) -> Self {
        Self {
            node,
            children: node.children.iter().collect::<Vec<_>>().into_iter(),
            finished: Vec::with_capacity(node.children.len()),
        }
    }
}

impl Indexed {
    fn of(root: &LabeledTree) -> Self {
        let n = root.size();
        if n == 0 {
            return Self {
                labels: vec![String::new()],
                leftmost_leaf: vec![0],
                key_roots: Vec::new(),
                n: 0,
            };
        }

        let mut labels: Vec<String> = std::iter::repeat(String::new()).take(n + 1).collect();
        let mut leftmost_leaf = vec![0usize; n + 1];
        let mut parent = vec![0usize; n + 1];

        let mut stack: Vec<Frame> = vec![Frame::enter(root)];
        let mut idx = 1usize;
        while let Some(frame) = stack.last_mut() {
            if let Some(child) = frame.children.next() {
                stack.push(Frame::enter(child));
                continue;
            }
            let frame = stack.pop().expect("frame checked non-empty");
            let my = idx;
            idx += 1;
            labels[my] = frame.node.label.clone();
            // Postorder: each child's subtree occupies a contiguous slot
            // range ending just before the next sibling, so the FIRST
            // finished child holds the leftmost leaf.
            leftmost_leaf[my] = if frame.finished.is_empty() {
                my
            } else {
                leftmost_leaf[frame.finished[0]]
            };
            for ci in &frame.finished {
                parent[*ci] = my;
            }
            if let Some(parent_frame) = stack.last_mut() {
                parent_frame.finished.push(my);
            }
        }

        let mut key_roots = Vec::new();
        for i in 1..=n {
            if parent[i] == 0 || leftmost_leaf[i] != leftmost_leaf[parent[i]] {
                key_roots.push(i);
            }
        }
        key_roots.sort_unstable();

        Self {
            labels,
            leftmost_leaf,
            key_roots,
            n,
        }
    }
}

/// Build the structural tree for a function body: parse `src`, locate the
/// node spanning exactly `span`, project to kind labels with leaves pruned.
/// Returns `None` when parsing fails or the projection exceeds
/// [`MAX_TREE_NODES`] — callers fall back to shingle Jaccard rather than
/// guessing.
pub fn structural_tree(
    src: &str,
    lang: Language,
    span: crate::types::ByteSpan,
) -> Option<LabeledTree> {
    let ts_lang = crate::graph::CodeGraph::ts_language(&lang)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_lang).ok()?;
    let tree = parser.parse(src, None)?;

    // Locate the body node by exact byte-span match (iterative).
    let root = tree.root_node();
    let mut target = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_byte() == span.start && node.end_byte() == span.end {
            target = Some(node);
            break;
        }
        if node.end_byte() < span.end || node.start_byte() > span.start {
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    let target = target?;

    // Bottom-up projection with the same parent-directly-below discipline as
    // `Indexed::of`; every node participates, leaves are pruned afterwards
    // (keeps the MAX_TREE_NODES check exact over the full AST).
    struct PFrame<'a> {
        node: tree_sitter::Node<'a>,
        children: Vec<tree_sitter::Node<'a>>,
        next: usize,
        built: Vec<LabeledTree>,
    }
    let mut cursor = target.walk();
    let root_kids: Vec<tree_sitter::Node> = target.children(&mut cursor).collect();
    let mut pstack: Vec<PFrame> = vec![PFrame {
        node: target,
        children: root_kids,
        next: 0,
        built: Vec::new(),
    }];
    loop {
        let frame = pstack.last_mut().expect("projection stack never empties");
        if frame.next < frame.children.len() {
            let child = frame.children[frame.next];
            frame.next += 1;
            let mut ccursor = child.walk();
            let kid_kids: Vec<tree_sitter::Node> = child.children(&mut ccursor).collect();
            pstack.push(PFrame {
                node: child,
                children: kid_kids,
                next: 0,
                built: Vec::new(),
            });
            continue;
        }
        let frame = pstack.pop().expect("frame checked non-empty");
        let t = LabeledTree {
            label: frame.node.kind().to_string(),
            children: frame.built,
        };
        if t.size() > MAX_TREE_NODES {
            return None;
        }
        match pstack.last_mut() {
            Some(parent) => parent.built.push(t),
            None => return Some(t), // popped the target itself
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(label: &str) -> LabeledTree {
        LabeledTree::new(label)
    }

    /// Independent brute-force TED over forests — the Zhang-Shasha ground
    /// truth for tiny trees (fossil keeps the same dual-implementation
    /// discipline). Exponential; test trees stay under 10 nodes.
    fn brute_forest(s: &[&LabeledTree], t: &[&LabeledTree]) -> usize {
        match (s.split_first(), t.split_first()) {
            (None, None) => 0,
            (Some((a, rest_a)), None) => a.size() + brute_forest(rest_a, t),
            (None, Some((b, rest_b))) => b.size() + brute_forest(s, rest_b),
            (Some((a, rest_a)), Some((b, rest_b))) => {
                let del = a.size() + brute_forest(rest_a, t);
                let ins = b.size() + brute_forest(s, rest_b);
                let relabel_cost = usize::from(a.label != b.label);
                let a_kids: Vec<&LabeledTree> = a.children.iter().collect();
                let b_kids: Vec<&LabeledTree> = b.children.iter().collect();
                let match_both =
                    relabel_cost + brute_forest(&a_kids, &b_kids) + brute_forest(rest_a, rest_b);
                del.min(ins).min(match_both)
            }
        }
    }

    fn brute(a: &LabeledTree, b: &LabeledTree) -> usize {
        brute_forest(&[a], &[b])
    }

    // --- Fossil's basic correctness vectors ---

    #[test]
    fn identical_trees_distance_zero() {
        let a = LabeledTree::with_children("if", vec![n("assign"), n("return")]);
        assert_eq!(apted_distance(&a, &a), 0);
        assert_eq!(apted_similarity(&a, &a), 1.0);
    }

    #[test]
    fn single_node_rename_costs_one() {
        assert_eq!(apted_distance(&n("if"), &n("while")), 1);
    }

    #[test]
    fn insert_and_delete_cost_one() {
        let one = LabeledTree::with_children("root", vec![n("a")]);
        let two = LabeledTree::with_children("root", vec![n("a"), n("b")]);
        assert_eq!(apted_distance(&one, &two), 1);
        assert_eq!(apted_distance(&two, &one), 1);
    }

    #[test]
    fn distance_is_symmetric() {
        let a = LabeledTree::with_children(
            "root",
            vec![n("a"), LabeledTree::with_children("b", vec![n("c")])],
        );
        let b = LabeledTree::with_children(
            "root",
            vec![n("x"), LabeledTree::with_children("b", vec![n("y")])],
        );
        assert_eq!(apted_distance(&a, &b), apted_distance(&b, &a));
    }

    // --- Cross-validation against independent brute force ---

    #[test]
    fn matches_brute_force_on_fossil_zs_vectors() {
        // Renamed sibling under shared structure.
        let a = LabeledTree::with_children(
            "root",
            vec![n("a"), LabeledTree::with_children("b", vec![n("c")])],
        );
        let b = LabeledTree::with_children(
            "root",
            vec![n("a"), LabeledTree::with_children("b", vec![n("d")])],
        );
        assert_eq!(apted_distance(&a, &b), brute(&a, &b));

        // Linear chain, one relabel deep down.
        let chain = |labels: &[&str]| -> LabeledTree {
            let mut t = n(labels[labels.len() - 1]);
            for &l in labels[..labels.len() - 1].iter().rev() {
                t = LabeledTree::with_children(l, vec![t]);
            }
            t
        };
        let a = chain(&["a", "b", "c", "d", "e"]);
        let b = chain(&["a", "b", "c", "x", "y"]);
        assert_eq!(apted_distance(&a, &b), brute(&a, &b));

        // Wide tree, two renames.
        let a = LabeledTree::with_children(
            "root",
            vec![n("a"), n("b"), n("c"), n("d"), n("e")],
        );
        let b = LabeledTree::with_children(
            "root",
            vec![n("a"), n("x"), n("c"), n("y"), n("e")],
        );
        assert_eq!(apted_distance(&a, &b), brute(&a, &b));

        // Asymmetric reshuffle.
        let a = LabeledTree::with_children(
            "root",
            vec![
                LabeledTree::with_children("left", vec![n("a"), n("b"), n("c")]),
                n("right"),
            ],
        );
        let b = LabeledTree::with_children(
            "root",
            vec![n("left"), LabeledTree::with_children("right", vec![n("x"), n("y")])],
        );
        assert_eq!(apted_distance(&a, &b), brute(&a, &b));
    }

    #[test]
    fn structural_tree_sees_reorder_renames_and_structure() {
        use crate::types::ByteSpan;
        // Derive the body (block) span from the parsed AST exactly like
        // extraction does — block nodes start at their first token, so
        // hand-computed line starts would miss by the indent width.
        let mk = |body: &str| -> (String, ByteSpan) {
            let src = format!("def f(a, b):
{body}");
            let ts_lang = crate::graph::CodeGraph::ts_language(&Language::Python).unwrap();
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&ts_lang).unwrap();
            let tree = parser.parse(&src, None).unwrap();
            let mut cursor = tree.root_node().walk();
            let func = tree
                .root_node()
                .children(&mut cursor)
                .find(|n| n.kind() == "function_definition")
                .unwrap();
            let mut fcursor = func.walk();
            let blk = func
                .children(&mut fcursor)
                .find(|n| n.kind() == "block")
                .unwrap();
            (src.clone(), ByteSpan { start: blk.start_byte(), end: blk.end_byte() })
        };

        let (src1, sp1) = mk("    if a:
        return b
    return a
");
        let (src2, sp2) = mk("    if zz:
        return yy
    return zz
");
        let (src3, sp3) = mk("    return a
    if zz:
        return yy
");

        let t1 = structural_tree(&src1, Language::Python, sp1).expect("parses");
        let t2 = structural_tree(&src2, Language::Python, sp2).expect("parses");
        let t3 = structural_tree(&src3, Language::Python, sp3).expect("parses");

        // Renamed identifiers are invisible (Type-2 equivalence).
        assert_eq!(apted_distance(&t1, &t2), 0);
        // Reordered statements are visible.
        assert!(apted_similarity(&t1, &t3) < 0.85);
    }

    #[test]
    fn statement_reordering_is_visible_to_ted() {
        // The whole point of Stage 6.1: bag-of-shingles sees these two as
        // near-identical; ordered TED must not.
        let a = LabeledTree::with_children(
            "block",
            vec![n("assign"), n("call"), n("return")],
        );
        let b = LabeledTree::with_children(
            "block",
            vec![n("return"), n("call"), n("assign")],
        );
        let sim = apted_similarity(&a, &b);
        assert!(sim < 0.85, "reordered statements: sim={sim}");
    }
}
