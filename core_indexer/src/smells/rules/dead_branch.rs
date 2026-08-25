// CodeRadar Stage 6.2 rule — statically-decided conditions ("dead branches").
//
// Fires Medium when the CFG-upgrade pass found conditions whose truth is
// decidable from literals alone (`if False:`, `while 1 > 2`, `if flag ==
// True`). Reads the `dead_branches` signal produced by
// `const_eval::count_decided_conditions`; absent signal = nothing decided =
// no finding (same honest-degradation contract as intra-dead-statements).

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Severity, Scope};
use std::collections::HashMap;

pub struct DeadBranch;

impl SmellRule for DeadBranch {
    fn id(&self) -> &'static str {
        "dead-branch"
    }

    fn scope(&self) -> crate::smells::types::Scope {
        crate::smells::types::Scope::Method
    }

    fn signals_needed(&self) -> &'static [&'static str] {
        &["dead_branches"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let n = *ctx.metrics.get("dead_branches")? as usize;
        if n == 0 {
            return None;
        }
        let mut signals = HashMap::new();
        signals.insert("dead_branches".to_string(), n as f64);
        ctx.attach_centrality(&mut signals);
        Some(Finding {
            rule_id: self.id().to_string(),
            entity_id: ctx.entity_id.to_string(),
            severity: Severity::Medium,
            message: format!(
                "Function '{}' contains {n} statically-decided condition(s) \
                 - branch outcome is fixed regardless of inputs",
                ctx.entity_name
            ),
            signals,
        })
    }
}
