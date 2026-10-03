use super::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};
use super::CodeGraph;
use super::ImportGraph;
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
        let mro_methods: std::collections::HashMap<String, String> = if let Some(ref class_id) =
            my_parent_class
        {
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

        // A class named in this function's module or imported into it.
        let parent_module = projection
            .functions
            .get(func_id)
            .map(|f| f.parent_module.clone())
            .unwrap_or_default();
        let class_named = |name: &str| -> Option<EntityId> {
            if let Some(m) = projection.modules.get(&parent_module) {
                for cid in &m.classes {
                    if projection.classes.get(cid).map_or(false, |c| c.name == name) {
                        return Some(cid.clone());
                    }
                }
            }
            let tgt = import_targets.get(name)?;
            let id = find_symbol_in_module(projection, tgt, name)?;
            projection.classes.contains_key(&id).then_some(id)
        };
        // `name` on `class_id` or anything in its MRO.
        let method_of = |class_id: &str, name: &str| -> Option<EntityId> {
            let find = |cid: &str| {
                methods_by_class
                    .get(cid)
                    .and_then(|ms| ms.iter().find(|(n, _)| n == name).map(|(_, id)| id.clone()))
            };
            find(class_id).or_else(|| {
                projection.classes.get(class_id)?.mro.iter().find_map(|node| match node {
                    MroNode::Class(cid) => find(cid),
                    _ => None,
                })
            })
        };

        // ── Receiver typing (plan 1.2) ───────────────────────────────────
        // Flow-insensitive, one hop: constructor assignments, annotations
        // and return annotations. Anything ambiguous yields `None`.
        // All the evidence must agree on one class: a single piece that names
        // no class (`x = other()`, `x: int`) makes the whole name unknown.
        let unique = |v: Vec<Option<EntityId>>| -> Option<EntityId> {
            let mut v = v.into_iter().collect::<Option<Vec<_>>>()?;
            v.sort();
            v.dedup();
            (v.len() == 1).then(|| v.remove(0))
        };
        // Every name below is looked up in a `scope` module: an annotation or
        // constructor reads in the scope of the function that wrote it, not
        // of the caller.
        let class_in_scope = |scope: &str, name: &str| -> Option<EntityId> {
            if scope == parent_module {
                return class_named(name);
            }
            let id = find_symbol_in_module(projection, scope, name)?;
            projection.classes.contains_key(&id).then_some(id)
        };
        // `Foo` or `pkg.Foo` -> class id.
        let class_of_ref = |scope: &str, path: &[String], name: &str| -> Option<EntityId> {
            if path.is_empty() {
                return class_in_scope(scope, name);
            }
            let mid = find_module_by_dotted_name(projection, &path.join("."), scope)?;
            let id = find_symbol_in_module(projection, &mid, name)?;
            projection.classes.contains_key(&id).then_some(id)
        };
        // `Foo`, `"Foo"`, `Optional[Foo]`, `Foo | None`, `pkg.Foo` -> class id.
        let class_of_annotation = |ann: &str, scope: &str| -> Option<EntityId> {
            let mut a = ann.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            if let Some(inner) = a.strip_prefix("Optional[").and_then(|s| s.strip_suffix(']')) {
                a = inner.trim();
            }
            let a = a.split('|').map(str::trim).find(|p| *p != "None")?;
            if a.is_empty() || !a.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                return None;
            }
            let (path, name) = match a.rsplit_once('.') {
                Some((p, n)) => (p.split('.').map(String::from).collect::<Vec<_>>(), n),
                None => (vec![], a),
            };
            class_of_ref(scope, &path, name)
        };
        // What `name(...)` / `path.name(...)` produces: the class itself, or
        // what the function's return annotation / `return Ctor()` names.
        let class_of_call = |scope: &str, path: &[String], name: &str| -> Option<EntityId> {
            if let Some(c) = class_of_ref(scope, path, name) {
                return Some(c);
            }
            let fid = if path.is_empty() {
                find_symbol_in_module(projection, scope, name)?
            } else {
                let mid = find_module_by_dotted_name(projection, &path.join("."), scope)?;
                find_symbol_in_module(projection, &mid, name)?
            };
            let f = projection.functions.get(&fid)?;
            if let Some(c) = f.return_type.as_deref().and_then(|a| class_of_annotation(a, &f.parent_module)) {
                return Some(c);
            }
            // One hop only: `return Ctor()`, never a chain of factories.
            unique(
                f.bindings
                    .iter()
                    .filter(|b| b.target.len() == 1 && b.target[0] == "<return>")
                    .filter_map(|b| b.rhs.as_ref())
                    .map(|r| class_of_ref(&f.parent_module, &r.path, &r.name))
                    .collect(),
            )
        };
        let class_of_binding = |b: &Binding, scope: &str| -> Option<EntityId> {
            if let Some(c) = b.annotation.as_deref().and_then(|a| class_of_annotation(a, scope)) {
                return Some(c);
            }
            let r = b.rhs.as_ref()?;
            class_of_call(scope, &r.path, &r.name)
        };
        let this_func = projection.functions.get(func_id);
        let local_type = |name: &str| -> Option<EntityId> {
            let f = this_func?;
            let mut found = Vec::new();
            for p in f.parameters.iter().filter(|p| p.name == name && p.annotation.is_some()) {
                found.push(p.annotation.as_deref().and_then(|a| class_of_annotation(a, &parent_module)));
            }
            for b in f.bindings.iter().filter(|b| b.target.len() == 1 && b.target[0] == name) {
                found.push(class_of_binding(b, &parent_module));
            }
            unique(found)
        };
        // Type of `self.<attr>` on a class: bindings in its methods and
        // annotated class-level fields, each read in its own module's scope.
        let attr_type = |class_id: &str, attr: &str| -> Option<EntityId> {
            let mut scan = vec![class_id.to_string()];
            if let Some(c) = projection.classes.get(class_id) {
                scan.extend(c.mro.iter().filter_map(|n| match n {
                    MroNode::Class(cid) => Some(cid.clone()),
                    _ => None,
                }));
            }
            let mut found = Vec::new();
            for cid in &scan {
                if let Some(c) = projection.classes.get(cid) {
                    for fld in c.fields.iter().filter(|f| f.name == attr && f.annotation.is_some()) {
                        found.push(
                            fld.annotation.as_deref().and_then(|a| class_of_annotation(a, &c.parent_module)),
                        );
                    }
                }
                for (_, fid) in methods_by_class.get(cid).into_iter().flatten() {
                    let Some(f) = projection.functions.get(fid) else { continue };
                    for b in f.bindings.iter().filter(|b| b.target.len() == 2 && b.target[1] == attr) {
                        found.push(class_of_binding(b, &f.parent_module));
                    }
                }
            }
            unique(found)
        };
        let type_of_path = |path: &[String]| -> Option<EntityId> {
            let first = path.first()?;
            let mut t = if first == "self" || first == "cls" {
                my_parent_class.clone()?
            } else if let Some(call) = first.strip_prefix("<call:").and_then(|s| s.strip_suffix('>')) {
                let (p, n) = match call.rsplit_once('.') {
                    Some((p, n)) => (p.split('.').map(String::from).collect::<Vec<_>>(), n),
                    None => (vec![], call),
                };
                class_of_call(&parent_module, &p, n)?
            } else {
                local_type(first)?
            };
            for seg in &path[1..] {
                t = attr_type(&t, seg)?;
            }
            Some(t)
        };

        let resolved: Vec<_> = resolved
            .into_iter()
            .map(|rc| {
                if let crate::types::ResolvedCall::Unresolved { reason, raw } = &rc {
                    // The enclosing class's own MRO answers `self.m()` / `cls.m()`
                    // and nothing else: `self.attr.m()` or `obj.m()` must not bind
                    // to a same-named method of the caller's class.
                    let on_self = matches!(raw.path.as_slice(), [p] if p == "self" || p == "cls");
                    if on_self
                        && matches!(
                            reason,
                            crate::types::UnresolvedReason::TypeInferenceRequired
                        )
                    {
                        if let Some(target_id) = mro_methods.get(&raw.name) {
                            return crate::types::ResolvedCall::Function(target_id.clone());
                        }
                    }
                    // `var.m()`, `self.attr.m()`, `f().m()`: type the receiver.
                    if !raw.path.is_empty() {
                        if let Some(mid) =
                            type_of_path(&raw.path).and_then(|t| method_of(&t, &raw.name))
                        {
                            return crate::types::ResolvedCall::Function(mid);
                        }
                    }
                    // `mod.f()` / `pkg.sub.f()` where the dotted prefix names an
                    // imported module: look `f` up inside that module.
                    if !on_self && !raw.path.is_empty() && !raw.path.iter().any(|s| s.starts_with('<'))
                    {
                        let dotted = raw.path.join(".");
                        if let Some(mid) = find_module_by_dotted_name(projection, &dotted, &parent_module) {
                            if let Some(id) = find_symbol_in_module(projection, &mid, &raw.name) {
                                return if projection.classes.contains_key(&id) {
                                    crate::types::ResolvedCall::Constructor(id)
                                } else {
                                    crate::types::ResolvedCall::Function(id)
                                };
                            }
                        }
                    }
                    return rc;
                }
                // `ClassName.method()`: the orchestrator guesses a class from
                // the capital letter and synthesises "Class::method", which is
                // not an entity id. Look the class up; never invent an id.
                if let crate::types::ResolvedCall::Method {
                    receiver: crate::types::ReceiverShape::ClassRef(prefix),
                    method,
                } = &rc
                {
                    let name = method.rsplit("::").next().unwrap_or(method);
                    let bound = class_named(prefix).and_then(|cid| method_of(&cid, name));
                    return match bound {
                        Some(mid) => crate::types::ResolvedCall::Function(mid),
                        None => crate::types::ResolvedCall::External(format!("{prefix}.{name}")),
                    };
                }
                if let crate::types::ResolvedCall::External(name) = &rc {
                    if let Some(target_id) = sibling_funcs.get(name.as_str()) {
                        return crate::types::ResolvedCall::Function(target_id.clone());
                    }
                    if let Some(target_mod_id) = import_targets.get(name.as_str()) {
                        if let Some(imported_func_id) =
                            find_symbol_in_module(projection, target_mod_id, name)
                        {
                            return crate::types::ResolvedCall::Function(imported_func_id);
                        }
                    }
                    if let Some(cid) = class_named(name) {
                        return crate::types::ResolvedCall::Constructor(cid);
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
                crate::types::ResolvedCall::Function(target_id) => {
                    edge_pairs.push((func_id.to_string(), target_id.clone()));
                }
                // Instantiation runs `__init__` when the class (or a base)
                // defines one; otherwise the edge lands on the class itself.
                crate::types::ResolvedCall::Constructor(class_id) => {
                    let target = method_of(class_id, "__init__").unwrap_or_else(|| class_id.clone());
                    edge_pairs.push((func_id.to_string(), target));
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
    pub(super) fn resolve_calls_scoped(
        &self,
        projection: &mut ProjectedGraph,
        scope_file: Option<&str>,
    ) {
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
        let calls_to_resolve: Vec<&(String, Vec<crate::types::UnresolvedRef>)> =
            if let Some(fp) = scope_file {
                all_calls
                    .iter()
                    .filter(|(fid, _)| {
                        projection
                            .functions
                            .get(fid.as_str())
                            .map(|f| {
                                // `parent_module` is "<path>::module". This compared
                                // with `contains`, so scoping to `a.py` also swept in
                                // `xa.py` and `a.pyi` — re-resolving their calls and
                                // clearing their edges on an edit that missed them.
                                let path = f
                                    .parent_module
                                    .rsplit_once("::")
                                    .map(|(p, _)| p)
                                    .unwrap_or(f.parent_module.as_str());
                                path == fp
                            })
                            .unwrap_or(false)
                    })
                    .collect()
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
        // Issue 8: the clear used to drop synthetic edges (framework routes,
        // registered bulk pairs) whose source sits in the updated file — a
        // no-op update_file then lost `run -> combine` forever. Synthetic
        // pairs are tracked apart in `synthetic_edges`; snapshot and
        // re-assert them so only natural CALLS edges are rebuilt.
        if scope_file.is_some() {
            let scoped: std::collections::HashSet<String> = calls_to_resolve
                .iter()
                .map(|(fid, _)| (*fid).clone())
                .collect();
            let keep: Vec<(String, String)> = projection
                .synthetic_edges
                .iter()
                .filter(|(s, _)| scoped.contains(s))
                .cloned()
                .collect();
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
            for (s, t) in keep {
                projection
                    .callees_by_caller
                    .entry(s.clone())
                    .or_default()
                    .insert(t.clone());
                projection.callers_by_callee.entry(t).or_default().insert(s);
            }
        } else {
            let keep: Vec<(String, String)> = projection.synthetic_edges.iter().cloned().collect();
            projection.callers_by_callee.clear();
            projection.callees_by_caller.clear();
            for (s, t) in keep {
                projection
                    .callees_by_caller
                    .entry(s.clone())
                    .or_default()
                    .insert(t.clone());
                projection.callers_by_callee.entry(t).or_default().insert(s);
            }
        }

        // Group calls by parent module so we build sibling_funcs and
        // import_targets once per module, not once per function.
        // Technique adopted from CodeGraph's inline def-use tracking
        // (codegraph-kernel/src/python.rs): same-file lookups built once
        // during the walk and reused. MIT license.
        // https://github.com/opticsWolf/codegraph
        let mut by_module: std::collections::HashMap<
            EntityId,
            Vec<(&String, &Vec<crate::types::UnresolvedRef>)>,
        > = std::collections::HashMap::new();
        for entry in &calls_to_resolve {
            let pm = projection
                .functions
                .get(entry.0.as_str())
                .map(|f| f.parent_module.clone())
                .unwrap_or_default();
            by_module.entry(pm).or_default().push((&entry.0, &entry.1));
        }

        // One pass over the functions, replacing two scans that each ran once
        // per module (siblings) and once per MRO node per function (methods).
        // Both were O(F) inside an O(F) loop; this is the grouping they were
        // recomputing.
        let mut siblings_by_module: std::collections::HashMap<
            EntityId,
            std::collections::HashMap<String, String>,
        > = std::collections::HashMap::new();
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
        let siblings_by_module: std::collections::HashMap<
            EntityId,
            std::sync::Arc<std::collections::HashMap<String, String>>,
        > = siblings_by_module
            .into_iter()
            .map(|(k, v)| (k, std::sync::Arc::new(v)))
            .collect();

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
                            crate::types::ImportKind::FromImport {
                                module: src_mod,
                                names,
                            } => {
                                let target_mod_id =
                                    find_module_by_dotted_name(projection, src_mod, parent_module);
                                for (name, _alias) in names {
                                    if let Some(ref tgt_id) = target_mod_id {
                                        import_targets_map.insert(name.clone(), tgt_id.clone());
                                    }
                                }
                            }
                            crate::types::ImportKind::ModuleImport {
                                module: src_mod,
                                alias: _,
                            } => {
                                if let Some(tgt_id) =
                                    find_module_by_dotted_name(projection, src_mod, parent_module)
                                {
                                    let short_name = src_mod.rsplit('.').next().unwrap_or(src_mod);
                                    import_targets_map.insert(short_name.to_string(), tgt_id);
                                }
                            }
                            crate::types::ImportKind::StarImport { module: src_mod } => {
                                if let Some(tgt_id) =
                                    find_module_by_dotted_name(projection, src_mod, parent_module)
                                {
                                    if let Some(tgt_module) = projection.modules.get(&tgt_id) {
                                        if let Some(ref exports) = tgt_module.star_exports {
                                            for name in exports {
                                                import_targets_map
                                                    .insert(name.clone(), tgt_id.clone());
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
                    lookups.clone(), // Arc clone (reference count only)
                ));
            }
        }

        // Phase B: Resolve calls (parallel if enough work, else sequential).
        // Threads read from projection (immutable); writes are collected
        // and applied in Phase C.
        type ResolveResult = (
            String,
            Vec<crate::types::ResolvedCall>,
            Vec<(String, String)>,
        );
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
            let projection_ro: &ProjectedGraph = projection; // shared borrow
            let methods_ref: &MethodsByClass = &methods_by_class;

            let import_cfg: &crate::graph::ImportGraphConfig = &self.config.import_graph;

            std::thread::scope(|s| {
                let results_ref = &results_mutex;
                for chunk in all_work.chunks(chunk_size) {
                    let chunk_owned: Vec<WorkItem> = chunk
                        .iter()
                        .map(|(fid, c, lkp)| (fid.clone(), c.clone(), lkp.clone()))
                        .collect();
                    let import_graph = import_graph_ref;
                    s.spawn(move || {
                        let mut local = Vec::new();
                        let mut orch = ResolutionOrchestrator::with_config(import_cfg);
                        for (fid, calls, lkp) in &chunk_owned {
                            let (rc, ep) = Self::resolve_one_function(
                                fid,
                                calls,
                                &lkp.0,
                                &lkp.1,
                                methods_ref,
                                projection_ro,
                                import_graph,
                                &mut orch,
                            );
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
                    fid,
                    calls,
                    &lkp.0,
                    &lkp.1,
                    &methods_by_class,
                    projection,
                    import_graph_ref,
                    &mut orchestrator,
                );
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
                    projection
                        .functions
                        .insert(func_id.clone(), std::sync::Arc::new(updated));
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
