// CodeRadar — Resolution Module
// Cascade: L1 Import Graph → L2 Signature Match → L3 Embedding / L4 LSP
// (both Python-side, deferred). The Stack Graphs placeholder layer is gone;
// layers are numbered as they run (see orchestrator.rs header).
pub mod cache;
pub mod import_graph;
pub mod orchestrator;
pub mod signature;

/// A reference parsed out of a source file, waiting for the cascade.
///
/// Declared in the `stack_graph` placeholder module until that was retired;
/// it is the input type of `ResolutionOrchestrator::resolve_file` and has
/// nothing to do with Stack Graphs.
#[derive(Clone, Debug)]
pub struct ParsedReference {
    pub name: String,
    pub file_path: String,
    pub line: u32,
    pub column: u32,
    pub kind: crate::types::ReferenceKind,
}
