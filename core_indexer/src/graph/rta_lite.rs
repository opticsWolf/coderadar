// CodeRadar Stage 6.3 — RTA-lite: rapid type analysis, conservatively scoped.
//
// Fossil reference: none — this is the feature the plan says fossil cannot
// do (§11.3). Problem it addresses: `compute_reachable` extends liveness
// through virtual dispatch (a reachable base method keeps every override
// alive), which over-approximates. An override in a class that is never
// constructed anywhere in the indexed root can still be reported live — a
// dead-code false negative.
//
// Scope (per plan §11.3 conservative v1):
//   * Python-claims only in spirit; the mechanism is language-agnostic but
//     MRO/subclass data is exact for Python. Other languages inherit the
//     documented precision caveat from v0.8 Phase 2.3.
//   * We never DEMOTE anything the base detector calls live. We only ADD
//     findings: methods whose sole liveness comes from virtual dispatch and
//     whose defining class is never instantiated anywhere in the root.
//     Reported with the weakest evidence tier — deleting them is a judgment
//     call for the human, not an instruction.
//
// External-construction honesty: if instances could come from outside the
// indexed root (library consumed elsewhere), these findings would be wrong.
// The Speculative tier and the kind name carry that caveat.

use std::collections::HashSet;

use crate::types::{ProjectedGraph, ResolvedCall};

/// Classes constructed somewhere in the indexed root.
///
/// Two signals, unioned conservatively:
///   1. resolved `ResolvedCall::Constructor` edges (rare today — kept so
///      future resolver work lights this up automatically),
///   2. raw call sites whose name matches a known class simple name
///      (`x = Greeter()` currently resolves to nothing, but its UnresolvedRef
///      still carries the name). Name collisions only ever ADD classes here,
///      which suppresses RTA findings — the safe direction.
pub fn instantiated_classes(graph: &ProjectedGraph) -> HashSet<String> {
    let by_name: std::collections::HashMap<&str, &String> = graph
        .classes
        .iter()
        .map(|(id, c)| (c.name.as_str(), id))
        .collect();

    let mut out = HashSet::new();
    for f in graph.functions.values() {
        for rc in &f.resolved_calls {
            if let ResolvedCall::Constructor(class_id) = rc {
                out.insert(class_id.clone());
            }
        }
        for r in &f.calls {
            if let Some(class_id) = by_name.get(r.name.as_str()) {
                out.insert((*class_id).clone());
            }
        }
    }
    // Class-level field defaults construct at class-definition time
    // (`session_interface: SessionInterface = SecureCookieSessionInterface()`
    // builds the instance before any `__init__` runs; bare class-name
    // defaults (`= DefaultJSONProvider`) mark class-valued attrs whose call
    // sites construct). Both only ADD classes here — the safe direction.
    for c in graph.classes.values() {
        for fld in &c.fields {
            let Some(dv) = &fld.default_value else {
                continue;
            };
            // `Sub()`, `_ProxyIOp(operator.iadd)` or a bare
            // `DefaultJSONProvider`: the leading dotted identifier is the
            // class being constructed (or held for later construction).
            let head = dv.trim().split('(').next().unwrap_or("").trim();
            if head.is_empty()
                || !head
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.')
            {
                continue;
            }
            let name = head.rsplit('.').next().unwrap_or(head);
            if let Some(class_id) = by_name.get(name) {
                out.insert((*class_id).clone());
            }
        }
    }
    out
}

/// A method kept alive ONLY by virtual dispatch whose class is never
/// instantiated: prime dead-code candidate the base detector must miss.
#[derive(Clone, Debug)]
pub struct UninstantiatedOverride {
    /// The override method.
    pub entity_id: String,
    /// The class it is defined on.
    pub class_id: Option<String>,
}

/// Methods live in `reachable` (which includes virtual-dispatch extension)
/// but NOT reachable via direct call edges alone, on classes that are never
/// instantiated. `direct_reachable` must be computed WITHOUT the
/// overridden_by extension — see [`direct_call_reachable`].
pub fn uninstantiated_overrides(
    graph: &ProjectedGraph,
    reachable: &HashSet<String>,
    direct_reachable: &HashSet<String>,
) -> Vec<UninstantiatedOverride> {
    let instantiated = instantiated_classes(graph);

    let mut out = Vec::new();
    for id in reachable {
        // Directly called => its liveness doesn't rest on dispatch.
        if direct_reachable.contains(id) {
            continue;
        }
        let Some(f) = graph.functions.get(id) else {
            continue;
        };
        let Some(ref class_id) = f.parent_class else {
            continue;
        };
        // The method's own class must be un-instantiated AND every strict
        // subclass that overrides further down must not rescue it — covered
        // because each such override gets its own entry via the same rule.
        if instantiated.contains(class_id) {
            continue;
        }
        out.push(UninstantiatedOverride {
            entity_id: id.clone(),
            class_id: Some(class_id.clone()),
        });
    }
    out.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
    out
}

/// BFS over direct call edges, plus dispatch hops into classes that ARE
/// instantiated — same as `compute_reachable` minus the speculative part of
/// the virtual-dispatch extension.
///
/// The distinction matters (0.10 §2.5 re-run, flask `SessionInterface`): a
/// dispatch hop from a base method into an override whose class is built
/// somewhere in the root is real execution — the override runs, and whatever
/// it calls directly is live too. Only hops into NEVER-instantiated classes
/// are the speculation RTA exists to prune, and those stay out of this set so
/// their overrides remain candidates.
pub fn direct_call_reachable(graph: &ProjectedGraph, roots: &HashSet<String>) -> HashSet<String> {
    let instantiated = instantiated_classes(graph);
    let mut seen = roots.clone();
    let mut queue = std::collections::VecDeque::from_iter(roots.iter().cloned());
    while let Some(current) = queue.pop_front() {
        if let Some(callees) = graph.callees_by_caller.get(&current) {
            for callee in callees {
                if seen.insert(callee.clone()) {
                    queue.push_back(callee.clone());
                }
            }
        }
        if let Some(subs) = graph.overridden_by.get(&current) {
            for sub in subs {
                let sub_is_built = graph
                    .functions
                    .get(sub)
                    .and_then(|f| f.parent_class.as_deref())
                    .is_some_and(|c| instantiated.contains(c));
                if sub_is_built && seen.insert(sub.clone()) {
                    queue.push_back(sub.clone());
                }
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ReceiverShape, ResolvedCall};
    use std::sync::Arc;

    fn method_func(id: &str, class: Option<&str>) -> crate::types::Function {
        let mut f = crate::graph::deadcode::tests::func(id, id, "m.py::module");
        f.parent_class = class.map(|c| c.to_string());
        f
    }

    /// main() calls Base.b directly; Derived(Base) overrides b.
    fn fixture(with_derived_ctor: bool) -> ProjectedGraph {
        let mut g = crate::smells::engine::tests::empty_graph();
        for (id, class) in [
            ("m.py::main", None),
            ("m.py::Base.b", Some("m.py::Base")),
            ("m.py::Derived.b", Some("m.py::Derived")),
        ] {
            let mut f = method_func(id, class);
            if id == "m.py::main" && with_derived_ctor {
                // `d = Derived(...)` somewhere in the root.
                f.resolved_calls = vec![ResolvedCall::Constructor("m.py::Derived".into())];
            }
            g.functions.insert(id.to_string(), Arc::new(f));
        }
        let edge = |g: &mut ProjectedGraph, caller: &str, callee: &str| {
            g.callees_by_caller
                .entry(caller.into())
                .or_default()
                .insert(callee.into());
            g.callers_by_callee
                .entry(callee.into())
                .or_default()
                .insert(caller.into());
        };
        edge(&mut g, "m.py::main", "m.py::Base.b");
        // Virtual dispatch: Base.b can reach Derived.b.
        g.overridden_by
            .entry("m.py::Base.b".into())
            .or_default()
            .insert("m.py::Derived.b".into());
        g
    }

    fn roots() -> HashSet<String> {
        ["m.py::main".to_string()].into_iter().collect()
    }

    #[test]
    fn dispatch_only_liveness_detected_when_class_never_built() {
        let g = fixture(false);
        let direct = direct_call_reachable(&g, &roots());
        assert!(direct.contains("m.py::Base.b"));
        assert!(!direct.contains("m.py::Derived.b"));

        let live = crate::graph::deadcode::reachability::compute_reachable(&g, &roots());
        assert!(live.reachable.contains("m.py::Derived.b")); // base detector's view

        let cands = uninstantiated_overrides(&g, &live.reachable, &direct);
        let ids: Vec<&str> = cands.iter().map(|c| c.entity_id.as_str()).collect();
        assert_eq!(ids, vec!["m.py::Derived.b"]);
        assert_eq!(cands[0].class_id.as_deref(), Some("m.py::Derived"));
    }

    #[test]
    fn constructed_class_keeps_override_live() {
        let g = fixture(true); // main() constructs Derived somewhere
        let direct = direct_call_reachable(&g, &roots());
        let live = crate::graph::deadcode::reachability::compute_reachable(&g, &roots());
        let instantiated = instantiated_classes(&g);
        assert!(instantiated.contains("m.py::Derived"));
        let cands = uninstantiated_overrides(&g, &live.reachable, &direct);
        assert!(
            !cands.iter().any(|c| c.entity_id == "m.py::Derived.b"),
            "an override on a constructed class must never be flagged"
        );
    }

    #[test]
    fn detect_dead_emits_rta_dead_kind() {
        let g = fixture(false);
        let findings = crate::graph::deadcode::detect_dead(&g, Default::default());
        let hit = findings
            .iter()
            .find(|f| f.entity_id == "m.py::Derived.b")
            .expect("override must be re-flagged by RTA");
        assert_eq!(hit.kind, crate::graph::deadcode::DeadKind::RtaDead);
        // Weakest evidence class: must never reach the confident tiers.
        assert!(hit.score < 0.80, "rta-dead scored {}", hit.score);

        // With construction evidence the finding disappears entirely.
        let g2 = fixture(true);
        let findings2 = crate::graph::deadcode::detect_dead(&g2, Default::default());
        assert!(!findings2.iter().any(|f| f.entity_id == "m.py::Derived.b"
            && f.kind == crate::graph::deadcode::DeadKind::RtaDead));
    }

    #[test]
    fn directly_called_override_is_never_flagged() {
        let mut g = fixture(false);
        // Someone also calls Derived.b directly.
        let edge = |g: &mut ProjectedGraph, caller: &str, callee: &str| {
            g.callees_by_caller
                .entry(caller.into())
                .or_default()
                .insert(callee.into());
            g.callers_by_callee
                .entry(callee.into())
                .or_default()
                .insert(caller.into());
        };
        edge(&mut g, "m.py::main", "m.py::Derived.b");
        let direct = direct_call_reachable(&g, &roots());
        let live = crate::graph::deadcode::reachability::compute_reachable(&g, &roots());
        let cands = uninstantiated_overrides(&g, &live.reachable, &direct);
        assert!(cands.is_empty(), "direct calls are stronger evidence");
    }

    /// main -> Base.run (direct); Base.run overridden by Sub.run; Sub.run
    /// calls Base.helper (an MRO-resolved direct edge). Whether Base.helper
    /// counts as directly reachable depends on Sub being instantiated: the
    /// dispatch hop into an instantiated class is real execution (flask
    /// `SessionInterface.get_cookie_*`), the hop into an unbuilt one is the
    /// speculation RTA prunes.
    fn fixture_with_instantiated_sub(built: bool) -> ProjectedGraph {
        let mut g = crate::smells::engine::tests::empty_graph();
        for (id, class) in [
            ("m.py::main", None),
            ("m.py::Base.run", Some("m.py::Base")),
            ("m.py::Sub.run", Some("m.py::Sub")),
            ("m.py::Base.helper", Some("m.py::Base")),
        ] {
            let mut f = method_func(id, class);
            if built && id == "m.py::main" {
                f.resolved_calls = vec![ResolvedCall::Constructor("m.py::Sub".into())];
            }
            g.functions.insert(id.to_string(), Arc::new(f));
        }
        let edge = |g: &mut ProjectedGraph, caller: &str, callee: &str| {
            g.callees_by_caller
                .entry(caller.into())
                .or_default()
                .insert(callee.into());
            g.callers_by_callee
                .entry(callee.into())
                .or_default()
                .insert(caller.into());
        };
        edge(&mut g, "m.py::main", "m.py::Base.run");
        edge(&mut g, "m.py::Sub.run", "m.py::Base.helper");
        g.overridden_by
            .entry("m.py::Base.run".into())
            .or_default()
            .insert("m.py::Sub.run".into());
        g
    }

    #[test]
    fn dispatch_hop_into_instantiated_class_makes_callees_direct() {
        let built = fixture_with_instantiated_sub(true);
        let direct = direct_call_reachable(&built, &roots());
        assert!(
            direct.contains("m.py::Base.helper"),
            "an instantiated override's direct callees are live: {direct:?}"
        );

        let unbuilt = fixture_with_instantiated_sub(false);
        let direct = direct_call_reachable(&unbuilt, &roots());
        assert!(
            !direct.contains("m.py::Base.helper"),
            "an unbuilt class's override stays speculation: {direct:?}"
        );
    }

    #[test]
    fn receiver_shape_irrelevant_to_construction_scan() {
        // Constructor detection reads resolved_calls regardless of how the
        // call was shaped (Method with Unknown receiver still counts when a
        // Constructor call exists elsewhere).
        let g = fixture(true);
        assert_eq!(instantiated_classes(&g).len(), 1);
    }
}
