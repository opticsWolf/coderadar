// God Class: a class that centralizes too much weight (WMC) AND couples to too
// many other classes (CBO).

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

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
    fn id(&self) -> &'static str {
        "god-class"
    }
    fn scope(&self) -> Scope {
        Scope::Class
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["WMC", "CBO"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let wmc = ctx.metrics.get("WMC").copied().unwrap_or(0.0) as usize;
        let cbo = ctx.metrics.get("CBO").copied().unwrap_or(0.0) as usize;

        // Both limits scale with strictness; for the CBO floor, lowering it
        // under `strict` widens the net exactly like lowering WMC does.
        let wmc_limit = ctx.strictness.scale(self.wmc_threshold);
        let cbo_limit = ctx.strictness.scale(self.cbo_threshold);
        if wmc >= wmc_limit && cbo >= cbo_limit {
            let severity = if wmc >= 100 {
                Severity::Critical
            } else if wmc >= 70 {
                Severity::High
            } else {
                Severity::Medium
            };

            let mut signals = HashMap::new();
            signals.insert("WMC".to_string(), wmc as f64);
            signals.insert("CBO".to_string(), cbo as f64);
            ctx.attach_centrality(&mut signals);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Class '{}' has WMC={} and CBO={} - likely God Class",
                    ctx.entity_name, wmc, cbo
                ),
                signals,
            })
        } else {
            None
        }
    }
}
