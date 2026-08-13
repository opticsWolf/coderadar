// Deeply Nested Code: excessive control-flow nesting.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct DeepNesting {
    pub nesting_threshold: usize,
}

impl Default for DeepNesting {
    fn default() -> Self {
        Self { nesting_threshold: 4 }
    }
}

impl SmellRule for DeepNesting {
    fn id(&self) -> &'static str {
        "deep-nesting"
    }
    fn scope(&self) -> Scope {
        Scope::Method
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["nesting_depth"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let depth = ctx.metrics.get("nesting_depth").copied().unwrap_or(0.0) as usize;

        if depth >= self.nesting_threshold {
            let severity = if depth >= 6 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut signals = HashMap::new();
            signals.insert("nesting_depth".to_string(), depth as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Method '{}' is nested {} levels deep - consider using guard clauses or extracting methods",
                    ctx.entity_name, depth
                ),
                signals,
            })
        } else {
            None
        }
    }
}
