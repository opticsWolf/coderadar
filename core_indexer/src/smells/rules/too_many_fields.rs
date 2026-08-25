// Too Many Fields: a class accumulating too much state (SRP violation).

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct TooManyFields {
    pub field_threshold: usize,
}

impl Default for TooManyFields {
    fn default() -> Self {
        Self { field_threshold: 10 }
    }
}

impl SmellRule for TooManyFields {
    fn id(&self) -> &'static str {
        "too-many-fields"
    }
    fn scope(&self) -> Scope {
        Scope::Class
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["field_count"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let fields = ctx.metrics.get("field_count").copied().unwrap_or(0.0) as usize;

        if fields >= ctx.strictness.scale(self.field_threshold) {
            let severity = if fields >= 20 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut signals = HashMap::new();
            signals.insert("field_count".to_string(), fields as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Class '{}' has {} fields - possible violation of Single Responsibility Principle",
                    ctx.entity_name, fields
                ),
                signals,
            })
        } else {
            None
        }
    }
}
