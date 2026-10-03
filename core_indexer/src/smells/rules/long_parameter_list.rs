// Long Parameter List: too many arguments.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct LongParameterList {
    pub param_threshold: usize,
}

impl Default for LongParameterList {
    fn default() -> Self {
        Self { param_threshold: 4 }
    }
}

impl SmellRule for LongParameterList {
    fn id(&self) -> &'static str {
        "long-parameter-list"
    }
    fn scope(&self) -> Scope {
        Scope::Method
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["param_count", "positional_param_count"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        // Positional-or-keyword only: keyword-only parameters are the
        // documented remedy for this smell (plan §6.4), so counting them
        // reported the fix as the problem.
        let params = ctx
            .metrics
            .get("positional_param_count")
            .copied()
            .unwrap_or_else(|| ctx.metrics.get("param_count").copied().unwrap_or(0.0))
            as usize;
        let total = ctx.metrics.get("param_count").copied().unwrap_or(0.0) as usize;

        if params > ctx.strictness.scale(self.param_threshold) {
            let severity = if params >= 7 {
                Severity::High
            } else if params >= 6 {
                Severity::Medium
            } else {
                Severity::Info
            };

            let mut signals = HashMap::new();
            signals.insert("param_count".to_string(), params as f64);
            signals.insert("total_param_count".to_string(), total as f64);

            Some(Finding {
                rule_id: self.id().to_string(),
                entity_id: ctx.entity_id.to_string(),
                severity,
                message: format!(
                    "Method '{}' has {} positional parameters{} - consider grouping into an object",
                    ctx.entity_name,
                    params,
                    if total > params {
                        format!(" ({total} total, {} keyword-only)", total - params)
                    } else {
                        String::new()
                    }
                ),
                signals,
            })
        } else {
            None
        }
    }
}
