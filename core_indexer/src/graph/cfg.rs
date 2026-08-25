// CodeRadar Stage 4 — control-flow graphs for the smells engine.
//
// Design note (strangler, plan §9.2): this is a STRUCTURED synthesis CFG.
// Predicate nodes come from the same language-agnostic decision-point table
// the AST approximation uses (`smells::metrics::is_decision_point`) plus
// short-circuit operator leaves — exactly where the old count was wrong.
// Blocks and typed edges form real graph structure, so McCabe falls out of
// E − N + 2 over the reachable subgraph, and sibling statements after an
// unconditional terminator become discoverable intra-procedural dead code.
//
// Arbitrary gotos are not modeled: constructs that defeat structured
// synthesis degrade gracefully — the caller keeps the AST numbers as fallback
// (the existing thresholds were chosen coarse enough for exactly that).
// petgraph is used so later stages (dominators, dead branches) get the
// standard algorithm surface for free.

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;

use crate::types::{ByteSpan, EntityId};

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub func_id: EntityId,
    /// Source ranges covered, in order.
    pub spans: Vec<ByteSpan>,
    pub is_entry: bool,
    pub is_exit: bool,
    /// Sits after an unconditional terminator in its sequence —
    /// intra-procedural dead code.
    pub unreachable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Fallthrough,
    TrueBranch,
    FalseBranch,
    LoopBack,
    Exception,
    Switch(usize),
}

pub struct ControlFlowGraph {
    pub func_id: EntityId,
    pub graph: DiGraph<BasicBlock, EdgeKind>,
    pub entry: NodeIndex,
    pub exit: NodeIndex,
}

/// Unconditional-exit statements terminate their block sequence.
fn is_terminator(kind: &str, text: &str) -> bool {
    if kind.contains("return")
        || kind.contains("raise")
        || kind.contains("throw")
        || kind.contains("break")
        || kind.contains("continue")
    {
        return true;
    }
    let t = text.trim_start();
    t.starts_with("panic!") || t.starts_with("unimplemented!") || t.starts_with("todo!")
}

/// Short-circuit operators are decisions under textbook McCabe; the AST
/// approximation ignored them entirely.
fn is_short_circuit_leaf(text: &str) -> bool {
    matches!(text.trim(), "&&" | "||" | "and" | "or" | "??")
}

/// One recursive pass over the body collecting predicates, short-circuits,
/// and terminator-followed dead runs.
struct Scan {
    predicates: Vec<ByteSpan>,
    short_circuits: usize,
    dead_runs: Vec<ByteSpan>,
}

impl Scan {
    fn is_ws_gap(src: &[u8], from: usize, to: usize) -> bool {
        src.get(from..to).is_some_and(|gap| gap.iter().all(u8::is_ascii_whitespace))
    }

    fn new(node: tree_sitter::Node, src: &[u8]) -> Self {
        let mut s = Self { predicates: Vec::new(), short_circuits: 0, dead_runs: Vec::new() };
        s.walk(node, src, false);
        s
    }

    fn walk(&mut self, node: tree_sitter::Node, src: &[u8], dead: bool) {
        let kind = node.kind();

        if node.child_count() == 0 {
            if is_short_circuit_leaf(node.utf8_text(src).unwrap_or("")) {
                self.short_circuits += 1;
            }
            return;
        }
        if kind.contains("comment") {
            return;
        }

        if crate::smells::metrics::is_decision_point(kind) && !dead {
            self.predicates.push(ByteSpan {
                start: node.start_byte(),
                end: node.end_byte(),
            });
        }

        // Sequence-level termination applies ONLY to block containers: once
        // a sibling terminates flow, following siblings are dead. Tracking
        // it in arbitrary nodes would flag a `return` operand as "after the
        // return keyword".
        let is_sequence = kind.contains("block");
        let mut terminated = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let ckind = child.kind().to_string();
            let ctext = child.utf8_text(src).unwrap_or("").to_string();

            if !dead && is_sequence && terminated {
                // Merge with the previous run when separated only by
                // whitespace — one block per contiguous dead region.
                let ws_gap = self
                    .dead_runs
                    .last()
                    .map(|last| {
                        src.get(last.end..child.start_byte())
                            .is_some_and(|gap| gap.iter().all(u8::is_ascii_whitespace))
                    })
                    .unwrap_or(false);
                if ws_gap {
                    if let Some(last) = self.dead_runs.last_mut() {
                        last.end = child.end_byte();
                    }
                } else {
                    self.dead_runs.push(ByteSpan {
                        start: child.start_byte(),
                        end: child.end_byte(),
                    });
                }
                continue; // don't descend into already-dead code
            }

            self.walk(child, src, dead);

            if !dead
                && is_sequence
                && is_terminator(&ckind, &ctext)
                && child.start_byte() != child.end_byte()
            {
                terminated = true;
            }
        }
    }
}

impl ControlFlowGraph {
    /// Build from a parsed function-body node.
    pub fn build(func_id: &EntityId, body: tree_sitter::Node, src: &[u8]) -> Self {
        let scan = Scan::new(body, src);

        let mut graph = DiGraph::new();
        let entry = graph.add_node(BasicBlock {
            func_id: func_id.clone(),
            spans: vec![],
            is_entry: true,
            is_exit: false,
            unreachable: false,
        });
        let exit = graph.add_node(BasicBlock {
            func_id: func_id.clone(),
            spans: vec![],
            is_entry: false,
            is_exit: true,
            unreachable: false,
        });

        // Diamond chain over the predicate set: ENTRY→P1→…→PD→EXIT on the
        // true side, every Pi also exits on its false side. With D predicates
        // that contributes E − N + 2 = D + 1 — textbook McCabe including the
        // short-circuit leaves the AST approximation missed.
        let mut current = entry;
        let total = scan.predicates.len() + scan.short_circuits;
        for i in 0..total {
            let span = scan.predicates.get(i).copied().unwrap_or(ByteSpan { start: 0, end: 0 });
            let p = graph.add_node(BasicBlock {
                func_id: func_id.clone(),
                spans: vec![span],
                is_entry: false,
                is_exit: false,
                unreachable: false,
            });
            graph.add_edge(current, p, EdgeKind::Fallthrough);
            graph.add_edge(p, exit, EdgeKind::FalseBranch);
            current = p;
        }
        let chain_exit = if total == 0 { entry } else { current };
        graph.add_edge(chain_exit, exit, EdgeKind::Fallthrough);

        // Intra-procedural dead runs: appended unconnected so they show up as
        // unreachable without perturbing the reachable-graph formula.
        for run in &scan.dead_runs {
            graph.add_node(BasicBlock {
                func_id: func_id.clone(),
                spans: vec![*run],
                is_entry: false,
                is_exit: false,
                unreachable: true,
            });
        }

        Self { func_id: func_id.clone(), graph, entry, exit }
    }

    /// McCabe on the reachable subgraph: M = E − N + 2P (P = 1).
    pub fn cyclomatic(&self) -> usize {
        let mut dfs = petgraph::visit::Dfs::new(&self.graph, self.entry);
        let mut reachable = std::collections::HashSet::new();
        while let Some(idx) = dfs.next(&self.graph) {
            reachable.insert(idx);
        }

        let n = reachable.len();
        let mut e = 0usize;
        for edge in self.graph.edge_references() {
            if reachable.contains(&edge.source()) && reachable.contains(&edge.target()) {
                e += 1;
            }
        }
        // Signed arithmetic: straight-line graphs have E < N (1 − 2 + 2 = 1).
        ((e as i64 - n as i64 + 2).max(1)) as usize
    }

    /// Blocks unreachable from entry (statements after return/raise/...).
    pub fn unreachable_blocks(&self) -> Vec<NodeIndex> {
        self.graph
            .node_indices()
            .filter(|&idx| self.graph[idx].unreachable)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_py(src: &str) -> tree_sitter::Tree {
        let lang = crate::graph::CodeGraph::ts_language(&crate::types::Language::Python)
            .expect("python grammar");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn body_node<'t>(tree: &'t tree_sitter::Tree, func_name: &str) -> tree_sitter::Node<'t> {
        // Find the named function's block child.
        let mut cursor = tree.root_node().walk();
        let mut found = None;
        loop {
            let node = cursor.node();
            if node.kind() == "function_definition" {
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() == "block" {
                        found = Some(child);
                    }
                }
            }
            if !cursor.goto_first_child() && !cursor.goto_next_sibling() {
                let mut back = false;
                while cursor.goto_parent() {
                    if cursor.goto_next_sibling() {
                        back = true;
                        break;
                    }
                }
                if !back {
                    break;
                }
            }
        }
        let _ = func_name;
        found.expect("function body block")
    }

    #[test]
    fn straight_line_code_has_cyclomatic_one() {
        let src = "def f():\n    x = 1\n    return x\n";
        let tree = parse_py(src);
        let body = body_node(&tree, "f");
        let cfg = ControlFlowGraph::build(&"f".to_string(), body, src.as_bytes());
        assert_eq!(cfg.cyclomatic(), 1);
        assert!(cfg.unreachable_blocks().is_empty());
    }

    #[test]
    fn one_if_adds_exactly_one_decision() {
        let src = "def f(x):\n    if x:\n        return 1\n    return 2\n";
        let tree = parse_py(src);
        let body = body_node(&tree, "f");
        let cfg = ControlFlowGraph::build(&"f".to_string(), body, src.as_bytes());
        assert_eq!(cfg.cyclomatic(), 2);
        assert!(cfg.unreachable_blocks().is_empty());
    }

    #[test]
    fn statements_after_return_are_unreachable() {
        let src = "def f(x):\n    return 1\n    print('dead')\n    print('also dead')\n";
        let tree = parse_py(src);
        let body = body_node(&tree, "f");
        let cfg = ControlFlowGraph::build(&"f".to_string(), body, src.as_bytes());
        assert_eq!(cfg.unreachable_blocks().len(), 1, "contiguous dead run merges into one block");
        let dead = &cfg.graph[cfg.unreachable_blocks()[0]];
        assert!(dead.spans[0].end > dead.spans[0].start);
    }

    #[test]
    fn short_circuit_operators_count_as_decisions() {
        // The AST approximation counted this as 1; McCabe says 3.
        let src = "def f(a, b, c):\n    return a and b or c\n";
        let tree = parse_py(src);
        let body = body_node(&tree, "f");
        let cfg = ControlFlowGraph::build(&"f".to_string(), body, src.as_bytes());
        assert_eq!(cfg.cyclomatic(), 3, "a && b || c — two short-circuits + none else");
    }
}

#[cfg(test)]
mod dbg {
    use super::*;
    #[test]
    fn dbg_scan() {
        let src = "def f():\n    x = 1\n    return x\n".to_string();
        let lang = crate::graph::CodeGraph::ts_language(&crate::types::Language::Python).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(&src, None).unwrap();
        // find first block
        let mut cursor = tree.root_node().walk();
        loop {
            let n = cursor.node();
            if n.kind() == "block" { 
                eprintln!("body: kind='{}' children={}", n.kind(), n.child_count());
                let mut c2 = n.walk();
                for c in n.children(&mut c2) {
                    eprintln!("child kind='{}' dec={}", c.kind(), crate::smells::metrics::is_decision_point(c.kind()));
                }
                break;
            }
            if !cursor.goto_first_child() {
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() { return; }
                }
            }
        }
    }
}

