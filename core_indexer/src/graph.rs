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
use crate::extract;

/// Normalize a file path string: convert backslashes to forward slashes,
/// strip leading ./ or .\ for consistent keying.
fn normalize_path_str(p: &str) -> String {
    let s = p.trim_start_matches("./").trim_start_matches(".\\");
    s.replace('\\', "/")
}

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

/// Find a module by its dotted Python name (e.g., "coderadar.config" → config.py).
/// Converts the dotted name to path segments and matches against suffixes of
/// all module file paths. Also builds a reverse lookup for common patterns.
fn find_module_by_dotted_name(
    projection: &ProjectedGraph,
    dotted_name: &str,
    _current_module: &str,
) -> Option<String> {
    let segments: Vec<&str> = dotted_name.split('.').collect();

    // Build candidate path suffixes by matching the last N segments
    // e.g., "coderadar.config" → try matching "coderadar/config.py" suffix
    for n in (1..=segments.len()).rev() {
        let suffix_parts = &segments[segments.len() - n..];
        let suffix_slash = suffix_parts.join("/");
        let suffix_py = format!("{}.py", suffix_slash);
        let suffix_init = format!("{}/__init__.py", suffix_slash);

        for (_, module) in &projection.modules {
            let path_str = module.path.to_string_lossy().to_string();
            let path_normalized = path_str.replace('\\', "/");
            if path_normalized.ends_with(&suffix_py) || path_normalized.ends_with(&suffix_init) {
                return Some(module.id.clone());
            }
        }
    }

    // Fallback: check if any module name matches the last segment
    let last_segment = segments.last().unwrap_or(&"");
    for (_, module) in &projection.modules {
        if module.name == *last_segment {
            let path_str = module.path.to_string_lossy().to_string();
            let path_normalized = path_str.replace('\\', "/");
            // Verify all segments match in reverse order
            let file_segments: Vec<&str> = path_normalized
                .trim_end_matches("/__init__.py")
                .trim_end_matches(".py")
                .split('/')
                .collect();
            if file_segments.len() >= segments.len() {
                let file_suffix = &file_segments[file_segments.len() - segments.len()..];
                if file_suffix == segments.as_slice() {
                    return Some(module.id.clone());
                }
            }
        }
    }

    None
}

/// Find a symbol (function or class) with a given name within a specific module.
fn find_symbol_in_module(
    projection: &ProjectedGraph,
    module_id: &str,
    symbol_name: &str,
) -> Option<String> {
    if let Some(module) = projection.modules.get(module_id) {
        // Search functions
        for func_id in &module.functions {
            if let Some(func) = projection.functions.get(func_id) {
                if func.name == symbol_name {
                    return Some(func.id.clone());
                }
            }
        }
        // Search classes
        for class_id in &module.classes {
            if let Some(class) = projection.classes.get(class_id) {
                if class.name == symbol_name {
                    return Some(class.id.clone());
                }
            }
        }
    }
    None
}

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

    /// Run the resolution cascade (L1-L3) on all functions in the current projection.
    /// After resolution, rewires callees_by_caller/callers_by_callee with resolved targets.
    pub fn resolve_all_calls(&self, projection: &mut ProjectedGraph) {
        use crate::resolve::orchestrator::ResolutionOrchestrator;
        use crate::resolve::signature::ScoredDef;

        let orchestrator = ResolutionOrchestrator::new();
        let import_graph = crate::graph::ImportGraph::new();

        // Build definition pool for signature matching (L3)
        let _definitions_pool: Vec<ScoredDef> = projection
            .functions
            .values()
            .map(|f| ScoredDef {
                entity_id: f.id.clone(),
                name: f.name.clone(),
                arity: f.parameters.len(),
                file_path: f.parent_module.clone(),
                score: 0.0,
            })
            .collect();

        // Clear old call edges — rebuild from resolution
        projection.callers_by_callee.clear();
        projection.callees_by_caller.clear();

        // Collect calls (before mutating functions map)
        let all_calls: Vec<(String, Vec<crate::types::UnresolvedRef>)> = projection
            .functions
            .iter()
            .map(|(id, f)| (id.clone(), f.calls.clone()))
            .collect();

        for (func_id, calls) in &all_calls {
            // Same-file intra-resolution: resolve calls to functions in the same module.
            // Get the function's parent module to find sibling functions.
            let parent_module = projection
                .functions
                .get(func_id)
                .map(|f| f.parent_module.clone())
                .unwrap_or_default();

            // Get the calling function's parent class for MRO resolution
            let my_parent_class = projection
                .functions
                .get(func_id)
                .and_then(|f| f.parent_class.clone());

            // Build a set of sibling function names in the same module for quick lookup
            let sibling_funcs: std::collections::HashMap<String, String> = projection
                .functions
                .iter()
                .filter(|(_, f)| f.parent_module == parent_module)
                .map(|(id, f)| (f.name.clone(), id.clone()))
                .collect();

            // Build methods of the same class for self.method() resolution
            let class_methods: std::collections::HashMap<String, String> =
                if let Some(ref class_id) = my_parent_class {
                    projection
                        .functions
                        .iter()
                        .filter(|(_, f)| f.parent_class.as_ref() == Some(class_id))
                        .map(|(id, f)| (f.name.clone(), id.clone()))
                        .collect()
                } else {
                    std::collections::HashMap::new()
                };

            // Cross-file import resolution: build lookup from imports of this module
            let mut import_targets: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            if let Some(module) = projection.modules.get(&parent_module) {
                for import_id in &module.imports {
                    if let Some(import) = projection.imports.get(import_id) {
                        match &import.kind {
                            ImportKind::FromImport { module: src_mod, names } => {
                                // Look up target module by dotted path
                                let target_mod_id = find_module_by_dotted_name(
                                    projection, src_mod, &parent_module);
                                for (name, _alias) in names {
                                    // Map imported name → potential target
                                    if let Some(ref tgt_id) = target_mod_id {
                                        import_targets.insert(name.clone(), tgt_id.clone());
                                    }
                                }
                            }
                            ImportKind::ModuleImport { module: src_mod, alias: _ } => {
                                // `import foo.bar` — makes foo.bar.baz() available
                                // Store the module prefix mapping
                                if let Some(tgt_id) = find_module_by_dotted_name(
                                    projection, src_mod, &parent_module) {
                                    // The module itself is available as the last segment
                                    let short_name = src_mod.rsplit('.').next().unwrap_or(src_mod);
                                    import_targets.insert(short_name.to_string(), tgt_id);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Resolve calls using the orchestrator
            let resolved = orchestrator.resolve_calls(calls, func_id, &import_graph);

            // Override with same-file, cross-file, and method resolution
            let resolved: Vec<_> = resolved
                .into_iter()
                .map(|rc| {
                    // ── Method resolution: self.method() calls ──────
                    if let crate::types::ResolvedCall::Unresolved { reason, raw } = &rc {
                        if matches!(reason, crate::types::UnresolvedReason::TypeInferenceRequired) {
                            // Try to resolve via class_methods when caller is a method
                            if let Some(target_id) = class_methods.get(&raw.name) {
                                return crate::types::ResolvedCall::Function(target_id.clone());
                            }
                        }
                        return rc;
                    }
                    // ── External → same-file, cross-file ────────────
                    if let crate::types::ResolvedCall::External(name) = &rc {
                        if let Some(target_id) = sibling_funcs.get(name.as_str()) {
                            return crate::types::ResolvedCall::Function(target_id.clone());
                        }
                        if let Some(target_mod_id) = import_targets.get(name.as_str()) {
                            if let Some(imported_func_id) = find_symbol_in_module(
                                projection, target_mod_id, name) {
                                return crate::types::ResolvedCall::Function(imported_func_id);
                            }
                        }
                        // Method on self that the orchestrator tagged as External
                        if let Some(target_id) = class_methods.get(name.as_str()) {
                            return crate::types::ResolvedCall::Function(target_id.clone());
                        }
                    }
                    if let crate::types::ResolvedCall::Builtin(name) = &rc {
                        if let Some(target_id) = sibling_funcs.get(name.as_str()) {
                            return crate::types::ResolvedCall::Function(target_id.clone());
                        }
                    }
                    rc
                })
                .collect();

            // Update resolved_calls on the function
            if let Some(func_arc) = projection.functions.get(func_id) {
                let mut updated = (**func_arc).clone();
                updated.resolved_calls = resolved.clone();
                projection.functions.insert(func_id.clone(), std::sync::Arc::new(updated));
            }

            // Wire callees_by_caller from resolved calls
            for rc in &resolved {
                match rc {
                    crate::types::ResolvedCall::Function(target_id)
                    | crate::types::ResolvedCall::Constructor(target_id) => {
                        projection
                            .callees_by_caller
                            .entry(func_id.clone())
                            .or_default()
                            .insert(target_id.clone());
                        projection
                            .callers_by_callee
                            .entry(target_id.clone())
                            .or_default()
                            .insert(func_id.clone());
                    }
                    crate::types::ResolvedCall::Method { method, .. } => {
                        projection
                            .callees_by_caller
                            .entry(func_id.clone())
                            .or_default()
                            .insert(method.clone());
                        projection
                            .callers_by_callee
                            .entry(method.clone())
                            .or_default()
                            .insert(func_id.clone());
                    }
                    crate::types::ResolvedCall::Builtin(name)
                    | crate::types::ResolvedCall::External(name) => {
                        let ext_id = format!("external::{}", name);
                        projection
                            .callees_by_caller
                            .entry(func_id.clone())
                            .or_default()
                            .insert(ext_id.clone());
                        projection
                            .callers_by_callee
                            .entry(ext_id)
                            .or_default()
                            .insert(func_id.clone());
                    }
                    crate::types::ResolvedCall::Unresolved { .. } => {}
                }
            }
        }
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

    // ── File Indexing Pipeline ────────────────────────────────────────────

    /// Get the tree-sitter Language for a CodeRadar Language.
    pub fn ts_language(lang: &Language) -> Option<tree_sitter::Language> {
        match lang {
            Language::Python => Some(tree_sitter_python::LANGUAGE.into()),
            Language::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            Language::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            Language::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            Language::Go => Some(tree_sitter_go::LANGUAGE.into()),
            Language::Java => Some(tree_sitter_java::LANGUAGE.into()),
            Language::C => Some(tree_sitter_c::LANGUAGE.into()),
            Language::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
            Language::Ruby => Some(tree_sitter_ruby::LANGUAGE.into()),
            Language::Php => Some(tree_sitter_php::LANGUAGE_PHP.into()),
            Language::CSharp => Some(tree_sitter_c_sharp::LANGUAGE.into()),
            Language::Kotlin | Language::OtherTen => None,
        }
    }

    /// Index a single source file: parse → tag → walk → extract → insert.
    /// Returns the number of entities extracted and added to the graph.
    pub fn index_file(
        &self,
        source: &str,
        file_path: &str,
        language: &Language,
    ) -> Result<usize, String> {
        let ts_lang = Self::ts_language(language)
            .ok_or_else(|| format!("No tree-sitter grammar for {:?}", language))?;

        // Phase 1: Parse with tree-sitter
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang)
            .map_err(|e| format!("Failed to set language: {}", e))?;
        let tree = parser.parse(source, None)
            .ok_or_else(|| "Failed to parse source".to_string())?;
        let root_node = tree.root_node();

        // Phase 2: Tag the tree with queries
        let tagged = crate::extract::tagger::tag_tree(
            source, root_node, language.clone(), ts_lang);

        // Phase 3: Walk and extract (needs the root node from our parse)
        let units = crate::extract::walker::walk_and_extract(
            &tagged, root_node, file_path);

        // Phase 3: Insert into ProjectedGraph
        let count = units.len();
        let mut projection = (*self.snapshot()).clone();
        self.insert_extracted(&mut projection, &units, file_path, language);
        self.commit_projection(projection);

        // Phase 4: Persist to Macrame if store attached
        let lang_str = format!("{:?}", language).to_lowercase();
        let _ = self.persist_entities(&units, file_path, &lang_str);

        Ok(count)
    }

    /// Remove all entities belonging to a file from the projection.
    /// Returns set of entity IDs that were removed (for downstream resolution).
    pub fn remove_file_entities(
        &self,
        projection: &mut ProjectedGraph,
        file_path: &str,
    ) -> BTreeSet<EntityId> {
        let mut removed = BTreeSet::new();

        // Normalize the lookup path for cross-platform consistency
        let lookup = normalize_path_str(file_path);

        // Find all module IDs for this file — try both raw and normalized
        let module_ids: Vec<EntityId> = projection
            .file_to_modules
            .get(&PathBuf::from(&lookup))
            .or_else(|| projection.file_to_modules.get(&PathBuf::from(file_path)))
            .cloned()
            .unwrap_or_default();

        if module_ids.is_empty() {
            // Fallback: entity IDs start with file_path, find them by prefix scan
            let prefix = format!("{}::", lookup);
            for (func_id, _) in projection.functions.iter() {
                if func_id.starts_with(&prefix) && !removed.contains(func_id) {
                    removed.insert(func_id.clone());
                }
            }
            for (class_id, _) in projection.classes.iter() {
                if class_id.starts_with(&prefix) && !removed.contains(class_id) {
                    removed.insert(class_id.clone());
                }
            }
            // Remove entities directly without module lookup
            for id in &removed.clone() {
                projection.functions.remove(id);
                projection.classes.remove(id);
                projection.imports.remove(id);
                projection.constants.remove(id);
                projection.type_aliases.remove(id);
                projection.callers_by_callee.remove(id);
                projection.callees_by_caller.remove(id);
                projection.subclasses.remove(id);
                projection.overridden_by.remove(id);
            }
            // Clean up stale entries
            projection.callers_by_callee.retain(|_, callees| {
                callees.retain(|cid| !removed.contains(cid));
                !callees.is_empty()
            });
            projection.callees_by_caller.retain(|_, callers| {
                callers.retain(|cid| !removed.contains(cid));
                !callers.is_empty()
            });
            return removed;
        }

        for module_id in &module_ids {
            removed.insert(module_id.clone());

            // Collect entity IDs to remove by checking parent_module
            let funcs_to_remove: Vec<EntityId> = projection
                .functions
                .iter()
                .filter(|(_, f)| &f.parent_module == module_id)
                .map(|(id, _)| id.clone())
                .collect();

            let classes_to_remove: Vec<EntityId> = projection
                .classes
                .iter()
                .filter(|(_, c)| &c.parent_module == module_id)
                .map(|(id, _)| id.clone())
                .collect();

            let imports_to_remove: Vec<EntityId> = projection
                .imports
                .iter()
                .filter(|(id, _)| id.starts_with(&format!("{}::", file_path)))
                .map(|(id, _)| id.clone())
                .collect();

            let constants_to_remove: Vec<EntityId> = projection
                .constants
                .iter()
                .filter(|(id, _)| id.starts_with(&format!("{}::", file_path)))
                .map(|(id, _)| id.clone())
                .collect();

            let aliases_to_remove: Vec<EntityId> = projection
                .type_aliases
                .iter()
                .filter(|(id, _)| id.starts_with(&format!("{}::", file_path)))
                .map(|(id, _)| id.clone())
                .collect();

            // Remove entities and their edge entries
            for id in &funcs_to_remove {
                projection.functions.remove(id);
                projection.callers_by_callee.remove(id);
                projection.callees_by_caller.remove(id);
                removed.insert(id.clone());
            }
            for id in &classes_to_remove {
                projection.classes.remove(id);
                projection.callers_by_callee.remove(id);
                projection.callees_by_caller.remove(id);
                projection.subclasses.remove(id);
                projection.overridden_by.remove(id);
                removed.insert(id.clone());
            }
            for id in &imports_to_remove {
                projection.imports.remove(id);
                removed.insert(id.clone());
            }
            for id in &constants_to_remove {
                projection.constants.remove(id);
                removed.insert(id.clone());
            }
            for id in &aliases_to_remove {
                projection.type_aliases.remove(id);
                removed.insert(id.clone());
            }

            // Remove the module itself
            projection.modules.remove(module_id);
        }

        // Clean up callers_by_callee entries that reference removed entities
        projection.callers_by_callee.retain(|_, callees| {
            callees.retain(|cid| !removed.contains(cid));
            !callees.is_empty()
        });
        projection.callees_by_caller.retain(|_, callers| {
            callers.retain(|cid| !removed.contains(cid));
            !callers.is_empty()
        });

        removed
    }

    /// Update a single file in-place: re-index and diff against the current graph.
    /// Returns (entities_added, entities_removed, affected_files).
    pub fn update_file(
        &self,
        file_path: &str,
        content: Option<&str>,
        _force: Option<bool>,
    ) -> Result<(usize, usize, Vec<String>), String> {
        // Normalize path for consistent lookups
        let normalized = normalize_path_str(file_path);
        let file_path = normalized.as_str();
        let lang = Language::from_extension(
            std::path::Path::new(file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("py")
        );

        let ts_lang = Self::ts_language(&lang)
            .ok_or_else(|| format!("No tree-sitter grammar for {:?}", lang))?;

        // Read source (or use provided content)
        let source = match content {
            Some(c) => c.to_string(),
            None => std::fs::read_to_string(file_path)
                .map_err(|e| format!("Failed to read file: {}", e))?,
        };

        // Phase 1-2: Parse and extract
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang)
            .map_err(|e| format!("Failed to set language: {}", e))?;
        let tree = parser.parse(&source, None)
            .ok_or_else(|| "Failed to parse source".to_string())?;
        let root_node = tree.root_node();

        let tagged = crate::extract::tagger::tag_tree(
            &source, root_node, lang.clone(), ts_lang);
        let units = crate::extract::walker::walk_and_extract(
            &tagged, root_node, file_path);

        let new_count = units.len();

        // Phase 3: Remove old entities for this file and insert new ones
        let mut projection = (*self.snapshot()).clone();
        let removed_count = self.remove_file_entities(&mut projection, file_path).len();
        self.insert_extracted(&mut projection, &units, file_path, &lang);

        // Phase 4: Re-run resolution for affected functions
        self.resolve_all_calls(&mut projection);
        self.commit_projection(projection);

        // Track affected files (this file + any callers)
        let mut affected_files = vec![file_path.to_string()];
        // Find functions that call into entities defined in this file
        let snap = self.snapshot();
        for (caller_id, callees) in &snap.callees_by_caller {
            for callee_id in callees {
                if callee_id.starts_with(file_path) {
                    // This caller calls something in the changed file
                    if let Some(caller_path) = caller_id.split("::").next() {
                        if !affected_files.contains(&caller_path.to_string()) {
                            affected_files.push(caller_path.to_string());
                        }
                    }
                    break;
                }
            }
        }

        // Phase 5: Persist to Macrame if store attached
        let lang_str = format!("{:?}", lang).to_lowercase();
        let _ = self.persist_entities(&units, file_path, &lang_str);

        Ok((new_count, removed_count, affected_files))
    }

    /// Insert ExtractedUnits into a ProjectedGraph (used by index_file and update).
    pub fn insert_extracted(
        &self,
        projection: &mut ProjectedGraph,
        units: &[ExtractedUnit],
        file_path: &str,
        language: &Language,
    ) {
        // Compute the synthetic module ID up front
        let file_stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let module_id = format!("{}::module", file_path);
        let mut module_classes: Vec<EntityId> = Vec::new();
        let mut module_functions: Vec<EntityId> = Vec::new();
        let mut module_imports: Vec<EntityId> = Vec::new();
        let mut module_constants: Vec<EntityId> = Vec::new();
        let mut module_type_aliases: Vec<EntityId> = Vec::new();

        for unit in units {
            match unit {
                ExtractedUnit::Module(m) => {
                    // If a module entity is present, use its ID
                    // (but tree-sitter walker doesn't emit Module entities)
                    let _ = m;
                }
                ExtractedUnit::Class(c) => {
                    let class = Class {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        parent_module: module_id.clone(), // fix up
                        parent_class: c.parent_class.clone(),
                        bases: c.bases.clone(),
                        resolved_bases: vec![],
                        mro: vec![],
                        mro_error: false,
                        methods: vec![],
                        fields: c.fields.iter().map(|ef| Field {
                            name: ef.name.clone(),
                            annotation: ef.annotation.clone(),
                            source: ef.source.clone(),
                            default_value: ef.default_value.clone(),
                            is_class_var: ef.is_class_var,
                            span: ef.name_span,
                            name_span: ef.name_span,
                        }).collect(),
                        source: c.source.clone(),
                        decorators: c.decorators.clone(),
                        effective: EffectiveClass::Plain,
                        is_type_checking_only: c.is_type_checking_only,
                        line: c.line,
                        exit_line: c.exit_line,
                        docstring: c.docstring.clone(),
                        parse_quality: ParseQuality::Clean,
                        content_hash: 0,
                        span: c.span,
                        name_span: c.name_span,
                        body_span: c.body_span,
                        decorators_span: c.decorators_span,
                    };
                    projection.classes.insert(class.id.clone(), Arc::new(class));
                    module_classes.push(c.id.clone());
                }
                ExtractedUnit::Function(f) => {
                    let func = Function {
                        id: f.id.clone(),
                        name: f.name.clone(),
                        parent_module: module_id.clone(), // fix up
                        parent_class: f.parent_class.clone(),
                        parameters: f.parameters.clone(),
                        return_type: f.return_type.clone(),
                        calls: f.calls.clone(),
                        resolved_calls: vec![],
                        decorators: f.decorators.clone(),
                        setter_of: None,
                        line: f.line,
                        exit_line: f.exit_line,
                        docstring: f.docstring.clone(),
                        kind: f.kind.clone(),
                        is_async: f.is_async,
                        is_generator: f.is_generator,
                        source: f.source.clone(),
                        signature_hash: f.signature_hash,
                        body_hash: f.body_hash,
                        is_type_checking_only: f.is_type_checking_only,
                        parse_quality: ParseQuality::Clean,
                        content_hash: 0,
                        span: f.span,
                        name_span: f.name_span,
                        params_span: f.name_span,
                        body_span: f.body_span,
                        decorators_span: f.decorators_span,
                        embedding: vec![],
                    };
                    projection.functions.insert(func.id.clone(), Arc::new(func));
                    module_functions.push(f.id.clone());
                }
                ExtractedUnit::Import(i) => {
                    let import = Import {
                        id: i.id.clone(),
                        raw: i.raw.clone(),
                        kind: i.kind.clone(),
                        resolution: ImportResolution::Unresolved,
                        line: i.line,
                        is_type_only: i.is_type_only,
                        name_span: i.name_span,
                    };
                    projection.imports.insert(import.id.clone(), Arc::new(import));
                    module_imports.push(i.id.clone());
                }
                ExtractedUnit::Constant(k) => {
                    let constant = Constant {
                        id: k.id.clone(),
                        name: k.name.clone(),
                        annotation: k.annotation.clone(),
                        source: k.source.clone(),
                        default_value: k.default_value.clone(),
                        span: k.span,
                        name_span: k.name_span,
                    };
                    projection.constants.insert(constant.id.clone(), Arc::new(constant));
                    module_constants.push(k.id.clone());
                }
                ExtractedUnit::TypeAlias(ta) => {
                    let alias = TypeAlias {
                        id: ta.id.clone(),
                        name: ta.name.clone(),
                        target: ta.target.clone(),
                        source: ta.source.clone(),
                        span: ta.span,
                        name_span: ta.name_span,
                    };
                    projection.type_aliases.insert(alias.id.clone(), Arc::new(alias));
                    module_type_aliases.push(ta.id.clone());
                }
                _ => {}
            }
        }

        // Wire call edges: for each function's calls, create target IDs
        // using the same-module heuristic: target = "{file_path}::{call_name}"
        {
            for func_id in &module_functions {
                if let Some(_func) = projection.functions.get(func_id) {
                    let calls = _func.calls.clone();
                    for call in &calls {
                        // Heuristic: same-file function target
                        let target_id = if call.path.is_empty() {
                            format!("{}::{}", file_path, call.name)
                        } else {
                            format!("{}::{}.{}", file_path, call.path.join("."), call.name)
                        };
                        projection
                            .callees_by_caller
                            .entry(func_id.clone())
                            .or_default()
                            .insert(target_id.clone());
                        projection
                            .callers_by_callee
                            .entry(target_id)
                            .or_default()
                            .insert(func_id.clone());
                    }
                }
            }
        }

        // Insert the synthetic module
        let module = Module {
            id: module_id.clone(),
            name: file_stem.to_string(),
            path: PathBuf::from(file_path),
            language: language.clone(),
            package: None,
            exports: vec![],
            star_exports: None,
            classes: module_classes,
            functions: module_functions,
            imports: module_imports,
            constants: module_constants,
            type_aliases: module_type_aliases,
            parse_quality: ParseQuality::Clean,
            file_version: 1,
            content_hash: 0,
        };
        projection.modules.insert(module_id.clone(), Arc::new(module));
        projection.file_to_modules
            .entry(PathBuf::from(file_path))
            .or_default()
            .push(module_id);
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

    // ── Import Parsing & Cross-File Resolution Tests ──────────────

    /// Helper: index a source string with language auto-detection from extension.
    fn index_source(graph: &CodeGraph, source: &str, file_path: &str) {
        let lang = Language::from_extension(
            std::path::Path::new(file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("py")
        );
        graph.index_file(source, file_path, &lang).unwrap();
    }

    #[test]
    fn test_typescript_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "function hello(name: string): string {\n  return `Hello ${name}`;\n}\n\nconst add = (a: number, b: number): number => a + b;\n",
            "src/util.ts");

        let snap = graph.snapshot();
        // hello should be indexed
        assert!(snap.functions.values().any(|f| f.name == "hello"),
                "Should have function hello");
    }

    #[test]
    fn test_typescript_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Animal {\n  speak() { return 'hi'; }\n  move() { this.speak(); }\n}\n",
            "src/animal.ts");

        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Animal"),
                "Should have class Animal");
        assert!(snap.functions.values().any(|f| f.name == "speak"),
                "Should have method speak");
    }

    // ── Go Indexing Tests ──────────────────────────────────────

    #[test]
    fn test_go_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "package main\nfunc hello(name string) string { return \"hi\" }\n",
            "main.go");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "hello"),
                "Should have Go function hello");
    }

    #[test]
    fn test_go_struct_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "package main\ntype Dog struct { Name string }\nfunc (d *Dog) Bark() {}\n",
            "dog.go");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Dog"),
                "Should have Go struct Dog");
        assert!(snap.functions.values().any(|f| f.name == "Bark"),
                "Should have Go method Bark");
    }

    // ── Java Indexing Tests ────────────────────────────────────

    #[test]
    fn test_java_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Cat { void meow() { this.eat(); } void eat() {} }\n",
            "Cat.java");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Cat"),
                "Should have Java class Cat");
        assert!(snap.functions.values().any(|f| f.name == "meow"),
                "Should have Java method meow");
    }

    #[test]
    fn test_java_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Foo { void bar() { baz(); } void baz() {} }\n",
            "Foo.java");
        let snap = graph.snapshot();
        let bar = snap.functions.values().find(|f| f.name == "bar");
        assert!(bar.is_some(), "Should have bar");
        assert!(!bar.unwrap().calls.is_empty(), "bar should have calls");
    }

    // ── C++ Indexing Tests ─────────────────────────────────────

    #[test]
    fn test_cpp_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "int add(int a, int b) { return a + b; }\n",
            "math.cpp");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "add"),
                "Should have C++ function add");
    }

    #[test]
    fn test_cpp_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Widget { public: void render() {} void paint() {} };\n",
            "widget.cpp");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Widget"),
                "Should have C++ class Widget");
        assert!(snap.functions.values().any(|f| f.name == "render"),
                "Should have C++ method render");
    }

    // ── Go Receiver / Method Mapping ────────────────────────────

    #[test]
    fn test_go_method_receiver() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "package main\ntype Dog struct { Name string }\nfunc (d *Dog) Bark() {}\n",
            "dog.go");
        let snap = graph.snapshot();
        if let Some(bark) = snap.functions.values().find(|f| f.name == "Bark") {
            assert!(bark.parent_class.is_some(),
                    "Go method Bark should have parent_class (receiver type Dog)");
        }
    }

    // ── Embedding Pipeline Tests ────────────────────────────────

    #[test]
    fn test_function_embedding_field() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");
        let mut projection = (*graph.snapshot()).clone();
        if let Some(add_fn) = projection.functions.get("math.py::add") {
            let mut updated = (**add_fn).clone();
            updated.embedding = vec![0.1, 0.2, 0.3];
            projection.functions.insert("math.py::add".to_string(), std::sync::Arc::new(updated));
        }
        graph.commit_projection(projection);
        let snap = graph.snapshot();
        let add = snap.functions.get("math.py::add").unwrap();
        assert_eq!(add.embedding.len(), 3);
    }

    #[test]
    fn test_cosine_similarity() {
        use crate::cosine_similarity;
        let sim = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]);
        assert!((sim - 0.0).abs() < 0.001, "Orthogonal=0, got {}", sim);
        let sim = cosine_similarity(&[1.0, 2.0], &[1.0, 2.0]);
        assert!((sim - 1.0).abs() < 0.001, "Identical=1, got {}", sim);
    }

    #[test]
    fn test_import_parsing_from_import() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "from os import path\ndef foo(): path.join('x')\n", "mod.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);

        // The import should create an Import entity with FromImport kind
        let imports: Vec<_> = projection.imports.values().collect();
        assert!(!imports.is_empty(), "Should have at least one import");
        let import = &imports[0];
        match &import.kind {
            ImportKind::FromImport { module, names } => {
                assert_eq!(module, "os");
                assert_eq!(names.len(), 1);
                assert_eq!(names[0].0, "path");
            }
            other => panic!("Expected FromImport, got {:?}", other),
        }
    }

    #[test]
    fn test_import_parsing_module_import() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "import os.path as p\ndef foo(): p.join('x')\n", "mod.py");

        let projection = graph.snapshot();
        let imports: Vec<_> = projection.imports.values().collect();
        assert!(!imports.is_empty());
        match &imports[0].kind {
            ImportKind::ModuleImport { module, alias } => {
                assert_eq!(module, "os.path");
                assert_eq!(alias.as_deref(), Some("p"));
            }
            other => panic!("Expected ModuleImport, got {:?}", other),
        }
    }

    #[test]
    fn test_cross_file_resolution_same_dir() {
        let graph = CodeGraph::new(GraphConfig::default());

        // module_a defines helper
        index_source(&graph, "def helper(x): return x * 2\n", "src/module_a.py");
        // module_b imports and calls helper
        index_source(&graph, "from module_a import helper\ndef process(): return helper(42)\n", "src/module_b.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);
        graph.commit_projection(projection);

        // process should call helper from module_a
        let process_id = "src/module_b.py::process";
        let helper_id = "src/module_a.py::helper";

        let callees = graph.callees_of(process_id);
        assert!(!callees.is_empty(),
                "process should have at least one callee, got {:?}", callees);
        assert!(callees.contains(&helper_id.to_string()),
                "process should call {}, got {:?}", helper_id, callees);

        let callers = graph.callers_of(helper_id);
        assert!(callers.contains(&process_id.to_string()),
                "helper should be called by process, got {:?}", callers);
    }

    #[test]
    fn test_cross_file_resolution_nested_package() {
        let graph = CodeGraph::new(GraphConfig::default());

        // Simulate coderadar.config.Config
        index_source(&graph, "class Config:\n    pass\n",
                     "py_agent/src/coderadar/config.py");
        // pipeline imports Config
        index_source(&graph,
                     "from coderadar.config import Config\ndef make_cfg(): return Config()\n",
                     "py_agent/src/coderadar/pipeline.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);
        graph.commit_projection(projection);

        let make_cfg_id = "py_agent/src/coderadar/pipeline.py::make_cfg";
        let callees = graph.callees_of(make_cfg_id);
        // Config() is a constructor call — should resolve to the class in config.py
        assert!(!callees.is_empty(),
                "make_cfg should have at least one callee, got {:?}", callees);
    }

    // ── Rust Indexing & Method Resolution Tests ──────────────────

    #[test]
    fn test_rust_struct_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "pub struct Foo { x: i32 }\nimpl Foo { pub fn new() -> Self { Foo { x: 0 } } }\n",
            "src/lib.rs");

        let projection = graph.snapshot();
        // Should have struct Foo and method Foo::new
        let struct_id = "src/lib.rs::Foo";
        assert!(projection.classes.contains_key(struct_id),
                "Should have struct Foo");

        let method_id = "src/lib.rs::Foo.new";
        assert!(projection.functions.contains_key(method_id),
                "Should have method Foo::new");

        let method = projection.functions.get(method_id).unwrap();
        assert!(method.parent_class.is_some(),
                "new() should have parent_class set to Foo");
    }

    #[test]
    fn test_rust_method_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "pub struct Foo { x: i32 }\nimpl Foo { pub fn bar(&self) -> i32 { self.baz() } pub fn baz(&self) -> i32 { 42 } }\n",
            "src/lib.rs");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);
        graph.commit_projection(projection);

        let bar_id = "src/lib.rs::Foo.bar";
        let baz_id = "src/lib.rs::Foo.baz";

        // bar() calls self.baz() — should resolve via class_methods
        let callees = graph.callees_of(bar_id);
        assert!(callees.contains(&baz_id.to_string()),
                "bar() should call baz() via self, got {:?}", callees);
    }

    #[test]
    fn test_update_file_adds_entities() {
        let graph = CodeGraph::new(GraphConfig::default());

        // Verify basic indexing
        graph.index_file("def foo(): pass\ndef bar(): pass\n", "mod.py", &Language::Python).unwrap();
        let initial = graph.snapshot().functions.len();
        assert_eq!(initial, 2, "Expected 2 functions, got {}: {:?}",
            initial,
            graph.snapshot().functions.keys().collect::<Vec<_>>());

        // Update: change bar, add baz
        let result = graph.update_file(
            "mod.py",
            Some("def foo(): pass\ndef bar(): return 42\ndef baz(): pass\n"),
            None,
        );
        assert!(result.is_ok(), "update_file error: {:?}", result.err());
        let (added, removed, _affected) = result.unwrap();

        assert!(added >= 1, "Should add at least 1 entity, got {}", added);
        assert!(removed >= 1, "Should remove at least 1 entity, got {}", removed);

        let snap = graph.snapshot();
        assert!(snap.functions.contains_key("mod.py::baz"), "Should have new baz");
        assert!(snap.functions.contains_key("mod.py::foo"), "Foo should survive");
    }

    #[test]
    fn test_update_file_removes_entities() {
        let graph = CodeGraph::new(GraphConfig::default());

        index_source(&graph,
            "class Dog: pass\nclass Cat: pass\n", "animals.py");
        assert_eq!(graph.snapshot().classes.len(), 2);

        // Remove Cat
        let result = graph.update_file(
            "animals.py",
            Some("class Dog: pass\n"),
            None,
        );
        assert!(result.is_ok(), "update_file error: {:?}", result.err());
        let (added, removed, _) = result.unwrap();

        assert!(added >= 1);
        assert!(removed >= 2); // Cat class + associated

        let snap = graph.snapshot();
        assert!(snap.classes.contains_key("animals.py::Dog"));
        assert!(!snap.classes.contains_key("animals.py::Cat"));
    }
}
