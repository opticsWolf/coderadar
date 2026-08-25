// High Cyclomatic Complexity: too many independent paths in one method.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

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
    fn id(&self) -> &'static str {
        "high-cyclomatic-complexity"
    }
    fn scope(&self) -> Scope {
        Scope::Method
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["cyclomatic"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let cyclo = ctx.metrics.get("cyclomatic").copied().unwrap_or(0.0) as usize;

        let warning_limit = ctx.strictness.scale(self.warning_threshold);
        let critical_limit = ctx.strictness.scale(self.critical_threshold);
        if cyclo >= warning_limit {
            let severity = if cyclo >= critical_limit {
                Severity::Critical
            } else {
                Severity::High
            };

            let mut signals = HashMap::new();
            signals.insert("cyclomatic".to_string(), cyclo as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Method '{}' has high cyclomatic complexity ({}) - too many branching paths",
                    ctx.entity_name, cyclo
                ),
                signals,
            })
        } else {
            None
        }
    }
}
