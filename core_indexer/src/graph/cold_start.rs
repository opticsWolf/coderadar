//! Cold-start reconstruction (v0.8 P1).
//!
//! Rebuilds an in-memory [`ProjectedGraph`] from a Macrame-ledger
//! [`MaterializedState`]. Concept JSON v2 (see `crate::storage`) carries
//! post-resolution *base* state; the resolution cascade
//! (imports → class methods → MRO → hierarchy → overrides) is re-run by
//! the caller on the returned projection, mirroring `analyze_inner`
//! (lib.rs) minus `resolve_all_calls` — `resolved_calls` are already
//! carried on each function.
//!
//! Call indices are restored from two sources:
//!
//! 1. `Function.resolved_calls` — the exact edge-pair rules of
//!    `resolve_one_function` (resolve_calls.rs): Function/Constructor →
//!    target, Method → method id, Builtin/External → `external::{name}`,
//!    Unresolved → nothing.
//! 2. Ledger edges whose kind is **not** structural (CALLS / IMPORTS /
//!    EXTENDS / OVERRIDES) — i.e. synthetic framework edges registered
//!    under their original kind. They land in the same call indices,
//!    exactly as `register_synthetic_edges_bulk` does in memory.
//!
//! Structural edges are never read at load: `importers` is rebuilt by
//! `resolve_imports`, EXTENDS by `resolve_class_hierarchy` (via
//! `resolved_bases`), OVERRIDES by `resolve_overrides` (via
//! `overrides_base`). Ledger CALLS edges are a strict subset of (1) —
//! the persister skips external/builtin targets — so skipping them loses
//! nothing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use macrame::temporal::MaterializedState;

use crate::storage::{classify_v2_concept, parse_v2_concept, V2ConceptClass, V2Entity};
use crate::types::{ProjectedGraph, ResolvedCall};

/// Counts produced by a cold start, surfaced in the `load_snapshot`
/// result and used by tests.
#[derive(Debug)]
pub struct ColdStartStats {
    pub modules: usize,
    pub classes: usize,
    pub functions: usize,
    pub imports: usize,
    pub constants: usize,
    pub type_aliases: usize,
    /// Pairs restored into the call indices from `resolved_calls`.
    pub resolved_call_pairs: usize,
    /// Non-structural (synthetic-kind) ledger edges restored into the
    /// call indices.
    pub synthetic_edges: usize,
    /// Standalone `field` concepts skipped — v1 leftovers; v2 embeds
    /// fields in the owning class.
    pub skipped_field_concepts: usize,
}

impl ColdStartStats {
    fn new() -> Self {
        Self {
            modules: 0,
            classes: 0,
            functions: 0,
            imports: 0,
            constants: 0,
            type_aliases: 0,
            resolved_call_pairs: 0,
            synthetic_edges: 0,
            skipped_field_concepts: 0,
        }
    }
}

/// Rebuild a `ProjectedGraph` (and stats) from a ledger snapshot.
///
/// Hard errors:
/// * any concept lacking `meta_version: 2` (concept-JSON v1) — the store
///   must be upgraded with a fresh `analyze`;
/// * any concept whose content is not JSON at all.
pub fn projection_from_state(
    state: &MaterializedState,
) -> Result<(ProjectedGraph, ColdStartStats), String> {
    let mut g: ProjectedGraph = ProjectedGraph {
        modules: HashMap::new(),
        classes: HashMap::new(),
        functions: HashMap::new(),
        imports: HashMap::new(),
        constants: HashMap::new(),
        type_aliases: HashMap::new(),
        file_to_modules: HashMap::new(),
        module_by_dotted_name: HashMap::new(),
        module_path_index: HashMap::new(),
        importers: HashMap::new(),
        imports_by_importer: HashMap::new(),
        callers_by_callee: HashMap::new(),
        callees_by_caller: HashMap::new(),
        subclasses: HashMap::new(),
        overridden_by: HashMap::new(),
        overrides_base: HashMap::new(),
        ambiguous_bases: Vec::new(),
    };
    let mut stats = ColdStartStats::new();
    let mut v1_leftovers = 0usize;
    let mut unreadable = 0usize;

    for (id, attrs) in &state.concepts {
        match classify_v2_concept(&attrs.content) {
            V2ConceptClass::StandaloneField => stats.skipped_field_concepts += 1,
            V2ConceptClass::V1Leftover => v1_leftovers += 1,
            V2ConceptClass::Unreadable => unreadable += 1,
            V2ConceptClass::Canonical(_) => match parse_v2_concept(id, &attrs.content)? {
                V2Entity::Module(m) => {
                    g.modules.insert(m.id.clone(), Arc::new(m));
                    stats.modules += 1;
                }
                V2Entity::Class(c) => {
                    g.classes.insert(c.id.clone(), Arc::new(c));
                    stats.classes += 1;
                }
                V2Entity::Function(f) => {
                    g.functions.insert(f.id.clone(), Arc::new(f));
                    stats.functions += 1;
                }
                V2Entity::Import(i) => {
                    g.imports.insert(i.id.clone(), Arc::new(i));
                    stats.imports += 1;
                }
                V2Entity::Constant(k) => {
                    g.constants.insert(k.id.clone(), Arc::new(k));
                    stats.constants += 1;
                }
                V2Entity::TypeAlias(t) => {
                    g.type_aliases.insert(t.id.clone(), Arc::new(t));
                    stats.type_aliases += 1;
                }
            },
        }
    }

    if v1_leftovers > 0 {
        return Err(format!(
            "the Macrame store contains {v1_leftovers} concept(s) without meta_version: 2 \
             (concept-JSON v1). Re-run `coderadar analyze` on the project to upgrade the \
             store; the snapshot will not load."
        ));
    }
    if unreadable > 0 {
        return Err(format!(
            "{unreadable} concept(s) had unreadable (non-JSON) content — the store may be \
             corrupt; re-run `coderadar analyze` to rebuild it."
        ));
    }

    // `file_to_modules` from module paths; `module_by_dotted_name` stays
    // empty. `module_path_index` MUST be rebuilt here: without it the load
    // cascade's `resolve_imports` would scan every module per import (the
    // 8.3s benchmark regression).
    for m in g.modules.values() {
        g.file_to_modules.entry(m.path.clone()).or_default().push(m.id.clone());
    }
    super::module_resolution::rebuild_module_path_index(&mut g);

    restore_call_indices(&mut g, &mut stats);
    restore_synthetic_edges(&mut g, state, &mut stats);

    Ok((g, stats))
}

/// Structural edge kinds written by `persist_edges_scoped`. Excluded from
/// ledger restoration (see module docs).
const STRUCTURAL_EDGE_KINDS: &[&str] = &["CALLS", "IMPORTS", "EXTENDS", "OVERRIDES"];

/// Restore the CALLS reverse indices from `Function.resolved_calls`
/// (see module docs for the pair rules).
fn restore_call_indices(g: &mut ProjectedGraph, stats: &mut ColdStartStats) {
    for f in g.functions.values() {
        for call in &f.resolved_calls {
            let target: String = match call {
                ResolvedCall::Function(t) | ResolvedCall::Constructor(t) => t.clone(),
                ResolvedCall::Method { method, .. } => method.clone(),
                ResolvedCall::Builtin(name) | ResolvedCall::External(name) => {
                    format!("external::{name}")
                }
                ResolvedCall::Unresolved { .. } => continue,
            };
            g.callers_by_callee
                .entry(target.clone())
                .or_default()
                .insert(f.id.clone());
            g.callees_by_caller
                .entry(f.id.clone())
                .or_default()
                .insert(target);
            stats.resolved_call_pairs += 1;
        }
    }
}

/// Restore synthetic (non-structural-kind) ledger edges into the call
/// indices, mirroring `register_synthetic_edges_bulk`: source is the
/// caller side, target the callee side.
fn restore_synthetic_edges(
    g: &mut ProjectedGraph,
    state: &MaterializedState,
    stats: &mut ColdStartStats,
) {
    // Defensive (source, target, kind) triple dedup. macrame 0.12 enforces
    // a single open interval per triple, so duplicates are impossible in
    // practice; the guard stays cheap in case legacy rows ever appear.
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    // Only intervals open as of the reconstruction instant are facts: after a
    // retire → re-assert the fold keeps the closed interval in `state.edges`
    // too (last writer per `source|target|type|valid_from`, and the
    // retirement row carries the closed `valid_to`). Canonical ISO-8601
    // strings compare chronologically, so the check is lexicographic.
    let ts = state.timestamp.as_str();
    for (source_id, target_id, edge_type, valid_from, valid_to) in &state.edges {
        if STRUCTURAL_EDGE_KINDS.contains(&edge_type.as_str()) {
            continue;
        }
        if !(valid_from.as_str() <= ts && valid_to.as_str() > ts) {
            continue;
        }
        if !seen.insert((source_id.clone(), target_id.clone(), edge_type.clone())) {
            continue;
        }
        g.callees_by_caller
            .entry(source_id.clone())
            .or_default()
            .insert(target_id.clone());
        g.callers_by_callee
            .entry(target_id.clone())
            .or_default()
            .insert(source_id.clone());
        stats.synthetic_edges += 1;
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::build_v2_concepts_all;
    use crate::types::{
        ByteSpan, Class, EffectiveClass, EmbeddingVec, Function, FunctionKind, Language, Module,
        ParseQuality, ReceiverShape, ResolvedCall, SourceType,
    };
    use macrame::temporal::NodeAttributes;
    use std::path::PathBuf;

    fn span(s: usize, e: usize) -> ByteSpan {
        ByteSpan { start: s, end: e }
    }

    fn sample_projection() -> ProjectedGraph {
        let mut g: ProjectedGraph = ProjectedGraph {
            modules: HashMap::new(),
            classes: HashMap::new(),
            functions: HashMap::new(),
            imports: HashMap::new(),
            constants: HashMap::new(),
            type_aliases: HashMap::new(),
            file_to_modules: HashMap::new(),
            module_by_dotted_name: HashMap::new(),
            module_path_index: HashMap::new(),
            importers: HashMap::new(),
            imports_by_importer: HashMap::new(),
            callers_by_callee: HashMap::new(),
            callees_by_caller: HashMap::new(),
            subclasses: HashMap::new(),
            overridden_by: HashMap::new(),
            overrides_base: HashMap::new(),
            ambiguous_bases: Vec::new(),
        };

        g.modules.insert(
            "a.py::module".to_string(),
            Arc::new(Module {
                id: "a.py::module".to_string(),
                name: "a".to_string(),
                path: PathBuf::from("a.py"),
                language: Language::Python,
                package: None,
                exports: Vec::new(),
                star_exports: None,
                classes: vec!["a.py::C".to_string()],
                functions: vec!["a.py::foo".to_string(), "a.py::bar".to_string()],
                imports: vec![],
                constants: vec![],
                type_aliases: vec![],
                parse_quality: ParseQuality::Clean,
                file_version: 1,
                content_hash: 1234,
                embedding: EmbeddingVec::default(),
            }),
        );
        g.classes.insert(
            "a.py::C".to_string(),
            Arc::new(Class {
                id: "a.py::C".to_string(),
                name: "C".to_string(),
                grammar_kind: "class_definition".to_string(),
                parent_module: "a.py::module".to_string(),
                parent_class: None,
                source: SourceType::Impl,
                bases: Vec::new(),
                resolved_bases: vec![],
                mro: Vec::new(),
                mro_error: false,
                methods: vec!["a.py::C.run".to_string()],
                fields: Vec::new(),
                decorators: Vec::new(),
                effective: EffectiveClass::Plain,
                is_type_checking_only: false,
                line: 1,
                exit_line: 10,
                docstring: None,
                parse_quality: ParseQuality::Clean,
                content_hash: 99,
                span: span(0, 100),
                name_span: span(6, 7),
                body_span: span(10, 90),
                decorators_span: None,
                embedding: EmbeddingVec::default(),
            }),
        );
        g.functions.insert(
            "a.py::foo".to_string(),
            Arc::new(Function {
                id: "a.py::foo".to_string(),
                name: "foo".to_string(),
                parent_module: "a.py::module".to_string(),
                parent_class: None,
                parameters: Vec::new(),
                return_type: Some("int".to_string()),
                calls: Vec::new(),
                resolved_calls: vec![
                    ResolvedCall::Function("a.py::bar".to_string()),
                    ResolvedCall::Builtin("len".to_string()),
                ],
                decorators: Vec::new(),
                setter_of: None,
                line: 12,
                exit_line: 20,
                docstring: Some("the foo".to_string()),
                kind: FunctionKind::Free,
                is_async: false,
                is_generator: false,
                source: SourceType::Impl,
                signature_hash: 1,
                body_hash: 2,
                metrics: crate::types::FunctionMetrics {
                    cyclomatic: 1,
                    nesting_depth: 1,
                    return_count: 1,
                },
                is_type_checking_only: false,
                parse_quality: ParseQuality::Clean,
                content_hash: 7,
                span: span(0, 50),
                name_span: span(4, 7),
                params_span: span(7, 10),
                body_span: span(10, 40),
                decorators_span: None,
                embedding: EmbeddingVec::default(),
            }),
        );
        g.functions.insert(
            "a.py::bar".to_string(),
            Arc::new(Function {
                id: "a.py::bar".to_string(),
                name: "bar".to_string(),
                parent_module: "a.py::module".to_string(),
                parent_class: None,
                parameters: Vec::new(),
                return_type: None,
                calls: Vec::new(),
                resolved_calls: vec![ResolvedCall::Method {
                    receiver: ReceiverShape::SelfRef,
                    method: "a.py::C.run".to_string(),
                }],
                decorators: Vec::new(),
                setter_of: None,
                line: 22,
                exit_line: 30,
                docstring: None,
                kind: FunctionKind::Free,
                is_async: false,
                is_generator: false,
                source: SourceType::Impl,
                signature_hash: 3,
                body_hash: 4,
                metrics: crate::types::FunctionMetrics {
                    cyclomatic: 1,
                    nesting_depth: 1,
                    return_count: 1,
                },
                is_type_checking_only: false,
                parse_quality: ParseQuality::Clean,
                content_hash: 8,
                span: span(0, 50),
                name_span: span(4, 7),
                params_span: span(7, 10),
                body_span: span(10, 40),
                decorators_span: None,
                embedding: EmbeddingVec::default(),
            }),
        );
        g
    }

    fn state_from(
        g: &ProjectedGraph,
        edges: Vec<(String, String, String)>,
    ) -> MaterializedState {
        let concepts = build_v2_concepts_all(g);
        let mut map = HashMap::new();
        for c in &concepts {
            map.insert(
                c.id.clone(),
                NodeAttributes {
                    id: c.id.clone(),
                    title: c.title.clone(),
                    content: c.content.clone(),
                    embedding_model: None,
                },
            );
        }
        MaterializedState {
            seq_anchor: 42,
            timestamp: "2026-02-08T00:00:00.000000Z".to_string(),
            concepts: map,
            edges: edges
                .into_iter()
                .map(|(s, t, k)| {
                    (
                        s,
                        t,
                        k,
                        "2026-01-01T00:00:00.000000Z".to_string(),
                        "9999-12-31T23:59:59.999999Z".to_string(),
                    )
                })
                .collect(),
            predates_recorded_history: false,
        }
    }

    #[test]
    fn round_trip_restores_entities_and_indices() {
        let src = sample_projection();
        let state = state_from(
            &src,
            vec![
                // structural CALLS edge — must be skipped at load
                ("a.py::foo".to_string(), "a.py::bar".to_string(), "CALLS".to_string()),
                // synthetic-kind edges — must be restored
                ("a.py::foo".to_string(), "b.py::hook".to_string(), "CALLBACK".to_string()),
                ("a.py::module".to_string(), "a.py::foo".to_string(), "DEPENDS_ON".to_string()),
            ],
        );
        let (g, stats) = projection_from_state(&state).expect("load");

        assert_eq!(stats.modules, 1);
        assert_eq!(stats.classes, 1);
        assert_eq!(stats.functions, 2);
        // foo→bar, foo→external::len, bar→C.run
        assert_eq!(stats.resolved_call_pairs, 3);
        assert_eq!(stats.synthetic_edges, 2);
        assert_eq!(stats.skipped_field_concepts, 0);

        // Call indices: resolved pairs + synthetic pairs, BTreeSet-deduped.
        let callers = g.callers_by_callee.get("a.py::bar").cloned().unwrap_or_default();
        assert!(callers.contains("a.py::foo"));
        let callees = g.callees_by_caller.get("a.py::foo").cloned().unwrap_or_default();
        assert!(callees.contains("a.py::bar"));
        assert!(callees.contains("external::len"));
        assert!(callees.contains("b.py::hook")); // synthetic
        let mod_callers = g.callers_by_callee.get("a.py::foo").cloned().unwrap_or_default();
        assert!(mod_callers.contains("a.py::module")); // synthetic DEPENDS_ON
        let run_callers = g.callers_by_callee.get("a.py::C.run").cloned().unwrap_or_default();
        assert!(run_callers.contains("a.py::bar")); // Method pair

        // file_to_modules rebuilt from module paths.
        assert_eq!(
            g.file_to_modules.get(&PathBuf::from("a.py")).map(|v| v.len()),
            Some(1)
        );

        // Base state round-trips; cascade-rebuilt fields are cleared.
        let c = &g.classes["a.py::C"];
        assert!(c.resolved_bases.is_empty());
        assert!(c.mro.is_empty());
        assert!(c.methods.is_empty()); // populate_class_methods rebuilds
        assert_eq!(c.line, 1);
        let f = &g.functions["a.py::foo"];
        assert_eq!(f.resolved_calls.len(), 2);
        assert_eq!(f.kind, FunctionKind::Free);
        assert_eq!(f.parent_module, "a.py::module");
        assert!(f.calls.is_empty()); // raw refs never persisted
        let m = &g.modules["a.py::module"];
        assert_eq!(m.path, PathBuf::from("a.py"));
        assert_eq!(m.classes, vec!["a.py::C".to_string()]);
    }

    #[test]
    fn v1_concept_is_a_hard_error() {
        let mut state = state_from(&sample_projection(), Vec::new());
        // Simulate a v1 module concept (no meta_version).
        let mut concepts = HashMap::new();
        concepts.insert(
            "a.py::module".to_string(),
            NodeAttributes {
                id: "a.py::module".to_string(),
                title: "a".to_string(),
                content: "{\"id\":\"a.py::module\",\"name\":\"a\",\"kind\":\"module\",\"path\":\"a.py\",\"language\":\"Python\"}"
                    .to_string(),
                embedding_model: None,
            },
        );
        state.concepts = concepts;
        match projection_from_state(&state) {
            Err(err) => {
                assert!(err.contains("meta_version: 2"), "err: {err}");
                assert!(err.contains("analyze"), "err: {err}");
            }
            Ok(_) => panic!("v1 must not load"),
        }
    }

    #[test]
    fn standalone_field_concepts_are_skipped() {
        let mut state = state_from(&sample_projection(), Vec::new());
        state.concepts.insert(
            "a.py::C.x".to_string(),
            NodeAttributes {
                id: "a.py::C.x".to_string(),
                title: "x".to_string(),
                content: "{\"id\":\"a.py::C.x\",\"name\":\"x\",\"kind\":\"field\",\"module\":\"a.py::module\",\"class\":\"a.py::C\"}"
                    .to_string(),
                embedding_model: None,
            },
        );
        let (_g, stats) = projection_from_state(&state).expect("load");
        assert_eq!(stats.skipped_field_concepts, 1);
        assert_eq!(stats.classes, 1);
    }

    #[test]
    fn duplicate_synthetic_edges_are_deduped() {
        let state = state_from(
            &sample_projection(),
            vec![
                ("a.py::foo".to_string(), "b.py::hook".to_string(), "CALLBACK".to_string()),
                ("a.py::foo".to_string(), "b.py::hook".to_string(), "CALLBACK".to_string()),
            ],
        );
        let (_g, stats) = projection_from_state(&state).expect("load");
        assert_eq!(stats.synthetic_edges, 1);
    }
}
