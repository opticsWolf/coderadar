// CodeRadar Stage 1.4 — dead-code as a registered smell rule.
//
// Reads the precomputed reachability set from `ctx.analyses` (Stage 0.2
// plumbing) instead of walking the graph per call. If the analysis was not
// computed this run it degrades honestly: no finding, never a guess.

use std::collections::HashMap;

use crate::scoring::Tier;
use crate::graph::deadcode::DeadKind;
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
        let found = ctx.analyses.dead?.get(ctx.entity_id)?;
        let severity = match found.tier {
            Tier::Certain | Tier::High => Severity::High,
            Tier::Medium => Severity::Medium,
            Tier::Low | Tier::Speculative => Severity::Info,
        };
        Some(Finding {
            rule_id: self.id().into(),
            entity_id: ctx.entity_id.into(),
            severity,
            message: format!(
                "'{}' is {} — verify with `affected` before removing",
                ctx.entity_name,
                match found.kind {
                    DeadKind::Unreachable => "unreachable from any entry point",
                    DeadKind::TransitivelyDead => "only called from dead code",
                    DeadKind::TestOnly => "only used by tests",
                    DeadKind::RtaDead => "live only through a never-constructed class",
                }
            ),
            signals: HashMap::from([
                ("reachable".to_string(), 0.0),
                ("score".to_string(), found.score as f64),
                ("removable_lines".to_string(), found.removable_lines as f64),
            ]),
        })
    }
}
