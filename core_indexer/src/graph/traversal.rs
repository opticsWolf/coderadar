use super::CodeGraph;
use crate::types::*;

impl CodeGraph {
    // ── Traversal core (pure Rust, GIL-free, unit-testable) ──────────────
    // The lib.rs `traverse` pyfunction is a thin wrapper that validates args,
    // acquires the snapshot via with_graph, calls `traverse_bfs`, and
    // materializes PyDicts. Keeping the BFS here lets graph.rs unit tests
    // exercise it on a local CodeGraph snapshot without GLOBAL_GRAPH.

    /// One (entity, edge_kind, direction) neighbor lookup — pure Rust. The
    /// single place where each edge kind's reverse/forward index mapping is
    /// spelled out. `up`/`down` are pre-computed booleans (Send-friendly).
    pub(crate) fn neighbors_of(
        snap: &ProjectedGraph, id: &str, kind: &str, up: bool, down: bool,
    ) -> Vec<EntityId> {
        let mut out: Vec<EntityId> = Vec::new();
        let mut push_set = |s: Option<&std::collections::BTreeSet<String>>| {
            if let Some(s) = s { out.extend(s.iter().cloned()); }
        };
        match kind {
            "calls" => {
                if up { push_set(snap.callers_by_callee.get(id)); }
                if down { push_set(snap.callees_by_caller.get(id)); }
            }
            "imports" => {
                if up { push_set(snap.importers.get(id)); }
                if down { push_set(snap.imports_by_importer.get(id)); }
            }
            "extends" => {
                if up { push_set(snap.subclasses.get(id)); }
                if down { if let Some(c) = snap.classes.get(id) { out.extend(c.resolved_bases.iter().cloned()); } }
            }
            "overrides" => {
                if up { push_set(snap.overridden_by.get(id)); }
                if down { if let Some(b) = snap.overrides_base.get(id) { out.push(b.clone()); } }
            }
            _ => {}
        }
        out
    }

    /// Generalized edge-kind BFS over the in-memory `ProjectedGraph`.
    ///
    /// Returns `(entity_id, depth, edge_kind_that_reached_it)` tuples. The
    /// start entity is included at depth 0 with an empty edge-kind string.
    /// Cycles are handled by inserting into `visited` *before* enqueueing
    /// (not on pop), so diamonds produce one entry per reachable node.
    ///
    /// `kinds` must already be normalised lower-case (`inherits` → `extends`).
    pub(crate) fn traverse_bfs(
        snap: &ProjectedGraph,
        start_id: &str,
        max_depth: usize,
        kinds: &[String],
        up: bool,
        down: bool,
    ) -> Vec<(EntityId, usize, String)> {
        use std::collections::{HashSet, VecDeque};
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut out: Vec<(String, usize, String)> = Vec::new();

        queue.push_back((start_id.to_string(), 0usize));
        visited.insert(start_id.to_string());
        out.push((start_id.to_string(), 0, String::new()));

        while let Some((cur, depth)) = queue.pop_front() {
            if depth >= max_depth { continue; }
            for kind in kinds {
                for nb in Self::neighbors_of(snap, &cur, kind, up, down) {
                    if visited.insert(nb.clone()) {
                        out.push((nb.clone(), depth + 1, kind.clone()));
                        queue.push_back((nb, depth + 1));
                    }
                }
            }
        }
        out
    }

    /// Count outgoing targets that the traversal cannot follow for a node
    /// (downstream only — the reverse/upstream indexes are complete).
    /// Counts genuine resolution failures (`Unresolved`) and non-local
    /// (`External`) calls/imports, but NOT `Builtin` (expected, ubiquitous).
    /// Used by the `traverse_unresolved` pyfunction to surface silent
    /// traversal truncation (plan 2.3).
    pub(crate) fn count_unresolved_targets(
        snap: &ProjectedGraph, id: &str, kinds: &[String], down: bool,
    ) -> usize {
        if !down {
            return 0;
        }
        let mut total = 0;
        for kind in kinds {
            match kind.as_str() {
                "calls" => {
                    if let Some(f) = snap.functions.get(id) {
                        total += f.resolved_calls.iter()
                            .filter(|rc| matches!(rc,
                                crate::types::ResolvedCall::External(_)
                                | crate::types::ResolvedCall::Unresolved { .. }))
                            .count();
                    }
                }
                "imports" => {
                    if let Some(m) = snap.modules.get(id) {
                        total += m.imports.iter()
                            .filter(|imp_id| snap.imports.get(*imp_id).map_or(false, |i| {
                                matches!(i.resolution, crate::types::ImportResolution::Unresolved)
                            }))
                            .count();
                    }
                }
                _ => {}
            }
        }
        total
    }
}
