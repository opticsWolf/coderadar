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
}
