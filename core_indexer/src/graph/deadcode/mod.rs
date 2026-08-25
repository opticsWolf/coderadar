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
// Confidence combines isolation strength × size × parse quality through
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
}

impl DeadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeadKind::Unreachable => "unreachable",
            DeadKind::TransitivelyDead => "transitively-dead",
            DeadKind::TestOnly => "test-only",
        }
    }

    /// Isolation strength evidence — being provably untouched is stronger
    /// evidence than being merely part of a dead chain.
    fn isolation(self) -> f32 {
        match self {
            DeadKind::Unreachable => 0.9,
            DeadKind::TransitivelyDead => 0.65,
            DeadKind::TestOnly => 0.5,
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
}

/// Options for a detection run.
#[derive(Clone, Copy, Debug)]
pub struct DeadCodeOptions {
    /// Report functions that are live only from test code.
    pub include_test_only: bool,
}

impl Default for DeadCodeOptions {
    fn default() -> Self {
        Self { include_test_only: false }
    }
}

/// Detect dead functions over the resolved projection.
pub fn detect_dead(graph: &ProjectedGraph, options: DeadCodeOptions) -> Vec<DeadFinding> {
    let EntryPoints { production, test_only } = entry_points::detect_entry_points(graph);

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

        // Evidence combination: isolation × size × parse quality.
        let size_boost = ((f.body_span.len() as f32 / 200.0).min(1.0)).max(0.3);
        let quality = match f.parse_quality {
            ParseQuality::Clean => 1.0,
            ParseQuality::Partial | ParseQuality::Deferred => 0.8,
            ParseQuality::Tainted => 0.5,
        };
        let score = combine(&[kind.isolation(), size_boost, quality]);

        out.push(DeadFinding {
            entity_id: id.clone(),
            kind,
            tier: tier_of(score),
            score,
            removable_lines: f.exit_line.saturating_sub(f.line).saturating_add(1),
        });
    }

    // Rank by confidence, then by impact — most safely-deletable first.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.removable_lines.cmp(&a.removable_lines))
    });
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::{ByteSpan, EmbeddingVec, Function, FunctionKind, SourceType};
    use std::collections::{BTreeSet, HashMap};
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
            embedding: EmbeddingVec { vec: vec![], hash: String::new() },
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
            g.functions.insert(id.into(), std::sync::Arc::new(func(id, name, "app.py::module")));
        }
        g.functions.insert(
            "tests/test_x.py::f".into(),
            std::sync::Arc::new(func("tests/test_x.py::f", "f", "tests/test_x.py::module")),
        );
        // Module records (for test-path detection).
        g.modules.insert("app.py::module".into(), std::sync::Arc::new(mk_module("app.py::module")));
        g.modules.insert(
            "tests/test_x.py::module".into(),
            std::sync::Arc::new(mk_module("tests/test_x.py::module")),
        );

        let edge = |g: &mut ProjectedGraph, from: &str, to: &str| {
            g.callees_by_caller.entry(from.into()).or_default().insert(to.into());
            g.callers_by_callee.entry(to.into()).or_default().insert(from.into());
        };
        edge(&mut g, "app.py::main", "app.py::a");
        edge(&mut g, "app.py::a", "app.py::b");
        edge(&mut g, "app.py::b", "app.py::c");
        edge(&mut g, "app.py::d", "app.py::e");
        edge(&mut g, "tests/test_x.py::f", "app.py::c");
        g.importers.entry("app.py::module".into()).or_default().insert("x".into());
        g
    }

    #[test]
    fn mains_and_dead_chains_classify_correctly() {
        let g = fixture();
        let out = detect_dead(&g, DeadCodeOptions { include_test_only: true });

        let kind_of = |name: &str| {
            let f = out.iter().find(|f| f.entity_id.ends_with(&format!("::{name}")));
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
        g.functions.insert("api.py::index".into(), std::sync::Arc::new(routed));
        g.modules
            .insert("api.py::module".into(), std::sync::Arc::new(mk_module("api.py::module")));
        let eps = detect_entry_points(&g);
        assert!(eps.production.contains("app.py::main"));
        assert!(eps.production.contains("api.py::index"));
        assert!(!eps.production.contains("app.py::d"));
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
            classes: vec![],
            functions: vec![],
            imports: vec![],
            constants: vec![],
            type_aliases: vec![],
            parse_quality: ParseQuality::Clean,
            file_version: 1,
            content_hash: 0,
            embedding: EmbeddingVec { vec: vec![], hash: String::new() },
        }
    }
}
