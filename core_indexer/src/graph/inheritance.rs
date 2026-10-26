use std::collections::HashMap;
use std::sync::Arc;

use super::module_resolution::find_module_by_dotted_name;
use super::CodeGraph;
use crate::types::*;

impl CodeGraph {
    /// Resolve class inheritance: fill `Class.resolved_bases` (class
    /// targets) and `Class.resolved_interfaces` (interface targets —
    /// §2.6: previously folded into EXTENDS, now IMPLEMENTS), inverting
    /// into the `subclasses` and `implementers` reverse indexes. Clears
    /// both indexes first (idempotent rebuild). Same-module +
    /// global-unique heuristic.
    pub fn resolve_class_hierarchy(&self, projection: &mut ProjectedGraph) {
        projection.subclasses.clear();
        projection.implementers.clear();
        projection.ambiguous_bases.clear();
        let class_ids: Vec<String> = projection.classes.keys().cloned().collect();

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
            if is_tco {
                continue;
            }

            let mut resolved: Vec<String> = Vec::with_capacity(bases.len());
            for b in &bases {
                let candidates = Self::base_candidates(projection, &b.name, &parent_module);
                match candidates.len() {
                    1 => resolved.push(candidates[0].clone()),
                    n if n > 1 => projection
                        .ambiguous_bases
                        .push(crate::types::AmbiguousBase {
                            class_name: class_name.clone(),
                            base_name: b.name.clone(),
                            candidates,
                        }),
                    _ => {} // 0 candidates: external/builtin — correctly unresolved
                }
            }

            // Partition by target declaration: interfaces implement,
            // classes extend (§2.6). EXTENDS vs IMPLEMENTS is decided by
            // the target's declaration, not the clause spelling — so
            // `class C : B, IDisposable` (C#) and `implements I` against a
            // class (test FakeWorker) keep their honest kind. The borrow
            // ends before the write-back below.
            let (resolved_bases, resolved_ifaces): (Vec<String>, Vec<String>) = resolved
                .into_iter()
                .partition(|id| !Self::is_interface_target(projection, id));
            // Write back (only if changed — avoid needless clones).
            let (existing_bases, existing_ifaces) = projection
                .classes
                .get(cid)
                .map(|c| (c.resolved_bases.clone(), c.resolved_interfaces.clone()))
                .unwrap_or_default();
            if existing_bases != resolved_bases || existing_ifaces != resolved_ifaces {
                if let Some(class) = projection.classes.get(cid) {
                    let mut c = (**class).clone();
                    c.resolved_bases = resolved_bases.clone();
                    c.resolved_interfaces = resolved_ifaces.clone();
                    projection
                        .classes
                        .insert(cid.clone(), std::sync::Arc::new(c));
                }
            }
            // Invert: base → subclasses, interface → implementers.
            for base_id in &resolved_bases {
                projection
                    .subclasses
                    .entry(base_id.clone())
                    .or_default()
                    .insert(cid.clone());
            }
            for iface_id in &resolved_ifaces {
                projection
                    .implementers
                    .entry(iface_id.clone())
                    .or_default()
                    .insert(cid.clone());
            }
        }
    }

    /// Interface test for the §2.6 partition: true when `id` names a
    /// class entity whose declaration is an interface
    /// (`Class::is_interface` — TS/Java/C#/PHP `interface_declaration`).
    /// Unknown ids (externals) are never interfaces: they stay unresolved
    /// and produce no edge, as before.
    fn is_interface_target(projection: &ProjectedGraph, id: &str) -> bool {
        projection
            .classes
            .get(id)
            .map(|c| c.is_interface())
            .unwrap_or(false)
    }

    /// Resolve decorators: fill `resolved_decorators` on functions and
    /// classes, inverting into the `decorated_by` reverse index (§2.6
    /// slice C). Clears the index first (idempotent rebuild). Decorator
    /// text (`@lru_cache`, `@app.route("/x")`, `#[derive(Debug)]`) is
    /// reduced to a name by `decorator_name`, then resolved through the
    /// same three tiers as bases (same-module → import-aware →
    /// global-unique in the caller's language family) over classes *and*
    /// functions. Unique hit only — builtins (`@property`), ambiguous
    /// and unresolvable names leave no edge, exactly like externals.
    pub fn resolve_decorators(&self, projection: &mut ProjectedGraph) {
        projection.decorated_by.clear();
        // (entity id, parent module, decorator texts): classes and
        // functions both carry decorators.
        let mut work: Vec<(String, String, Vec<String>)> = Vec::new();
        for (fid, f) in &projection.functions {
            if !f.decorators.is_empty() {
                work.push((fid.clone(), f.parent_module.clone(), f.decorators.clone()));
            }
        }
        for (cid, c) in &projection.classes {
            if !c.decorators.is_empty() {
                work.push((cid.clone(), c.parent_module.clone(), c.decorators.clone()));
            }
        }
        let mut resolved_by: HashMap<String, Vec<String>> = HashMap::new();
        for (id, module, decorators) in &work {
            let mut resolved: Vec<String> = Vec::new();
            for dec in decorators {
                let name = decorator_name(dec);
                if name.is_empty() {
                    continue;
                }
                // Full dotted path first, then the leaf
                // (`app.route` resolves like a qualified base).
                let leaf = name.rsplit('.').next().unwrap_or(name);
                for cand in [name, leaf] {
                    let mut hits = Self::decorator_candidates(projection, cand, module);
                    hits.retain(|hit| hit != id);
                    if hits.len() == 1 {
                        let hit = hits.into_iter().next().unwrap_or_default();
                        if !hit.is_empty() && !resolved.contains(&hit) {
                            resolved.push(hit);
                        }
                        break;
                    }
                }
            }
            resolved_by.insert(id.clone(), resolved);
        }
        for (id, resolved) in &resolved_by {
            if let Some(f) = projection.functions.get(id) {
                if f.resolved_decorators != *resolved {
                    let mut updated = (**f).clone();
                    updated.resolved_decorators = resolved.clone();
                    projection.functions.insert(id.clone(), Arc::new(updated));
                }
            } else if let Some(c) = projection.classes.get(id) {
                if c.resolved_decorators != *resolved {
                    let mut updated = (**c).clone();
                    updated.resolved_decorators = resolved.clone();
                    projection.classes.insert(id.clone(), Arc::new(updated));
                }
            }
            for dec_id in resolved {
                projection
                    .decorated_by
                    .entry(dec_id.clone())
                    .or_default()
                    .insert(id.clone());
            }
        }
    }

    /// Candidate decorator targets (classes or functions) with the same
    /// three tiers as `base_candidates`: same-module exact, import-aware,
    /// global-unique in the caller's language family. The tiers run over
    /// the union of both maps — a class and a function sharing one name
    /// in the same module is ambiguous (2 hits), not a coin flip.
    fn decorator_candidates(
        projection: &ProjectedGraph,
        name: &str,
        current_module: &str,
    ) -> Vec<String> {
        // (1) same-module exact match over classes and functions.
        let same_mod: Vec<String> = projection
            .classes
            .iter()
            .filter(|(_, c)| c.name == name && c.parent_module == current_module)
            .map(|(id, _)| id.clone())
            .chain(
                projection
                    .functions
                    .iter()
                    .filter(|(_, f)| f.name == name && f.parent_module == current_module)
                    .map(|(id, _)| id.clone()),
            )
            .collect();
        if !same_mod.is_empty() {
            return same_mod;
        }
        // (2) import-aware: the caller imported this exact name; the
        // target module holds a unique class-or-function with that name.
        for target in &Self::import_target_modules(projection, name, current_module) {
            let mut found: Vec<String> = Vec::new();
            for (id, c) in &projection.classes {
                if c.name == name && &c.parent_module == target {
                    found.push(id.clone());
                }
            }
            for (id, f) in &projection.functions {
                if f.name == name && &f.parent_module == target {
                    found.push(id.clone());
                }
            }
            if found.len() == 1 {
                return found;
            }
        }
        // (3) global fallback, filtered to the caller's language family.
        let caller_lang = projection.modules.get(current_module).map(|m| m.language);
        let mut out: Vec<String> = Vec::new();
        for (id, c) in &projection.classes {
            if c.name != name {
                continue;
            }
            let candidate_lang = projection.modules.get(&c.parent_module).map(|m| m.language);
            match (caller_lang, candidate_lang) {
                (Some(cl), Some(cl2)) if !cl.same_family(&cl2) => {}
                _ => out.push(id.clone()),
            }
        }
        for (id, f) in &projection.functions {
            if f.name != name {
                continue;
            }
            let candidate_lang = projection.modules.get(&f.parent_module).map(|m| m.language);
            match (caller_lang, candidate_lang) {
                (Some(cl), Some(cl2)) if !cl.same_family(&cl2) => {}
                _ => out.push(id.clone()),
            }
        }
        out
    }

    /// Resolve imports: set `Import.resolution` (module-level) and build the
    /// `importers` reverse index (target-module → set of importer modules).
    /// This is a *module-level* edge ("module A imports module B") — the
    /// dominant relationship for traversal — rather than per-imported-name,
    /// because one Import entity has a single `ImportResolution`.
    /// Clears `importers` first (idempotent rebuild).
    pub fn resolve_imports(&self, projection: &mut ProjectedGraph) {
        projection.importers.clear();
        let module_ids: Vec<String> = projection.modules.keys().cloned().collect();

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

                let target_mod_id =
                    src_dotted.and_then(|s| find_module_by_dotted_name(projection, &s, mid));

                let new_resolution = match &target_mod_id {
                    Some(t) => crate::types::ImportResolution::Module(t.clone()),
                    None => crate::types::ImportResolution::Unresolved,
                };

                if new_resolution != imp.resolution {
                    let mut ni = (*imp).clone();
                    ni.resolution = new_resolution;
                    projection
                        .imports
                        .insert(imp_id.clone(), std::sync::Arc::new(ni));
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
                .is_some_and(|c| c.methods != methods);
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
        let class_ids: Vec<String> = projection.classes.keys().cloned().collect();

        for cid in &class_ids {
            let mro = match projection.classes.get(cid) {
                Some(c) => c.mro.clone(),
                None => continue,
            };

            // Methods declared directly on THIS class.
            let own_methods: std::collections::HashMap<String, String> = projection
                .functions
                .iter()
                .filter(|(_, f)| f.parent_class.as_ref() == Some(cid) && !f.name.is_empty())
                .map(|(fid, f)| (f.name.clone(), fid.clone()))
                .collect();
            if own_methods.is_empty() {
                continue;
            }

            // Walk MRO past self. For each method, the first base class in
            // MRO order declaring a same-named method is the overridden base.
            for (name, override_fid) in &own_methods {
                let mut base_fid: Option<String> = None;
                let mut skipped_self = false;
                for node in &mro {
                    if !skipped_self {
                        skipped_self = true;
                        continue;
                    }
                    match node {
                        crate::types::MroNode::Class(bid) => {
                            // base method = a function with parent_class == bid, same name
                            if let Some((bf, _)) = projection.functions.iter().find(|(_, f)| {
                                f.parent_class.as_ref() == Some(bid) && f.name == *name
                            }) {
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

/// Reduce decorator text to a resolvable name (§2.6 slice C).
///
/// Handles the spellings extraction produces: Python/TS/Java `@name`,
/// dotted `@app.route`, applied `@deco(args)` (arguments are not part of
/// the identity), Rust `#[derive(Debug)]`, C# `[TestCase(\"x\")]`.
/// Returns `\"\"` when nothing name-like remains — the caller skips it.
fn decorator_name(text: &str) -> &str {
    let mut t = text.trim();
    t = t.strip_prefix('@').unwrap_or(t);
    t = t.strip_prefix('#').unwrap_or(t);
    t = t.strip_prefix('[').unwrap_or(t);
    t = t.trim_end_matches(']');
    // Arguments (and Rust generics) are not identity: `@deco(args)`,
    // `#[derive(Debug)]`, `@Component({...})`.
    t = t.split(['(', '<']).next().unwrap_or(t);
    t = t.trim_end_matches('!');
    t.trim()
}

#[cfg(test)]
mod decorator_name_tests {
    use super::decorator_name;

    #[test]
    fn decorator_text_reduces_to_names() {
        assert_eq!(decorator_name("@property"), "property");
        assert_eq!(
            decorator_name("@functools.lru_cache"),
            "functools.lru_cache"
        );
        assert_eq!(decorator_name("@app.route(\"/x\")"), "app.route");
        assert_eq!(decorator_name("@Component({selector: 'a'})"), "Component");
        assert_eq!(decorator_name("#[derive(Debug)]"), "derive");
        assert_eq!(decorator_name("#[pyfunction]"), "pyfunction");
        assert_eq!(decorator_name("[TestCase(\"x\")]"), "TestCase");
        assert_eq!(decorator_name("@"), "");
        assert_eq!(decorator_name("  "), "");
    }
}
