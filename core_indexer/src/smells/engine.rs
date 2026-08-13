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
            for (id, c) in &graph.classes {
                let metrics = metrics_for_class(c, graph);
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

        findings
    }
}

/// Findings indexed by entity id and rule id for O(1) lookups.
pub struct SmellRegistry {
    findings_by_entity: HashMap<EntityId, Vec<Finding>>,
    findings_by_rule: HashMap<String, Vec<Finding>>,
    all_findings: Vec<Finding>,
}

impl SmellRegistry {
    pub fn new(graph: &ProjectedGraph) -> Self {
        let engine = SmellEngine::new();
        let findings = engine.run(graph);

        let mut findings_by_entity: HashMap<EntityId, Vec<Finding>> = HashMap::new();
        let mut findings_by_rule: HashMap<String, Vec<Finding>> = HashMap::new();
        for f in &findings {
            findings_by_entity
                .entry(f.entity_id.clone())
                .or_default()
                .push(f.clone());
            findings_by_rule
                .entry(f.rule_id.clone())
                .or_default()
                .push(f.clone());
        }

        Self {
            findings_by_entity,
            findings_by_rule,
            all_findings: findings,
        }
    }

    pub fn get_smells(&self, entity_id: &str) -> Vec<&Finding> {
        self.findings_by_entity
            .get(entity_id)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    pub fn get_all_smells(&self) -> &[Finding] {
        &self.all_findings
    }

    pub fn get_smells_by_rule(&self, rule_id: &str) -> Vec<&Finding> {
        self.findings_by_rule
            .get(rule_id)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
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

/// Class metrics: field_count from `fields`, WMC = Σ method cyclomatic,
/// max_method_cyclomatic = max over methods, CBO = distinct coupled classes.
///
/// `Class.methods` is always empty (see status doc §7.5) — method membership
/// is recovered by scanning `functions` for `parent_class == class.id`.
pub fn metrics_for_class(c: &Class, graph: &ProjectedGraph) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    m.insert("field_count".to_string(), c.fields.len() as f64);

    let mut wmc = 0usize;
    let mut max_cyclo = 0usize;
    for f in graph.functions.values() {
        if f.parent_class.as_ref() == Some(&c.id) {
            let cyc = f.metrics.cyclomatic;
            wmc += cyc;
            if cyc > max_cyclo {
                max_cyclo = cyc;
            }
        }
    }
    m.insert("WMC".to_string(), wmc as f64);
    m.insert("max_method_cyclomatic".to_string(), max_cyclo as f64);
    m.insert("CBO".to_string(), coupling_between_objects(c, graph) as f64);

    m
}

/// Coupling Between Objects: distinct other classes this class depends on,
/// via resolved bases and resolved call targets of its methods.
fn coupling_between_objects(c: &Class, graph: &ProjectedGraph) -> usize {
    let mut coupled: HashSet<EntityId> = HashSet::new();

    for b in &c.resolved_bases {
        if b != &c.id {
            coupled.insert(b.clone());
        }
    }

    for f in graph.functions.values() {
        if f.parent_class.as_ref() != Some(&c.id) {
            continue;
        }
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
}
