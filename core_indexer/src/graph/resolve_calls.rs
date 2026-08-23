use super::CodeGraph;
use super::ImportGraph;
use super::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};
use crate::types::*;

/// `class id → [(method name, method id)]`, in the order the projection's
/// function map yields them — the same order the scans it replaces saw, so
/// first-wins name resolution is unchanged.
type MethodsByClass = std::collections::HashMap<EntityId, Vec<(String, String)>>;

impl CodeGraph {
    /// Run the resolution cascade on all functions, or scoped to a single file.
    /// When `scope_file` is Some, only clears and rebuilds edges for functions
    /// in that file — used by `update_file` for O(changed) instead of O(all).
    pub fn resolve_all_calls(&self, projection: &mut ProjectedGraph) {
        self.resolve_calls_scoped(projection, None);
    }

    /// Pure resolution of a single function's calls — all data passed as parameters.
    /// Returns (resolved_calls, edge_pairs) without mutating the projection.
    /// The projection is read-only during resolution; edge pairs are applied
    /// sequentially afterward. Thread-safe: no `&self`, each thread creates
    /// its own orchestrator.
    ///
    /// Technique adopted from CodeGraph's per-file resolution in
    /// resolve/index.ts (MIT license, https://github.com/opticsWolf/codegraph).
    fn resolve_one_function(
        func_id: &str,
        calls: &[crate::types::UnresolvedRef],
        sibling_funcs: &std::collections::HashMap<String, String>,
        import_targets: &std::collections::HashMap<String, String>,
        methods_by_class: &MethodsByClass,
        projection: &ProjectedGraph,
        import_graph: &ImportGraph,
        orchestrator: &mut crate::resolve::orchestrator::ResolutionOrchestrator,
    ) -> (Vec<crate::types::ResolvedCall>, Vec<(String, String)>) {
        let mut edge_pairs = Vec::new();

        // MRO-aware method lookup: per-function
        let my_parent_class = projection
            .functions
            .get(func_id)
            .and_then(|f| f.parent_class.clone());

        // This used to scan *every* function once per MRO node and once more
        // for the function's own class — O(functions² × mro_depth), paid
        // independently by each resolve thread. `methods_by_class` is the same
        // grouping computed once per pass.
        let mro_methods: std::collections::HashMap<String, String> =
            if let Some(ref class_id) = my_parent_class {
                let mut methods = std::collections::HashMap::new();
                let absorb = |cid: &str, methods: &mut std::collections::HashMap<String, String>| {
                    for (name, fid) in methods_by_class.get(cid).into_iter().flatten() {
                        methods.entry(name.clone()).or_insert_with(|| fid.clone());
                    }
                };
                if let Some(class) = projection.classes.get(class_id) {
                    for node in &class.mro {
                        if let MroNode::Class(ref cid) = node {
                            absorb(cid, &mut methods);
                        }
                    }
                }
                absorb(class_id, &mut methods);
                methods
            } else {
                std::collections::HashMap::new()
            };

        let resolved = orchestrator.resolve_calls(calls, func_id, import_graph);

        let resolved: Vec<_> = resolved
            .into_iter()
            .map(|rc| {
                if let crate::types::ResolvedCall::Unresolved { reason, raw } = &rc {
                    if matches!(reason, crate::types::UnresolvedReason::TypeInferenceRequired) {
                        if let Some(target_id) = mro_methods.get(&raw.name) {
                            return crate::types::ResolvedCall::Function(target_id.clone());
                        }
                    }
                    return rc;
                }
                if let crate::types::ResolvedCall::External(name) = &rc {
                    if let Some(target_id) = sibling_funcs.get(name.as_str()) {
                        return crate::types::ResolvedCall::Function(target_id.clone());
                    }
                    if let Some(target_mod_id) = import_targets.get(name.as_str()) {
                        if let Some(imported_func_id) = find_symbol_in_module(
                            projection, target_mod_id, name) {
                            return crate::types::ResolvedCall::Function(imported_func_id);
                        }
                    }
                    if let Some(target_id) = mro_methods.get(name.as_str()) {
                        return crate::types::ResolvedCall::Function(target_id.clone());
                    }
                }
                if let crate::types::ResolvedCall::Builtin(name) = &rc {
                    if let Some(target_id) = sibling_funcs.get(name.as_str()) {
                        return crate::types::ResolvedCall::Function(target_id.clone());
                    }
                }
                rc
            })
            .collect();

        // Build edge pairs (applied later by caller)
        for rc in &resolved {
            match rc {
                crate::types::ResolvedCall::Function(target_id)
                | crate::types::ResolvedCall::Constructor(target_id) => {
                    edge_pairs.push((func_id.to_string(), target_id.clone()));
                }
                crate::types::ResolvedCall::Method { method, .. } => {
                    edge_pairs.push((func_id.to_string(), method.clone()));
                }
                crate::types::ResolvedCall::Builtin(name)
                | crate::types::ResolvedCall::External(name) => {
                    let ext_id = format!("external::{}", name);
                    edge_pairs.push((func_id.to_string(), ext_id));
                }
                crate::types::ResolvedCall::Unresolved { .. } => {}
            }
        }

        (resolved, edge_pairs)
    }

    /// Resolve calls scoped to a single file (or all if None).
    pub(super) fn resolve_calls_scoped(&self, projection: &mut ProjectedGraph, scope_file: Option<&str>) {
        use crate::resolve::orchestrator::ResolutionOrchestrator;

        let mut orchestrator = ResolutionOrchestrator::with_config(&self.config.import_graph);
        // v0.5: Use the shared import graph (edges built during insert_extracted)
        // instead of a fresh empty graph, enabling multi-hop transitive resolution.
        let import_graph_guard = self.import_graph.read();
        let import_graph_ref: &crate::graph::ImportGraph = &import_graph_guard;

        // Collect calls (before mutating functions map)
        let all_calls: Vec<(String, Vec<crate::types::UnresolvedRef>)> = projection
            .functions
            .iter()
            .map(|(id, f)| (id.clone(), f.calls.clone()))
            .collect();

        // Filter to scoped file if specified
        let calls_to_resolve: Vec<&(String, Vec<crate::types::UnresolvedRef>)> = if let Some(fp) = scope_file {
            all_calls.iter().filter(|(fid, _)| {
                projection.functions.get(fid.as_str())
                    .map(|f| {
                        // `parent_module` is "<path>::module". This compared
                        // with `contains`, so scoping to `a.py` also swept in
                        // `xa.py` and `a.pyi` — re-resolving their calls and
                        // clearing their edges on an edit that missed them.
                        let path = f.parent_module
                            .rsplit_once("::")
                            .map(|(p, _)| p)
                            .unwrap_or(f.parent_module.as_str());
                        path == fp
                    })
                    .unwrap_or(false)
            }).collect()
        } else {
            all_calls.iter().collect()
        };

        // Early exit if no calls to resolve — avoid clearing edge maps
        let has_calls = calls_to_resolve.iter().any(|(_, calls)| !calls.is_empty());
        if !has_calls {
            return;
        }

        // v0.5: Scoped edge clearing — only remove edges from affected functions.
        // In unscoped mode (batch analyze), clear all and rebuild.
        if scope_file.is_some() {
            for (func_id, _) in &calls_to_resolve {
                // Remove outgoing edges from this function
                if let Some(callees) = projection.callees_by_caller.remove(func_id.as_str()) {
                    for callee in &callees {
                        if let Some(callers) = projection.callers_by_callee.get_mut(callee) {
                            callers.remove(func_id.as_str());
                        }
                    }
                }
            }
        } else {
            projection.callers_by_callee.clear();
            projection.callees_by_caller.clear();
        }

        // Group calls by parent module so we build sibling_funcs and
        // import_targets once per module, not once per function.
        // Technique adopted from CodeGraph's inline def-use tracking
        // (codegraph-kernel/src/python.rs): same-file lookups built once
        // during the walk and reused. MIT license.
        // https://github.com/opticsWolf/codegraph
        let mut by_module: std::collections::HashMap<EntityId, Vec<(&String, &Vec<crate::types::UnresolvedRef>)>> =
            std::collections::HashMap::new();
        for entry in &calls_to_resolve {
            let pm = projection.functions.get(entry.0.as_str())
                .map(|f| f.parent_module.clone())
                .unwrap_or_default();
            by_module.entry(pm).or_default().push((&entry.0, &entry.1));
        }

        // One pass over the functions, replacing two scans that each ran once
        // per module (siblings) and once per MRO node per function (methods).
        // Both were O(F) inside an O(F) loop; this is the grouping they were
        // recomputing.
        let mut siblings_by_module: std::collections::HashMap<EntityId, std::collections::HashMap<String, String>> =
            std::collections::HashMap::new();
        let mut methods_by_class: MethodsByClass = std::collections::HashMap::new();
        for (id, f) in projection.functions.iter() {
            // `insert`, not `or_insert`: the scan this replaces collected into
            // a HashMap, so a duplicate name kept the last one seen.
            siblings_by_module
                .entry(f.parent_module.clone())
                .or_default()
                .insert(f.name.clone(), id.clone());
            if let Some(class_id) = &f.parent_class {
                methods_by_class
                    .entry(class_id.clone())
                    .or_default()
                    .push((f.name.clone(), id.clone()));
            }
        }
        let siblings_by_module: std::collections::HashMap<EntityId, std::sync::Arc<std::collections::HashMap<String, String>>> =
            siblings_by_module.into_iter().map(|(k, v)| (k, std::sync::Arc::new(v))).collect();

        // Phase A: Build per-module lookups and collect work items.
        // Use Arc<HashMap> so per-function work items share the module lookups
        // (avoids 501×501 HashMap clones in the single-file 500-varargs case).
        type ModuleLookups = (
            std::sync::Arc<std::collections::HashMap<String, String>>,
            std::sync::Arc<std::collections::HashMap<String, String>>,
        );
        type WorkItem = (String, Vec<crate::types::UnresolvedRef>, ModuleLookups);

        let mut all_work: Vec<WorkItem> = Vec::new();

        for (parent_module, module_entries) in &by_module {
            let sibling_funcs = siblings_by_module
                .get(parent_module.as_str())
                .cloned()
                .unwrap_or_default();

            let mut import_targets_map = std::collections::HashMap::new();
            if let Some(module) = projection.modules.get(parent_module) {
                for import_id in &module.imports {
                    if let Some(import) = projection.imports.get(import_id) {
                        match &import.kind {
                            crate::types::ImportKind::FromImport { module: src_mod, names } => {
                                let target_mod_id = find_module_by_dotted_name(
                                    projection, src_mod, parent_module);
                                for (name, _alias) in names {
                                    if let Some(ref tgt_id) = target_mod_id {
                                        import_targets_map.insert(name.clone(), tgt_id.clone());
                                    }
                                }
                            }
                            crate::types::ImportKind::ModuleImport { module: src_mod, alias: _ } => {
                                if let Some(tgt_id) = find_module_by_dotted_name(
                                    projection, src_mod, parent_module)
                                {
                                    let short_name = src_mod.rsplit('.').next().unwrap_or(src_mod);
                                    import_targets_map.insert(short_name.to_string(), tgt_id);
                                }
                            }
                            crate::types::ImportKind::StarImport { module: src_mod } => {
                                if let Some(tgt_id) = find_module_by_dotted_name(
                                    projection, src_mod, parent_module)
                                {
                                    if let Some(tgt_module) = projection.modules.get(&tgt_id) {
                                        if let Some(ref exports) = tgt_module.star_exports {
                                            for name in exports {
                                                import_targets_map.insert(
                                                    name.clone(), tgt_id.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            let import_targets = std::sync::Arc::new(import_targets_map);
            let lookups: ModuleLookups = (sibling_funcs, import_targets);

            for (func_id, calls) in module_entries {
                all_work.push((
                    (*func_id).clone(),
                    (*calls).clone(),
                    lookups.clone(),  // Arc clone (reference count only)
                ));
            }
        }

        // Phase B: Resolve calls (parallel if enough work, else sequential).
        // Threads read from projection (immutable); writes are collected
        // and applied in Phase C.
        type ResolveResult = (String, Vec<crate::types::ResolvedCall>, Vec<(String, String)>);
        let results: Vec<ResolveResult>;

        if all_work.len() > 50 {
            // Cap at 4: benchmarking shows the cross-file benchmark (1 heavy
            // item + 995 empty) doesn't benefit from parallelism, but real
            // codebases with balanced call distribution will. The cap prevents
            // excessive thread spawn while keeping overhead minimal (~30ms).
            let num_threads = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(2);
            let chunk_size = (all_work.len() + num_threads - 1) / num_threads;
            let results_mutex = std::sync::Mutex::new(Vec::<ResolveResult>::new());
            let projection_ro: &ProjectedGraph = projection;  // shared borrow
            let methods_ref: &MethodsByClass = &methods_by_class;

            let import_cfg: &crate::graph::ImportGraphConfig = &self.config.import_graph;

            std::thread::scope(|s| {
                let results_ref = &results_mutex;
                for chunk in all_work.chunks(chunk_size) {
                    let chunk_owned: Vec<WorkItem> = chunk.iter().map(|(fid, c, lkp)| {
                        (fid.clone(), c.clone(), lkp.clone())
                    }).collect();
                    let import_graph = import_graph_ref;
                    s.spawn(move || {
                        let mut local = Vec::new();
                        let mut orch = ResolutionOrchestrator::with_config(import_cfg);
                        for (fid, calls, lkp) in &chunk_owned {
                            let (rc, ep) = Self::resolve_one_function(
                                fid, calls,
                                &lkp.0, &lkp.1, methods_ref,
                                projection_ro, import_graph,
                                &mut orch);
                            local.push((fid.clone(), rc, ep));
                        }
                        results_ref.lock().unwrap().extend(local);
                    });
                }
            });
            // projection_ro borrow ends — projection is exclusively mutable again
            results = results_mutex.into_inner().unwrap();
        } else {
            // Small work set — sequential (avoid thread overhead)
            let mut results_vec = Vec::new();
            for (fid, calls, lkp) in &all_work {
                let (rc, ep) = Self::resolve_one_function(
                    fid, calls,
                    &lkp.0, &lkp.1, &methods_by_class,
                    projection, import_graph_ref,
                    &mut orchestrator);
                results_vec.push((fid.clone(), rc, ep));
            }
            results = results_vec;
        }

        // Phase C: Apply results to projection (sequential)
        for (func_id, resolved, edge_pairs) in &results {
            if let Some(func_arc) = projection.functions.get(func_id.as_str()) {
                if func_arc.resolved_calls != *resolved {
                    let mut updated = (**func_arc).clone();
                    updated.resolved_calls = resolved.clone();
                    projection.functions.insert(func_id.clone(), std::sync::Arc::new(updated));
                }
            }

            for (source, target) in edge_pairs {
                projection
                    .callees_by_caller
                    .entry(source.clone())
                    .or_default()
                    .insert(target.clone());
                projection
                    .callers_by_callee
                    .entry(target.clone())
                    .or_default()
                    .insert(source.clone());
            }
        }
    }
}
