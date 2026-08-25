// CodeRadar Stage 1.4 — dead-code as a registered smell rule.
//
// Reads the precomputed reachability set from `ctx.analyses` (Stage 0.2
// plumbing) instead of walking the graph per call. If the analysis was not
// computed this run it degrades honestly: no finding, never a guess.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct DeadCode;

impl SmellRule for DeadCode {
    fn id(&self) -> &'static str {
        "dead-code"
    }
    fn scope(&self) -> Scope {
        Scope::Method
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &[] // reads ctx.analyses
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let reachable = ctx.analyses.reachable?;
        if reachable.contains(ctx.entity_id) {
            return None;
        }
        Some(Finding {
            rule_id: self.id().into(),
            entity_id: ctx.entity_id.into(),
            severity: Severity::High,
            message: format!(
                "'{}' is unreachable from any entry point — verify with `affected` before removing",
                ctx.entity_name
            ),
            signals: HashMap::from([("reachable".to_string(), 0.0)]),
        })
    }
}
