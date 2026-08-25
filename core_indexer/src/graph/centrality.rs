// CodeRadar Stage 5 — structural-importance ranking.
//
// Fossil reference: `src/graph/centrality.rs`. Weighted-degree + harmonic
// hybrid rather than full PageRank: it explains better to users ("12 things
// depend on this within 3 hops") and costs one bounded BFS per node over the
// already-materialized `callers_by_callee` index.
//
// Direction matters: edges walk UPSTREAM (callee → callers), so an entity's
// score measures how much of the codebase transitively depends on it —
// exactly what blast-radius triage needs.
//
// Normalized to 0..=1 for stable thresholds and stable display ordering.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::types::{EntityId, ProjectedGraph};

/// Harmonic centrality with depth cutoff. Score(id) =
/// Σ_depth |callers at depth| / depth over 1..=max_depth, normalized by the
/// maximum so values land in 0..=1.
pub fn harmonic_centrality(graph: &ProjectedGraph, max_depth: usize) -> HashMap<EntityId, f64> {
    let mut out: HashMap<EntityId, f64> = HashMap::with_capacity(graph.functions.len());

    for id in graph.functions.keys() {
        // Bounded upstream BFS from this callee.
        let mut seen: HashSet<EntityId> = HashSet::new();
        seen.insert(id.clone());
        let mut frontier: Vec<EntityId> = vec![id.clone()];
        let mut score = 0.0f64;
        for depth in 1..=max_depth.max(1) {
            let mut next = Vec::new();
            for cur in &frontier {
                if let Some(callers) = graph.callers_by_callee.get(cur) {
                    for caller in callers {
                        if seen.insert(caller.clone()) {
                            next.push(caller.clone());
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            score += next.len() as f64 / depth as f64;
            frontier = next;
        }
        out.insert(id.clone(), score);
    }

    let max = out.values().cloned().fold(f64::MIN, f64::max);
    if max > f64::EPSILON {
        out.values_mut().for_each(|v| *v /= max);
    }
    out
}

/// Per-snapshot cache for the FFI path (`rank_by_centrality`): the full map
/// is O(V · bounded-BFS), so recompute it only when the graph revision
/// changes. Snapshots carry no timestamp, so the revision key is a cheap
/// content fingerprint: function count ⊕ all body hashes + caller index
/// size — any edit changes at least one hash.
static CENTRALITY_CACHE: std::sync::Mutex<Option<((u64, usize), HashMap<EntityId, f64>)>> =
    std::sync::Mutex::new(None);

fn graph_revision(graph: &ProjectedGraph) -> (u64, usize) {
    let mut fp: u64 = graph.functions.len() as u64;
    for f in graph.functions.values() {
        fp = fp.rotate_left(7) ^ f.body_hash;
    }
    (fp, graph.callers_by_callee.len())
}

/// Cached variant used by `rank_by_centrality`; same math as
/// [`harmonic_centrality`] but memoized per graph revision.
pub fn cached_harmonic_centrality(
    graph: &ProjectedGraph,
    max_depth: usize,
) -> HashMap<EntityId, f64> {
    let revision = graph_revision(graph);
    if let Ok(guard) = CENTRALITY_CACHE.lock() {
        if let Some((cached_rev, map)) = guard.as_ref() {
            if *cached_rev == revision {
                return map.clone();
            }
        }
    }
    let fresh = harmonic_centrality(graph, max_depth);
    if let Ok(mut guard) = CENTRALITY_CACHE.lock() {
        *guard = Some((revision, fresh.clone()));
    }
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn func(id: &str) -> crate::types::Function {
        crate::graph::deadcode::tests::func(id, id, "m.py::module")
    }

    /// five leaves -> core -> util ; plus one isolated loner.
    /// Harmonic centrality must rank core (5 direct dependents) above
    /// util (all 5 arrive only at depth 2, halved), above any leaf,
    /// above the loner (0).
    #[test]
    fn fan_in_core_outranks_relay_and_loner() {
        let mut g = crate::smells::engine::tests::empty_graph();
        let names: Vec<String> = (0..5)
            .map(|i| format!("m.py::leaf_{i}"))
            .chain(["m.py::core".into(), "m.py::util".into(), "m.py::loner".into()])
            .collect();
        for name in &names {
            g.functions.insert(name.clone(), Arc::new(func(name)));
        }
        let mut edge = |g: &mut ProjectedGraph, caller: &str, callee: &str| {
            g.callees_by_caller
                .entry(caller.into())
                .or_default()
                .insert(callee.into());
            g.callers_by_callee
                .entry(callee.into())
                .or_default()
                .insert(caller.into());
        };
        for i in 0..5 {
            edge(&mut g, &format!("m.py::leaf_{i}"), "m.py::core");
        }
        edge(&mut g, "m.py::core", "m.py::util");

        let scores = harmonic_centrality(&g, 3);

        let s = |n: &str| scores[&format!("m.py::{n}")];
        assert!(s("core") > s("util"), "direct fan-in beats relayed fan-in");
        assert!(s("util") > s("leaf_0"), "many dependents beat none");
        assert_eq!(s("loner"), 0.0);
        // Normalized: the maximum is exactly 1.0.
        let max = scores.values().cloned().fold(f64::MIN, f64::max);
        assert!((max - 1.0).abs() < 1e-9);
    }
}
