// Native Rust code-smell engine (Phase 4 of the `traverse_smell` branch).
//
// Design reference: `docs/smell-rules-reference.md`.
//
// Pipeline (per §1 of the reference):
//   1. Metrics pass      — AST metrics computed in `extract/single_pass.rs`
//                          via `metrics::compute_function_metrics`, carried on
//                          `Function.metrics`.
//   2. Rule definition   — structs implementing `rule::SmellRule`.
//   3. Engine execution  — `engine::SmellEngine::run` iterates the resolved
//                          `ProjectedGraph`, filters by scope, evaluates rules.
//   4. Annotation        — `engine::SmellRegistry` indexes findings by entity
//                          and rule id; `lib.rs::get_smells` formats for Python.
//
// Class-level roll-ups (WMC, max_method_cyclomatic, CBO) and the trivial
// signals (LOC, param_count, field_count) are computed on the fly in
// `engine.rs` from the resolved graph — no re-parse of source is required.

pub mod engine;
pub mod metrics;
pub mod rule;
pub mod rules;
pub mod types;
