// Core data structures for the smell engine (§3 of the reference).

use std::collections::HashMap;

use crate::types::{EntityId, ProjectedGraph};

/// The entity scope a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    File,
    Class,
    Method,
}

/// Finding severity, low → high.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Info,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Info => "Info",
            Severity::Medium => "Medium",
            Severity::High => "High",
            Severity::Critical => "Critical",
        }
    }
}

/// A detected instance of a code smell.
#[derive(Debug, Clone)]
pub struct Finding {
    pub rule_id: String,
    pub entity_id: EntityId,
    pub severity: Severity,
    pub message: String,
    /// The metric values that triggered this finding (for observability).
    pub signals: HashMap<String, f64>,
}

/// Context passed into every rule evaluation.
///
/// Adapted from the reference design's `entity: &EntityRef` — CodeRadar has
/// no unified entity enum (see reference §12), so we pass the entity's id and
/// name plus its precomputed metric map. The `graph` reference is retained for
/// future rules that need graph structure beyond their own metrics.
pub struct EvalContext<'a> {
    pub entity_id: &'a str,
    pub entity_name: &'a str,
    pub metrics: &'a HashMap<String, f64>,
    pub graph: &'a ProjectedGraph,
}
