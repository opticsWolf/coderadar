// The engine loop (§6) + findings registry (§7, adapted to pure Rust — the
// Python-side `#[pyclass]` from the reference is replaced by a plain struct
// because the FFI surface in this codebase is `with_graph` + pyfunctions, not
// passing `ProjectedGraph` into Python; see reference §12).

use std::collections::{HashMap, HashSet};

use crate::types::{Class, EntityId, Function, ProjectedGraph, ResolvedCall};

use super::rule::SmellRule;
use super::types::{EvalContext, Finding, Scope};

/// Orchestrates rule evaluation over the resolved `ProjectedGraph`.
pub struct SmellEngine {
    rules: Vec<Box<dyn SmellRule>>,
}

impl SmellEngine {
    pub fn new() -> Self {
        Self {
            rules: vec![
                Box::new(super::rules::god_class::GodClass::default()),
                Box::new(super::rules::long_method::LongMethod::default()),
                Box::new(super::rules::long_parameter_list::LongParameterList::default()),
                Box::new(super::rules::deep_nesting::DeepNesting::default()),
                Box::new(super::rules::data_class::DataClass::default()),
                Box::new(super::rules::high_cyclomatic_complexity::HighCyclomaticComplexity::default()),
                Box::new(super::rules::brain_method::BrainMethod::default()),
                Box::new(super::rules::excessive_returns::ExcessiveReturns::default()),
                Box::new(super::rules::too_many_fields::TooManyFields::default()),
            ],
        }
    }

    /// Run all rules over the graph. Rules are grouped by scope so the graph
    /// is iterated once per scope, not once per rule.
    ///
    /// Findings are deduplicated to one per (rule, entity) — stale + fresh
    /// versions of an entity can coexist in a snapshot under ID variants
    /// (different path prefixes or separators), which used to report the
    /// same violation two or three times (CODERADAR_BUGS_QUIRKS.md #9).
    pub fn run(&self, graph: &ProjectedGraph) -> Vec<Finding> {
        let mut findings = Vec::new();

        let mut rules_by_scope: HashMap<Scope, Vec<&Box<dyn SmellRule>>> = HashMap::new();
        for rule in &self.rules {
            rules_by_scope.entry(rule.scope()).or_default().push(rule);
        }

        // Method scope.
        if let Some(rules) = rules_by_scope.get(&Scope::Method) {
            for (id, f) in &graph.functions {
                let metrics = metrics_for_function(f);
                let ctx = EvalContext {
                    entity_id: id.as_str(),
                    entity_name: f.name.as_str(),
                    metrics: &metrics,
                    graph,
                };
                for rule in rules {
                    if let Some(finding) = rule.evaluate(&ctx) {
                        findings.push(finding);
                    }
                }
            }
        }

        // Class scope.
        if let Some(rules) = rules_by_scope.get(&Scope::Class) {
            // Both class metrics used to scan every function, so the pass was
            // O(classes × functions) twice over. One grouping serves both.
            let methods = methods_by_class(graph);
            for (id, c) in &graph.classes {
                let metrics = metrics_for_class(c, graph, &methods);
                let ctx = EvalContext {
                    entity_id: id.as_str(),
                    entity_name: c.name.as_str(),
                    metrics: &metrics,
                    graph,
                };
                for rule in rules {
                    if let Some(finding) = rule.evaluate(&ctx) {
                        findings.push(finding);
                    }
                }
            }
        }

        // File scope — no rules target it yet, but the routing is in place.

        // Dedupe: one finding per (rule, canonical entity identity), first
        // wins. See the run() doc comment for why this guard exists.
        let mut seen: HashSet<String> = HashSet::new();
        findings.retain(|f| seen.insert(finding_key(f)));

        findings
    }
}

/// Canonical dedupe key: `<rule>|<normalized-file>|<symbol>`.
///
/// Compares identities, not raw IDs: `.\a\b.py::f`, `./a/b.py::f` and any
/// absolute-prefix variant of the same entity collapse to one key.
fn finding_key(f: &Finding) -> String {
    let (file, sym) = match f.entity_id.rsplit_once("::") {
        Some((path, sym)) => (path, sym),
        None => (f.entity_id.as_str(), ""),
    };
    let file = file
        .trim_start_matches("./")
        .trim_start_matches(".\\")
        .replace('\\', "/")
        .to_lowercase();
    format!("{}|{}|{}", f.rule_id, file, sym)
}

// ── Metric resolution (Phase 4.1 class-level roll-ups) ─────────────────────

/// Method metrics: LOC, param_count come from struct fields; cyclomatic,
/// nesting_depth and return_count come from the extraction-time AST pass
/// (`Function.metrics`).
pub fn metrics_for_function(f: &Function) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    // Inclusive line count: (exit_line - line) + 1.
    m.insert("LOC".to_string(), (f.exit_line.saturating_sub(f.line) + 1) as f64);
    m.insert("param_count".to_string(), f.parameters.len() as f64);
    m.insert("cyclomatic".to_string(), f.metrics.cyclomatic as f64);
    m.insert("nesting_depth".to_string(), f.metrics.nesting_depth as f64);
    m.insert("return_count".to_string(), f.metrics.return_count as f64);
    m
}

/// `class id → its methods`, built once per pass.
pub type MethodsByClass<'a> = HashMap<&'a str, Vec<&'a Function>>;

/// Group every method under its class in one pass over `functions`.
pub fn methods_by_class(graph: &ProjectedGraph) -> MethodsByClass<'_> {
    let mut index: MethodsByClass = HashMap::new();
    for f in graph.functions.values() {
        if let Some(class_id) = &f.parent_class {
            index.entry(class_id.as_str()).or_default().push(f.as_ref());
        }
    }
    index
}

/// Class metrics: field_count from `fields`, WMC = Σ method cyclomatic,
/// max_method_cyclomatic = max over methods, CBO = distinct coupled classes.
///
/// `Class.methods` IS populated (2.7), but method membership is still recovered
/// by scanning `functions` for `parent_class == class.id` — keeping a single
/// source of truth and parity with the rest of the engine.
pub fn metrics_for_class(
    c: &Class,
    graph: &ProjectedGraph,
    methods: &MethodsByClass<'_>,
) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    m.insert("field_count".to_string(), c.fields.len() as f64);

    let mut wmc = 0usize;
    let mut max_cyclo = 0usize;
    for f in methods.get(c.id.as_str()).into_iter().flatten() {
        let cyc = f.metrics.cyclomatic;
        wmc += cyc;
        if cyc > max_cyclo {
            max_cyclo = cyc;
        }
    }
    m.insert("WMC".to_string(), wmc as f64);
    m.insert("max_method_cyclomatic".to_string(), max_cyclo as f64);
    m.insert("CBO".to_string(), coupling_between_objects(c, graph, methods) as f64);

    m
}

/// Coupling Between Objects: distinct other classes this class depends on,
/// via resolved bases and resolved call targets of its methods.
fn coupling_between_objects(
    c: &Class,
    graph: &ProjectedGraph,
    methods: &MethodsByClass<'_>,
) -> usize {
    let mut coupled: HashSet<EntityId> = HashSet::new();

    for b in &c.resolved_bases {
        if b != &c.id {
            coupled.insert(b.clone());
        }
    }

    for f in methods.get(c.id.as_str()).into_iter().flatten() {
        for call in &f.resolved_calls {
            if let Some(target_class) = target_class_of(call, graph) {
                if target_class != c.id {
                    coupled.insert(target_class);
                }
            }
        }
    }

    coupled.len()
}

fn target_class_of(call: &ResolvedCall, graph: &ProjectedGraph) -> Option<EntityId> {
    match call {
        ResolvedCall::Function(id) => graph.functions.get(id).and_then(|f| f.parent_class.clone()),
        ResolvedCall::Method { method, .. } => {
            graph.functions.get(method).and_then(|f| f.parent_class.clone())
        }
        ResolvedCall::Constructor(id) => Some(id.clone()),
        ResolvedCall::Builtin(_) | ResolvedCall::External(_) | ResolvedCall::Unresolved { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::smells::engine::SmellEngine;
    use crate::smells::rule::SmellRule;
    use crate::smells::types::{EvalContext, Severity};
    use crate::types::ProjectedGraph;

    fn empty_graph() -> ProjectedGraph {
        ProjectedGraph {
            modules: HashMap::new(),
            classes: HashMap::new(),
            functions: HashMap::new(),
            imports: HashMap::new(),
            constants: HashMap::new(),
            type_aliases: HashMap::new(),
            file_to_modules: HashMap::new(),
            module_by_dotted_name: HashMap::new(),
            importers: HashMap::new(),
            imports_by_importer: HashMap::new(),
            callers_by_callee: HashMap::new(),
            callees_by_caller: HashMap::new(),
            subclasses: HashMap::new(),
            overridden_by: HashMap::new(),
            overrides_base: HashMap::new(),
            ambiguous_bases: Vec::new(),
        }
    }

    fn ctx<'a>(
        graph: &'a ProjectedGraph,
        id: &'a str,
        name: &'a str,
        metrics: &'a HashMap<String, f64>,
    ) -> EvalContext<'a> {
        EvalContext { entity_id: id, entity_name: name, metrics, graph }
    }

    #[test]
    fn test_long_parameter_list_boundaries() {
        let g = empty_graph();
        let rule = crate::smells::rules::long_parameter_list::LongParameterList::default();

        let m4 = HashMap::from([("param_count".to_string(), 4.0)]);
        assert!(rule.evaluate(&ctx(&g, "a", "f", &m4)).is_none(), "4 params is the threshold, not above it");

        let m5 = HashMap::from([("param_count".to_string(), 5.0)]);
        let f = rule.evaluate(&ctx(&g, "a", "f", &m5)).expect("5 params triggers");
        assert_eq!(f.rule_id, "long-parameter-list");
        assert_eq!(f.severity, Severity::Info);
    }

    #[test]
    fn test_too_many_fields_boundary() {
        let g = empty_graph();
        let rule = crate::smells::rules::too_many_fields::TooManyFields::default();

        let m9 = HashMap::from([("field_count".to_string(), 9.0)]);
        assert!(rule.evaluate(&ctx(&g, "c", "C", &m9)).is_none());

        let m10 = HashMap::from([("field_count".to_string(), 10.0)]);
        assert!(rule.evaluate(&ctx(&g, "c", "C", &m10)).is_some());
    }

    #[test]
    fn test_god_class_requires_both_signals() {
        let g = empty_graph();
        let rule = crate::smells::rules::god_class::GodClass::default();

        let low_cbo = HashMap::from([("WMC".to_string(), 50.0), ("CBO".to_string(), 3.0)]);
        assert!(rule.evaluate(&ctx(&g, "c", "C", &low_cbo)).is_none());

        let hit = HashMap::from([("WMC".to_string(), 50.0), ("CBO".to_string(), 6.0)]);
        let f = rule.evaluate(&ctx(&g, "c", "C", &hit)).expect("both thresholds met");
        assert_eq!(f.severity, Severity::Medium);
    }

    #[test]
    fn test_engine_empty_graph_has_no_findings() {
        let g = empty_graph();
        let engine = SmellEngine::new();
        assert!(engine.run(&g).is_empty());
    }

    #[test]
    fn finding_key_collapses_id_variants_of_one_entity() {
        // BUGS_QUIRKS #9: stale + fresh versions of an entity coexist under
        // ID variants; the dedupe key must treat them as one.
        use super::finding_key;
        use crate::smells::types::Finding;
        let mk = |entity_id: &str| Finding {
            rule_id: "long-method".into(),
            entity_id: entity_id.into(),
            severity: Severity::High,
            message: "m".into(),
            signals: HashMap::new(),
        };
        assert_eq!(finding_key(&mk(".\\src\\a.py::f")), finding_key(&mk("./src/a.py::f")));
        assert_eq!(finding_key(&mk("./SRC/a.py::f")), finding_key(&mk("./src/a.py::f")));
        assert_ne!(finding_key(&mk("./src/a.py::f")), finding_key(&mk("./src/a.py::g")));
        assert_ne!(
            finding_key(&mk("./src/a.py::f")),
            finding_key(&mk("./src/b.py::f"))
        );
    }
}
