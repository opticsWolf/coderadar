use std::collections::{BTreeSet, HashMap};

use dashmap::DashMap;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};

use crate::types::*;

// ── Call Graph (§3.4a) — EntityId-based ─────────────────────────────────────

#[derive(Clone, Debug)]
pub struct CallNode {
    pub entity_id: EntityId,
    pub qualified_name: String,
}

#[derive(Clone, Debug)]
pub struct CallEdge {
    pub confidence: f32,
    pub resolution_method: ResolutionMethod,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
}

pub struct CallGraph {
    graph: StableDiGraph<CallNode, CallEdge>,
    path_to_node: DashMap<String, NodeIndex>,
}

impl CallGraph {
    pub fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            path_to_node: DashMap::new(),
        }
    }

    pub fn find_callers(&self, target_id: &str, max_depth: usize) -> Vec<(CallNode, usize)> {
        let target = match self.path_to_node.get(target_id) {
            Some(n) => *n,
            None => return vec![],
        };

        let mut visited = BTreeSet::new();
        let mut queue = vec![(target, 0usize)];
        let mut result = Vec::new();

        while let Some((node, depth)) = queue.pop() {
            if depth > max_depth || !visited.insert(node.index()) {
                continue;
            }
            if depth > 0 {
                if let Some(cn) = self.graph.node_weight(node) {
                    result.push((cn.clone(), depth));
                }
            }
            for neighbor in self.graph.neighbors_directed(node, petgraph::Incoming) {
                queue.push((neighbor, depth + 1));
            }
        }
        result
    }

    pub fn find_call_chain(
        &self,
        source_id: &str,
        target_id: &str,
        max_depth: usize,
    ) -> Option<Vec<CallNode>> {
        let (start, end) = match (
            self.path_to_node.get(source_id),
            self.path_to_node.get(target_id),
        ) {
            (Some(s), Some(e)) => (*s, *e),
            _ => return None,
        };

        let mut visited = BTreeSet::new();
        let mut parent: HashMap<NodeIndex, Option<NodeIndex>> = HashMap::new();
        let mut queue = vec![start];
        visited.insert(start.index());
        parent.insert(start, None);

        while let Some(node) = queue.pop() {
            if node == end {
                let mut chain = Vec::new();
                let mut current = Some(node);
                while let Some(n) = current {
                    if let Some(cn) = self.graph.node_weight(n) {
                        chain.push(cn.clone());
                    }
                    current = parent[&n];
                }
                chain.reverse();
                return Some(chain);
            }

            let current_depth = {
                let mut d = 0;
                let mut p = Some(node);
                while let Some(n) = p {
                    p = parent.get(&n).copied().flatten();
                    d += 1;
                }
                d
            };

            if current_depth >= max_depth {
                continue;
            }

            for neighbor in self.graph.neighbors_directed(node, petgraph::Outgoing) {
                if !visited.insert(neighbor.index()) {
                    continue;
                }
                parent.insert(neighbor, Some(node));
                queue.push(neighbor);
            }
        }
        None
    }
}
