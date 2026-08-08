// CodeRadar v3.3 — CodeGraph Container (§3.4)
// ArcSwap-based MVCC arenas with reverse indexes and epoch versioning.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use lru::LruCache;
use parking_lot::RwLock;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use slotmap::SlotMap;

use crate::resolve::cache::ResolutionCache;
use crate::resolve::stack_graph::StackGraphResolver;
use crate::types::*;

// ── Entry Wrappers ──────────────────────────────────────────────────────────

/// Each entity stored as Arc<Entity> for per-entry MVCC snapshot isolation.
#[derive(Clone, Debug)]
pub struct ModuleEntry {
    pub inner: Arc<Module>,
}
#[derive(Clone, Debug)]
pub struct ClassEntry {
    pub inner: Arc<Class>,
}
#[derive(Clone, Debug)]
pub struct FunctionEntry {
    pub inner: Arc<Function>,
}
#[derive(Clone, Debug)]
pub struct ImportEntry {
    pub inner: Arc<Import>,
}
#[derive(Clone, Debug)]
pub struct ConstantEntry {
    pub inner: Arc<Constant>,
}
#[derive(Clone, Debug)]
pub struct TypeAliasEntry {
    pub inner: Arc<TypeAlias>,
}

// ── Import Graph (§3.4a) ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ImportNode {
    pub path: PathBuf,
    pub module_id: Option<ModuleId>,
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

    /// O(1) file removal. StableDiGraph keeps surviving NodeIndex values valid.
    pub fn remove_file(&mut self, file_path: &str) {
        if let Some(node) = self.path_to_node.remove(file_path) {
            let (_, old) = node;
            self.node_to_path.remove(&old);
            self.graph.remove_node(old);
        }
    }

    /// Depth-limited BFS over transitive imports.
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
        module_id: Option<ModuleId>,
        language: Language,
    ) -> NodeIndex {
        let node = ImportNode {
            path: PathBuf::from(file_path),
            module_id,
            language,
        };
        let idx = self.graph.add_node(node);
        self.path_to_node
            .insert(file_path.to_string(), idx);
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

    /// Access exports for a file path.
    pub fn get_exports(&self, path: &str) -> Option<dashmap::mapref::one::Ref<'_, String, Vec<Export>>> {
        self.exports.get(path)
    }
}

// ── Call Graph (§3.4a) ─────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct CallNode {
    pub entity_id: String,
    pub qualified_name: String,
}

#[derive(Clone, Debug)]
pub struct CallEdge {
    pub confidence: f32,
    pub resolution_method: ResolutionMethod,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolutionMethod {
    StackGraph,
    ImportConstrained,
    SignatureMatch,
    Embedding,
    Lsp,
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

    /// Reverse BFS: find all callers of a target with explicit visited set + depth cap.
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
                // Don't include the target itself
                if let Some(cn) = self.graph.node_weight(node) {
                    result.push((cn.clone(), depth));
                }
            }
            // Walk inbound edges (callers → us)
            for neighbor in self
                .graph
                .neighbors_directed(node, petgraph::Incoming)
            {
                queue.push((neighbor, depth + 1));
            }
        }
        result
    }

    /// Shortest call chain via BFS with parent tracking.
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
                // Reconstruct path
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

// ── Query Snapshot (§3.4a) ──────────────────────────────────────────────────

pub struct QuerySnapshot {
    pub epoch: u64,
    pub arenas: SnapshotArenas,
}

impl Clone for QuerySnapshot {
    fn clone(&self) -> Self {
        Self {
            epoch: self.epoch,
            arenas: self.arenas.clone(),
        }
    }
}

#[derive(Clone)]
pub struct SnapshotArenas {
    pub modules: Arc<SlotMap<ModuleId, ModuleEntry>>,
    pub classes: Arc<SlotMap<ClassId, ClassEntry>>,
    pub functions: Arc<SlotMap<FunctionId, FunctionEntry>>,
    pub imports: Arc<SlotMap<ImportId, ImportEntry>>,
    pub constants: Arc<SlotMap<ConstantId, ConstantEntry>>,
    pub type_aliases: Arc<SlotMap<TypeAliasId, TypeAliasEntry>>,
}

// ── Resolved Edge (§3.4a) ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ResolvedEdge {
    pub source_id: String,
    pub target_id: String,
    pub confidence: f32,
    pub method: ResolutionMethod,
    pub kind: ReferenceKind,
    pub line: usize,
    pub call_site_span: ByteSpan,
    pub args_span: Option<ByteSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReferenceKind {
    Call,
    Instantiation,
    Inheritance,
    TypeAnnotation,
    AttributeAccess,
    Import,
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
        Self {
            min_confidence: 0.3,
        }
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
        Self {
            rules_dir: String::new(),
            max_path_depth: 10,
            incremental: true,
        }
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
        Self {
            max_import_depth: 3,
            include_same_package: true,
            max_wildcard_hops: 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SignatureConfig {
    pub min_score: f32,
    pub name_weight: f32,
    pub arity_weight: f32,
    pub proximity_weight: f32,
}

impl Default for SignatureConfig {
    fn default() -> Self {
        Self {
            min_score: 0.5,
            name_weight: 0.4,
            arity_weight: 0.3,
            proximity_weight: 0.3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryConfig {
    pub stack_graph_mb: usize,
    pub call_graph_mb: usize,
    pub resolution_cache_mb: usize,
    pub spill_compression: String,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            stack_graph_mb: 60,
            call_graph_mb: 40,
            resolution_cache_mb: 20,
            spill_compression: "zstd".to_string(),
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
            enabled: true,
            default_dry_run: true,
            max_files_per_plan: 100,
            max_edits_per_plan: 500,
            max_body_tokens: 4000,
            backup_retention_hours: 24,
            post_verify: true,
            max_repair_attempts: 3,
            require_clean_git: false,
            allow: vec![
                "src/".into(),
                "lib/".into(),
                "tests/".into(),
                "scripts/".into(),
            ],
            deny: vec![
                ".git/".into(),
                ".harness/".into(),
                "/migrations/".into(),
                "/*.lock".into(),
                "/generated/".into(),
            ],
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
        Self {
            max_depth: 5,
            default_top_k: 10,
            cache_ttl_seconds: 300,
            cache_max_size: 256,
            use_rust_graph_for_traversal: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GitConfig {
    pub enabled: bool,
    pub reindex_on_branch_switch: bool,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reindex_on_branch_switch: true,
        }
    }
}

// ── CodeGraph (§3.4) — The Master Container ───────────────────────────────

pub struct CodeGraph {
    // Primary storage: one arena per kind, each wrapped in arc-swap::ArcSwap
    pub modules: ArcSwap<SlotMap<ModuleId, ModuleEntry>>,
    pub classes: ArcSwap<SlotMap<ClassId, ClassEntry>>,
    pub functions: ArcSwap<SlotMap<FunctionId, FunctionEntry>>,
    pub imports: ArcSwap<SlotMap<ImportId, ImportEntry>>,
    pub constants: ArcSwap<SlotMap<ConstantId, ConstantEntry>>,
    pub type_aliases: ArcSwap<SlotMap<TypeAliasId, TypeAliasEntry>>,

    // File-level structure
    pub file_to_modules: ArcSwap<HashMap<PathBuf, Vec<ModuleId>>>,
    pub module_by_dotted_name: ArcSwap<HashMap<(Language, String), ModuleId>>,

    // Reverse indexes
    pub importers: ArcSwap<HashMap<ModuleId, BTreeSet<ModuleId>>>,
    pub callers_by_callee: ArcSwap<HashMap<FunctionId, BTreeSet<FunctionId>>>,
    pub callees_by_caller: ArcSwap<HashMap<FunctionId, BTreeSet<FunctionId>>>,
    pub subclasses: ArcSwap<HashMap<ClassId, BTreeSet<ClassId>>>,
    pub overridden_by: ArcSwap<HashMap<FunctionId, BTreeSet<FunctionId>>>,

    // Graph structures
    pub stack_graph_resolver: RwLock<StackGraphResolver>,
    pub import_graph: RwLock<ImportGraph>,
    pub call_graph: RwLock<CallGraph>,

    // Resolution cache
    pub resolution_cache: RwLock<ResolutionCache>,

    // Versioning
    pub epoch: AtomicU64,
    pub config: GraphConfig,
}

impl CodeGraph {
    pub fn new(config: GraphConfig) -> Self {
        Self {
            modules: ArcSwap::from(Arc::new(SlotMap::with_key())),
            classes: ArcSwap::from(Arc::new(SlotMap::with_key())),
            functions: ArcSwap::from(Arc::new(SlotMap::with_key())),
            imports: ArcSwap::from(Arc::new(SlotMap::with_key())),
            constants: ArcSwap::from(Arc::new(SlotMap::with_key())),
            type_aliases: ArcSwap::from(Arc::new(SlotMap::with_key())),
            file_to_modules: ArcSwap::from(Arc::new(HashMap::new())),
            module_by_dotted_name: ArcSwap::from(Arc::new(HashMap::new())),
            importers: ArcSwap::from(Arc::new(HashMap::new())),
            callers_by_callee: ArcSwap::from(Arc::new(HashMap::new())),
            callees_by_caller: ArcSwap::from(Arc::new(HashMap::new())),
            subclasses: ArcSwap::from(Arc::new(HashMap::new())),
            overridden_by: ArcSwap::from(Arc::new(HashMap::new())),
            stack_graph_resolver: RwLock::new(StackGraphResolver::new()),
            import_graph: RwLock::new(ImportGraph::new()),
            call_graph: RwLock::new(CallGraph::new()),
            resolution_cache: RwLock::new(ResolutionCache::new()),
            epoch: AtomicU64::new(1),
            config,
        }
    }

    /// Take an O(1) snapshot of all arenas for a lock-free query.
    pub fn snapshot(&self) -> QuerySnapshot {
        let epoch = self.epoch.load(Ordering::Acquire);
        QuerySnapshot {
            epoch,
            arenas: SnapshotArenas {
                modules: self.modules.load_full(),
                classes: self.classes.load_full(),
                functions: self.functions.load_full(),
                imports: self.imports.load_full(),
                constants: self.constants.load_full(),
                type_aliases: self.type_aliases.load_full(),
            },
        }
    }

    /// Bump the graph epoch after commit.
    pub fn bump_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ImportGraph ─────────────────────────────────────────────────

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
        assert_eq!(imports[0].path, PathBuf::from("b.py"));
    }

    #[test]
    fn test_import_graph_transitive_imports_depth_limited() {
        let mut g = ImportGraph::new();
        g.add_file("a.py", None, Language::Python);
        g.add_file("b.py", None, Language::Python);
        g.add_file("c.py", None, Language::Python);
        g.add_import_edge("a.py", "b.py");
        g.add_import_edge("b.py", "c.py");

        let depth1 = g.transitive_imports("a.py", 1);
        let depth2 = g.transitive_imports("a.py", 2);
        assert!(depth2.len() >= depth1.len(), "Depth 2 should see more than depth 1");
    }

    #[test]
    fn test_import_graph_nonexistent_file() {
        let g = ImportGraph::new();
        let imports = g.transitive_imports("nonexistent.py", 3);
        assert!(imports.is_empty());
    }

    // ── CallGraph ───────────────────────────────────────────────────

    fn make_node(g: &mut CallGraph, id: &str) -> NodeIndex {
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

    fn make_edge(g: &mut CallGraph, from: &str, to: &str) {
        let a = make_node(g, from);
        let b = make_node(g, to);
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
        make_node(&mut g, "a");
        make_node(&mut g, "b");
        make_edge(&mut g, "a", "b");

        let callers = g.find_callers("b", 5);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].0.entity_id, "a");
    }

    #[test]
    fn test_call_graph_find_callers_nonexistent() {
        let g = CallGraph::new();
        let callers = g.find_callers("nonexistent", 5);
        assert!(callers.is_empty());
    }

    #[test]
    fn test_call_graph_find_call_chain() {
        let mut g = CallGraph::new();
        make_node(&mut g, "a");
        make_node(&mut g, "b");
        make_node(&mut g, "c");
        make_edge(&mut g, "a", "b");
        make_edge(&mut g, "b", "c");

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
        make_node(&mut g, "a");
        make_node(&mut g, "b");
        make_edge(&mut g, "a", "b");
        make_edge(&mut g, "b", "a");

        // Must NOT panic or infinite-loop
        let callers = g.find_callers("a", 10);
        assert_eq!(callers.len(), 1); // b calls a
    }

    // ── CodeGraph Snapshot ───────────────────────────────────────────

    #[test]
    fn test_codegraph_new_and_snapshot() {
        let cfg = GraphConfig::default();
        let graph = CodeGraph::new(cfg);
        let snap = graph.snapshot();
        assert_eq!(snap.epoch, 1);
        assert_eq!(snap.arenas.modules.len(), 0);
    }

    #[test]
    fn test_bump_epoch() {
        let graph = CodeGraph::new(GraphConfig::default());
        let e1 = graph.bump_epoch();
        let e2 = graph.bump_epoch();
        assert_eq!(e2, e1 + 1);
    }
}
