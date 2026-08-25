// Long Method: too long by LOC or cyclomatic complexity.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

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
    fn id(&self) -> &'static str {
        "long-method"
    }
    fn scope(&self) -> Scope {
        Scope::Method
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["LOC", "cyclomatic"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let loc = ctx.metrics.get("LOC").copied().unwrap_or(0.0) as usize;
        let cyclo = ctx.metrics.get("cyclomatic").copied().unwrap_or(0.0) as usize;

        // Trigger if EITHER threshold is breached. Thresholds scale with the
        // run's strictness (Stage 0.4); severity bands stay fixed —
        // strictness decides WHAT is reported, severity HOW BAD it is.
        let loc_limit = ctx.strictness.scale(self.loc_threshold);
        let cyclo_limit = ctx.strictness.scale(self.cyclomatic_threshold);
        if loc >= loc_limit || cyclo >= cyclo_limit {
            let severity = if loc >= 100 || cyclo >= 20 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut signals = HashMap::new();
            signals.insert("LOC".to_string(), loc as f64);
            signals.insert("cyclomatic".to_string(), cyclo as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Method '{}' is too long (LOC={}, Cyclomatic={}) - consider extracting",
                    ctx.entity_name, loc, cyclo
                ),
                signals,
            })
        } else {
            None
        }
    }
}
