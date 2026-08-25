// Data Class: lots of fields, almost no logic (low WMC).

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct DataClass {
    pub field_threshold: usize,
    pub max_wmc_threshold: usize,
}

impl Default for DataClass {
    fn default() -> Self {
        Self { field_threshold: 5, max_wmc_threshold: 10 }
    }
}

impl SmellRule for DataClass {
    fn id(&self) -> &'static str {
        "data-class"
    }
    fn scope(&self) -> Scope {
        Scope::Class
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["field_count", "WMC"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let fields = ctx.metrics.get("field_count").copied().unwrap_or(0.0) as usize;
        let wmc = ctx.metrics.get("WMC").copied().unwrap_or(0.0) as usize;

        // Trigger if it has a lot of fields but very little logic.
        // max_wmc is an upper bound: scaling it down under `strict` narrows
        // the "little logic" escape hatch — same widening direction as every
        // other limit under the shared factor().
        if fields >= ctx.strictness.scale(self.field_threshold)
            && wmc <= ctx.strictness.scale(self.max_wmc_threshold)
        {
            let severity = if fields >= 10 && wmc <= 5 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut signals = HashMap::new();
            signals.insert("field_count".to_string(), fields as f64);
            signals.insert("WMC".to_string(), wmc as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Class '{}' is a Data Class (Fields={}, WMC={}) - consider moving behavior into the class",
                    ctx.entity_name, fields, wmc
                ),
                signals,
            })
        } else {
            None
        }
    }
}
