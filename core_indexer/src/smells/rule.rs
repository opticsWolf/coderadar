// The rule abstraction (§4 of the reference).

use super::types::{EvalContext, Finding, Scope};

pub trait SmellRule: Send + Sync {
    /// The unique identifier for the smell (e.g. "god-class").
    fn id(&self) -> &'static str;

    /// The entity scope this rule applies to.
    fn scope(&self) -> Scope;

    /// The metric keys this rule depends on (for future caching/parallelism).
    fn signals_needed(&self) -> &'static [&'static str];

    /// Evaluates the entity in context. Returns `Some(Finding)` if the smell
    /// is detected.
    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding>;
}
