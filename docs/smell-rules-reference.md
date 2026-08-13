# Code Smell Engine — Reference & Implementation Design

Status: **design reference (not yet implemented)**. This document is the
canonical spec for the native Rust smell engine that is the remaining half of
the `traverse_smell` branch (see `docs/traverse-smell-status.md` §3, Phase 4).

It replaces the earlier YAML/Python DSL idea with a **fully native Rust**
implementation: rules are Rust structs validated by the compiler, the
evaluation loop reads `ProjectedGraph` + metrics directly (no FFI per
entity, no `eval()`).

> **Implementation prerequisite.** Every rule below depends on a **metrics
> pass** (Phase 4.1) that must be built *first* — none of the signal keys
> (`WMC`, `CBO`, `LOC`, `cyclomatic`, `nesting_depth`, `param_count`,
> `field_count`, `return_count`, `max_method_cyclomatic`) exist in the graph
> today. See §2 and §12 for the full signal table and the adaptation notes
> mapping this design to CodeRadar's actual types.

---

## 1. Data Flow & Architecture

A 4-stage pipeline, executed synchronously during or immediately after graph
construction:

1. **Metrics Pass (AST)** — during tree-sitter extraction, compute raw
   structural metrics (cyclomatic complexity, nesting depth, …) and attach
   them to the entity.
2. **Rule Definition** — smells are Rust structs implementing `SmellRule`;
   thresholds are baked in as fields with `Default` impls.
3. **Engine Execution (`SmellEngine`)** — iterate `ProjectedGraph`, filter by
   scope (File / Class / Method), and run each registered rule.
4. **Annotation (`SmellRegistry`)** — collect findings, index by `EntityId`,
   expose to Python via a dedicated PyO3 `#[pyclass]`.

```
tree-sitter AST ──metrics pass──▶ metrics table ──┐
ProjectedGraph ──────────────────────────────────┤
                                                  ▼
                          SmellEngine.run(graph) ──▶ Vec<Finding>
                                                  │
                                                  ▼
                              SmellRegistry (pyclass) ──▶ MCP get_smells
```

---

## 2. Metrics Pass — the prerequisite (Phase 4.1)

Rules consume a flat `HashMap<String, f64>` of **signals** keyed by metric
name. This table is the contract between the metrics pass and the rules.

| Signal | Scope | Source | Notes |
|---|---|---|---|
| `LOC` | method | struct | `(exit_line - line) + 1` (inclusive; from `Function` struct) |
| `param_count` | method | struct | `Function.parameters.len()` |
| `cyclomatic` | method | AST | 1 + decision points (`if`, `for`, `while`, `case`, `catch`, `&&`, `\|\|`, `?:`, `loop`, …) — computed in `extract/single_pass.rs` via `smells::metrics` |
| `nesting_depth` | method | AST | max control-flow nesting depth in the body — same metrics pass |
| `return_count` | method | AST | count of `return` statements — same metrics pass |
| `field_count` | class | struct | `Class.fields.len()` — **now populated**: `extract/single_pass.rs` emits class-level `@field` captures into `ExtractedClass.fields` |
| `WMC` | class | AST+struct | Σ `cyclomatic` over the class's methods (scan by `parent_class`) |
| `max_method_cyclomatic` | class | AST+struct | max `cyclomatic` among the class's methods |
| `CBO` | class | edges+struct | distinct other classes referenced (resolved calls → `parent_class`, `resolved_bases`, field/instantiation types) |

**Where each signal is computed:** `LOC` / `param_count` / `field_count` come
from struct fields at engine-run time (`smells/engine.rs`); `cyclomatic` /
`nesting_depth` / `return_count` are computed in the extraction metrics pass
(`smells/metrics.rs`, stored on `Function.metrics`); the class roll-ups `WMC` /
`max_method_cyclomatic` and `CBO` are derived in `metrics_for_class` from the
resolved graph.

---

## 3. Core Data Structures

File: `core_indexer/src/smells/types.rs`

```rust
use crate::graph::{EntityId, ProjectedGraph, EntityRef};
use std::collections::HashMap;
use pyo3::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    File,
    Class,
    Method,
}

#[derive(Debug, Clone)]
pub enum Severity {
    Info,
    Medium,
    High,
    Critical,
}

/// A detected instance of a code smell.
#[derive(Debug, Clone)]
pub struct Finding {
    pub rule_id: String,
    pub entity_id: EntityId,
    pub severity: Severity,
    pub message: String,
    /// The metric values that triggered this finding (for observability)
    pub signals: HashMap<String, f64>,
}

/// Context passed into every rule evaluation.
/// Provides access to the entity, its precomputed metrics, and the global graph.
pub struct EvalContext<'a> {
    pub entity: &'a EntityRef,
    pub metrics: &'a HashMap<String, f64>,
    pub graph: &'a ProjectedGraph,
}
```

---

## 4. The Rule Abstraction (`SmellRule` Trait)

File: `core_indexer/src/smells/rule.rs`

```rust
use super::types::{Scope, Severity, Finding, EvalContext};

pub trait SmellRule: Send + Sync {
    /// The unique identifier for the smell (e.g., "god-class").
    fn id(&self) -> &'static str;

    /// The entity scope this rule applies to.
    fn scope(&self) -> Scope;

    /// The list of metric keys this rule depends on.
    /// (Useful for future caching/parallelization.)
    fn signals_needed(&self) -> &'static [&'static str];

    /// Evaluates the entity in context. Returns `Some(Finding)` if the smell
    /// is detected.
    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding>;
}
```

---

## 5. Concrete Rule Implementations

Directory: `core_indexer/src/smells/rules/`. Thresholds are struct fields so
they can be overridden programmatically later.

### 5.1 God Class (Class scope)

```rust
// smells/rules/god_class.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct GodClass {
    pub wmc_threshold: usize,
    pub cbo_threshold: usize,
}

impl Default for GodClass {
    fn default() -> Self {
        Self { wmc_threshold: 47, cbo_threshold: 5 }
    }
}

impl SmellRule for GodClass {
    fn id(&self) -> &'static str { "god-class" }
    fn scope(&self) -> Scope { Scope::Class }
    fn signals_needed(&self) -> &'static [&'static str] { &["WMC", "CBO"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let wmc = ctx.metrics.get("WMC").copied().unwrap_or(0.0) as usize;
        let cbo = ctx.metrics.get("CBO").copied().unwrap_or(0.0) as usize;

        if wmc >= self.wmc_threshold && cbo >= self.cbo_threshold {
            let severity = if wmc >= 100 { Severity::Critical }
                           else if wmc >= 70 { Severity::High }
                           else { Severity::Medium };

            let mut signals = HashMap::new();
            signals.insert("WMC".to_string(), wmc as f64);
            signals.insert("CBO".to_string(), cbo as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Class '{}' has WMC={} and CBO={} - likely God Class",
                    ctx.entity.name, wmc, cbo
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.2 Long Method (Method scope)

```rust
// smells/rules/long_method.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct LongMethod {
    pub loc_threshold: usize,
    pub cyclomatic_threshold: usize,
}

impl Default for LongMethod {
    fn default() -> Self {
        Self { loc_threshold: 50, cyclomatic_threshold: 10 }
    }
}

impl SmellRule for LongMethod {
    fn id(&self) -> &'static str { "long-method" }
    fn scope(&self) -> Scope { Scope::Method }
    fn signals_needed(&self) -> &'static [&'static str] { &["LOC", "cyclomatic"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let loc = ctx.metrics.get("LOC").copied().unwrap_or(0.0) as usize;
        let cyclo = ctx.metrics.get("cyclomatic").copied().unwrap_or(0.0) as usize;

        // Trigger if EITHER threshold is breached
        if loc >= self.loc_threshold || cyclo >= self.cyclomatic_threshold {
            let severity = if loc >= 100 || cyclo >= 20 { Severity::High }
                           else { Severity::Medium };

            let mut signals = HashMap::new();
            signals.insert("LOC".to_string(), loc as f64);
            signals.insert("cyclomatic".to_string(), cyclo as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Method '{}' is too long (LOC={}, Cyclomatic={}) - consider extracting",
                    ctx.entity.name, loc, cyclo
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.3 Long Parameter List (Method scope)

```rust
// smells/rules/long_parameter_list.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct LongParameterList {
    pub param_threshold: usize,
}

impl Default for LongParameterList {
    fn default() -> Self {
        Self { param_threshold: 4 } // Standard heuristic threshold
    }
}

impl SmellRule for LongParameterList {
    fn id(&self) -> &'static str { "long-parameter-list" }
    fn scope(&self) -> Scope { Scope::Method }
    fn signals_needed(&self) -> &'static [&'static str] { &["param_count"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let params = ctx.metrics.get("param_count").copied().unwrap_or(0.0) as usize;

        if params > self.param_threshold {
            let severity = if params >= 7 { Severity::High }
                           else if params >= 6 { Severity::Medium }
                           else { Severity::Info };

            let mut signals = HashMap::new();
            signals.insert("param_count".to_string(), params as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Method '{}' has {} parameters - consider grouping into an object",
                    ctx.entity.name, params
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.4 Deeply Nested Code (Method scope)

```rust
// smells/rules/deep_nesting.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct DeepNesting {
    pub nesting_threshold: usize,
}

impl Default for DeepNesting {
    fn default() -> Self {
        Self { nesting_threshold: 4 } // 4 levels deep is a common breaking point
    }
}

impl SmellRule for DeepNesting {
    fn id(&self) -> &'static str { "deep-nesting" }
    fn scope(&self) -> Scope { Scope::Method }
    fn signals_needed(&self) -> &'static [&'static str] { &["nesting_depth"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let depth = ctx.metrics.get("nesting_depth").copied().unwrap_or(0.0) as usize;

        if depth >= self.nesting_threshold {
            let severity = if depth >= 6 { Severity::High }
                           else { Severity::Medium };

            let mut signals = HashMap::new();
            signals.insert("nesting_depth".to_string(), depth as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Method '{}' is nested {} levels deep - consider using guard clauses or extracting methods",
                    ctx.entity.name, depth
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.5 Data Class (Class scope)

```rust
// smells/rules/data_class.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct DataClass {
    pub field_threshold: usize,
    pub max_wmc_threshold: usize, // Max WMC to be considered "dumb"
}

impl Default for DataClass {
    fn default() -> Self {
        Self { field_threshold: 5, max_wmc_threshold: 10 }
    }
}

impl SmellRule for DataClass {
    fn id(&self) -> &'static str { "data-class" }
    fn scope(&self) -> Scope { Scope::Class }
    fn signals_needed(&self) -> &'static [&'static str] { &["field_count", "WMC"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let fields = ctx.metrics.get("field_count").copied().unwrap_or(0.0) as usize;
        let wmc = ctx.metrics.get("WMC").copied().unwrap_or(0.0) as usize;

        // Trigger if it has a lot of fields but very little logic
        if fields >= self.field_threshold && wmc <= self.max_wmc_threshold {
            let severity = if fields >= 10 && wmc <= 5 { Severity::High }
                           else { Severity::Medium };

            let mut signals = HashMap::new();
            signals.insert("field_count".to_string(), fields as f64);
            signals.insert("WMC".to_string(), wmc as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Class '{}' is a Data Class (Fields={}, WMC={}) - consider moving behavior into the class",
                    ctx.entity.name, fields, wmc
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.6 High Cyclomatic Complexity (Method scope)

```rust
// smells/rules/high_cyclomatic_complexity.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct HighCyclomaticComplexity {
    pub warning_threshold: usize,
    pub critical_threshold: usize,
}

impl Default for HighCyclomaticComplexity {
    fn default() -> Self {
        Self { warning_threshold: 10, critical_threshold: 20 }
    }
}

impl SmellRule for HighCyclomaticComplexity {
    fn id(&self) -> &'static str { "high-cyclomatic-complexity" }
    fn scope(&self) -> Scope { Scope::Method }
    fn signals_needed(&self) -> &'static [&'static str] { &["cyclomatic"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let cyclo = ctx.metrics.get("cyclomatic").copied().unwrap_or(0.0) as usize;

        if cyclo >= self.warning_threshold {
            let severity = if cyclo >= self.critical_threshold { Severity::Critical }
                           else { Severity::High };

            let mut signals = HashMap::new();
            signals.insert("cyclomatic".to_string(), cyclo as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Method '{}' has high cyclomatic complexity ({}) - too many branching paths",
                    ctx.entity.name, cyclo
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.7 Brain Method (Class scope)

```rust
// smells/rules/brain_method.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct BrainMethod {
    pub max_method_cyclo_threshold: usize,
    pub min_class_wmc: usize, // Ensure the class actually has some logic overall
}

impl Default for BrainMethod {
    fn default() -> Self {
        Self { max_method_cyclo_threshold: 15, min_class_wmc: 20 }
    }
}

impl SmellRule for BrainMethod {
    fn id(&self) -> &'static str { "brain-method" }
    fn scope(&self) -> Scope { Scope::Class }
    // Requires the AST pass to track the max cyclomatic complexity of any
    // single method in the class.
    fn signals_needed(&self) -> &'static [&'static str] {
        &["max_method_cyclomatic", "WMC"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let max_cyclo = ctx.metrics.get("max_method_cyclomatic").copied().unwrap_or(0.0) as usize;
        let wmc = ctx.metrics.get("WMC").copied().unwrap_or(0.0) as usize;

        if max_cyclo >= self.max_method_cyclo_threshold && wmc >= self.min_class_wmc {
            let severity = if max_cyclo >= 30 { Severity::Critical }
                           else { Severity::High };

            let mut signals = HashMap::new();
            signals.insert("max_method_cyclomatic".to_string(), max_cyclo as f64);
            signals.insert("WMC".to_string(), wmc as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Class '{}' likely contains a Brain Method (Max Method Cyclomatic={}, WMC={}) - logic is too centralized",
                    ctx.entity.name, max_cyclo, wmc
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.8 Excessive Returns (Method scope)

```rust
// smells/rules/excessive_returns.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct ExcessiveReturns {
    pub return_threshold: usize,
}

impl Default for ExcessiveReturns {
    fn default() -> Self {
        Self { return_threshold: 5 } // > 5 exit points is generally frowned upon
    }
}

impl SmellRule for ExcessiveReturns {
    fn id(&self) -> &'static str { "excessive-returns" }
    fn scope(&self) -> Scope { Scope::Method }
    fn signals_needed(&self) -> &'static [&'static str] { &["return_count"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let returns = ctx.metrics.get("return_count").copied().unwrap_or(0.0) as usize;

        if returns > self.return_threshold {
            let severity = if returns >= 8 { Severity::High }
                           else { Severity::Medium };

            let mut signals = HashMap::new();
            signals.insert("return_count".to_string(), returns as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Method '{}' has {} return statements - consider consolidating control flow",
                    ctx.entity.name, returns
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.9 Too Many Fields (Class scope)

```rust
// smells/rules/too_many_fields.rs
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct TooManyFields {
    pub field_threshold: usize,
}

impl Default for TooManyFields {
    fn default() -> Self {
        Self { field_threshold: 10 } // > 10 fields is a common heuristic limit
    }
}

impl SmellRule for TooManyFields {
    fn id(&self) -> &'static str { "too-many-fields" }
    fn scope(&self) -> Scope { Scope::Class }
    fn signals_needed(&self) -> &'static [&'static str] { &["field_count"] }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let fields = ctx.metrics.get("field_count").copied().unwrap_or(0.0) as usize;

        if fields >= self.field_threshold {
            let severity = if fields >= 20 { Severity::High }
                           else { Severity::Medium };

            let mut signals = HashMap::new();
            signals.insert("field_count".to_string(), fields as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity.id.clone(),
                severity,
                message: format!(
                    "Class '{}' has {} fields - possible violation of Single Responsibility Principle",
                    ctx.entity.name, fields
                ),
                signals,
            })
        } else {
            None
        }
    }
}
```

### 5.10 Rule summary

| Rule | id | Scope | Signals | Default thresholds |
|---|---|---|---|---|
| God Class | `god-class` | Class | `WMC`, `CBO` | wmc≥47 & cbo≥5 |
| Long Method | `long-method` | Method | `LOC`, `cyclomatic` | loc≥50 \|\| cyclo≥10 |
| Long Parameter List | `long-parameter-list` | Method | `param_count` | >4 |
| Deep Nesting | `deep-nesting` | Method | `nesting_depth` | ≥4 |
| Data Class | `data-class` | Class | `field_count`, `WMC` | fields≥5 & wmc≤10 |
| High Cyclomatic Complexity | `high-cyclomatic-complexity` | Method | `cyclomatic` | ≥10 |
| Brain Method | `brain-method` | Class | `max_method_cyclomatic`, `WMC` | max≥15 & wmc≥20 |
| Excessive Returns | `excessive-returns` | Method | `return_count` | >5 |
| Too Many Fields | `too-many-fields` | Class | `field_count` | ≥10 |

---

## 6. The Engine Loop

File: `core_indexer/src/smells/engine.rs`

```rust
use crate::graph::ProjectedGraph;
use crate::smells::rule::SmellRule;
use crate::smells::types::*;
use std::collections::HashMap;

pub struct SmellEngine {
    rules: Vec<Box<dyn SmellRule>>,
}

impl SmellEngine {
    pub fn new() -> Self {
        // Register rules. Adding a new rule is a one-line change here.
        Self {
            rules: vec![
                Box::new(crate::smells::rules::god_class::GodClass::default()),
                Box::new(crate::smells::rules::long_method::LongMethod::default()),
                Box::new(crate::smells::rules::long_parameter_list::LongParameterList::default()),
                Box::new(crate::smells::rules::deep_nesting::DeepNesting::default()),
                Box::new(crate::smells::rules::data_class::DataClass::default()),
                Box::new(crate::smells::rules::high_cyclomatic_complexity::HighCyclomaticComplexity::default()),
                Box::new(crate::smells::rules::brain_method::BrainMethod::default()),
                Box::new(crate::smells::rules::excessive_returns::ExcessiveReturns::default()),
                Box::new(crate::smells::rules::too_many_fields::TooManyFields::default()),
            ],
        }
    }

    pub fn run(&self, graph: &ProjectedGraph) -> Vec<Finding> {
        let mut findings = Vec::new();

        // Group rules by scope for efficient iteration
        let mut rules_by_scope: HashMap<Scope, Vec<&Box<dyn SmellRule>>> = HashMap::new();
        for rule in &self.rules {
            rules_by_scope.entry(rule.scope()).or_default().push(rule);
        }

        // Helper to process a batch of entities
        let process_entities = |entities: &[&EntityRef], rules: &[&Box<dyn SmellRule>]| {
            for entity in entities {
                // graph.get_metrics() assumes a lookup table mapping EntityId -> HashMap<String, f64>
                let metrics = graph.get_metrics(&entity.id).unwrap_or_default();
                let ctx = EvalContext { entity, metrics: &metrics, graph };

                for rule in rules {
                    if let Some(finding) = rule.evaluate(&ctx) {
                        findings.push(finding);
                    }
                }
            }
        };

        // Iterate scoped entities (pseudocode - adapt to ProjectedGraph's actual API)
        if let Some(rules) = rules_by_scope.get(&Scope::File) {
            process_entities(&graph.iter_files().collect::<Vec<_>>(), rules);
        }
        if let Some(rules) = rules_by_scope.get(&Scope::Class) {
            process_entities(&graph.iter_classes().collect::<Vec<_>>(), rules);
        }
        if let Some(rules) = rules_by_scope.get(&Scope::Method) {
            process_entities(&graph.iter_methods().collect::<Vec<_>>(), rules);
        }

        findings
    }
}
```

---

## 7. Python Integration (`SmellRegistry`)

> **Implementation note (how this actually shipped).** The `#[pyclass]` sketch
> below was **adapted** during implementation (see §12): `SmellRegistry` is a
> pure-Rust struct in `smells/engine.rs` (same `get_smells` /
> `get_all_smells` / `get_smells_by_rule` API), and the FFI surface is a
> `#[pyfunction] fn get_smells(entity_id=None, rule_id=None)` in `lib.rs` that
> runs the engine via `with_graph` + `py.allow_threads` and materializes dicts
> — matching this codebase's existing `traverse` / `graph_stats` pattern
> (findings #5 in `traverse-smell-status.md`: the "pass `ProjectedGraph` into
> Python" sketch is outdated). No `registry.rs` file exists; the logic lives in
> `engine.rs`.

File: `core_indexer/src/smells/engine.rs` (registry) + `lib.rs` (pyfunction)

```rust
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use crate::graph::ProjectedGraph;
use crate::smells::engine::SmellEngine;
use crate::smells::types::*;
use std::collections::HashMap;

#[pyclass]
pub struct SmellRegistry {
    findings_by_entity: HashMap<String, Vec<Finding>>,
    all_findings: Vec<Finding>,
}

#[pymethods]
impl SmellRegistry {
    #[new]
    pub fn new(graph: &ProjectedGraph) -> Self {
        let engine = SmellEngine::new();
        let findings = engine.run(graph);

        let mut findings_by_entity = HashMap::new();
        for f in &findings {
            findings_by_entity.entry(f.entity_id.clone()).or_default().push(f.clone());
        }

        Self {
            findings_by_entity,
            all_findings: findings,
        }
    }

    /// Get all smells for a specific entity ID.
    pub fn get_smells(&self, py: Python<'_>, entity_id: &str) -> PyResult<Vec<Py<PyDict>>> {
        let findings = self.findings_by_entity.get(entity_id).cloned().unwrap_or_default();
        Self::format_findings(py, &findings)
    }

    /// Get all smells detected in the codebase.
    pub fn get_all_smells(&self, py: Python<'_>) -> PyResult<Vec<Py<PyDict>>> {
        Self::format_findings(py, &self.all_findings)
    }

    /// Get smells filtered by rule ID.
    pub fn get_smells_by_rule(&self, py: Python<'_>, rule_id: &str) -> PyResult<Vec<Py<PyDict>>> {
        let filtered: Vec<_> = self.all_findings.iter()
            .filter(|f| f.rule_id == rule_id)
            .cloned()
            .collect();
        Self::format_findings(py, &filtered)
    }
}

impl SmellRegistry {
    fn format_findings(py: Python<'_>, findings: &[Finding]) -> PyResult<Vec<Py<PyDict>>> {
        let mut results = Vec::with_capacity(findings.len());
        for finding in findings {
            let dict = PyDict::new(py);
            dict.set_item("rule_id", &finding.rule_id)?;
            dict.set_item("entity_id", &finding.entity_id)?;
            dict.set_item("severity", format!("{:?}", finding.severity))?;
            dict.set_item("message", &finding.message)?;

            let signals_dict = PyDict::new(py);
            for (k, v) in &finding.signals {
                signals_dict.set_item(k, *v)?;
            }
            dict.set_item("signals", signals_dict)?;

            results.push(dict.into());
        }
        Ok(results)
    }
}
```

---

## 8. MCP Tool Exposure (Python Side)

File: `py_agent/src/coderadar/mcp/server.py`

```python
# server.py
from ._core import SmellRegistry

@mcp.tool(description="Detect code smells (architectural issues) for a specific entity or across the codebase.")
def get_smells(entity_id: str = None, rule_id: str = None) -> list[dict]:
    """
    Analyzes the graph for code smells.
    Run without arguments to get all smells project-wide.
    """
    # graph is the ProjectedGraph instance initialized at server startup
    registry = SmellRegistry(graph)

    if entity_id:
        return registry.get_smells(entity_id)
    elif rule_id:
        return registry.get_smells_by_rule(rule_id)
    else:
        return registry.get_all_smells()
```

> **Registration note.** The MCP server currently exposes 17 tools via the
> `@mcp.tool(...)` decorator (see `server.py:135-547`). `get_smells` becomes
> tool #18. The decorator shape differs slightly from the sketch above — follow
> the existing pattern (name, description, `annotations={...}`), e.g.
> `@mcp.tool(description=..., annotations={...})` then a `def get_smells(...)`
> with a `_get_smells(graph, ...)` render helper, mirroring
> `codegraph_traverse` → `_traverse` at `server.py:371/1278`.

---

## 9. Advantages of the native approach

1. **Type Safety** — rule thresholds and metric lookups are validated by the
   Rust compiler.
2. **Performance** — zero FFI round-trips during the evaluation loop; the
   graph is traversed in native Rust.
3. **Extensibility** — adding a new smell = one struct + one trait impl + one
   line in `SmellEngine::new()`.
4. **Clean Separation** — smells live outside the core `ProjectedGraph`
   entity dictionaries, preventing namespace pollution and decoupling analysis
   logic from structural data.

---

## 10. AST Requirements (the metrics pass must emit)

For the rules to function, the tree-sitter metrics pass must emit:

- `cyclomatic` — per method
- `return_count` — per method
- `nesting_depth` — per method
- `param_count` — per method (already in `Function.parameters`)
- `LOC` — per method (already derivable from `Function.line`/`exit_line`)
- `field_count` — per class (already in `Class.fields`)
- `WMC` — per class (Σ of child methods' `cyclomatic`)
- `max_method_cyclomatic` — per class (max `cyclomatic` of its methods)
- `CBO` — per class (distinct coupled classes via resolved edges + bases)

---

## 11. Implementation order (matches status doc §3, Phase 4)

1. **4.1 Metrics pass** — compute the §2/§10 signals, attach to a metrics
   table (`HashMap<EntityId, HashMap<String, f64>>`) or to the entity structs.
   This unblocks everything else.
2. **4.2 `smells/types.rs` + `smells/rule.rs`** — `Scope`, `Severity`,
   `Finding`, `EvalContext`, `SmellRule`.
3. **4.3 Rules + `SmellEngine`** — 9 rules in `smells/rules/`, wired in
   `SmellEngine::new()`.
4. **4.4 `SmellRegistry` pyclass + `get_smells` MCP tool** — registry in
   Rust, tool in `server.py`.
5. **Tests** — Rust unit tests per rule (boundary thresholds, scope routing,
   severity tiers) + a Python e2e for the MCP tool.

---

## 12. Adaptation notes — mapping this design to CodeRadar's actual API

The design above uses a small target API that **does not exist yet** and must
be built (or adapted) as part of Phase 4. Honest deltas against the current
codebase:

| Design symbol | Reality in `core_indexer` | Required work |
|---|---|---|
| `EntityRef` | No unified entity enum — `ProjectedGraph` holds `functions: HashMap<EntityId, Arc<Function>>`, `classes: …Arc<Class>`, `modules: …Arc<Module>`, `imports`, `constants`, `type_aliases` (types.rs:998). | Either add `enum EntityRef { Function(Arc<Function>), Class(Arc<Class>), Module(Arc<Module>) }`, or make rules scope-specific (a `ClassRule` takes `&Class`, a `MethodRule` takes `&Function`). The scope-specific form avoids a new enum and is simpler. |
| `graph.get_metrics(id)` | No metrics table exists | Phase 4.1 must build `HashMap<EntityId, HashMap<String, f64>>` (or store metrics on the structs). |
| `graph.iter_files()` / `iter_classes()` / `iter_methods()` | No such iterators | Iterate `graph.modules.values()` (File), `graph.classes.values()` (Class), `graph.functions.values()` (Method) directly. |
| `ctx.entity.name` / `ctx.entity.id` | Concrete fields on `Function`/`Class`/`Module` (`name`, `id`) | Straightforward with a scope-specific context or a tiny `name()/id()` helper on the enum. |
| `CBO` metric | No coupling metric | Approximate from `Function.resolved_calls` → callee's `parent_class`, plus `Class.resolved_bases` (both populated after the Phase-D resolve back-fill). |
| `PyList` import in registry.rs | unused in the sketch | drop it. |
| `Severity` needs `#[pyclass]`/`IntoPy` or string conversion | plain enum in the sketch | registry already stringifies via `format!("{:?}")` — fine for v1. |

**Key structural constraint** (already documented in status doc §7.5):
`class.methods` is always `vec![]`; method lookup must scan
`projection.functions` by `parent_class` (the `resolve_one_function` pattern).
The metrics pass for `WMC`/`max_method_cyclomatic` must follow the same
`parent_class` scan, not read `class.methods`.

**Call ordering:** metrics must be computed *after* the resolve cascade
(`compute_all_mro` → `resolve_class_hierarchy` → `resolve_imports` →
`resolve_overrides` → `resolve_all_calls`) so that `CBO`/call-derived signals
see resolved targets. The smell engine should run on the resolved
`ProjectedGraph` snapshot, exactly like `persist_edges` does today.
