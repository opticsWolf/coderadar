use std::collections::HashMap;
use std::sync::Arc;

use super::CodeGraph;
use super::module_resolution::find_module_by_dotted_name;
use crate::types::*;

impl CodeGraph {
    /// Resolve class inheritance: fill `Class.resolved_bases` and invert
    /// into the `subclasses` reverse index. Clears `subclasses` first
    /// (idempotent rebuild). Same-module + global-unique heuristic.
    pub fn resolve_class_hierarchy(&self, projection: &mut ProjectedGraph) {
        projection.subclasses.clear();
        projection.ambiguous_bases.clear();
        let class_ids: Vec<String> =
            projection.classes.keys().cloned().collect();

        for cid in &class_ids {
            let (bases, parent_module, is_tco, class_name) = match projection.classes.get(cid) {
                Some(c) => (
                    c.bases.clone(),
                    c.parent_module.clone(),
                    c.is_type_checking_only,
                    c.name.clone(),
                ),
                None => continue,
            };
            if is_tco { continue; }

            let mut resolved: Vec<String> = Vec::with_capacity(bases.len());
            for b in &bases {
                let candidates = Self::base_candidates(projection, &b.name, &parent_module);
                match candidates.len() {
                    1 => resolved.push(candidates[0].clone()),
                    n if n > 1 => projection.ambiguous_bases.push(crate::types::AmbiguousBase {
                        class_name: class_name.clone(),
                        base_name: b.name.clone(),
                        candidates,
                    }),
                    _ => {} // 0 candidates: external/builtin — correctly unresolved
                }
            }

            // Write back resolved_bases (only if changed — avoid needless clones)
            let existing = projection.classes.get(cid).map(|c| c.resolved_bases.clone()).unwrap_or_default();
            if existing != resolved {
                if let Some(class) = projection.classes.get(cid) {
                    let mut c = (**class).clone();
                    c.resolved_bases = resolved.clone();
                    projection.classes.insert(cid.clone(), std::sync::Arc::new(c));
                }
            }
            // Invert: base → subclasses
            for base_id in &resolved {
                projection
                    .subclasses
                    .entry(base_id.clone())
                    .or_default()
                    .insert(cid.clone());
            }
        }
    }

    /// Resolve imports: set `Import.resolution` (module-level) and build the
    /// `importers` reverse index (target-module → set of importer modules).
    /// This is a *module-level* edge ("module A imports module B") — the
    /// dominant relationship for traversal — rather than per-imported-name,
    /// because one Import entity has a single `ImportResolution`.
    /// Clears `importers` first (idempotent rebuild).
    pub fn resolve_imports(&self, projection: &mut ProjectedGraph) {
        projection.importers.clear();
        let module_ids: Vec<String> =
            projection.modules.keys().cloned().collect();

        for mid in &module_ids {
            let imports_list = match projection.modules.get(mid) {
                Some(m) => m.imports.clone(),
                None => continue,
            };
            for imp_id in &imports_list {
                let imp = match projection.imports.get(imp_id) {
                    Some(i) => i.clone(),
                    None => continue,
                };
                // Source dotted-name per kind (RelativeImport best-effort).
                let src_dotted: Option<String> = match &imp.kind {
                    crate::types::ImportKind::ModuleImport { module, .. }
                    | crate::types::ImportKind::FromImport { module, .. }
                    | crate::types::ImportKind::StarImport { module }
                    | crate::types::ImportKind::Side { module } => Some(module.clone()),
                    crate::types::ImportKind::RelativeImport { module, .. } => module.clone(),
                };

                let target_mod_id = src_dotted
                    .and_then(|s| find_module_by_dotted_name(projection, &s, mid));

                let new_resolution = match &target_mod_id {
                    Some(t) => crate::types::ImportResolution::Module(t.clone()),
                    None => crate::types::ImportResolution::Unresolved,
                };

                if new_resolution != imp.resolution {
                    let mut ni = (*imp).clone();
                    ni.resolution = new_resolution;
                    projection.imports.insert(imp_id.clone(), std::sync::Arc::new(ni));
                }
                if let Some(t) = target_mod_id {
                    projection
                        .importers
                        .entry(t.clone())
                        .or_default()
                        .insert(mid.clone());
                    // Forward index: importer → targets it depends on.
                    projection
                        .imports_by_importer
                        .entry(mid.clone())
                        .or_default()
                        .insert(t);
                }
            }
        }
    }

    /// 2.7: Populate `Class.methods` as a **derived** field — computed from
    /// `projection.functions` grouped by `parent_class` on every resolve
    /// cascade. The single source of truth stays `functions` + `parent_class`;
    /// this is read-only denormalization (no separate write path), so it can
    /// never drift. Runs after all fragments are merged, so cross-file methods
    /// (e.g. a Rust `impl` block in another file) are captured.
    pub fn populate_class_methods(&self, projection: &mut ProjectedGraph) {
        let mut by_parent: HashMap<String, Vec<String>> = HashMap::new();
        for (fid, f) in projection.functions.iter() {
            if let Some(pc) = &f.parent_class {
                by_parent.entry(pc.clone()).or_default().push(fid.clone());
            }
        }
        let class_ids: Vec<String> = projection.classes.keys().cloned().collect();
        for cid in class_ids {
            let mut methods = by_parent.remove(&cid).unwrap_or_default();
            methods.sort();
            let changed = projection
                .classes
                .get(&cid)
                .map_or(false, |c| c.methods != methods);
            if changed {
                if let Some(class) = projection.classes.get(&cid) {
                    let mut c = (**class).clone();
                    c.methods = methods;
                    projection.classes.insert(cid, Arc::new(c));
                }
            }
        }
    }

    /// Detect method overrides across the class MRO and populate the
    /// `overridden_by` reverse index (base → overriding methods) and the
    /// `overrides_base` forward index (override → its single base).
    /// Uses `Class.mro` (built by `compute_all_mro`) and name-based matching
    /// (consistent with `resolve_one_function`'s MRO method lookup).
    /// Clears both indexes first (idempotent rebuild).
    pub fn resolve_overrides(&self, projection: &mut ProjectedGraph) {
        projection.overridden_by.clear();
        projection.overrides_base.clear();

        // Per-class method map (name → own func id), built by parent_class scan.
        // class.methods IS now populated (populate_class_methods, 2.7), but we
        // still scan functions here for parity with resolve_one_function's MRO
        // method lookup (which also scans, not reads class.methods).
        let class_ids: Vec<String> =
            projection.classes.keys().cloned().collect();

        for cid in &class_ids {
            let mro = match projection.classes.get(cid) {
                Some(c) => c.mro.clone(),
                None => continue,
            };

            // Methods declared directly on THIS class.
            let own_methods: std::collections::HashMap<String, String> =
                projection
                    .functions
                    .iter()
                    .filter(|(_, f)| f.parent_class.as_ref() == Some(cid) && !f.name.is_empty())
                    .map(|(fid, f)| (f.name.clone(), fid.clone()))
                    .collect();
            if own_methods.is_empty() { continue; }

            // Walk MRO past self. For each method, the first base class in
            // MRO order declaring a same-named method is the overridden base.
            for (name, override_fid) in &own_methods {
                let mut base_fid: Option<String> = None;
                let mut skipped_self = false;
                for node in &mro {
                    if !skipped_self { skipped_self = true; continue; }
                    match node {
                        crate::types::MroNode::Class(bid) => {
                            // base method = a function with parent_class == bid, same name
                            if let Some((bf, _)) = projection
                                .functions
                                .iter()
                                .find(|(_, f)| f.parent_class.as_ref() == Some(bid) && f.name == *name)
                            {
                                base_fid = Some(bf.clone());
                                break;
                            }
                        }
                        crate::types::MroNode::External { .. } => {}
                    }
                }
                if let Some(bf) = base_fid {
                    projection
                        .overridden_by
                        .entry(bf.clone())
                        .or_default()
                        .insert(override_fid.clone());
                    projection
                        .overrides_base
                        .insert(override_fid.clone(), bf.clone());
                }
            }
        }
    }
}
