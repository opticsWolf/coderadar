use super::CodeGraph;
use crate::types::*;

/// One extracted call site and the resolver's verdict on it.
#[derive(Clone, Debug)]
pub(crate) struct CallSiteInfo {
    pub name: String,
    /// Receiver segments as extracted (`self.x.m()` -> `["self.x"]` today).
    pub path: Vec<String>,
    pub line: usize,
    pub col: usize,
    /// `function | method | constructor | builtin | external | unresolved |
    /// pending` (pending = resolution has not run for this function).
    pub status: &'static str,
    /// Entity id (or name, for builtin/external) the call was bound to.
    pub target: Option<String>,
    /// `UnresolvedReason` for `unresolved` calls.
    pub reason: Option<String>,
}

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
        snap: &ProjectedGraph,
        id: &str,
        kind: &str,
        up: bool,
        down: bool,
    ) -> Vec<EntityId> {
        let mut out: Vec<EntityId> = Vec::new();
        let mut push_set = |s: Option<&std::collections::BTreeSet<String>>| {
            if let Some(s) = s {
                out.extend(s.iter().cloned());
            }
        };
        match kind {
            "calls" => {
                if up {
                    push_set(snap.callers_by_callee.get(id));
                }
                if down {
                    push_set(snap.callees_by_caller.get(id));
                }
            }
            "imports" => {
                if up {
                    push_set(snap.importers.get(id));
                }
                if down {
                    push_set(snap.imports_by_importer.get(id));
                }
            }
            "extends" => {
                if up {
                    push_set(snap.subclasses.get(id));
                }
                if down {
                    if let Some(c) = snap.classes.get(id) {
                        out.extend(c.resolved_bases.iter().cloned());
                    }
                }
            }
            "overrides" => {
                if up {
                    push_set(snap.overridden_by.get(id));
                }
                if down {
                    if let Some(b) = snap.overrides_base.get(id) {
                        out.push(b.clone());
                    }
                }
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
            if depth >= max_depth {
                continue;
            }
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

    /// Every call site extracted from a function, paired with how the
    /// resolver classified it. `None` when the id is not a function. Unlike
    /// `list_unresolved_targets` this includes resolved and builtin calls, so
    /// "was this call extracted at all?" has an answer.
    pub(crate) fn list_call_sites(snap: &ProjectedGraph, id: &str) -> Option<Vec<CallSiteInfo>> {
        use crate::types::ResolvedCall as R;
        let f = snap.functions.get(id)?;
        let mut out = Vec::with_capacity(f.calls.len());
        for (i, call) in f.calls.iter().enumerate() {
            // `resolved_calls` is parallel to `calls` once resolution has run.
            let (status, target, reason) = match f.resolved_calls.get(i) {
                None => ("pending", None, None),
                Some(R::Function(t)) => ("function", Some(t.clone()), None),
                Some(R::Method { method, .. }) => ("method", Some(method.clone()), None),
                Some(R::Constructor(t)) => ("constructor", Some(t.clone()), None),
                Some(R::Builtin(n)) => ("builtin", Some(n.clone()), None),
                Some(R::External(n)) => ("external", Some(n.clone()), None),
                Some(R::Unresolved { reason, .. }) => {
                    ("unresolved", None, Some(format!("{reason:?}")))
                }
            };
            out.push(CallSiteInfo {
                name: call.name.clone(),
                path: call.path.clone(),
                line: call.line,
                col: call.col,
                status,
                target,
                reason,
            });
        }
        Some(out)
    }

    /// Names of the outgoing call targets the traversal cannot follow for
    /// a function (R2-12): the same `Unresolved` + `External` (not `Builtin`)
    /// population `count_unresolved_targets` counts, spelled out so
    /// `diagnose --unresolved` can show WHICH targets, not just how many.
    /// Dotted for member calls (`recv.method`).
    pub(crate) fn list_unresolved_targets(snap: &ProjectedGraph, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(f) = snap.functions.get(id) {
            for rc in f.resolved_calls.iter() {
                match rc {
                    crate::types::ResolvedCall::External(name) => out.push(name.clone()),
                    crate::types::ResolvedCall::Unresolved { raw, .. } => {
                        if raw.path.is_empty() {
                            out.push(raw.name.clone());
                        } else {
                            out.push(format!("{}.{}", raw.path.join("."), raw.name));
                        }
                    }
                    _ => {}
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Count outgoing targets that the traversal cannot follow for a node
    /// (downstream only — the reverse/upstream indexes are complete).
    /// Counts genuine resolution failures (`Unresolved`) and non-local
    /// (`External`) calls/imports, but NOT `Builtin` (expected, ubiquitous).
    /// Used by the `traverse_unresolved` pyfunction to surface silent
    /// traversal truncation (plan 2.3).
    pub(crate) fn count_unresolved_targets(
        snap: &ProjectedGraph,
        id: &str,
        kinds: &[String],
        down: bool,
    ) -> usize {
        if !down {
            return 0;
        }
        let mut total = 0;
        for kind in kinds {
            match kind.as_str() {
                "calls" => {
                    if let Some(f) = snap.functions.get(id) {
                        total += f
                            .resolved_calls
                            .iter()
                            .filter(|rc| {
                                matches!(
                                    rc,
                                    crate::types::ResolvedCall::External(_)
                                        | crate::types::ResolvedCall::Unresolved { .. }
                                )
                            })
                            .count();
                    }
                }
                "imports" => {
                    if let Some(m) = snap.modules.get(id) {
                        total += m
                            .imports
                            .iter()
                            .filter(|imp_id| {
                                snap.imports.get(*imp_id).map_or(false, |i| {
                                    matches!(
                                        i.resolution,
                                        crate::types::ImportResolution::Unresolved
                                    )
                                })
                            })
                            .count();
                    }
                }
                _ => {}
            }
        }
        total
    }
}
