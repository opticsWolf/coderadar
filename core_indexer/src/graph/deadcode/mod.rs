// CodeRadar Stage 1.3 — dead-code detection: classification + confidence.
//
// The mirror image of `affected`: "what can be deleted?" Fossil reference:
// `src/dead_code/detector.rs` + `classifier.rs`, re-derived for CodeRadar's
// resolved projection and the Stage-0 scoring module.
//
// Classification (per unreachable function):
//   * TestOnly        — reachable only when test entry points are seeded
//   * TransitivelyDead— has callers, but every caller is itself dead
//   * Unreachable     — zero inbound call edges at all
//
// Confidence combines isolation strength × parse quality through
// `scoring::combine` so tiers stay consistent with every other derived
// analysis (Stage 0.1).

pub mod entry_points;
pub mod reachability;

use crate::scoring::{combine, tier_of, Tier};
use crate::types::{EntityId, ParseQuality, ProjectedGraph};

use self::entry_points::EntryPoints;
use self::reachability::compute_reachable;

/// Why a function counts as dead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeadKind {
    /// No path from any production entry point and no inbound edges at all.
    Unreachable,
    /// Reached only from other dead code (a dead chain — fossil's
    /// "3-level dead chain" reported transitively, not per-member).
    TransitivelyDead,
    /// Reachable only from test code; deleting it breaks tests.
    TestOnly,
    /// Stage 6.3 (RTA-lite): live ONLY via virtual dispatch, on a class
    /// that is never constructed anywhere in the indexed root. Weakest
    /// evidence — instances could come from outside the root.
    RtaDead,
}

impl DeadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeadKind::Unreachable => "unreachable",
            DeadKind::TransitivelyDead => "transitively-dead",
            DeadKind::TestOnly => "test-only",
            DeadKind::RtaDead => "rta-dead",
        }
    }

    /// Isolation strength evidence — being provably untouched is stronger
    /// evidence than being merely part of a dead chain.
    fn isolation(self) -> f32 {
        match self {
            DeadKind::Unreachable => 0.9,
            DeadKind::TransitivelyDead => 0.65,
            DeadKind::TestOnly => 0.5,
            DeadKind::RtaDead => 0.45,
        }
    }
}

/// A ranked deletability finding.
#[derive(Clone, Debug)]
pub struct DeadFinding {
    pub entity_id: EntityId,
    pub kind: DeadKind,
    pub tier: Tier,
    /// Combined confidence in (0, 1].
    pub score: f32,
    /// Lines removable if this entity goes away — drives ranking/severity.
    pub removable_lines: usize,
    /// Why this is believed dead — entry-point rejections the ladder made
    /// plus the isolation fact itself (plan v0.10 §2.6). Machine-oriented
    /// short phrases; a finding is trustworthy — or obviously-wrong — only
    /// when its reasons travel with it.
    pub evidence: Vec<String>,
    /// `TransitivelyDead` only: caller hops up to the head of the dead chain
    /// (a function with no callers at all). Tells a reviewer how deep a dead
    /// island they are wading into; `None` elsewhere or on a caller cycle.
    pub nearest_root_distance: Option<usize>,
}

/// Options for a detection run.
#[derive(Clone, Debug, Default)]
pub struct DeadCodeOptions {
    /// Report functions that are live only from test code.
    pub include_test_only: bool,
    /// The analyzed root (plan §2.4): test-path detection walks ancestors up
    /// to — never past — this directory, and `pyproject.toml` entry points
    /// are read from it. `None` keeps the immediate-parent rule only.
    pub root: Option<std::path::PathBuf>,
}

/// Ceiling for a finding whose liveness could go either way: an unresolvable
/// receiver on an instantiated class. Keeps it in the report (Low) instead of
/// either silencing it or claiming High.
const WEAK_SURFACE_CAP: f32 = 0.45;

/// Detect dead functions over the resolved projection.
pub fn detect_dead(graph: &ProjectedGraph, options: DeadCodeOptions) -> Vec<DeadFinding> {
    let EntryPoints {
        production,
        test_only,
        weak_surface,
    } = entry_points::detect_entry_points(graph, options.root.as_deref());

    let live_prod = compute_reachable(graph, &production);
    // Second pass: seed with test entries too, to classify test-only liveness.
    let mut all_roots = production.clone();
    all_roots.extend(test_only.iter().cloned());
    let live_with_tests = compute_reachable(graph, &all_roots);
    let _ = test_only.len(); // roots retained inside live sets

    let mut out = Vec::new();
    for (id, f) in &graph.functions {
        if live_prod.reachable.contains(id) {
            continue; // live in production — not a candidate
        }

        let kind = if live_with_tests.reachable.contains(id) {
            DeadKind::TestOnly
        } else {
            match graph.callers_by_callee.get(id) {
                Some(callers) if !callers.is_empty() => DeadKind::TransitivelyDead,
                _ => DeadKind::Unreachable,
            }
        };
        if matches!(kind, DeadKind::TestOnly) && !options.include_test_only {
            continue;
        }

        // Evidence combination: isolation × parse quality. Size is impact,
        // not probability of being dead, so it ranks (removable_lines) but
        // never scores.
        let quality = match f.parse_quality {
            ParseQuality::Clean => 1.0,
            ParseQuality::Partial | ParseQuality::Deferred => 0.8,
            ParseQuality::Tainted => 0.5,
        };
        let score = combine(&[kind.isolation(), quality]);
        let mut evidence = base_evidence(graph, id, f, kind);
        let nearest_root_distance = if matches!(kind, DeadKind::TransitivelyDead) {
            distance_to_dead_chain_head(graph, id)
        } else {
            None
        };

        // Plan §2.4 weighting: a finding kept alive ONLY through weakly
        // inferred receiver edges (pytest fixture / ambiguous chains) is not
        // "live for sure" — report it, at ×0.8 of its evidence score. Edges
        // without a tag (plain `name()` calls) stay strong.
        let score = if matches!(kind, DeadKind::TestOnly) {
            let all_weak = graph
                .callers_by_callee
                .get(id)
                .map(|c| !c.is_empty())
                .unwrap_or(false)
                && graph.callers_by_callee[id].iter().all(|caller| {
                    graph
                        .call_evidence
                        .get(&(id.clone(), caller.clone()))
                        .is_some_and(|ev| ev.is_weak())
                });
            if all_weak {
                evidence.push("kept alive only by inferred receiver types (fixture)".to_string());
                score * 0.8
            } else {
                score
            }
        } else {
            score
        };

        // §7.2 weak surface: the class is instantiated here, so an untyped
        // receiver we could not resolve may call this. Reported, at the
        // weakest tier — a wrong 0.9 costs more than a missing 0.9.
        let (score, evidence) = match weak_surface.get(id) {
            Some(reason) if matches!(kind, DeadKind::Unreachable | DeadKind::TransitivelyDead) => {
                let mut evidence = evidence;
                evidence.push((*reason).to_string());
                (score.min(WEAK_SURFACE_CAP), evidence)
            }
            _ => (score, evidence),
        };

        out.push(DeadFinding {
            entity_id: id.clone(),
            kind,
            tier: tier_of(score),
            score,
            removable_lines: f.exit_line.saturating_sub(f.line).saturating_add(1),
            evidence,
            nearest_root_distance,
        });
    }

    // Rank by confidence, then by impact — most safely-deletable first.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.removable_lines.cmp(&a.removable_lines))
    });

    // Stage 6.3 (RTA-lite): functions live ONLY through virtual dispatch on
    // classes that are never constructed anywhere in the root. The base pass
    // skips them as live; RTA re-flags them with the weakest evidence tier.
    let direct = crate::graph::rta_lite::direct_call_reachable(graph, &production);
    for cand in
        crate::graph::rta_lite::uninstantiated_overrides(graph, &live_prod.reachable, &direct)
    {
        let Some(f) = graph.functions.get(&cand.entity_id) else {
            continue;
        };
        let quality = match f.parse_quality {
            ParseQuality::Clean => 1.0,
            ParseQuality::Partial | ParseQuality::Deferred => 0.8,
            ParseQuality::Tainted => 0.5,
        };
        let score = combine(&[DeadKind::RtaDead.isolation(), quality]);
        let class_evidence = cand
            .class_id
            .as_deref()
            .and_then(|cid| graph.classes.get(cid))
            .map(|c| format!("class '{}' is never instantiated", c.name))
            .unwrap_or_else(|| "defining class is never instantiated".to_string());
        out.push(DeadFinding {
            entity_id: cand.entity_id.clone(),
            kind: DeadKind::RtaDead,
            tier: tier_of(score),
            score,
            removable_lines: f.exit_line.saturating_sub(f.line).saturating_add(1),
            evidence: vec![
                "live only through virtual dispatch".to_string(),
                class_evidence,
            ],
            nearest_root_distance: None,
        });
    }

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.removable_lines.cmp(&a.removable_lines))
    });
    out
}

/// Entry-point rejections plus the isolation fact, for one finding (§2.6).
///
/// Every item answers a question the ladder asked — bounded and cheap, no
/// second pass over the graph. Order is-by-question: what protects callers,
/// then what protects this definition, then what protects the module.
fn base_evidence(
    graph: &ProjectedGraph,
    id: &str,
    f: &crate::types::Function,
    kind: DeadKind,
) -> Vec<String> {
    let mut ev = Vec::new();
    match kind {
        DeadKind::Unreachable => ev.push("no inbound callers".to_string()),
        DeadKind::TransitivelyDead => {
            let n = graph
                .callers_by_callee
                .get(id)
                .map(|c| c.len())
                .unwrap_or(0);
            ev.push(format!("all {n} caller(s) are themselves dead"));
        }
        DeadKind::TestOnly => ev.push("only reachable from test code".to_string()),
        DeadKind::RtaDead => {} // RTA loop writes its own two items
    }

    // Method-specific §2.3 rejections: why external dispatch does not rescue
    // this definition.
    if let Some(cls) = f
        .parent_class
        .as_ref()
        .and_then(|cid| graph.classes.get(cid))
    {
        let has_external_base = cls
            .mro
            .iter()
            .any(|n| matches!(n, crate::types::MroNode::External { .. }));
        if !has_external_base {
            ev.push("class has external base: none".to_string());
        } else if f.name.starts_with('_') {
            ev.push("private method of a framework subclass: not a dispatch target".to_string());
        } else {
            ev.push("override of a visible in-repo method".to_string());
        }
    }

    if f.name.starts_with('_') {
        ev.push("private name".to_string());
    }

    // Module-level gates: __all__ and the never-imported heuristic.
    if let Some(m) = graph.modules.get(&f.parent_module) {
        if let Some(names) = m.star_exports.as_ref() {
            if !names.contains(&f.name) {
                ev.push("not in __all__".to_string());
            }
        }
    }
    ev
}

/// Caller hops from a `TransitivelyDead` node up to the head of its dead
/// chain — the first function with no callers of its own (§2.6). A cycle
/// among callers returns `None` rather than looping.
fn distance_to_dead_chain_head(graph: &ProjectedGraph, id: &str) -> Option<usize> {
    const MAX_HOPS: usize = 64;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    seen.insert(id);
    let mut frontier = vec![id.to_string()];
    for depth in 1..=MAX_HOPS {
        let mut next = Vec::new();
        for current in &frontier {
            let callers = graph
                .callers_by_callee
                .get(current.as_str())
                .map(|c| c.iter())
                .into_iter()
                .flatten();
            for caller in callers {
                if !seen.insert(caller.as_str()) {
                    continue;
                }
                let has_callers = graph
                    .callers_by_callee
                    .get(caller.as_str())
                    .is_some_and(|c| !c.is_empty());
                if !has_callers {
                    return Some(depth);
                }
                next.push(caller.clone());
            }
        }
        if next.is_empty() {
            return None;
        }
        frontier = next;
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::{ByteSpan, EmbeddingVec, Function, FunctionKind, SourceType};

    use std::path::PathBuf;

    pub(crate) fn func(id: &str, name: &str, module: &str) -> Function {
        Function {
            id: id.into(),
            name: name.into(),
            parent_module: module.into(),
            parent_class: None,
            parameters: vec![],
            return_type: None,
            calls: vec![],
            resolved_calls: vec![],
            decorators: vec![],
            setter_of: None,
            bindings: Vec::new(),
            refs: Vec::new(),
            resolved_refs: Vec::new(),
            line: 1,
            exit_line: 2,
            docstring: None,
            kind: FunctionKind::Free,
            is_async: false,
            is_generator: false,
            source: SourceType::Impl,
            signature_hash: 0,
            body_hash: 0,
            metrics: Default::default(),
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            content_hash: 0,
            span: ByteSpan { start: 0, end: 10 },
            name_span: ByteSpan { start: 0, end: 1 },
            params_span: ByteSpan { start: 0, end: 1 },
            body_span: ByteSpan { start: 0, end: 200 },
            decorators_span: None,
            embedding: EmbeddingVec {
                vec: vec![],
                hash: String::new(),
            },
        }
    }

    /// A -> B -> C live chain; D uncalled; E <- D dead chain; F called only
    /// from a test module's main.
    fn fixture() -> ProjectedGraph {
        let mut g = crate::smells::engine::tests::empty_graph();
        for (id, name) in [
            ("app.py::main", "main"),
            ("app.py::a", "a"),
            ("app.py::b", "b"),
            ("app.py::c", "c"),
            ("app.py::d", "d"),
            ("app.py::e", "e"),
        ] {
            g.functions.insert(
                id.into(),
                std::sync::Arc::new(func(id, name, "app.py::module")),
            );
        }
        g.functions.insert(
            "tests/test_x.py::f".into(),
            std::sync::Arc::new(func("tests/test_x.py::f", "f", "tests/test_x.py::module")),
        );
        // Module records (for test-path detection).
        g.modules.insert(
            "app.py::module".into(),
            std::sync::Arc::new(mk_module("app.py::module")),
        );
        g.modules.insert(
            "tests/test_x.py::module".into(),
            std::sync::Arc::new(mk_module("tests/test_x.py::module")),
        );

        let edge = |g: &mut ProjectedGraph, from: &str, to: &str| {
            g.callees_by_caller
                .entry(from.into())
                .or_default()
                .insert(to.into());
            g.callers_by_callee
                .entry(to.into())
                .or_default()
                .insert(from.into());
        };
        edge(&mut g, "app.py::main", "app.py::a");
        edge(&mut g, "app.py::a", "app.py::b");
        edge(&mut g, "app.py::b", "app.py::c");
        edge(&mut g, "app.py::d", "app.py::e");
        edge(&mut g, "tests/test_x.py::f", "app.py::c");
        g.importers
            .entry("app.py::module".into())
            .or_default()
            .insert("x".into());
        g
    }

    #[test]
    fn mains_and_dead_chains_classify_correctly() {
        let g = fixture();
        let out = detect_dead(
            &g,
            DeadCodeOptions {
                include_test_only: true,
                root: None,
            },
        );

        let kind_of = |name: &str| {
            let f = out
                .iter()
                .find(|f| f.entity_id.ends_with(&format!("::{name}")));
            (f.map(|f| f.kind), f.is_some())
        };

        // Live chain is not reported.
        assert_eq!(kind_of("main"), (None, false));
        assert_eq!(kind_of("a"), (None, false));
        assert!(!out.iter().any(|f| f.entity_id.ends_with("::b")));
        assert!(!out.iter().any(|f| f.entity_id.ends_with("::c")));

        // D has zero callers → Unreachable.
        let (kind, present) = kind_of("d");
        assert!(present);
        assert_eq!(kind, Some(DeadKind::Unreachable));

        // E is called only by dead D → TransitivelyDead.
        let (kind, _) = kind_of("e");
        assert_eq!(kind, Some(DeadKind::TransitivelyDead));

        // F lives only via the test module → TestOnly.
        let (kind, present) = kind_of("f");
        assert!(present);
        assert_eq!(kind, Some(DeadKind::TestOnly));

        // Default options hide test-only findings.
        let default_out = detect_dead(&g, DeadCodeOptions::default());
        assert!(!default_out.iter().any(|f| f.entity_id.ends_with("::f")));
    }

    #[test]
    fn findings_are_ranked_by_confidence_then_size() {
        let g = fixture();
        let out = detect_dead(&g, DeadCodeOptions::default());
        for w in out.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "not ranked by score: {} < {}",
                w[0].score,
                w[1].score
            );
        }
    }

    #[test]
    fn entry_detection_tables_drive_roots() {
        use super::entry_points::detect_entry_points;
        let mut g = fixture();
        // A route-decorated function becomes a production root even with no callers.
        let mut routed = func("api.py::index", "index", "api.py::module");
        routed.decorators = vec!["app.route(\"/\")".into()];
        g.functions
            .insert("api.py::index".into(), std::sync::Arc::new(routed));
        g.modules.insert(
            "api.py::module".into(),
            std::sync::Arc::new(mk_module("api.py::module")),
        );
        let eps = detect_entry_points(&g, None);
        assert!(eps.production.contains("app.py::main"));
        assert!(eps.production.contains("api.py::index"));
        assert!(!eps.production.contains("app.py::d"));
    }

    #[test]
    fn pyfunction_bridge_and_rust_pub_are_production_roots_f7() {
        use super::entry_points::detect_entry_points;
        let mut g = fixture();
        // #[pyfunction]-decorated free fn: bridge entry with no callers.
        let mut bridged = func("lib.rs::find_things", "find_things", "lib.rs::module");
        bridged.decorators = vec!["#[pyfunction]".into()];
        g.functions
            .insert("lib.rs::find_things".into(), std::sync::Arc::new(bridged));
        // Rust module backed by a real temp file: pub exports root,
        // pub(crate) and private fall through.
        let dir = tempfile::tempdir().unwrap();
        let rs = dir.path().join("bridge.rs");
        std::fs::write(
            &rs,
            "#[pymethods]\nimpl Engine {\n    pub fn apply(&self) {}\n    pub(crate) fn internal(&self) {}\n    fn private(&self) {}\n}\n",
        )
        .unwrap();
        let mut m = mk_module("bridge.rs::module");
        m.language = crate::types::Language::Rust;
        m.path = rs;
        g.modules
            .insert("bridge.rs::module".into(), std::sync::Arc::new(m));
        for (id, name, line) in [
            ("bridge.rs::Engine.apply", "apply", 3),
            ("bridge.rs::Engine.internal", "internal", 4),
            ("bridge.rs::Engine.private", "private", 5),
        ] {
            let mut f = func(id, name, "bridge.rs::module");
            f.line = line;
            f.exit_line = line;
            f.parent_class = Some("bridge.rs::Engine".into());
            g.functions.insert(id.into(), std::sync::Arc::new(f));
        }
        // R1§6-14/15/17: #[pymodule] init fns are bridge roots too —
        // called by the Python import system, never by name in-repo.
        let mut moduled = func("lib.rs::core_init", "core_init", "lib.rs::module");
        moduled.decorators = vec!["#[pymodule]".into()];
        g.functions
            .insert("lib.rs::core_init".into(), std::sync::Arc::new(moduled));
        let eps = detect_entry_points(&g, None);
        assert!(eps.production.contains("lib.rs::find_things"));
        assert!(eps.production.contains("lib.rs::core_init"));
        assert!(eps.production.contains("bridge.rs::Engine.apply"));
        assert!(!eps.production.contains("bridge.rs::Engine.internal"));
        assert!(!eps.production.contains("bridge.rs::Engine.private"));
        // …and the dead detector no longer reports the bridge surface.
        let out = detect_dead(&g, DeadCodeOptions::default());
        assert!(!out.iter().any(|f| f.entity_id == "lib.rs::find_things"));
        assert!(!out.iter().any(|f| f.entity_id == "bridge.rs::Engine.apply"));
    }

    /// Dogfood findings (Phase 7): every rule below was added because
    /// CodeRadar reported a real false positive on its own source tree.
    #[test]
    fn registration_decorators_and_decorator_definitions_are_roots() {
        use super::entry_points::detect_entry_points;
        let mut g = fixture();
        // `@mcp.tool(...)`: an unlisted registration decorator (24 MCP tools
        // were reported unreachable before this rule).
        let mut tool = func(
            "srv.py::codegraph_search",
            "codegraph_search",
            "srv.py::module",
        );
        tool.decorators = vec!["@mcp.tool(description=\"find symbols\")".into()];
        // A definition used as a decorator is called by the decoration
        // machinery: `@requires_index` applied 24 times.
        let mut decorator = func("srv.py::requires_index", "requires_index", "srv.py::module");
        decorator.decorators = vec![];
        let mut guarded = func("srv.py::codegraph_node", "codegraph_node", "srv.py::module");
        guarded.decorators = vec!["@requires_index".into()];
        for f in [tool, decorator, guarded] {
            g.functions.insert(f.id.clone(), std::sync::Arc::new(f));
        }
        g.modules.insert(
            "srv.py::module".into(),
            std::sync::Arc::new(mk_module("srv.py::module")),
        );
        let eps = detect_entry_points(&g, None);
        assert!(eps.production.contains("srv.py::codegraph_search"));
        assert!(eps.production.contains("srv.py::requires_index"));
        assert!(eps.production.contains("srv.py::codegraph_node"));
        // `@lru_cache` is a bare attribute reference, not a registration
        // call. Private, so the unimported-module export rule cannot root it
        // either: the decorator is the only question left.
        let mut plain = func("srv.py::_memoized", "_memoized", "srv.py::module");
        plain.decorators = vec!["@functools.lru_cache".into()];
        g.functions
            .insert(plain.id.clone(), std::sync::Arc::new(plain));
        let eps = detect_entry_points(&g, None);
        assert!(!eps.production.contains("srv.py::_memoized"));
    }

    #[test]
    fn package_facade_surface_is_a_root() {
        use super::entry_points::detect_entry_points;
        let mut g = fixture();
        // `pkg/__init__.py` exists to be imported: `coderadar.watch(...)` and
        // `CodeGraph.query` are the documented API even though nothing in-repo
        // calls them.
        let mut m = mk_module("pkg/__init__.py::module");
        m.path = PathBuf::from("pkg/__init__.py");
        m.functions = vec!["pkg/__init__.py::watch".into()];
        m.classes = vec!["pkg/__init__.py::CodeGraph".into()];
        g.modules
            .insert("pkg/__init__.py::module".into(), std::sync::Arc::new(m));
        let watch = func("pkg/__init__.py::watch", "watch", "pkg/__init__.py::module");
        g.functions
            .insert(watch.id.clone(), std::sync::Arc::new(watch));
        let cls = crate::types::Class {
            id: "pkg/__init__.py::CodeGraph".into(),
            name: "CodeGraph".into(),
            grammar_kind: "class_definition".into(),
            parent_module: "pkg/__init__.py::module".into(),
            parent_class: None,
            bases: vec![],
            resolved_bases: vec![],
            mro: vec![],
            mro_error: false,
            methods: vec![],
            fields: vec![],
            source: SourceType::Impl,
            decorators: vec![],
            effective: crate::types::EffectiveClass::Plain,
            is_type_checking_only: false,
            line: 1,
            exit_line: 2,
            docstring: None,
            parse_quality: ParseQuality::Clean,
            content_hash: 0,
            span: ByteSpan { start: 0, end: 10 },
            name_span: ByteSpan { start: 6, end: 15 },
            body_span: ByteSpan { start: 10, end: 10 },
            decorators_span: None,
            embedding: EmbeddingVec::default(),
        };
        g.classes.insert(cls.id.clone(), std::sync::Arc::new(cls));
        let mut query = func(
            "pkg/__init__.py::CodeGraph.query",
            "query",
            "pkg/__init__.py::module",
        );
        query.parent_class = Some("pkg/__init__.py::CodeGraph".into());
        let mut helper = func(
            "pkg/__init__.py::CodeGraph._helper",
            "_helper",
            "pkg/__init__.py::module",
        );
        helper.parent_class = Some("pkg/__init__.py::CodeGraph".into());
        for f in [query, helper] {
            g.functions.insert(f.id.clone(), std::sync::Arc::new(f));
        }
        let eps = detect_entry_points(&g, None);
        assert!(eps.production.contains("pkg/__init__.py::watch"));
        assert!(eps.production.contains("pkg/__init__.py::CodeGraph.query"));
        // Private helpers of a public class are still reported.
        assert!(!eps
            .production
            .contains("pkg/__init__.py::CodeGraph._helper"));
    }

    #[test]
    fn module_level_uses_root_import_time_initializers() {
        use super::entry_points::detect_entry_points;
        let mut g = fixture();
        // `__version__ = _resolve_version()` at module scope: the extractor
        // records the use and resolution binds it.
        let mut m = mk_module("ver.py::module");
        m.functions = vec!["ver.py::_resolve_version".into(), "ver.py::_orphan".into()];
        m.resolved_uses = vec!["ver.py::_resolve_version".into()];
        g.modules
            .insert("ver.py::module".into(), std::sync::Arc::new(m));
        for name in ["_resolve_version", "_orphan"] {
            let f = func(&format!("ver.py::{name}"), name, "ver.py::module");
            g.functions.insert(f.id.clone(), std::sync::Arc::new(f));
        }
        let eps = detect_entry_points(&g, None);
        assert!(eps.production.contains("ver.py::_resolve_version"));
        assert!(!eps.production.contains("ver.py::_orphan"));
    }

    #[test]
    fn value_references_and_unresolved_receivers_cap_at_low() {
        // A function handed around (`self._check = _index_is_empty`) or named
        // by a receiver we could not resolve (`self._fire()`) is not
        // 0.9-dead. It stays in the report, at the weakest tier.
        let mut g = fixture();
        let mut referenced = func(
            "lazy.py::_index_is_empty",
            "_index_is_empty",
            "lazy.py::module",
        );
        referenced.parent_class = Some("lazy.py::Retry".into());
        g.functions
            .insert(referenced.id.clone(), std::sync::Arc::new(referenced));
        let mut unresolved = func("life.py::_fire", "_fire", "life.py::module");
        unresolved.parent_class = Some("life.py::Watchdog".into());
        unresolved.resolved_calls = vec![crate::types::ResolvedCall::Unresolved {
            reason: crate::types::UnresolvedReason::TypeInferenceRequired,
            raw: crate::types::UnresolvedRef {
                name: "_fire".into(),
                path: vec!["self".into()],
                line: 1,
                col: 0,
                name_span: ByteSpan { start: 0, end: 5 },
            },
        }];
        g.functions
            .insert(unresolved.id.clone(), std::sync::Arc::new(unresolved));
        let mut caller = func("life.py::check_once", "check_once", "life.py::module");
        caller.resolved_refs = vec!["lazy.py::_index_is_empty".into()];
        g.functions
            .insert(caller.id.clone(), std::sync::Arc::new(caller));
        g.modules.insert(
            "lazy.py::module".into(),
            std::sync::Arc::new(mk_module("lazy.py::module")),
        );
        g.modules.insert(
            "life.py::module".into(),
            std::sync::Arc::new(mk_module("life.py::module")),
        );
        let out = detect_dead(&g, DeadCodeOptions::default());
        for id in ["lazy.py::_index_is_empty", "life.py::_fire"] {
            let f = out
                .iter()
                .find(|f| f.entity_id == id)
                .unwrap_or_else(|| panic!("{id} must stay in the report"));
            assert_eq!(f.tier, Tier::Low, "{id} must not claim 0.9");
        }
    }

    #[test]
    fn click_subcommand_decorators_are_production_roots() {
        use super::entry_points::detect_entry_points;
        let mut g = fixture();
        let mut cmd = func("cli.py::serve", "serve", "cli.py::module");
        cmd.decorators = vec!["main.command()".into()];
        g.functions
            .insert("cli.py::serve".into(), std::sync::Arc::new(cmd));
        g.modules.insert(
            "cli.py::module".into(),
            std::sync::Arc::new(mk_module("cli.py::module")),
        );
        // Imported module: step-4 (public API of never-imported modules)
        // cannot root it — only the decorator rule can.
        g.importers
            .entry("cli.py::module".into())
            .or_default()
            .insert("y".into());
        let eps = detect_entry_points(&g, None);
        assert!(eps.production.contains("cli.py::serve"));
    }

    fn mk_module(id: &str) -> crate::types::Module {
        crate::types::Module {
            id: id.into(),
            name: id.into(),
            path: PathBuf::from(id.split("::").next().unwrap_or(id)),
            language: crate::types::Language::Python,
            package: None,
            exports: vec![],
            star_exports: None,
            uses: Vec::new(),
            resolved_uses: Vec::new(),
            attr_reads: Vec::new(),
            classes: vec![],
            functions: vec![],
            imports: vec![],
            constants: vec![],
            type_aliases: vec![],
            parse_quality: ParseQuality::Clean,
            file_version: 1,
            content_hash: 0,
            embedding: EmbeddingVec {
                vec: vec![],
                hash: String::new(),
            },
        }
    }
}
