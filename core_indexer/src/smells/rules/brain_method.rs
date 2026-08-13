// Brain Method: one method in a class centralizes almost all the logic.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct BrainMethod {
    pub max_method_cyclo_threshold: usize,
    pub min_class_wmc: usize,
}

impl Default for BrainMethod {
    fn default() -> Self {
        Self { max_method_cyclo_threshold: 15, min_class_wmc: 20 }
    }
}

impl SmellRule for BrainMethod {
    fn id(&self) -> &'static str {
        "brain-method"
    }
    fn scope(&self) -> Scope {
        Scope::Class
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["max_method_cyclomatic", "WMC"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let max_cyclo = ctx
            .metrics
            .get("max_method_cyclomatic")
            .copied()
            .unwrap_or(0.0) as usize;
        let wmc = ctx.metrics.get("WMC").copied().unwrap_or(0.0) as usize;

        if max_cyclo >= self.max_method_cyclo_threshold && wmc >= self.min_class_wmc {
            let severity = if max_cyclo >= 30 {
                Severity::Critical
            } else {
                Severity::High
            };

            let mut signals = HashMap::new();
            signals.insert("max_method_cyclomatic".to_string(), max_cyclo as f64);
            signals.insert("WMC".to_string(), wmc as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Class '{}' likely contains a Brain Method (Max Method Cyclomatic={}, WMC={}) - logic is too centralized",
                    ctx.entity_name, max_cyclo, wmc
                ),
                signals,
            })
        } else {
            None
        }
    }
}
