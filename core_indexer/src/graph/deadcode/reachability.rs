// CodeRadar Stage 1.2 — forward reachability over the resolved graph.
//
// `callees_by_caller` gives downstream adjacency; this module adds the
// closure. Two details that matter more than they look:
//
// * Overrides extend liveness: calling `base.draw()` reaches `Circle.draw`.
//   Fossil needs RTA for this; for us it is a two-line loop over the
//   already-resolved `overridden_by` map. Skipping it produces the classic
//   "this method is dead" false positive that destroys user trust.
//
// * Test-only liveness: a second pass seeded with test entry points lets the
//   classifier mark entities live ONLY from tests instead of silently
//   omitting them (fossil's `include_test_reachable`, restructured).

use std::collections::{HashSet, VecDeque};

use crate::types::{EntityId, ProjectedGraph};

/// Everything transitively callable from a set of roots.
#[derive(Clone, Debug)]
pub struct Reachability {
    /// Every entity reachable from the roots, including the roots.
    pub reachable: HashSet<EntityId>,
    /// The roots themselves (useful for "why is this live?" explanations).
    pub roots: HashSet<EntityId>,
}

/// BFS over call edges + virtual-dispatch edges.
pub fn compute_reachable(graph: &ProjectedGraph, roots: &HashSet<EntityId>) -> Reachability {
    let mut seen = roots.clone();
    let mut queue: VecDeque<EntityId> = roots.iter().cloned().collect();

    while let Some(current) = queue.pop_front() {
        if let Some(callees) = graph.callees_by_caller.get(&current) {
            for callee in callees {
                if seen.insert(callee.clone()) {
                    queue.push_back(callee.clone());
                }
            }
        }
        // Virtual dispatch: a call to a base method can reach any override.
        if let Some(subs) = graph.overridden_by.get(&current) {
            for sub in subs {
                if seen.insert(sub.clone()) {
                    queue.push_back(sub.clone());
                }
            }
        }
    }

    Reachability { reachable: seen, roots: roots.clone() }
}
