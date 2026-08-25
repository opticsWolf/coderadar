// CodeRadar Stage 4 — intra-procedural dead statements.
//
// "Statements after return/raise/throw in the same function." Only possible
// with CFG data: the signal is emitted by the engine's strangler refinement
// (engine.rs, gated on `analysis.use_cfg_metrics`). When the CFG was not
// built this run the signal is absent and the rule degrades honestly —
// no finding, never a guess.

use std::collections::HashMap;

use crate::smells::rule::SmellRule;
use crate::smells::types::{EvalContext, Finding, Scope, Severity};

pub struct IntraDeadStatements;

impl SmellRule for IntraDeadStatements {
    fn id(&self) -> &'static str {
        "intra-dead-statements"
    }
    fn scope(&self) -> Scope {
        Scope::Method
    }
    fn signals_needed(&self) -> &'static [&'static str] {
        &["unreachable_blocks"]
    }

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let n = ctx.metrics.get("unreachable_blocks").copied()? as usize;
        if n == 0 {
            return None;
        }
        Some(Finding {
            rule_id: self.id().into(),
            entity_id: ctx.entity_id.into(),
            severity: Severity::Medium,
            message: format!(
                "{n} unreachable basic block(s) — code after return/raise? It can never execute."
            ),
            signals: HashMap::from([("unreachable_blocks".to_string(), n as f64)]),
        })
    }
}
