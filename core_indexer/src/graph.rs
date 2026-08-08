// CodeRadar v3.5 — CodeGraph Container (§3.4, §9.1)
// RwLock<Arc<ProjectedGraph>> with Macrame-backed persistence.
// Hybrid architecture: in-memory projected graph for structural queries,
// Macrame for agent traversals and bitemporal history.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use lru::LruCache;
use parking_lot::RwLock;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};

use crate::resolve::cache::ResolutionCache;
use crate::resolve::stack_graph::StackGraphResolver;
use crate::storage;
use crate::types::*;

// ── Import Graph (§3.4a) — EntityId-based ───────────────────────────────────

#[derive(Clone, Debug)]
pub struct ImportNode {
    pub path: PathBuf,
    pub module_id: Option<EntityId>,
    pub language: Language,
}

pub struct ImportGraph {
    graph: StableDiGraph<ImportNode, ()>,
    path_to_node: DashMap<String, NodeIndex>,
    node_to_path: DashMap<NodeIndex, String>,
    exports: DashMap<String, Vec<Export>>,
}

impl ImportGraph {
    pub fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            path_to_node: DashMap::new(),
            node_to_path: DashMap::new(),
            exports: DashMap::new(),
        }
    }

    pub fn remove_file(&mut self, file_path: &str) {
        if let Some(node) = self.path_to_node.remove(file_path) {
            let (_, old) = node;
            self.node_to_path.remove(&old);
            self.graph.remove_node(old);
        }
    }

    pub fn transitive_imports(&self, file_path: &str, max_depth: usize) -> Vec<ImportNode> {
        let path_str = file_path.to_string();
        let start = match self.path_to_node.get(&path_str) {
            Some(n) => *n,
            None => return vec![],
        };

        let mut visited = BTreeSet::new();
        let mut queue = vec![(start, 0usize)];
        let mut result = Vec::new();

        while let Some((node, depth)) = queue.pop() {
            if depth > max_depth || !visited.insert(node.index()) {
                continue;
            }
            if let Some(import_node) = self.graph.node_weight(node) {
                result.push(import_node.clone());
            }
            for neighbor in self.graph.neighbors_directed(node, petgraph::Outgoing) {
                queue.push((neighbor, depth + 1));
            }
        }
        result
    }

    pub fn add_file(
        &mut self,
        file_path: &str,
        module_id: Option<EntityId>,
        language: Language,
    ) -> NodeIndex {
        let node = ImportNode {
            path: PathBuf::from(file_path),
            module_id,
            language,
        };
        let idx = self.graph.add_node(node);
        self.path_to_node.insert(file_path.to_string(), idx);
        self.node_to_path.insert(idx, file_path.to_string());
        idx
    }

    pub fn add_import_edge(&mut self, importer: &str, imported: &str) {
        let from = self.path_to_node.get(importer);
        let to = self.path_to_node.get(imported);
        if let (Some(f), Some(t)) = (from, to) {
            self.graph.add_edge(*f, *t, ());
        }
    }

    pub fn get_exports(
        &self,
        path: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, Vec<Export>>> {
        self.exports.get(path)
    }
}

// ── Call Graph (§3.4a) — EntityId-based ─────────────────────────────────────

#[derive(Clone, Debug)]
pub struct CallNode {
    pub entity_id: EntityId,
    pub qualified_name: String,
}

#[derive(Clone, Debug)]
pub struct CallEdge {
    pub confidence: f32,
    pub resolution_method: ResolutionMethod,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
}

pub struct CallGraph {
    graph: StableDiGraph<CallNode, CallEdge>,
    path_to_node: DashMap<String, NodeIndex>,
}

impl CallGraph {
    pub fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            path_to_node: DashMap::new(),
        }
    }

    pub fn find_callers(&self, target_id: &str, max_depth: usize) -> Vec<(CallNode, usize)> {
        let target = match self.path_to_node.get(target_id) {
            Some(n) => *n,
            None => return vec![],
        };

        let mut visited = BTreeSet::new();
        let mut queue = vec![(target, 0usize)];
        let mut result = Vec::new();

        while let Some((node, depth)) = queue.pop() {
            if depth > max_depth || !visited.insert(node.index()) {
                continue;
            }
            if depth > 0 {
                if let Some(cn) = self.graph.node_weight(node) {
                    result.push((cn.clone(), depth));
                }
            }
            for neighbor in self.graph.neighbors_directed(node, petgraph::Incoming) {
                queue.push((neighbor, depth + 1));
            }
        }
        result
    }

    pub fn find_call_chain(
        &self,
        source_id: &str,
        target_id: &str,
        max_depth: usize,
    ) -> Option<Vec<CallNode>> {
        let (start, end) = match (
            self.path_to_node.get(source_id),
            self.path_to_node.get(target_id),
        ) {
            (Some(s), Some(e)) => (*s, *e),
            _ => return None,
        };

        let mut visited = BTreeSet::new();
        let mut parent: HashMap<NodeIndex, Option<NodeIndex>> = HashMap::new();
        let mut queue = vec![start];
        visited.insert(start.index());
        parent.insert(start, None);

        while let Some(node) = queue.pop() {
            if node == end {
                let mut chain = Vec::new();
                let mut current = Some(node);
                while let Some(n) = current {
                    if let Some(cn) = self.graph.node_weight(n) {
                        chain.push(cn.clone());
                    }
                    current = parent[&n];
                }
                chain.reverse();
                return Some(chain);
            }

            let current_depth = {
                let mut d = 0;
                let mut p = Some(node);
                while let Some(n) = p {
                    p = parent.get(&n).copied().flatten();
                    d += 1;
                }
                d
            };

            if current_depth >= max_depth {
                continue;
            }

            for neighbor in self.graph.neighbors_directed(node, petgraph::Outgoing) {
                if !visited.insert(neighbor.index()) {
                    continue;
                }
                parent.insert(neighbor, Some(node));
                queue.push(neighbor);
            }
        }
        None
    }
}

// ── Graph Config (§15) ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct GraphConfig {
    pub resolution: ResolutionConfig,
    pub stack_graph: StackGraphConfig,
    pub import_graph: ImportGraphConfig,
    pub signature: SignatureConfig,
    pub memory: MemoryConfig,
    pub mutation: MutationConfig,
    pub query: QueryConfig,
    pub git: GitConfig,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            resolution: ResolutionConfig::default(),
            stack_graph: StackGraphConfig::default(),
            import_graph: ImportGraphConfig::default(),
            signature: SignatureConfig::default(),
            memory: MemoryConfig::default(),
            mutation: MutationConfig::default(),
            query: QueryConfig::default(),
            git: GitConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolutionConfig {
    pub min_confidence: f32,
}
impl Default for ResolutionConfig {
    fn default() -> Self {
        Self { min_confidence: 0.3 }
    }
}

#[derive(Clone, Debug)]
pub struct StackGraphConfig {
    pub rules_dir: String,
    pub max_path_depth: usize,
    pub incremental: bool,
}
impl Default for StackGraphConfig {
    fn default() -> Self {
        Self { rules_dir: String::new(), max_path_depth: 10, incremental: true }
    }
}

#[derive(Clone, Debug)]
pub struct ImportGraphConfig {
    pub max_import_depth: usize,
    pub include_same_package: bool,
    pub max_wildcard_hops: u8,
}
impl Default for ImportGraphConfig {
    fn default() -> Self {
        Self { max_import_depth: 3, include_same_package: true, max_wildcard_hops: 3 }
    }
}

#[derive(Clone, Debug)]
pub struct SignatureConfig {
    pub min_score: f32,
    pub name_weight: f32,
    pub arity_weight: f32,
    pub proximity_weight: f32,
    /// Pattern from CodeGraph's name-matcher.ts: when a name is defined more
    /// than this many times, fuzzy resolution strategies decline to prevent
    /// near-certain-wrong edges and O(K²) blowup (vendored themes, SDK copies).
    /// Precise strategies (qualified-name, import-based) still run unaffected.
    pub ambiguous_name_ceiling: usize,
}
impl Default for SignatureConfig {
    fn default() -> Self {
        Self { min_score: 0.5, name_weight: 0.4, arity_weight: 0.3, proximity_weight: 0.3, ambiguous_name_ceiling: 500 }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryConfig {
    pub stack_graph_mb: usize,
    pub call_graph_mb: usize,
    pub resolution_cache_mb: usize,
    pub projected_graph_mb: usize,
    pub spill_compression: String,
}
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            stack_graph_mb: 60, call_graph_mb: 40, resolution_cache_mb: 20,
            projected_graph_mb: 200, spill_compression: "zstd".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MutationConfig {
    pub enabled: bool,
    pub default_dry_run: bool,
    pub max_files_per_plan: usize,
    pub max_edits_per_plan: usize,
    pub max_body_tokens: usize,
    pub backup_retention_hours: u64,
    pub post_verify: bool,
    pub max_repair_attempts: u32,
    pub require_clean_git: bool,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}
impl Default for MutationConfig {
    fn default() -> Self {
        Self {
            enabled: true, default_dry_run: true, max_files_per_plan: 100,
            max_edits_per_plan: 500, max_body_tokens: 4000,
            backup_retention_hours: 24, post_verify: true, max_repair_attempts: 3,
            require_clean_git: false,
            allow: vec!["src/".into(), "lib/".into(), "tests/".into(), "scripts/".into()],
            deny: vec![".git/".into(), ".harness/".into(), ".codegraph/".into(),
                       "/migrations/".into(), "/*.lock".into(), "/generated/".into()],
        }
    }
}

#[derive(Clone, Debug)]
pub struct QueryConfig {
    pub max_depth: usize,
    pub default_top_k: usize,
    pub cache_ttl_seconds: u64,
    pub cache_max_size: usize,
    pub use_rust_graph_for_traversal: bool,
}
impl Default for QueryConfig {
    fn default() -> Self {
        Self { max_depth: 5, default_top_k: 10, cache_ttl_seconds: 300,
               cache_max_size: 256, use_rust_graph_for_traversal: true }
    }
}

#[derive(Clone, Debug)]
pub struct GitConfig {
    pub enabled: bool,
    pub reindex_on_branch_switch: bool,
}
impl Default for GitConfig {
    fn default() -> Self {
        Self { enabled: true, reindex_on_branch_switch: true }
    }
}

// ── CodeGraph (§3.4, §9.1) — v3.5 Hybrid Architecture ──────────────────────

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
    pub stack_graph_resolver: RwLock<StackGraphResolver>,
    pub import_graph: RwLock<ImportGraph>,
    pub call_graph: RwLock<CallGraph>,

    // Resolution cache
    pub resolution_cache: RwLock<ResolutionCache>,

    // Configuration (immutable after construction)
    pub config: GraphConfig,
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
            callers_by_callee: HashMap::new(),
            callees_by_caller: HashMap::new(),
            subclasses: HashMap::new(),
            overridden_by: HashMap::new(),
        };

        Self {
            store: None, // set by caller with CodeGraph::with_store() for persistence
            projection: RwLock::new(Arc::new(projection)),
            stack_graph_resolver: RwLock::new(StackGraphResolver::new()),
            import_graph: RwLock::new(ImportGraph::new()),
            call_graph: RwLock::new(CallGraph::new()),
            resolution_cache: RwLock::new(ResolutionCache::new()),
            config,
        }
    }

    /// Take an O(1) read snapshot — one Arc clone, one atomic increment on RwLock read.
    pub fn snapshot(&self) -> Arc<ProjectedGraph> {
        self.projection.read().clone()
    }

    /// Atomically swap the projection with a new version (caller holds write lock).
    pub fn commit_projection(&self, new_projection: ProjectedGraph) {
        *self.projection.write() = Arc::new(new_projection);
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

    /// Persist extracted entities to Macrame (async via block_on).
    /// Returns the count of upserted entities.
    pub fn persist_entities(
        &self,
        units: &[crate::types::ExtractedUnit],
        file_path: &str,
        language: &str,
    ) -> Result<usize, macrame::DbError> {
        if let Some(ref store) = self.store {
            store.upsert_entities(units, file_path, language)?;
            Ok(units.len())
        } else {
            Ok(0) // no-op when no store attached
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_import_graph_add_and_find() {
        let mut g = ImportGraph::new();
        g.add_file("src/main.py", None, Language::Python);
        let imports = g.transitive_imports("src/main.py", 3);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].path, PathBuf::from("src/main.py"));
    }

    #[test]
    fn test_import_graph_remove_file() {
        let mut g = ImportGraph::new();
        g.add_file("a.py", None, Language::Python);
        g.add_file("b.py", None, Language::Python);
        g.remove_file("a.py");
        let imports = g.transitive_imports("b.py", 1);
        assert_eq!(imports.len(), 1);
    }

    #[test]
    fn test_import_graph_transitive() {
        let mut g = ImportGraph::new();
        g.add_file("a.py", None, Language::Python);
        g.add_file("b.py", None, Language::Python);
        g.add_file("c.py", None, Language::Python);
        g.add_import_edge("a.py", "b.py");
        g.add_import_edge("b.py", "c.py");
        let depth2 = g.transitive_imports("a.py", 2);
        assert!(depth2.len() >= 2);
    }

    #[test]
    fn test_import_graph_nonexistent() {
        let g = ImportGraph::new();
        assert!(g.transitive_imports("nope.py", 3).is_empty());
    }

    fn make_call_node(g: &mut CallGraph, id: &str) -> NodeIndex {
        if let Some(existing) = g.path_to_node.get(id) {
            return *existing;
        }
        let idx = g.graph.add_node(CallNode {
            entity_id: id.into(),
            qualified_name: format!("mod.{}", id),
        });
        g.path_to_node.insert(id.into(), idx);
        idx
    }

    fn make_call_edge(g: &mut CallGraph, from: &str, to: &str) {
        let a = make_call_node(g, from);
        let b = make_call_node(g, to);
        g.graph.add_edge(a, b, CallEdge {
            confidence: 0.95,
            resolution_method: ResolutionMethod::StackGraph,
            call_site_span: ByteSpan { start: 0, end: 1 },
            args_span: None,
        });
    }

    #[test]
    fn test_call_graph_find_callers() {
        let mut g = CallGraph::new();
        make_call_edge(&mut g, "a", "b");
        let callers = g.find_callers("b", 5);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].0.entity_id, "a");
    }

    #[test]
    fn test_call_graph_chain() {
        let mut g = CallGraph::new();
        make_call_edge(&mut g, "a", "b");
        make_call_edge(&mut g, "b", "c");
        let chain = g.find_call_chain("a", "c", 5);
        assert!(chain.is_some());
        let chain = chain.unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].entity_id, "a");
        assert_eq!(chain[2].entity_id, "c");
    }

    #[test]
    fn test_call_graph_cycle_safe() {
        let mut g = CallGraph::new();
        make_call_edge(&mut g, "a", "b");
        make_call_edge(&mut g, "b", "a");
        let callers = g.find_callers("a", 10);
        assert_eq!(callers.len(), 1);
    }

    #[test]
    fn test_codegraph_snapshot() {
        let graph = CodeGraph::new(GraphConfig::default());
        let snap = graph.snapshot();
        assert!(snap.modules.is_empty());
        assert!(snap.functions.is_empty());
    }

    #[test]
    fn test_codegraph_callers_of_empty() {
        let graph = CodeGraph::new(GraphConfig::default());
        assert!(graph.callers_of("nonexistent").is_empty());
    }
}
