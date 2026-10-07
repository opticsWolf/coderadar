use super::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};
use super::CodeGraph;
use super::ImportGraph;
use crate::types::*;

use super::receiver_types::MethodsByClass;

/// What one function resolution produces: bound calls, plain edge pairs,
/// and edge pairs with receiver evidence. Factored out for
/// `type_complexity` (clippy 0).
type ResolveOutcome = (
    Vec<crate::types::ResolvedCall>,
    Vec<(String, String)>,
    Vec<((String, String), crate::types::ReceiverEvidence)>,
);

impl CodeGraph {
    /// Run the resolution cascade on all functions, or scoped to a single file.
    /// When `scope_file` is Some, only clears and rebuilds edges for functions
    /// in that file — used by `update_file` for O(changed) instead of O(all).
    pub fn resolve_all_calls(&self, projection: &mut ProjectedGraph) {
        self.resolve_calls_scoped(projection, None);
    }

    /// Resolve every function's `refs` (callbacks passed as values) to the
    /// functions they name, storing the result on `resolved_refs`. These are
    /// deliberately not call edges; dead-code treats them as liveness.
    fn resolve_refs_scoped(&self, projection: &mut ProjectedGraph, scope_file: Option<&str>) {
        let mut methods_by_class: MethodsByClass = std::collections::HashMap::new();
        for (id, f) in projection.functions.iter() {
            if let Some(class_id) = &f.parent_class {
                methods_by_class
                    .entry(class_id.clone())
                    .or_default()
                    .push((f.name.clone(), id.clone()));
            }
        }
        let types = super::receiver_types::TypeCtx {
            projection,
            methods_by_class: &methods_by_class,
        };
        let in_scope = |module_id: &str| {
            scope_file
                .is_none_or(|fp| module_id.rsplit_once("::").map_or(module_id, |(p, _)| p) == fp)
        };
        // Every class below `class_id`, transitively.
        let descendants = |class_id: &str| {
            let mut seen = std::collections::BTreeSet::new();
            let mut stack = vec![class_id.to_string()];
            while let Some(c) = stack.pop() {
                for sub in projection.subclasses.get(&c).into_iter().flatten() {
                    if seen.insert(sub.clone()) {
                        stack.push(sub.clone());
                    }
                }
            }
            seen
        };
        let mut updates: Vec<(EntityId, Vec<EntityId>)> = Vec::new();
        for (id, f) in projection.functions.iter() {
            if !in_scope(&f.parent_module) {
                continue;
            }
            // Nothing raw to resolve (a cold start restores only the
            // resolved side): keep what is there.
            if f.refs.is_empty() && f.calls.is_empty() {
                continue;
            }
            let mut targets: Vec<EntityId> = f
                .refs
                .iter()
                .filter_map(|r| types.resolve_ref(id, &r.path, &r.name))
                .collect();
            // Template methods: a mixin's `self.m()` that its own MRO cannot
            // answer dispatches to the subclasses that define `m`.
            if let Some(class_id) = &f.parent_class {
                for c in &f.calls {
                    let on_self = matches!(c.path.as_slice(), [p] if p == "self" || p == "cls");
                    if !on_self || types.method_of(class_id, &c.name).is_some() {
                        continue;
                    }
                    for sub in descendants(class_id) {
                        if let Some(ms) = methods_by_class.get(&sub) {
                            targets.extend(
                                ms.iter()
                                    .filter(|(n, _)| *n == c.name)
                                    .map(|(_, mid)| mid.clone()),
                            );
                        }
                    }
                }
            }
            targets.sort();
            targets.dedup();
            if targets != f.resolved_refs {
                updates.push((id.clone(), targets));
            }
        }
        let mut module_updates: Vec<(EntityId, Vec<EntityId>)> = Vec::new();
        for (mid, m) in projection.modules.iter() {
            if m.uses.is_empty() || !in_scope(mid) {
                continue;
            }
            let mut targets: Vec<EntityId> = m
                .uses
                .iter()
                .filter_map(|r| types.resolve_module_use(mid, &r.path, &r.name))
                .collect();
            targets.sort();
            targets.dedup();
            if targets != m.resolved_uses {
                module_updates.push((mid.clone(), targets));
            }
        }
        for (id, targets) in updates {
            if let Some(arc) = projection.functions.get(&id) {
                let mut updated = (**arc).clone();
                updated.resolved_refs = targets;
                projection
                    .functions
                    .insert(id, std::sync::Arc::new(updated));
            }
        }
        for (id, targets) in module_updates {
            if let Some(arc) = projection.modules.get(&id) {
                let mut updated = (**arc).clone();
                updated.resolved_uses = targets;
                projection.modules.insert(id, std::sync::Arc::new(updated));
            }
        }
    }

    /// Pure resolution of a single function's calls — all data passed as parameters.
    /// Returns (resolved_calls, edge_pairs) without mutating the projection.
    /// The projection is read-only during resolution; edge pairs are applied
    /// sequentially afterward. Thread-safe: no `&self`, each thread creates
    /// its own orchestrator.
    ///
    /// Technique adopted from CodeGraph's per-file resolution in
    /// resolve/index.ts (MIT license, https://github.com/opticsWolf/codegraph).
    /// Eight params because resolution needs every index at once; bundling
    /// them into a struct would churn every caller for no behavior gain.
    #[allow(clippy::too_many_arguments)]
    fn resolve_one_function(
        func_id: &str,
        calls: &[crate::types::UnresolvedRef],
        sibling_funcs: &std::collections::HashMap<String, String>,
        import_targets: &std::collections::HashMap<String, (String, String)>,
        methods_by_class: &MethodsByClass,
        projection: &ProjectedGraph,
        import_graph: &ImportGraph,
        orchestrator: &mut crate::resolve::orchestrator::ResolutionOrchestrator,
    ) -> ResolveOutcome {
        let mut edge_pairs = Vec::new();
        let mut evidence_pairs: Vec<((String, String), crate::types::ReceiverEvidence)> =
            Vec::new();

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

        let parent_module = projection
            .functions
            .get(func_id)
            .map(|f| f.parent_module.clone())
            .unwrap_or_default();
        // Receiver typing lives in `receiver_types`; these are thin shims.
        let types = super::receiver_types::TypeCtx {
            projection,
            methods_by_class,
        };
        let class_named = |name: &str| types.class_in_scope(&parent_module, name);
        let method_of = |class_id: &str, name: &str| types.method_of(class_id, name);

        // Python resolves a bare name lexically: the enclosing functions'
        // locals (nested defs; a parameter or assignment shadows), then the
        // module's own names and imports. Methods are never in that chain,
        // so `apply_fix()` inside `Model.apply_fix` is the imported function.
        let is_python = projection
            .modules
            .get(&parent_module)
            .is_some_and(|m| matches!(m.language, Language::Python));
        let python_bare = |name: &str| -> Option<crate::types::ResolvedCall> {
            let shadows = |g: &Function| {
                g.parameters.iter().any(|p| p.name == name)
                    || g.bindings
                        .iter()
                        .any(|b| b.target.len() == 1 && b.target[0] == name)
            };
            let mut outer = func_id;
            while let Some(f) = projection.functions.get(outer) {
                let nested = format!("{outer}.{name}");
                if projection.functions.contains_key(&nested) {
                    return Some(crate::types::ResolvedCall::Function(nested));
                }
                if shadows(f) {
                    // A local value: whatever it holds, it is not resolvable.
                    return Some(crate::types::ResolvedCall::External(name.to_string()));
                }
                match outer.rsplit_once('.') {
                    Some((head, _)) => outer = head,
                    None => break,
                }
            }
            let id = find_symbol_in_module(projection, &parent_module, name)?;
            Some(if projection.classes.contains_key(&id) {
                crate::types::ResolvedCall::Constructor(id)
            } else {
                crate::types::ResolvedCall::Function(id)
            })
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
                        match types.resolve_call_value(func_id, &raw.path, &raw.name) {
                            Some(crate::types::CallTarget::Method(mid, ev)) => {
                                evidence_pairs.push(((mid.clone(), func_id.to_string()), ev));
                                return crate::types::ResolvedCall::Function(mid);
                            }
                            // Class-valued attribute: constructs the class held
                            // in the attribute.
                            Some(crate::types::CallTarget::Ctor(cid)) => {
                                return crate::types::ResolvedCall::Constructor(cid);
                            }
                            None => {}
                        }
                    }
                    // `mod.f()` / `pkg.sub.f()` where the dotted prefix names an
                    // imported module: look `f` up inside that module.
                    if !on_self
                        && !raw.path.is_empty()
                        && !raw.path.iter().any(|s| s.starts_with('<'))
                    {
                        let dotted = raw.path.join(".");
                        if let Some(mid) =
                            find_module_by_dotted_name(projection, &dotted, &parent_module)
                        {
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
                if is_python {
                    if let crate::types::ResolvedCall::External(name)
                    | crate::types::ResolvedCall::Builtin(name) = &rc
                    {
                        if let Some(bound) = python_bare(name) {
                            return bound;
                        }
                        if matches!(rc, crate::types::ResolvedCall::Builtin(_)) {
                            return rc;
                        }
                    }
                }
                if let crate::types::ResolvedCall::External(name) = &rc {
                    if is_python {
                        // Only the import alias map is left to try: Python
                        // never binds a bare name to a method.
                        if let Some((target_mod_id, original)) = import_targets.get(name.as_str()) {
                            let symbol = if original.is_empty() { name } else { original };
                            if let Some(imported_func_id) =
                                find_symbol_in_module(projection, target_mod_id, symbol)
                            {
                                return crate::types::ResolvedCall::Function(imported_func_id);
                            }
                        }
                        return rc;
                    }
                    if let Some(target_id) = sibling_funcs.get(name.as_str()) {
                        return crate::types::ResolvedCall::Function(target_id.clone());
                    }
                    // (module id, original name): `from x import a as b` binds
                    // `b`, but the symbol in `x` is still called `a`.
                    if let Some((target_mod_id, original)) = import_targets.get(name.as_str()) {
                        let symbol = if original.is_empty() { name } else { original };
                        if let Some(imported_func_id) =
                            find_symbol_in_module(projection, target_mod_id, symbol)
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
                    let target =
                        method_of(class_id, "__init__").unwrap_or_else(|| class_id.clone());
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

        (resolved, edge_pairs, evidence_pairs)
    }

    /// Resolve calls scoped to a single file (or all if None).
    pub(super) fn resolve_calls_scoped(
        &self,
        projection: &mut ProjectedGraph,
        scope_file: Option<&str>,
    ) {
        use crate::resolve::orchestrator::ResolutionOrchestrator;

        self.resolve_refs_scoped(projection, scope_file);

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
            std::sync::Arc<std::collections::HashMap<String, (String, String)>>,
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
                            }
                            | crate::types::ImportKind::RelativeImport {
                                module: Some(src_mod),
                                names,
                                ..
                            } => {
                                let target_mod_id =
                                    find_module_by_dotted_name(projection, src_mod, parent_module);
                                for (name, alias) in names {
                                    if let Some(ref tgt_id) = target_mod_id {
                                        // The *local* binding is what a call site
                                        // uses: `from x import a as b` calls `b`.
                                        let local = alias.as_deref().unwrap_or(name.as_str());
                                        import_targets_map.insert(
                                            local.to_string(),
                                            (tgt_id.clone(), name.clone()),
                                        );
                                    }
                                }
                            }
                            // `from . import x`: the names live in the package
                            // itself, which `parent_module` already names.
                            crate::types::ImportKind::RelativeImport {
                                module: None,
                                names,
                                ..
                            } => {
                                let pkg = parent_module
                                    .rsplit_once("::")
                                    .map(|(p, _)| p)
                                    .unwrap_or(parent_module.as_str());
                                let pkg_dir = pkg.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                                let target_mod_id = find_module_by_dotted_name(
                                    projection,
                                    &format!("{pkg_dir}/__init__.py"),
                                    parent_module,
                                );
                                if let Some(tgt_id) = target_mod_id {
                                    for (name, alias) in names {
                                        let local = alias.as_deref().unwrap_or(name.as_str());
                                        import_targets_map.insert(
                                            local.to_string(),
                                            (tgt_id.clone(), name.clone()),
                                        );
                                    }
                                }
                            }
                            crate::types::ImportKind::ModuleImport {
                                module: src_mod,
                                alias,
                            } => {
                                if let Some(tgt_id) =
                                    find_module_by_dotted_name(projection, src_mod, parent_module)
                                {
                                    // `import numpy as np` binds `np`, not
                                    // `numpy`; the short name is the fallback.
                                    let local = alias.clone().unwrap_or_else(|| {
                                        src_mod.rsplit('.').next().unwrap_or(src_mod).to_string()
                                    });
                                    // `mod.f()` resolves `f` inside `mod`; the
                                    // empty second slot means "look up the name
                                    // being called".
                                    import_targets_map.insert(local, (tgt_id, String::new()));
                                }
                            }
                            crate::types::ImportKind::StarImport { module: src_mod } => {
                                if let Some(tgt_id) =
                                    find_module_by_dotted_name(projection, src_mod, parent_module)
                                {
                                    if let Some(tgt_module) = projection.modules.get(&tgt_id) {
                                        if let Some(ref exports) = tgt_module.star_exports {
                                            for name in exports {
                                                import_targets_map.insert(
                                                    name.clone(),
                                                    (tgt_id.clone(), name.clone()),
                                                );
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
            Vec<((String, String), crate::types::ReceiverEvidence)>,
        );

        let results: Vec<ResolveResult> = if all_work.len() > 50 {
            // Cap at 4: benchmarking shows the cross-file benchmark (1 heavy
            // item + 995 empty) doesn't benefit from parallelism, but real
            // codebases with balanced call distribution will. The cap prevents
            // excessive thread spawn while keeping overhead minimal (~30ms).
            let num_threads = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(2);
            let chunk_size = all_work.len().div_ceil(num_threads);
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
                            let (rc, ep, ev) = Self::resolve_one_function(
                                fid,
                                calls,
                                &lkp.0,
                                &lkp.1,
                                methods_ref,
                                projection_ro,
                                import_graph,
                                &mut orch,
                            );
                            local.push((fid.clone(), rc, ep, ev));
                        }
                        results_ref.lock().unwrap().extend(local);
                    });
                }
            });
            // projection_ro borrow ends — projection is exclusively mutable again
            results_mutex.into_inner().unwrap()
        } else {
            // Small work set — sequential (avoid thread overhead)
            let mut results_vec = Vec::new();
            for (fid, calls, lkp) in &all_work {
                let (rc, ep, ev) = Self::resolve_one_function(
                    fid,
                    calls,
                    &lkp.0,
                    &lkp.1,
                    &methods_by_class,
                    projection,
                    import_graph_ref,
                    &mut orchestrator,
                );
                results_vec.push((fid.clone(), rc, ep, ev));
            }
            results_vec
        };

        // Phase C: Apply results to projection (sequential)
        for (func_id, resolved, edge_pairs, evidence_pairs) in &results {
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
            // Receiver-typing provenance for the §2.4 weighting. Scoped runs
            // re-resolve only the changed file's functions, so their entries
            // override the previous tags for the same (callee, caller) pairs;
            // pairs whose caller is gone vanish with the caller index.
            for (key, ev) in evidence_pairs {
                projection.call_evidence.insert(key.clone(), ev.clone());
            }
        }
    }
}
