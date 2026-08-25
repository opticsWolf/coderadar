// Excessive Returns: fragmented control flow via too many exit points.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct ExcessiveReturns {
    pub return_threshold: usize,
}

impl Default for ExcessiveReturns {
    fn default() -> Self {
        Self { return_threshold: 5 }
    }
}

impl SmellRule for ExcessiveReturns {
    fn id(&self) -> &'static str {
        "excessive-returns"
    }
    fn scope(&self) -> Scope {
        Scope::Method
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["return_count"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let returns = ctx.metrics.get("return_count").copied().unwrap_or(0.0) as usize;

        if returns > ctx.strictness.scale(self.return_threshold) {
            let severity = if returns >= 8 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut signals = HashMap::new();
            signals.insert("return_count".to_string(), returns as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Method '{}' has {} return statements - consider consolidating control flow",
                    ctx.entity_name, returns
                ),
                signals,
            })
        } else {
            None
        }
    }
}
