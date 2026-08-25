// CodeRadar v3.6 — CodeGraph Container (§3.4, §9.1)
// RwLock<Arc<ProjectedGraph>> with Macrame-backed persistence.
// Hybrid architecture: in-memory projected graph for structural queries,
// Macrame for agent traversals and bitemporal history.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::resolve::cache::ResolutionCache;
use crate::types::*;

pub mod cfg;
pub mod config;
pub use config::*;

pub mod deadcode;

pub mod import_graph;

pub use import_graph::{ImportGraph, ImportNode};

pub mod call_graph;

pub use call_graph::{CallEdge, CallGraph, CallNode};

pub mod module_resolution;

// Re-exported for crate-internal callers (and the test module); production
// code imports directly from `module_resolution`, so this is only used under
// cfg(test).
#[allow(unused_imports)]
pub(crate) use module_resolution::find_module_by_dotted_name;

pub mod mro;

pub mod inheritance;

pub mod traversal;

pub mod resolve_calls;

pub mod persistence;

pub mod embeddings;

pub mod indexing;


pub mod projection_ops;

// ── CodeGraph (§3.4, §9.1) — v3.6 Hybrid Architecture ──────────────────────

pub struct CodeGraph {
    /// Macrame bitemporal graph store — source of truth for all entities and edges.
    /// Opened once at CodeGraph construction, lives for the process lifetime.
    /// Optional: None when running without persistence (tests, embedded mode).
    pub store: Option<Arc<crate::storage::CodeGraphStore>>,

    /// The current projected graph, behind an RwLock.
    /// Reads clone the Arc (one atomic increment); writes build a new
    /// ProjectedGraph and swap the Arc.
    pub projection: RwLock<Arc<ProjectedGraph>>,

    // Graph structures (separate locks — not part of projection)
    pub import_graph: RwLock<ImportGraph>,
    pub call_graph: RwLock<CallGraph>,

    // Resolution cache
    pub resolution_cache: RwLock<ResolutionCache>,

    // Configuration (immutable after construction)
    pub config: GraphConfig,

    /// Unix seconds of the last committed projection. See `commit_projection`.
    pub indexed_at: RwLock<f64>,
}

impl CodeGraph {
    pub fn new(config: GraphConfig) -> Self {
        let projection = ProjectedGraph {
            modules: HashMap::new(),
            classes: HashMap::new(),
            functions: HashMap::new(),
            imports: HashMap::new(),
            constants: HashMap::new(),
            type_aliases: HashMap::new(),
            file_to_modules: HashMap::new(),
            module_by_dotted_name: HashMap::new(),
            importers: HashMap::new(),
            imports_by_importer: HashMap::new(),
            callers_by_callee: HashMap::new(),
            callees_by_caller: HashMap::new(),
            subclasses: HashMap::new(),
            overridden_by: HashMap::new(),
            overrides_base: HashMap::new(),
            ambiguous_bases: Vec::new(),
        };

        Self {
            store: None, // set by caller with CodeGraph::with_store() for persistence
            projection: RwLock::new(Arc::new(projection)),
            import_graph: RwLock::new(ImportGraph::new()),
            call_graph: RwLock::new(CallGraph::new()),
            resolution_cache: RwLock::new(ResolutionCache::new()),
            config,
            indexed_at: RwLock::new(0.0),
        }
    }

    /// Take an O(1) read snapshot — one Arc clone, one atomic increment on RwLock read.
    pub fn snapshot(&self) -> Arc<ProjectedGraph> {
        self.projection.read().clone()
    }

    /// Atomically swap the projection with a new version (caller holds write lock).
    pub fn commit_projection(&self, new_projection: ProjectedGraph) {
        *self.projection.write() = Arc::new(new_projection);
        // Unix seconds of the last commit. `graph_stats()` hands this to the
        // MCP layer, whose staleness banner compares it against file mtimes;
        // it used to read a key named "epoch" that nothing ever set, so every
        // banner in the server was unreachable.
        *self.indexed_at.write() = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
    }

    /// Unix seconds of the last `commit_projection`, or 0.0 before the first.
    pub fn indexed_at(&self) -> f64 {
        *self.indexed_at.read()
    }

    // ── Entity access helpers ──────────────────────────────────────────

    /// Look up a function by entity ID in the current snapshot.
    pub fn get_function(&self, id: &str) -> Option<Arc<Function>> {
        self.snapshot().functions.get(id).cloned()
    }

    /// Look up a class by entity ID.
    pub fn get_class(&self, id: &str) -> Option<Arc<Class>> {
        self.snapshot().classes.get(id).cloned()
    }

    /// Look up a module by entity ID.
    pub fn get_module(&self, id: &str) -> Option<Arc<Module>> {
        self.snapshot().modules.get(id).cloned()
    }

    /// List callers of a function from the reverse index.
    pub fn callers_of(&self, id: &str) -> Vec<EntityId> {
        self.snapshot()
            .callers_by_callee
            .get(id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// List callees from the reverse index.
    pub fn callees_of(&self, id: &str) -> Vec<EntityId> {
        self.snapshot()
            .callees_by_caller
            .get(id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    // ── Macrame Store Integration (§10) ────────────────────────────────────

    /// Attach a Macrame store for persistence. Called once after construction.
    pub fn with_store(mut self, store: crate::storage::CodeGraphStore) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// True if a persistent Macrame store is attached (not test/embedded mode).
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;