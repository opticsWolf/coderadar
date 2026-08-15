use super::CodeGraph;
use crate::types::*;

/// C3 merge: repeatedly find a head that does not appear in the tail
/// of any other list, append it to result, and remove it from all heads.
fn c3_merge(mut lists: Vec<Vec<MroNode>>) -> Vec<MroNode> {
    let mut result: Vec<MroNode> = Vec::new();

    loop {
        lists.retain(|l| !l.is_empty());
        if lists.is_empty() {
            break;
        }

        // Find a good head: first element not in any other list's tail
        let mut found = false;
        for i in 0..lists.len() {
            let candidate = &lists[i][0];
            let in_tail = lists.iter().enumerate().any(|(j, l)| {
                i != j && l[1..].contains(candidate)
            });
            if !in_tail {
                // Good head found — remove it from the front of ALL lists
                let good = candidate.clone();
                for list in lists.iter_mut() {
                    if list.first() == Some(&good) {
                        list.remove(0);
                    }
                }
                result.push(good);
                found = true;
                break;
            }
        }

        if !found {
            // Cyclic or inconsistent inheritance — break by taking next available
            for list in &mut lists {
                if !list.is_empty() {
                    let head = list.remove(0);
                    result.push(head);
                    break;
                }
            }
        }
    }

    result
}

impl CodeGraph {
    /// Compute C3 linearization MRO for all classes in the projection.
    pub fn compute_all_mro(&self, projection: &mut ProjectedGraph) {
        let class_ids: Vec<String> = projection.classes.iter()
            .map(|(id, _)| id.clone()).collect();
        for class_id in &class_ids {
            let mro = self.compute_c3_mro(projection, class_id);
            if let Some(class) = projection.classes.get(class_id) {
                let mut c = (**class).clone();
                c.mro = mro;
                projection.classes.insert(class_id.clone(), std::sync::Arc::new(c));
            }
        }
    }

    /// C3 linearization: L[C] = C + merge(L[B1],...,L[Bn], [B1,...,Bn])
    fn compute_c3_mro(&self, projection: &ProjectedGraph, class_id: &str) -> Vec<MroNode> {
        let class = match projection.classes.get(class_id) {
            Some(c) => c.clone(),
            None => return vec![MroNode::Class(class_id.to_string())],
        };
        if class.bases.is_empty() {
            return vec![MroNode::Class(class_id.to_string())];
        }
        // Recursively compute MRO for each base.
        // Phase D: route base-name resolution through the shared heuristic
        // (same-module first, then global-unique fallback) so cross-file
        // inheritance enters the MRO — unlocks override detection and makes
        // `inherits` traversal meaningful on real codebases. Same-module
        // cases resolve identically to the old inline match.
        let parent_module = class.parent_module.clone();
        let base_mros: Vec<Vec<MroNode>> = class.bases.iter().map(|base| {
            let resolved = Self::resolve_base_by_name(projection, &base.name, &parent_module);
            match resolved {
                Some(id) => self.compute_c3_mro(projection, &id),
                None => vec![MroNode::External { name: base.name.clone() }],
            }
        }).collect();
        let base_nodes: Vec<MroNode> = class.bases.iter().map(|b| {
            Self::resolve_base_by_name(projection, &b.name, &parent_module)
                .map(|id| MroNode::Class(id.clone()))
                .unwrap_or_else(|| MroNode::External { name: b.name.clone() })
        }).collect();
        let mut merge_lists: Vec<Vec<MroNode>> = base_mros.clone();
        merge_lists.push(base_nodes);
        let merged = c3_merge(merge_lists);
        let mut result = vec![MroNode::Class(class_id.to_string())];
        result.extend(merged);
        result
    }

    // ── Phase D: Inheritance / Import / Override back-fill ─────────────────
    //
    // These three passes populate the reverse/forward indexes that the
    // Rust `traverse` binding (and the MCP `codegraph_traverse` tool) read:
    //   - `resolved_bases`  (forward `extends`) + `subclasses` (reverse)
    //   - `Import.resolution` (forward `imports`)            + `importers` (reverse)
    //   - `overrides_base`   (forward `overrides`)            + `overridden_by` (reverse)
    // Until these land, `imports`/`inherits`/`overrides` traversal returns
    // empty because the indexes were never populated (see
    // `docs/traversal-matrix.md` §0).

    /// Resolve a base-class *name* to a concrete class EntityId.
    ///
    /// Heuristic, three tiers (deliberately *separate* from `compute_c3_mro`
    /// so MRO behaviour — and thus call resolution — is untouched):
    ///   1. exact same-`parent_module` name match;
    ///   2. import-aware match (the caller imported this exact name from one
    ///      module — 2.1c);
    ///   3. project-global unique-name match filtered to the caller's
    ///      language family (2.1a).
    /// Returns `None` when ambiguous (multiple candidates) or not found.
    fn resolve_base_by_name(
        projection: &ProjectedGraph, base_name: &str, current_module: &str,
    ) -> Option<String> {
        let candidates = Self::base_candidates(projection, base_name, current_module);
        if candidates.len() == 1 {
            Some(candidates[0].clone())
        } else {
            None
        }
    }

    /// All candidate classes for a base name, in resolution priority order.
    /// `resolve_base_by_name` and the ambiguity findings (2.1b) both read
    /// this; the latter needs the full candidate set, not just the winner.
    pub(super) fn base_candidates(
        projection: &ProjectedGraph, base_name: &str, current_module: &str,
    ) -> Vec<String> {
        // (1) same-module exact match
        let same_mod: Vec<String> = projection
            .classes
            .iter()
            .filter(|(_, c)| c.name == base_name && c.parent_module == current_module)
            .map(|(id, _)| id.clone())
            .collect();
        if !same_mod.is_empty() {
            return same_mod;
        }

        // (2) import-aware: the caller imported this exact name from one module
        if let Some(id) = Self::import_aware_base(projection, base_name, current_module) {
            return vec![id];
        }

        // (3) global fallback, filtered to the caller's language family (2.1a)
        let caller_lang = projection.modules.get(current_module).map(|m| m.language);
        projection
            .classes
            .iter()
            .filter(|(_, c)| c.name == base_name)
            .filter(|(_, c)| {
                let candidate_lang = projection.modules.get(&c.parent_module).map(|m| m.language);
                match (caller_lang, candidate_lang) {
                    (Some(cl), Some(cl2)) => cl.same_family(&cl2),
                    _ => true, // missing language info → keep candidate
                }
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Import-aware base resolution (2.1c). If the caller's module imports the
    /// base name from exactly one module and that module holds a unique class
    /// with that name, resolve to it. Reads `Import.resolution`, which is
    /// populated by `resolve_imports` — so `resolve_imports` must run first.
    fn import_aware_base(
        projection: &ProjectedGraph, base_name: &str, current_module: &str,
    ) -> Option<String> {
        let module = projection.modules.get(current_module)?;
        for imp_id in &module.imports {
            let imp = match projection.imports.get(imp_id) {
                Some(i) => i,
                None => continue,
            };
            // Named imports carry the symbol we can match; module/star/side
            // imports do not name a specific class, so they can't disambiguate.
            let names: Vec<&str> = match &imp.kind {
                crate::types::ImportKind::FromImport { names, .. }
                | crate::types::ImportKind::RelativeImport { names, .. } => {
                    names.iter().map(|(n, _)| n.as_str()).collect()
                }
                _ => Vec::new(),
            };
            if !names.iter().any(|n| *n == base_name) {
                continue;
            }
            if let crate::types::ImportResolution::Module(target) = &imp.resolution {
                let mut found: Option<String> = None;
                for (id, c) in &projection.classes {
                    if c.name == base_name && &c.parent_module == target {
                        if found.is_some() { return None; } // 2+ in target → ambiguous
                        found = Some(id.clone());
                    }
                }
                if found.is_some() { return found; }
            }
        }
        None
    }
}
