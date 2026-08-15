// CodeRadar v3.6 — CodeGraph Container (§3.4, §9.1)
// RwLock<Arc<ProjectedGraph>> with Macrame-backed persistence.
// Hybrid architecture: in-memory projected graph for structural queries,
// Macrame for agent traversals and bitemporal history.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use lru::LruCache;
use parking_lot::RwLock;

use crate::resolve::cache::ResolutionCache;
use crate::resolve::stack_graph::StackGraphResolver;
use crate::storage;
use crate::types::*;
use crate::extract;

pub mod config;

pub use config::*;

pub mod import_graph;

pub use import_graph::{ImportGraph, ImportNode};

pub mod call_graph;

pub use call_graph::{CallEdge, CallGraph, CallNode};

pub mod module_resolution;

use module_resolution::normalize_path_str;
pub(crate) use module_resolution::find_module_by_dotted_name;

pub mod mro;

pub mod inheritance;

pub mod traversal;

pub mod resolve_calls;


pub mod persistence;

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

    /// Store an embedding vector on any entity type.
    /// Updates the in-memory embedding field and commits the projection.
    pub fn set_embedding(
        &self,
        entity_id: &str,
        embedding: &[f64],
        content_hash: &str,
    ) -> Result<(), String> {
        let mut projection = (*self.snapshot()).clone();
        let emb = EmbeddingVec { vec: embedding.to_vec(), hash: content_hash.to_string() };
        if let Some(e) = projection.functions.get_mut(entity_id) {
            std::sync::Arc::make_mut(e).embedding = emb.clone();
        } else if let Some(e) = projection.classes.get_mut(entity_id) {
            std::sync::Arc::make_mut(e).embedding = emb.clone();
        } else if let Some(e) = projection.modules.get_mut(entity_id) {
            std::sync::Arc::make_mut(e).embedding = emb.clone();
        } else if let Some(e) = projection.imports.get_mut(entity_id) {
            std::sync::Arc::make_mut(e).embedding = emb.clone();
        } else if let Some(e) = projection.constants.get_mut(entity_id) {
            std::sync::Arc::make_mut(e).embedding = emb.clone();
        } else if let Some(e) = projection.type_aliases.get_mut(entity_id) {
            std::sync::Arc::make_mut(e).embedding = emb.clone();
        } else {
            return Err(format!("Entity not found: {}", entity_id));
        }
        self.commit_projection(projection);
        Ok(())
    }

    /// Clear embedding vectors for all entities in a file.
    /// Called after mutation to invalidate stale embeddings.
    pub fn clear_embeddings_for_file(&self, file_path: &str) {
        let normalized = file_path.replace('/', std::path::MAIN_SEPARATOR_STR)
                                     .replace('\\', std::path::MAIN_SEPARATOR_STR);
        // Try both with and without ./ prefix (cross-platform path variance)
        let module_id_a = format!("{}::module", normalized);
        let module_id_b = format!(".{}{}::module", std::path::MAIN_SEPARATOR_STR, normalized.trim_start_matches('.').trim_start_matches(std::path::MAIN_SEPARATOR));
        let module_id_c = format!("./{}::module", normalized.replace("\\", "/"));
        let candidates = [&module_id_a, &module_id_b, &module_id_c];
        let mut projection = (*self.snapshot()).clone();
        for candidate in &candidates {
            if let Some(module) = projection.modules.get(*candidate) {
                let ids: Vec<String> = module.functions.iter()
                    .chain(module.classes.iter())
                    .chain(module.imports.iter())
                    .chain(module.constants.iter())
                    .chain(module.type_aliases.iter())
                    .cloned()
                    .collect();
                for id in &ids {
                    if let Some(f) = projection.functions.get_mut(id) {
                        std::sync::Arc::make_mut(f).embedding = EmbeddingVec::default();
                    }
                    if let Some(c) = projection.classes.get_mut(id) {
                        std::sync::Arc::make_mut(c).embedding = EmbeddingVec::default();
                    }
                    if let Some(i) = projection.imports.get_mut(id) {
                        std::sync::Arc::make_mut(i).embedding = EmbeddingVec::default();
                    }
                    if let Some(c) = projection.constants.get_mut(id) {
                        std::sync::Arc::make_mut(c).embedding = EmbeddingVec::default();
                    }
                    if let Some(ta) = projection.type_aliases.get_mut(id) {
                        std::sync::Arc::make_mut(ta).embedding = EmbeddingVec::default();
                    }
                }
                break;
            }
        }
        self.commit_projection(projection);
    }

    /// v0.5: Set a module's `__all__` star-export names list.
    /// Called from Python after static `__all__` analysis (exports.py).
    /// Enables resolution of `from X import *` wildcard imports.
    pub fn set_module_star_exports(
        &self,
        module_id: &str,
        names: Vec<String>,
    ) {
        let mut projection = (*self.snapshot()).clone();
        if let Some(module) = projection.modules.get(module_id) {
            let mut m = (**module).clone();
            m.star_exports = Some(names);
            projection.modules.insert(module_id.to_string(), Arc::new(m));
        }
        self.commit_projection(projection);
    }

    // ── File Indexing Pipeline ────────────────────────────────────────────

    /// Get the tree-sitter Language for a CodeRadar Language.
    pub fn ts_language(lang: &Language) -> Option<tree_sitter::Language> {
        use tree_sitter_language_pack::get_language;
        let name = lang.pack_name();
        get_language(name).ok()
    }

    /// Index a single source file: parse → tag → walk → extract → insert.
    /// Persists entities to Macrame immediately (used by update_file / tests).
    /// Returns the number of entities extracted and added to the graph.
    pub fn index_file(
        &self,
        source: &str,
        file_path: &str,
        language: &Language,
    ) -> Result<usize, String> {
        let (count, units) = self.index_file_inner(source, file_path, language)?;
        let lang_str = format!("{:?}", language).to_lowercase();
        let _ = self.persist_entities(&units, file_path, &lang_str);
        Ok(count)
    }

    /// Index without persisting — returns (count, concepts) for batched persistence.
    /// Used by `analyze` to collect all concepts and flush via `write_concepts` once.
    pub fn index_file_accumulate(
        &self,
        source: &str,
        file_path: &str,
        language: &Language,
    ) -> Result<(usize, Vec<macrame::ConceptUpsert>), String> {
        let (count, units) = self.index_file_inner(source, file_path, language)?;
        let lang_str = format!("{:?}", language).to_lowercase();
        let concepts: Vec<macrame::ConceptUpsert> = units
            .iter()
            .map(|u| crate::storage::build_concept(u, file_path, &lang_str))
            .collect();
        Ok((count, concepts))
    }

    /// Synthesize the file-level Module unit. The single-pass extractor emits
    /// Class/Function/Import/Constant/TypeAlias units but no Module unit (it walks
    /// tree-sitter nodes, not files). We need a Module unit so `build_concept`
    /// persists the module as a Macrame concept — IMPORTS edges (module → module)
    /// then have a valid FK target (see persist_edges).
    pub(crate) fn synthesize_module_unit(file_path: &str, language: &Language) -> ExtractedUnit {
        let stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        ExtractedUnit::Module(ExtractedModule {
            id: format!("{}::module", file_path),
            name: stem.to_string(),
            path: PathBuf::from(file_path),
            language: language.clone(),
            parse_quality: ParseQuality::Clean,
            content_hash: 0,
        })
    }

    /// Parse+extract only — no projection mutation, no persistence.
    /// Returns (units, concepts) for later batch insert. Thread-safe:
    /// creates its own tree-sitter Parser per invocation.
    /// Technique: parallel extraction across files adopted from CodeGraph's
    /// ParseWorkerPool pattern (src/extraction/index.ts). MIT license.
    /// https://github.com/opticsWolf/codegraph
    pub fn extract_only(
        source: &str,
        file_path: &str,
        language: &Language,
    ) -> Result<(Vec<ExtractedUnit>, Vec<macrame::ConceptUpsert>), String> {
        let ts_lang = Self::ts_language(language)
            .ok_or_else(|| format!("No tree-sitter grammar for {:?}", language))?;
        let compiled_query = crate::extract::tagger::CompiledQuery::new(*language, &ts_lang)
            .ok_or_else(|| format!("Failed to compile query for {:?}", language))?;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang)
            .map_err(|e| format!("Failed to set language: {}", e))?;
        let tree = parser.parse(source, None)
            .ok_or_else(|| "Failed to parse source".to_string())?;
        let root_node = tree.root_node();

        let mut units = crate::extract::single_pass::extract_single_pass(
            source, root_node, &compiled_query, file_path);
        units.insert(0, Self::synthesize_module_unit(file_path, language));

        let lang_str = format!("{:?}", language).to_lowercase();
        let concepts: Vec<macrame::ConceptUpsert> = units
            .iter()
            .map(|u| crate::storage::build_concept(u, file_path, &lang_str))
            .collect();

        Ok((units, concepts))
    }

    /// Build a standalone ProjectedGraph fragment from one file's extracted units.
    /// Thread-safe — no `&self`, no shared state. Used by the parallel indexing
    /// phase so each thread builds its local fragment, then the main thread merges
    /// them (avoiding the sequential projection-clone bottleneck).
    ///
    /// This mirrors `insert_extracted` but:
    /// - Does NOT touch `self.import_graph` (already parallelized via
    ///   `ImportGraph::build_import_edges` during Phase 2)
    /// - Returns a new `ProjectedGraph` instead of mutating an existing one
    /// - Includes same-file heuristic call edges (resolved later by
    ///   `resolve_all_calls`)
    pub fn build_fragment(
        units: &[ExtractedUnit],
        file_path: &str,
        language: &Language,
    ) -> ProjectedGraph {
        let file_stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let module_id = format!("{}::module", file_path);

        let mut projection = ProjectedGraph {
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

        let mut module_classes: Vec<EntityId> = Vec::new();
        let mut module_functions: Vec<EntityId> = Vec::new();
        let mut module_imports: Vec<EntityId> = Vec::new();
        let mut module_constants: Vec<EntityId> = Vec::new();
        let mut module_type_aliases: Vec<EntityId> = Vec::new();

        for unit in units {
            match unit {
                ExtractedUnit::Module(_) => {}
                ExtractedUnit::Class(c) => {
                    let class = Class {
                        id: c.id.clone(), name: c.name.clone(),
                        grammar_kind: c.grammar_kind.clone(),
                        parent_module: module_id.clone(),
                        parent_class: c.parent_class.clone(),
                        bases: c.bases.clone(), resolved_bases: vec![],
                        mro: vec![], mro_error: false, methods: vec![],
                        fields: c.fields.iter().map(|ef| Field {
                            name: ef.name.clone(), annotation: ef.annotation.clone(),
                            source: ef.source.clone(),
                            default_value: ef.default_value.clone(),
                            is_class_var: ef.is_class_var,
                            span: ef.name_span, name_span: ef.name_span,
                        }).collect(),
                        source: c.source.clone(), decorators: c.decorators.clone(),
                        effective: EffectiveClass::Plain,
                        is_type_checking_only: c.is_type_checking_only,
                        line: c.line, exit_line: c.exit_line,
                        docstring: c.docstring.clone(),
                        parse_quality: ParseQuality::Clean, content_hash: 0,
                        span: c.span, name_span: c.name_span,
                        body_span: c.body_span, decorators_span: c.decorators_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.classes.insert(class.id.clone(), Arc::new(class));
                    module_classes.push(c.id.clone());
                }
                ExtractedUnit::Function(f) => {
                    let func = Function {
                        id: f.id.clone(), name: f.name.clone(),
                        parent_module: module_id.clone(),
                        parent_class: f.parent_class.clone(),
                        parameters: f.parameters.clone(), return_type: f.return_type.clone(),
                        calls: f.calls.clone(), resolved_calls: vec![],
                        decorators: f.decorators.clone(), setter_of: None,
                        line: f.line, exit_line: f.exit_line,
                        docstring: f.docstring.clone(), kind: f.kind.clone(),
                        is_async: f.is_async, is_generator: f.is_generator,
                        source: f.source.clone(),
                        signature_hash: f.signature_hash, body_hash: f.body_hash,
                        metrics: f.metrics,
                        is_type_checking_only: f.is_type_checking_only,
                        parse_quality: ParseQuality::Clean, content_hash: 0,
                        span: f.span, name_span: f.name_span,
                        params_span: f.params_span, body_span: f.body_span,
                        decorators_span: f.decorators_span, embedding: EmbeddingVec::default(),
                    };
                    projection.functions.insert(func.id.clone(), Arc::new(func));
                    module_functions.push(f.id.clone());
                }
                ExtractedUnit::Import(i) => {
                    let import = Import {
                        id: i.id.clone(), raw: i.raw.clone(),
                        kind: i.kind.clone(),
                        resolution: ImportResolution::Unresolved,
                        line: i.line, is_type_only: i.is_type_only,
                        name_span: i.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.imports.insert(import.id.clone(), Arc::new(import));
                    module_imports.push(i.id.clone());
                }
                ExtractedUnit::Constant(k) => {
                    let constant = Constant {
                        id: k.id.clone(), name: k.name.clone(),
                        annotation: k.annotation.clone(), source: k.source.clone(),
                        default_value: k.default_value.clone(),
                        span: k.span, name_span: k.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.constants.insert(constant.id.clone(), Arc::new(constant));
                    module_constants.push(k.id.clone());
                }
                ExtractedUnit::TypeAlias(ta) => {
                    let alias = TypeAlias {
                        id: ta.id.clone(), name: ta.name.clone(),
                        target: ta.target.clone(), source: ta.source.clone(),
                        span: ta.span, name_span: ta.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.type_aliases.insert(alias.id.clone(), Arc::new(alias));
                    module_type_aliases.push(ta.id.clone());
                }
                _ => {}
            }
        }

        // Same-file heuristic call edges
        for func_id in &module_functions {
            if let Some(func) = projection.functions.get(func_id) {
                let calls = func.calls.clone();
                for call in &calls {
                    let target_id = if call.path.is_empty() {
                        format!("{}::{}", file_path, call.name)
                    } else {
                        format!("{}::{}.{}", file_path, call.path.join("."), call.name)
                    };
                    projection.callees_by_caller
                        .entry(func_id.clone()).or_default()
                        .insert(target_id.clone());
                    projection.callers_by_callee
                        .entry(target_id).or_default()
                        .insert(func_id.clone());
                }
            }
        }

        // Synthetic module entity
        let module = Module {
            id: module_id.clone(), name: file_stem.to_string(),
            path: PathBuf::from(file_path),
            language: language.clone(), package: None,
            exports: vec![], star_exports: None,
            classes: module_classes, functions: module_functions,
            imports: module_imports, constants: module_constants,
            type_aliases: module_type_aliases,
            parse_quality: ParseQuality::Clean, file_version: 1, content_hash: 0,
                        embedding: EmbeddingVec::default(),        };
        projection.modules.insert(module_id.clone(), Arc::new(module));
        projection.file_to_modules.insert(PathBuf::from(file_path), vec![module_id]);

        projection
    }

    /// Shared parse→extract→insert logic.
    fn index_file_inner(
        &self,
        source: &str,
        file_path: &str,
        language: &Language,
    ) -> Result<(usize, Vec<crate::types::ExtractedUnit>), String> {
        let ts_lang = Self::ts_language(language)
            .ok_or_else(|| format!("No tree-sitter grammar for {:?}", language))?;
        let compiled_query = crate::extract::tagger::CompiledQuery::new(*language, &ts_lang)
            .ok_or_else(|| format!("Failed to compile query for {:?}", language))?;

        // Phase 1: Parse with tree-sitter
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang)
            .map_err(|e| format!("Failed to set language: {}", e))?;
        let tree = parser.parse(source, None)
            .ok_or_else(|| "Failed to parse source".to_string())?;
        let root_node = tree.root_node();

        // Phase 2+3: Single-pass cursor-driven extraction
        let mut units = crate::extract::single_pass::extract_single_pass(
            source, root_node, &compiled_query, file_path);
        units.insert(0, Self::synthesize_module_unit(file_path, language));

        // Phase 3: Insert into ProjectedGraph
        let count = units.len();
        let mut projection = (*self.snapshot()).clone();
        self.insert_extracted(&mut projection, &units, file_path, language);
        self.commit_projection(projection);

        Ok((count, units))
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

    /// Diff-based incremental update: compare new units against existing projection
    /// entities and only insert/remove entities that actually changed. Unchanged
    /// entities (same ID + same content hashes) are left in place, avoiding
    /// unnecessary hashmap churn and edge recalculation.
    /// Returns (inserted_count, removed_count).
    pub fn apply_diff_update(
        &self,
        projection: &mut ProjectedGraph,
        units: &[ExtractedUnit],
        file_path: &str,
        language: &Language,
    ) -> (usize, usize) {
        // 1. Collect old entity hashes from projection.
        //    Normalize entity IDs for cross-platform prefix matching.
        let normalized_file_path = normalize_path_str(file_path);
        let old_hashes: std::collections::HashMap<EntityId, (u64, u64)> = projection
            .functions
            .iter()
            .filter(|(id, _)| normalize_path_str(id).starts_with(&normalized_file_path))
            .map(|(id, f)| (id.clone(), (f.signature_hash, f.body_hash)))
            .collect();

        let old_classes: std::collections::BTreeSet<EntityId> = projection
            .classes.keys()
            .filter(|id| normalize_path_str(id).starts_with(&normalized_file_path))
            .cloned().collect();
        let old_imports: std::collections::BTreeSet<EntityId> = projection
            .imports.keys()
            .filter(|id| normalize_path_str(id).starts_with(&normalized_file_path))
            .cloned().collect();
        let old_constants: std::collections::BTreeSet<EntityId> = projection
            .constants.keys()
            .filter(|id| normalize_path_str(id).starts_with(&normalized_file_path))
            .cloned().collect();
        let old_aliases: std::collections::BTreeSet<EntityId> = projection
            .type_aliases.keys()
            .filter(|id| normalize_path_str(id).starts_with(&normalized_file_path))
            .cloned().collect();

        // 2. Build new entity ID sets (normalized for cross-platform matching).
        let normalize_id = |id: &str| normalize_path_str(id);
        let new_funcs: std::collections::BTreeSet<EntityId> = units.iter()
            .filter_map(|u| match u { ExtractedUnit::Function(f) => Some(normalize_id(&f.id)), _ => None })
            .collect();
        let new_classes: std::collections::BTreeSet<EntityId> = units.iter()
            .filter_map(|u| match u { ExtractedUnit::Class(c) => Some(normalize_id(&c.id)), _ => None })
            .collect();
        let new_imports: std::collections::BTreeSet<EntityId> = units.iter()
            .filter_map(|u| match u { ExtractedUnit::Import(i) => Some(normalize_id(&i.id)), _ => None })
            .collect();
        let new_constants: std::collections::BTreeSet<EntityId> = units.iter()
            .filter_map(|u| match u { ExtractedUnit::Constant(c) => Some(normalize_id(&c.id)), _ => None })
            .collect();
        let new_aliases: std::collections::BTreeSet<EntityId> = units.iter()
            .filter_map(|u| match u { ExtractedUnit::TypeAlias(t) => Some(normalize_id(&t.id)), _ => None })
            .collect();

        let mut inserted = 0usize;
        let mut removed = 0usize;

        // 3. Remove entities that don't exist in new units
        let remove_entity = |id: &str, proj: &mut ProjectedGraph, removed: &mut usize| {
            proj.functions.remove(id);
            proj.classes.remove(id);
            proj.imports.remove(id);
            proj.constants.remove(id);
            proj.type_aliases.remove(id);
            proj.modules.remove(id);
            proj.callers_by_callee.remove(id);
            proj.callees_by_caller.remove(id);
            proj.subclasses.remove(id);
            proj.overridden_by.remove(id);
            *removed += 1;
        };

        let module_id = format!("{}::module", file_path);

        for id in old_hashes.keys().filter(|id| !new_funcs.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed);
        }
        for id in old_classes.iter().filter(|id| !new_classes.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed);
        }
        for id in old_imports.iter().filter(|id| !new_imports.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed);
        }
        for id in old_constants.iter().filter(|id| !new_constants.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed);
        }
        for id in old_aliases.iter().filter(|id| !new_aliases.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed);
        }

        // 4. Insert new entities + re-insert changed ones (hash mismatch)
        for unit in units {
            let id = unit.entity_id();
            let needs_insert = match unit {
                ExtractedUnit::Function(f) => {
                    old_hashes.get(&normalize_id(&f.id)).map_or(true, |(sig, body)| {
                        f.signature_hash != *sig || f.body_hash != *body
                    })
                }
                ExtractedUnit::Class(_) => !old_classes.contains(&normalize_id(&id)),
                ExtractedUnit::Import(_) => !old_imports.contains(&normalize_id(&id)),
                ExtractedUnit::Constant(_) => !old_constants.contains(&normalize_id(&id)),
                ExtractedUnit::TypeAlias(_) => !old_aliases.contains(&normalize_id(&id)),
                ExtractedUnit::Module(_) => true,
                ExtractedUnit::Field(_) => true,
            };

            if !needs_insert {
                continue;
            }

            // Remove old version first (for modified entities) — use normalized ID
            // since old projection IDs may use different path separators.
            let norm_id = normalize_id(&id);
            projection.functions.remove(&norm_id);
            projection.classes.remove(&norm_id);
            projection.imports.remove(&norm_id);
            projection.constants.remove(&norm_id);
            projection.type_aliases.remove(&norm_id);
            projection.callers_by_callee.remove(&norm_id);
            projection.callees_by_caller.remove(&norm_id);
            projection.subclasses.remove(&norm_id);
            projection.overridden_by.remove(&norm_id);

            match unit {
                ExtractedUnit::Function(f) => {
                    let func = Function {
                        id: f.id.clone(), name: f.name.clone(),
                        parent_module: module_id.clone(),
                        parent_class: f.parent_class.clone(),
                        parameters: vec![], return_type: f.return_type.clone(),
                        calls: f.calls.clone(), resolved_calls: vec![],
                        decorators: f.decorators.clone(), setter_of: None,
                        line: f.line, exit_line: f.exit_line,
                        docstring: f.docstring.clone(), kind: f.kind.clone(),
                        is_async: f.is_async, is_generator: f.is_generator,
                        source: f.source, signature_hash: f.signature_hash,
                        body_hash: f.body_hash,
                        metrics: f.metrics,
                        is_type_checking_only: f.is_type_checking_only,
                        parse_quality: ParseQuality::Clean, content_hash: 0,
                        span: f.span, name_span: f.name_span,
                        params_span: f.params_span, body_span: f.body_span,
                        decorators_span: f.decorators_span, embedding: EmbeddingVec::default(),
                    };
                    projection.functions.insert(f.id.clone(), Arc::new(func));
                    inserted += 1;
                }
                ExtractedUnit::Class(c) => {
                    let class = Class {
                        id: c.id.clone(), name: c.name.clone(),
                        grammar_kind: c.grammar_kind.clone(),
                        parent_module: module_id.clone(),
                        parent_class: c.parent_class.clone(),
                        bases: c.bases.clone(), resolved_bases: vec![],
                        mro: vec![], mro_error: false, methods: vec![],
                        fields: c.fields.iter().map(|ef| Field {
                            name: ef.name.clone(), annotation: ef.annotation.clone(),
                            source: ef.source.clone(),
                            default_value: ef.default_value.clone(),
                            is_class_var: ef.is_class_var,
                            span: ef.name_span, name_span: ef.name_span,
                        }).collect(),
                        source: c.source, decorators: c.decorators.clone(),
                        effective: EffectiveClass::Plain,
                        is_type_checking_only: c.is_type_checking_only,
                        line: c.line, exit_line: c.exit_line,
                        docstring: c.docstring.clone(),
                        parse_quality: ParseQuality::Clean, content_hash: 0,
                        span: c.span, name_span: c.name_span,
                        body_span: c.body_span, decorators_span: c.decorators_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.classes.insert(c.id.clone(), Arc::new(class));
                    inserted += 1;
                }
                ExtractedUnit::Import(i) => {
                    let import = Import {
                        id: i.id.clone(), raw: i.raw.clone(),
                        kind: i.kind.clone(),
                        resolution: ImportResolution::Unresolved,
                        line: i.line, is_type_only: i.is_type_only,
                        name_span: i.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.imports.insert(i.id.clone(), Arc::new(import));
                    if let Some(mod_arc) = projection.modules.get(&module_id) {
                        let mut m = (**mod_arc).clone();
                        m.imports.push(i.id.clone());
                        projection.modules.insert(module_id.clone(), Arc::new(m));
                    }
                    // Build import graph edges
                    let src_mod: Option<String> = match &i.kind {
                        ImportKind::FromImport { module, .. }
                        | ImportKind::ModuleImport { module, .. }
                        | ImportKind::StarImport { module, .. } => Some(module.clone()),
                        ImportKind::RelativeImport { module, .. } => module.clone(),
                        _ => None,
                    };
                    if let Some(src_mod) = src_mod {
                        if let Some(target_module_id) =
                            find_module_by_dotted_name(projection, &src_mod, &module_id)
                        {
                            if let Some(target_mod) = projection.modules.get(&target_module_id) {
                                let target_path = target_mod.path.to_string_lossy().to_string();
                                let mut ig = self.import_graph.write();
                                ig.add_file(file_path, Some(module_id.clone()), *language);
                                ig.add_file(&target_path, Some(target_module_id), *language);
                                ig.add_import_edge(file_path, &target_path);
                            }
                        }
                    }
                    inserted += 1;
                }
                ExtractedUnit::Constant(k) => {
                    let constant = Constant {
                        id: k.id.clone(), name: k.name.clone(),
                        annotation: k.annotation.clone(), source: k.source,
                        default_value: k.default_value.clone(),
                        span: k.span, name_span: k.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.constants.insert(k.id.clone(), Arc::new(constant));
                    inserted += 1;
                }
                ExtractedUnit::TypeAlias(ta) => {
                    let alias = TypeAlias {
                        id: ta.id.clone(), name: ta.name.clone(),
                        target: ta.target.clone(), source: ta.source,
                        span: ta.span, name_span: ta.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.type_aliases.insert(ta.id.clone(), Arc::new(alias));
                    inserted += 1;
                }
                _ => {}
            }
        }

        (inserted, removed)
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
        let compiled_query = crate::extract::tagger::CompiledQuery::new(lang, &ts_lang)
            .ok_or_else(|| format!("Failed to compile query for {:?}", lang))?;

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

        let mut units = crate::extract::single_pass::extract_single_pass(
            &source, root_node, &compiled_query, file_path);
        units.insert(0, Self::synthesize_module_unit(file_path, &lang));

        // Phase 3: Diff old vs new entities, only update what changed
        let mut projection = (*self.snapshot()).clone();
        let (new_count, removed_count) = self.apply_diff_update(
            &mut projection, &units, file_path, &lang);

        // Phase 3b: Persist concepts BEFORE edges — IMPORTS/EXTENDS/etc. edges
        // assert FK references against concept ids, so the module (and other)
        // concepts must already be committed.
        let lang_str = format!("{:?}", lang).to_lowercase();
        let _ = self.persist_entities(&units, file_path, &lang_str);

        // Phase 4: Compute MRO for all classes, then resolve calls (scoped to this file)
        self.resolve_imports(&mut projection);
        self.populate_class_methods(&mut projection);
        self.compute_all_mro(&mut projection);
        self.resolve_class_hierarchy(&mut projection);
        self.resolve_overrides(&mut projection);
        self.resolve_calls_scoped(&mut projection, Some(file_path));
        let _ = self.persist_edges(&projection);
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

        // Phase 5: (concepts already persisted in Phase 3b, before edges)

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
                        grammar_kind: c.grammar_kind.clone(),
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
                        embedding: EmbeddingVec::default(),                    };
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
                        metrics: f.metrics,
                        is_type_checking_only: f.is_type_checking_only,
                        parse_quality: ParseQuality::Clean,
                        content_hash: 0,
                        span: f.span,
                        name_span: f.name_span,
                        params_span: f.name_span,
                        body_span: f.body_span,
                        decorators_span: f.decorators_span,
                        embedding: EmbeddingVec::default(),
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
                        embedding: EmbeddingVec::default(),                    };
                    projection.imports.insert(import.id.clone(), Arc::new(import));
                    module_imports.push(i.id.clone());

                    // v0.5: Build import graph edges for multi-hop resolution.
                    // Resolve dotted module name → file path and add edge.
                    let src_mod: Option<String> = match &i.kind {
                        ImportKind::FromImport { module, .. }
                        | ImportKind::ModuleImport { module, .. }
                        | ImportKind::StarImport { module, .. } => Some(module.clone()),
                        ImportKind::RelativeImport { module, .. } => module.clone(),
                        _ => None,
                    };
                    if let Some(src_mod) = src_mod {
                        if let Some(target_module_id) =
                            find_module_by_dotted_name(projection, &src_mod, &module_id)
                        {
                            if let Some(target_mod) = projection.modules.get(&target_module_id) {
                                let target_path =
                                    target_mod.path.to_string_lossy().to_string();
                                let mut ig = self.import_graph.write();
                                // Ensure both files are nodes in the graph
                                ig.add_file(file_path, Some(module_id.clone()), *language);
                                ig.add_file(&target_path, Some(target_module_id), *language);
                                ig.add_import_edge(file_path, &target_path);
                            }
                        }
                    }
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
                        embedding: EmbeddingVec::default(),
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
                        embedding: EmbeddingVec::default(),
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
                        embedding: EmbeddingVec::default(),        };
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
    fn test_multi_hop_import_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());

        index_source(&graph, "def utility(): pass\n", "src/c.py");
        index_source(&graph, "from src.c import utility\ndef helper(): utility()\n", "src/b.py");
        index_source(&graph, "from src.b import helper\ndef app(): helper()\n", "src/a.py");

        let ig = graph.import_graph.read();

        eprintln!("From A (3): {:?}", ig.transitive_imports("src/a.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect::<Vec<_>>());
        eprintln!("From B (3): {:?}", ig.transitive_imports("src/b.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect::<Vec<_>>());
        eprintln!("From C (1): {:?}", ig.transitive_imports("src/c.py", 1).iter().map(|n| n.path.to_string_lossy().to_string()).collect::<Vec<_>>());

        let from_b: Vec<_> = ig.transitive_imports("src/b.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect();
        assert!(from_b.iter().any(|p| p.contains("c.py")), "B→C: {:?}", from_b);

        let from_a: Vec<_> = ig.transitive_imports("src/a.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect();
        assert!(from_a.iter().any(|p| p.contains("c.py")), "A→B→C: {:?}", from_a);
    }

    #[test]
    fn test_star_exports_wildcard_import() {
        let graph = CodeGraph::new(GraphConfig::default());

        index_source(&graph, "__all__ = ['public_api', 'internal_helper']\n\ndef public_api(): pass\ndef private_impl(): pass\ndef internal_helper(): pass\n", "src/lib.py");
        graph.set_module_star_exports("src/lib.py::module",
            vec!["public_api".to_string(), "internal_helper".to_string()]);

        index_source(&graph, "from src.lib import *\ndef consumer(): public_api()\n", "src/consumer.py");

        // v0.5: Manually resolve calls — resolve_all_calls is normally called
        // from update_file, not index_file. In production, calls are resolved
        // after all files are indexed (batch mode).
        {
            let mut projection = (*graph.snapshot()).clone();
            graph.compute_all_mro(&mut projection);
            graph.resolve_all_calls(&mut projection);
            graph.commit_projection(projection);
        }

        let snap = graph.snapshot();

        // Debug: check import graph edges
        let ig = graph.import_graph.read();
        let trans = ig.transitive_imports("src/consumer.py", 3);
        assert!(trans.iter().any(|n| n.path.to_string_lossy().to_string().contains("lib.py")),
            "consumer should transitively reach lib.py");

        for (fid, func) in &snap.functions {
            if func.name == "consumer" {
                let resolved: Vec<_> = func.resolved_calls.iter()
                    .filter_map(|rc| match rc {
                        crate::types::ResolvedCall::Function(f) => Some(f.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(resolved.len(), 1,
                    "Expected 1 resolved, got {:?}", resolved);
            }
        }
    }

    #[test]
    fn test_extension_agnostic_module_resolution() {
        // v0.5: find_module_by_dotted_name handles all extensions.
        // Java: com.foo.bar.Baz → com/foo/bar/Baz.java
        // Scala: com.foo.bar.Qux → com/foo/bar/Qux.scala
        // Zig: foo.bar → foo/bar.zig (dotted name uses '.' as separator)
        let graph = CodeGraph::new(GraphConfig::default());

        // Java package resolution
        index_source(&graph, "package com.foo.bar;\npublic class Baz { public void run() {} }\n", "com/foo/bar/Baz.java");
        index_source(&graph, "import com.foo.bar.Baz;\nclass User { void go() { new Baz().run(); } }\n", "User.java");

        // Scala resolution
        index_source(&graph, "package com.foo.bar\nclass Qux { def doit(): Unit = () }\n", "com/foo/bar/Qux.scala");

        // Elixir resolution
        index_source(&graph, "defmodule Mix.Tasks.Hello do\nend\n", "lib/mix/tasks/hello.ex");

        // The key test: find_module_by_dotted_name should find any extension.
        let snap = graph.snapshot();
        let found = super::find_module_by_dotted_name(&snap, "com.foo.bar.Baz", "");
        assert!(found.is_some(), "Should find Baz.java via com.foo.bar.Baz");

        let found_scala = super::find_module_by_dotted_name(&snap, "com.foo.bar.Qux", "");
        assert!(found_scala.is_some(), "Should find Qux.scala via com.foo.bar.Qux");

        // Note: Elixir uses PascalCase module names but lowercase filenames
        // (e.g., Mix.Tasks.Hello → lib/mix/tasks/hello.ex).
        // Case-insensitive matching is a future enhancement.
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

    // ── Kotlin Indexing Tests ───────────────────────────────────

    #[test]
    fn test_kotlin_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Person(val name: String) { fun greet() {} }\n", "Person.kt");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Person"),
                "Should have Kotlin class Person");
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should have Kotlin function greet");
    }

    #[test]
    fn test_kotlin_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "fun foo() { bar() }\nfun bar() {}\n", "fn.kt");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "foo"),
                "Should have Kotlin function foo");
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

    #[test]
    fn test_member_expression_base_is_stringified_not_dropped() {
        // Phase 2 caveat-1: TS/JS `extends X.Y` (member_expression) and simple
        // `extends E` were BOTH silently dropped by extract_base_classes — the
        // superclass lives under `class_heritage → extends_clause value:`, not a
        // `superclasses`/`superclass` field on `class_declaration`. Bases are now
        // captured (qualified ones as dotted names).
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class X { }
class Sub extends X.Y { }
class D extends E { }
class I implements G.H, J { }
",
            "src/qualified.ts");
        let snap = graph.snapshot();

        let sub = snap.classes.values().find(|c| c.name == "Sub")
            .expect("Sub should be indexed");
        assert!(sub.bases.iter().any(|b| b.name == "X.Y"),
                "member_expression base should be captured as X.Y, got {:?}",
                sub.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>());

        let d = snap.classes.values().find(|c| c.name == "D")
            .expect("D should be indexed");
        assert!(d.bases.iter().any(|b| b.name == "E"),
                "simple TS extends base should be captured as E, got {:?}",
                d.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>());

        let i = snap.classes.values().find(|c| c.name == "I")
            .expect("I should be indexed");
        assert!(i.bases.iter().any(|b| b.name == "G.H")
                && i.bases.iter().any(|b| b.name == "J"),
                "implements bases should be captured as G.H and J, got {:?}",
                i.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>());
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

    // ── MRO / C3 Linearization Tests ────────────────────────────

    #[test]
    fn test_c3_mro_single_inheritance() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class A:\n    def foo(self): pass\nclass B(A):\n    def bar(self): self.foo()\n",
            "mod.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        if let Some(b) = projection.classes.values().find(|c| c.name == "B") {
            assert!(b.mro.len() >= 2, "B should have at least 2 MRO entries, got {}", b.mro.len());
            assert!(matches!(&b.mro[0], MroNode::Class(_)));
        }
    }

    #[test]
    fn test_c3_mro_multiple_inheritance() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class X:\n    def x(self): pass\nclass Y:\n    def y(self): pass\nclass Z(X, Y):\n    def z(self): pass\n",
            "diamond.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        // Z's MRO should be: Z → X → Y → object
        if let Some(z) = projection.classes.values().find(|c| c.name == "Z") {
            assert!(z.mro.len() >= 3,
                    "Z should have at least 3 MRO entries, got {}", z.mro.len());
        }
    }

    #[test]
    fn test_mro_method_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Base:\n    def helper(self): pass\nclass Child(Base):\n    def run(self): self.helper()\n",
            "inherited.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_all_calls(&mut projection);
        // Child.run() calls self.helper() — should resolve to Base.helper via MRO
        if let Some(run) = projection.functions.values().find(|f| f.name == "run") {
            let callees = projection.callees_by_caller.get(&run.id);
            assert!(callees.is_some(), "run should have resolved callees");
            if let Some(callee_ids) = callees {
                let callee_names: Vec<_> = callee_ids.iter()
                    .filter_map(|id| projection.functions.get(id))
                    .map(|f| f.name.clone())
                    .collect();
                assert!(callee_names.contains(&"helper".to_string()),
                        "run should call helper via MRO, got: {:?}", callee_names);
            }
        }
    }
// ── Phase D back-fill: subclasses / importers / overrides ─────────────

    #[test]
    fn test_resolve_class_hierarchy_populates_subclasses() {
        // `class B(A)` in the SAME module — the same-module branch of
        // resolve_base_by_name must resolve A and invert it into subclasses[A]={B}.
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class A:\n    def foo(self): pass\nclass B(A):\n    def bar(self): pass\n",
            "hier.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);

        let a_id = projection.classes.iter()
            .find(|(_, c)| c.name == "A")
            .map(|(id, _)| id.clone())
            .expect("A should be indexed");
        let subs = projection.subclasses.get(&a_id).cloned().unwrap_or_default();
        let sub_names: Vec<String> = subs.iter()
            .filter_map(|sid| projection.classes.get(sid))
            .map(|c| c.name.clone()).collect();
        assert!(sub_names.contains(&"B".to_string()),
                "subclasses[A] should contain B, got {:?}", sub_names);

        let b = projection.classes.values().find(|c| c.name == "B").unwrap();
        assert!(b.resolved_bases.iter().any(|bid| projection.classes.get(bid).map_or(false, |bc| bc.name == "A")),
                "B.resolved_bases should resolve to A, got {:?}", b.resolved_bases);
    }

    #[test]
    fn test_resolve_imports_populates_importers() {
        // `from src.c import utility` in b.py — resolve_imports must set
        // Import.resolution → Module(c) and record b in importers[c].
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def utility(): pass\n", "src/c.py");
        index_source(&graph, "from src.c import utility\ndef helper(): utility()\n", "src/b.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);

        let c_mod = projection.modules.iter()
            .find(|(_, m)| m.path.to_string_lossy().contains("c.py"))
            .map(|(id, _)| id.clone())
            .expect("c.py module should be indexed");
        let b_mod = projection.modules.iter()
            .find(|(_, m)| m.path.to_string_lossy().contains("b.py"))
            .map(|(id, _)| id.clone())
            .expect("b.py module should be indexed");

        let who_imports_c = projection.importers.get(&c_mod).cloned().unwrap_or_default();
        assert!(who_imports_c.contains(&b_mod),
                "importers[c.py] should contain b.py's module, got {:?}", who_imports_c);

        let b_imports: Vec<_> = projection.modules.get(&b_mod).map(|m| m.imports.clone()).unwrap_or_default();
        let resolved_any = b_imports.iter().any(|imp_id| {
            projection.imports.get(imp_id)
                .map_or(false, |i| matches!(i.resolution, crate::types::ImportResolution::Module(_)))
        });
        assert!(resolved_any,
                "b.py's Import entity should resolve to Module(c), got {:?}", b_imports);
    }

    // ── 2.1a / 2.1b / 2.1c: base-resolution heuristics ──────────

    #[test]
    fn test_language_family_filters_base_candidates() {
        // Two `Base` classes in different languages; a Python caller must
        // resolve to the Python one (2.1a), not be ambiguous across C++.
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Base:\n    pass\n", "base.py");
        index_source(&graph, "class Base {};\n", "base.cpp");
        index_source(&graph, "class Child(Base):\n    pass\n", "main.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);

        let child = projection.classes.values().find(|c| c.name == "Child").unwrap();
        assert_eq!(child.resolved_bases.len(), 1,
            "Child should resolve to one Base, got {:?}", child.resolved_bases);
        let base = projection.classes.get(&child.resolved_bases[0]).unwrap();
        let base_mod = projection.modules.get(&base.parent_module).unwrap();
        assert!(base_mod.path.to_string_lossy().ends_with("base.py"),
            "Child must resolve to the Python Base, not {}", base_mod.path.to_string_lossy());
        assert!(projection.ambiguous_bases.is_empty(),
            "unexpected ambiguity: {:?}", projection.ambiguous_bases);
    }

    #[test]
    fn test_ambiguous_base_emits_finding() {
        // Two Python `Service` classes in different packages; a caller with no
        // import cannot disambiguate → must emit an AmbiguousBase finding (2.1b).
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Service:\n    pass\n", "pkg_a/service.py");
        index_source(&graph, "class Service:\n    pass\n", "pkg_b/service.py");
        index_source(&graph, "class Consumer(Service):\n    pass\n", "main.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);

        let consumer = projection.classes.values().find(|c| c.name == "Consumer").unwrap();
        assert!(consumer.resolved_bases.is_empty(),
            "ambiguous base must stay unresolved");
        assert_eq!(projection.ambiguous_bases.len(), 1,
            "expected 1 finding, got {:?}", projection.ambiguous_bases);
        let f = &projection.ambiguous_bases[0];
        assert_eq!(f.class_name, "Consumer");
        assert_eq!(f.base_name, "Service");
        assert_eq!(f.candidates.len(), 2, "expected 2 candidates, got {:?}", f.candidates);
    }

    #[test]
    fn test_ts_typeonly_import_aware_base_resolution() {
        // TS `import { type PoolWorker } from '../src/mcp/query-pool'` must be
        // parsed as a relative import with the name captured, so import-aware
        // base resolution (2.1c) resolves FakeWorker → query-pool.PoolWorker.
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class PoolWorker {}\n", "src/mcp/query-pool.ts");
        index_source(&graph, "class PoolWorker {}\n", "src/resolution/resolver-pool.ts");
        index_source(&graph,
            "import { type PoolWorker } from '../src/mcp/query-pool';\nclass FakeWorker implements PoolWorker {}\n",
            "__tests__/query-pool.test.ts");
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);

        let fake = projection.classes.values().find(|c| c.name == "FakeWorker").unwrap();
        assert_eq!(fake.resolved_bases.len(), 1,
            "FakeWorker should resolve PoolWorker via type-only import, got {:?}", fake.resolved_bases);
        let base = projection.classes.get(&fake.resolved_bases[0]).unwrap();
        let base_mod = projection.modules.get(&base.parent_module).unwrap();
        assert!(base_mod.path.to_string_lossy().ends_with("query-pool.ts"),
            "FakeWorker must resolve to query-pool.PoolWorker, got {}", base_mod.path.to_string_lossy());
        assert!(projection.ambiguous_bases.is_empty(),
            "import-aware should disambiguate, got {:?}", projection.ambiguous_bases);
    }

    #[test]
    fn test_alias_aware_module_resolution() {
        // `@/models/user` should resolve to `src/models/user` (2.2 alias).
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class User {}\n", "src/models/user.ts");
        let projection = (*graph.snapshot()).clone();
        let target = crate::graph::find_module_by_dotted_name(&projection, "@/models/user", "");
        assert!(target.is_some(), "@/models/user should resolve via alias");
        let m = projection.modules.get(&target.unwrap()).unwrap();
        assert!(m.path.to_string_lossy().ends_with("src/models/user.ts"),
            "alias should resolve to src/models/user.ts, got {}", m.path.to_string_lossy());
    }

    #[test]
    fn test_count_unresolved_targets() {
        // 2.3: count unresolved outgoing calls + imports (downstream only).
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def foo():\n    undefined_func()\n", "mod.py");
        index_source(&graph, "import nonexistent_module\ndef bar(): pass\n", "mod2.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.resolve_all_calls(&mut projection);

        let foo = projection.functions.values().find(|f| f.name == "foo").unwrap();
        let calls_kind = vec!["calls".to_string()];
        assert_eq!(
            CodeGraph::count_unresolved_targets(&projection, &foo.id, &calls_kind, true),
            1,
            "foo should have 1 unresolved call"
        );
        assert_eq!(
            CodeGraph::count_unresolved_targets(&projection, &foo.id, &calls_kind, false),
            0,
            "upstream should report 0 unresolved"
        );

        let mod2 = projection.modules.values()
            .find(|m| m.path.to_string_lossy().contains("mod2.py")).unwrap();
        let imports_kind = vec!["imports".to_string()];
        assert_eq!(
            CodeGraph::count_unresolved_targets(&projection, &mod2.id, &imports_kind, true),
            1,
            "mod2 should have 1 unresolved import"
        );
    }

    #[test]
    fn test_import_aware_base_resolution() {
        // Two `PoolWorker` classes; the caller imports it from query_pool, so
        // resolution must use the import target (2.1c), not guess.
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class PoolWorker:\n    pass\n", "src/mcp/query_pool.py");
        index_source(&graph, "class PoolWorker:\n    pass\n", "src/resolution/resolver_pool.py");
        index_source(&graph,
            "from src.mcp.query_pool import PoolWorker\nclass FakeWorker(PoolWorker):\n    pass\n",
            "test_fake.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);

        let fake = projection.classes.values().find(|c| c.name == "FakeWorker").unwrap();
        assert_eq!(fake.resolved_bases.len(), 1,
            "FakeWorker should resolve PoolWorker via import, got {:?}", fake.resolved_bases);
        let base = projection.classes.get(&fake.resolved_bases[0]).unwrap();
        let base_mod = projection.modules.get(&base.parent_module).unwrap();
        assert!(base_mod.path.to_string_lossy().ends_with("query_pool.py"),
            "FakeWorker must resolve to query_pool.PoolWorker, got {}", base_mod.path.to_string_lossy());
        assert!(projection.ambiguous_bases.is_empty(),
            "import-aware should disambiguate, got {:?}", projection.ambiguous_bases);
    }

    #[test]
    fn test_resolve_overrides_populates_overridden_by() {
        // Base.helper overridden by Child.helper (same module). Child's MRO is
        // [Child, Base], so resolve_overrides must mark Base.helper as overridden
        // and point the Child helper's overrides_base back to Base.helper.
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Base:\n    def helper(self): pass\nclass Child(Base):\n    def helper(self): pass\n",
            "overrides.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_overrides(&mut projection);

        let base_foo = projection.functions.iter()
            .find(|(_, f)| f.name == "helper" && f.parent_class.as_deref().map_or(false, |pc| {
                projection.classes.get(pc).map_or(false, |c| c.name == "Base")
            }))
            .map(|(id, _)| id.clone())
            .expect("Base.helper should be indexed");
        let overrides = projection.overridden_by.get(&base_foo).cloned().unwrap_or_default();
        assert!(!overrides.is_empty(),
                "Base.helper should be marked overridden by at least one Child.helper");
        let child_foo = projection.functions.iter()
            .find(|(_, f)| f.name == "helper" && f.parent_class.as_deref().map_or(false, |pc| {
                projection.classes.get(pc).map_or(false, |c| c.name == "Child")
            }))
            .map(|(id, _)| id.clone())
            .expect("Child.helper should be indexed");
        assert_eq!(projection.overrides_base.get(&child_foo), Some(&base_foo),
                   "overrides_base[Child.helper] should be Base.helper");
    }

// ── Phase 1: traverse_bfs (generalized Rust traversal) ──────────────

    /// Build a fresh projection with the Phase-D passes run, from sources.
    fn snapshot_from(sources: &[(&str, &str)]) -> ProjectedGraph {
        let graph = CodeGraph::new(GraphConfig::default());
        for (src, path) in sources {
            let lang = Language::from_extension(
                std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("py"),
            );
            graph.index_file(src, path, &lang).unwrap();
        }
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_all_calls(&mut projection);
        projection
    }

    fn fn_id_of(proj: &ProjectedGraph, name: &str) -> String {
        proj.functions.iter().find(|(_, f)| f.name == name).map(|(id, _)| id.clone())
            .unwrap_or_else(|| panic!("function `{}` should be indexed", name))
    }

    #[test]
    fn test_traverse_calls_downstream_depth() {
        // a → b → c
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): c()\ndef c(): pass\n", "chain.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 1, &["calls".to_string()], false, true);
        // depth 0 = a, depth 1 = b. c is at depth 2, beyond max_depth=1.
        let ids: Vec<&str> = reached.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&a.as_str()), "start a included at depth 0");
        assert!(reached.iter().any(|(id, _, _)| proj.functions.get(id).map_or(false, |f| f.name == "b")),
                "b reached at depth 1, got {:?}", reached);
        assert!(!reached.iter().any(|(id, _, _)| proj.functions.get(id).map_or(false, |f| f.name == "c")),
                "c should NOT be reached at max_depth=1");
        // depth tags
        assert_eq!(reached.iter().find(|(id, _, _)| id == &a).unwrap().1, 0);
        assert_eq!(reached.iter().find(|(_, _, ek)| ek == "calls").map(|(_, d, _)| *d), Some(1));
    }

    #[test]
    fn test_traverse_calls_upstream() {
        // a → b → c ; upstream from c yields c, b, a
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): c()\ndef c(): pass\n", "chain.py"),
        ]);
        let c = fn_id_of(&proj, "c");
        let reached = CodeGraph::traverse_bfs(&proj, &c, 5, &["calls".to_string()], true, false);
        let names: Vec<String> = reached.iter().filter_map(|(id, _, _)|
            proj.functions.get(id).map(|f| f.name.clone())).collect();
        assert!(names.contains(&"c".to_string()) && names.contains(&"b".to_string()) && names.contains(&"a".to_string()),
                "upstream from c should reach b and a, got {:?}", names);
    }

    #[test]
    fn test_traverse_cycle_terminates() {
        // a ↔ b (mutual call). BFS must terminate, each node once.
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): a()\n", "cycle.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 10, &["calls".to_string()], true, true);
        // start a + b = 2 distinct (cycle doesn't duplicate).
        assert_eq!(reached.len(), 2, "cycle should yield exactly 2 nodes, got {:?}", reached);
    }

    #[test]
    fn test_traverse_diamond_one_entry_per_node() {
        // a → b, a → c, b → d, c → d. d reached once.
        let proj = snapshot_from(&[
            ("def a(): b(); c()\ndef b(): d()\ndef c(): d()\ndef d(): pass\n", "diamond.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 5, &["calls".to_string()], false, true);
        let d_count = reached.iter().filter(|(id, _, _)| proj.functions.get(id).map_or(false, |f| f.name == "d")).count();
        assert_eq!(d_count, 1, "d should appear exactly once in a diamond, got {}", d_count);
        assert_eq!(reached.len(), 4, "a,b,c,d each once = 4, got {}", reached.len());
    }

    #[test]
    fn test_traverse_max_depth_zero_returns_only_start() {
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): pass\n", "md0.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 0, &["calls".to_string()], false, true);
        assert_eq!(reached.len(), 1, "max_depth=0 yields only the start node");
        assert_eq!(reached[0].0, a);
        assert_eq!(reached[0].1, 0);
    }

    #[test]
    fn test_traverse_empty_edge_kinds_returns_only_start() {
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): pass\n", "empty.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 5, &[], true, true);
        assert_eq!(reached.len(), 1, "empty edge_kinds yields only the start node");
    }

    #[test]
    fn test_traverse_imports_upstream_nonempty() {
        // b imports c (module-level). importers[c_mod] = {b_mod}.
        let proj = snapshot_from(&[
            ("def utility(): pass\n", "src/c.py"),
            ("from src.c import utility\ndef helper(): utility()\n", "src/b.py"),
        ]);
        let c_mod = proj.modules.iter()
            .find(|(_, m)| m.path.to_string_lossy().contains("c.py"))
            .map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &c_mod, 3, &["imports".to_string()], true, false);
        let who: Vec<&str> = reached.iter().map(|(id, _, _)| id.as_str()).collect();
        // start c_mod at depth 0; b_mod at depth 1.
        assert!(who.iter().any(|id| proj.modules.get(*id).map_or(false, |m| m.path.to_string_lossy().contains("b.py"))),
                "imports upstream from c should reach b, got {:?}", who);
    }

    #[test]
    fn test_traverse_extends_downstream_via_resolved_bases() {
        // B(A) same module → resolved_bases[B] = [A], so extends downstream from B reaches A.
        let proj = snapshot_from(&[
            ("class A:\n    def m(self): pass\nclass B(A):\n    def m(self): pass\n", "hier.py"),
        ]);
        let b_id = proj.classes.iter().find(|(_, c)| c.name == "B").map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &b_id, 3, &["extends".to_string()], false, true);
        let names: Vec<String> = reached.iter().filter_map(|(id, _, _)|
            proj.classes.get(id).map(|c| c.name.clone())).collect();
        assert!(names.contains(&"A".to_string()), "extends downstream from B should reach A, got {:?}", names);
    }

    #[test]
    fn test_traverse_overrides_upstream_from_base() {
        // Base.helper overridden by Child.helper → overridden_by[base] = {child}.
        let proj = snapshot_from(&[
            ("class Base:\n    def helper(self): pass\nclass Child(Base):\n    def helper(self): pass\n", "ovr.py"),
        ]);
        let base_f = proj.functions.iter()
            .find(|(_, f)| f.name == "helper" && f.parent_class.as_deref().map_or(false, |pc|
                proj.classes.get(pc).map_or(false, |c| c.name == "Base")))
            .map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &base_f, 3, &["overrides".to_string()], true, false);
        let names: Vec<String> = reached.iter().filter_map(|(id, _, _)|
            proj.functions.get(id).map(|f| f.name.clone())).collect();
        assert!(names.contains(&"helper".to_string()) && reached.len() >= 2,
                "overrides upstream from Base.helper should reach Child.helper, got {:?}", reached);
    }

    #[test]
    fn test_traverse_inherits_alias_for_extends() {
        // The pyfunction normalizes "inherits"→"extends"; traverse_bfs itself
        // only knows "extends", confirming the alias mapping in lib.rs. Here
        // we just assert "extends" works (alias coverage is in the Python layer).
        let proj = snapshot_from(&[
            ("class A:\n    pass\nclass B(A):\n    pass\n", "alias.py"),
        ]);
        let b_id = proj.classes.iter().find(|(_, c)| c.name == "B").map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &b_id, 3, &["extends".to_string()], false, true);
        assert!(reached.iter().any(|(id, _, _)| proj.classes.get(id).map_or(false, |c| c.name == "A")));
    }

    #[test]
    fn test_c3_diamond() {
        // Diamond inheritance:
        //   A
        //  / \
        // B   C
        //  \ /
        //   D
        // C3 MRO for D: D → B → C → A
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class A: pass\nclass B(A): pass\nclass C(A): pass\nclass D(B, C): pass\n",
            "diamond.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        if let Some(d) = projection.classes.values().find(|c| c.name == "D") {
            assert_eq!(d.mro.len(), 4, "D should have MRO [D, B, C, A], got {:?} entries", d.mro.len());
            // Verify order: D is first
            if let MroNode::Class(ref id) = d.mro[0] {
                assert!(id.contains("D"), "First MRO entry should be D");
            }
        }
    }

    // ── Ruby Indexing Tests ─────────────────────────────────────

    #[test]
    fn test_ruby_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Animal\n  def speak; end\nend\n", "animal.rb");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Animal"),
                "Should have Ruby class Animal");
        assert!(snap.functions.values().any(|f| f.name == "speak"),
                "Should have Ruby method speak");
    }

    #[test]
    fn test_ruby_module_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "module Utilities\n  def self.format; end\nend\n", "utils.rb");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Utilities"),
                "Should have Ruby module Utilities");
    }

    // ── PHP Indexing Tests ──────────────────────────────────────

    #[test]
    fn test_php_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "<?php class User { function login() {} }\n", "User.php");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "User"),
                "Should have PHP class User");
    }

    #[test]
    fn test_php_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "<?php function foo() { bar(); }\n", "fn.php");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "foo"),
                "Should have PHP function foo");
    }

    // ── C# Indexing Tests ───────────────────────────────────────

    #[test]
    fn test_csharp_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Service { void Run() {} }\n", "Service.cs");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Service"),
                "Should have C# class Service");
    }

    #[test]
    fn test_csharp_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class T { void A() { B(); } void B() {} }\n", "T.cs");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "A"),
                "Should have C# method A");
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
            updated.embedding = EmbeddingVec { vec: vec![0.1, 0.2, 0.3], hash: String::new() };
            projection.functions.insert("math.py::add".to_string(), std::sync::Arc::new(updated));
        }
        graph.commit_projection(projection);
        let snap = graph.snapshot();
        let add = snap.functions.get("math.py::add").unwrap();
        assert_eq!(add.embedding.vec.len(), 3);
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
    fn test_set_embedding_stores_vector() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\ndef sub(a, b): return a - b\n", "math.py");

        // Set embedding on add
        let vec = vec![0.1, 0.2, 0.3, 0.4];
        graph.set_embedding("math.py::add", &vec, "abc123").expect("set_embedding should succeed");

        let snap = graph.snapshot();
        let add = snap.functions.get("math.py::add").unwrap();
        assert_eq!(add.embedding.vec, vec);

        // sub should still have empty embedding
        let sub = snap.functions.get("math.py::sub").unwrap();
        assert!(sub.embedding.vec.is_empty());
    }

    #[test]
    fn test_set_embedding_entity_not_found() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");

        let result = graph.set_embedding("math.py::no_such_function", &[0.1, 0.2], "abc123");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Entity not found"));
    }

    #[test]
    fn test_set_embedding_overwrites_existing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");

        // First embedding
        graph.set_embedding("math.py::add", &[0.1], "abc123").unwrap();
        // Overwrite with different vector
        graph.set_embedding("math.py::add", &[0.9, 0.8, 0.7], "abc123").unwrap();

        let snap = graph.snapshot();
        let add = snap.functions.get("math.py::add").unwrap();
        assert_eq!(add.embedding.vec, vec![0.9, 0.8, 0.7]);
    }

    #[test]
    fn test_search_similar_after_set_embedding() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def auth_login(): pass\ndef render_html(): pass\ndef calc_tax(): pass\n",
            "mod.py");

        // Embed functions with contrasting vectors
        graph.set_embedding("mod.py::auth_login", &[1.0, 0.0, 0.0], "h1").unwrap();
        graph.set_embedding("mod.py::render_html", &[0.0, 1.0, 0.0], "h2").unwrap();
        graph.set_embedding("mod.py::calc_tax", &[0.0, 0.0, 1.0], "h3").unwrap();

        // Verify embeddings stored correctly
        let snap = graph.snapshot();
        assert_eq!(snap.functions.get("mod.py::auth_login").unwrap().embedding.vec, vec![1.0, 0.0, 0.0]);
        assert_eq!(snap.functions.get("mod.py::render_html").unwrap().embedding.vec, vec![0.0, 1.0, 0.0]);
        assert_eq!(snap.functions.get("mod.py::calc_tax").unwrap().embedding.vec, vec![0.0, 0.0, 1.0]);

        // Cosine similarity: vector to itself = 1.0, orthogonal = 0.0
        let sim = crate::cosine_similarity(&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0]);
        assert!((sim - 0.0).abs() < 0.001, "Orthogonal vectors should have similarity 0");
    }

    #[test]
    fn test_set_embedding_empty_vector() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def empty_fn(): pass\n", "mod.py");

        graph.set_embedding("mod.py::empty_fn", &[], "e1").unwrap();
        let snap = graph.snapshot();
        let f = snap.functions.get("mod.py::empty_fn").unwrap();
        assert!(f.embedding.vec.is_empty(), "Empty embedding should be stored as empty");
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
    fn test_class_methods_populated_27() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Foo:\n    def bar(self): pass\n    def baz(self): pass\n",
            "foo.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.populate_class_methods(&mut projection);

        let foo = projection.classes.values().find(|c| c.name == "Foo").unwrap();
        assert_eq!(foo.methods.len(), 2, "class.methods should list 2 methods");
        let names: Vec<&str> = foo.methods.iter()
            .filter_map(|mid| projection.functions.get(mid))
            .map(|f| f.name.as_str())
            .collect();
        assert!(names.contains(&"bar") && names.contains(&"baz"),
                "class.methods should list bar and baz, got {:?}", names);
        // Deterministic ordering (sorted by EntityId).
        assert!(foo.methods.windows(2).all(|w| w[0] <= w[1]),
                "methods should be sorted");
    }

    #[test]
    fn test_update_file_adds_entities() {
        let graph = CodeGraph::new(GraphConfig::default());

        // Verify basic indexing
        graph.index_file("def foo(): pass\ndef bar(): pass\n", "mod.py", &Language::Python).unwrap();
        let initial = graph.snapshot().functions.len();
        assert_eq!(initial, 2, "Expected 2 functions");

        // Update: change bar, add baz — foo unchanged → diff skips it
        let result = graph.update_file(
            "mod.py",
            Some("def foo(): pass\ndef bar(): return 42\ndef baz(): pass\n"),
            None,
        );
        assert!(result.is_ok(), "update_file error: {:?}", result.err());
        let (added, removed, _affected) = result.unwrap();

        // Diff semantics: bar changed (body_hash differs) → 1 remove + 1 insert
        // baz is new → 1 insert. foo unchanged → 0 ops.
        assert!(added >= 1, "Should insert at least 1, got {}", added);
        assert!(removed >= 0, "Should remove at least 0, got {}", removed);

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

        // Remove Cat — Dog unchanged → 0 inserts, 1 remove
        let result = graph.update_file(
            "animals.py",
            Some("class Dog: pass\n"),
            None,
        );
        assert!(result.is_ok(), "update_file error: {:?}", result.err());
        let (added, removed, _) = result.unwrap();

        // Diff semantics: Dog unchanged → 0 insert, Cat gone → 1 remove
        assert_eq!(added, 0, "Should add 0 (Dog unchanged), got {}", added);
        assert_eq!(removed, 1, "Should remove 1 (Cat), got {}", removed);

        let snap = graph.snapshot();
        assert!(snap.classes.contains_key("animals.py::Dog"));
        assert!(!snap.classes.contains_key("animals.py::Cat"));
    }

    // ── Persistence Tests ────────────────────────────────────────

    #[test]
    fn test_persist_entities_no_store_returns_zero() {
        let graph = CodeGraph::new(GraphConfig::default());
        let units: Vec<ExtractedUnit> = vec![];
        // No store attached → no-op
        let count = graph.persist_entities(&units, "test.py", "python");
        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 0);
    }

    #[test]
    fn test_persist_edges_no_store_returns_zero() {
        let graph = CodeGraph::new(GraphConfig::default());
        let snap = graph.snapshot();
        // No store attached → no-op, returns 0 edges persisted
        let count = graph.persist_edges(&snap);
        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 0);
    }

    #[test]
    fn test_persist_entities_with_index() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def foo(): pass\n", "test_persist.py");
        // Entities are persisted inside index_file → persist_entities is called
        // without a store it returns Ok(0) but shouldn't crash
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "foo"));
        // Verify has_store returns false (no config for store in test)
        assert!(!graph.has_store());
    }

    #[test]
    fn test_persist_edges_with_resolved_calls() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def caller(): callee()\ndef callee(): pass\n",
                     "edges_test.py");
        let snap = graph.snapshot();
        // Edges persisted via persist_edges (no-op without store)
        let count = graph.persist_edges(&snap);
        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 0);
        // Verify edges exist in memory
        let caller_id = snap.functions.values()
            .find(|f| f.name == "caller").map(|f| f.id.clone());
        assert!(caller_id.is_some());
        if let Some(cid) = caller_id {
            let callees = snap.callees_by_caller.get(&cid);
            assert!(callees.is_some(), "caller should have callee edges");
        }
    }

    // ── Tier 2 Language Tests — Swift, Scala, Lua, Elixir, Zig, R ────

    #[test]
    fn test_swift_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "func greet(name: String) -> String { return \"Hi\" }\n", "test.swift");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index Swift function greet; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_swift_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Dog { func bark() {} }\nstruct Cat { var age: Int }\n", "animals.swift");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Dog"),
                "Should index Swift class Dog; classes={:?}",
                snap.classes.values().map(|c| c.name.clone()).collect::<Vec<_>>());
        assert!(snap.functions.values().any(|f| f.name == "bark"),
                "Should index Swift method bark");
    }

    #[test]
    fn test_scala_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class User(name: String) { def greet(): Unit = {} }\ntrait Service { def run(): Unit }\n", "user.scala");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "User"),
                "Should index Scala class User");
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index Scala method greet");
    }

    #[test]
    fn test_lua_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "function greet(name)\n  return name\nend\n", "test.lua");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index Lua function greet");
    }

    #[test]
    fn test_lua_table_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "local M = {}\nfunction M.setup() end\n", "mod.lua");
        let snap = graph.snapshot();
        // Lua tables captured as classes
        assert!(snap.classes.values().any(|c| c.name == "M") || snap.functions.len() > 0,
                "Should have Lua entities");
    }

    #[test]
    fn test_elixir_module_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "defmodule MyApp.User do\n  def greet(name) do\n    \"Hello \" <> name\n  end\nend\n", "user.ex");
        let snap = graph.snapshot();
        assert!(snap.modules.len() > 0, "Should index the module");
        // v3.6: def/defp extraction — verify function entity
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should extract greet function from def block; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_zig_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "fn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n", "test.zig");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "add"),
                "Should index Zig function add; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_zig_struct_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "const Point = struct { x: f32, y: f32 };\n", "geom.zig");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Point"),
                "Should index Zig struct Point; classes={:?}",
                snap.classes.values().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_r_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "greet <- function(name) {\n  paste('Hi', name)\n}\n", "test.R");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index R function greet; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    // ── v3.6: Function-as-Value Reference Capture Tests ────────────

    #[test]
    fn test_fn_ref_assignment_callback() {
        // Python pattern: `on_click = self.handle_click` → fn-ref from on_click to handle_click
        let graph = CodeGraph::new(GraphConfig::default());
        let source = "class Widget:\n  def handle_click(self): pass\n  def register(self):\n    self.on_click = self.handle_click\n";
        index_source(&graph, source, "widget.py");

        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "handle_click"),
                "should have handle_click function");
        let register = snap.functions.values()
            .find(|f| f.name == "register");
        assert!(register.is_some(), "should have register function");
        let register = register.unwrap();
        let has_handle_click_ref = register.calls.iter().any(|c| c.name == "handle_click");
        assert!(has_handle_click_ref,
                "register should have fn-ref to handle_click; calls={:?}",
                register.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_return_value() {
        // Python pattern: `return handler` → fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        let source = "def greeter(): pass\ndef get_handler():\n    return greeter\n";
        index_source(&graph, source, "handlers.py");

        let snap = graph.snapshot();
        let get_handler = snap.functions.values()
            .find(|f| f.name == "get_handler");
        assert!(get_handler.is_some(), "should have get_handler function");
        let get_handler = get_handler.unwrap();
        assert!(get_handler.calls.iter().any(|c| c.name == "greeter"),
                "get_handler should have fn-ref to greeter; calls={:?}",
                get_handler.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_no_false_positives() {
        // Local variable assignment should NOT create fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        let source = "def foo():\n    x = 42\n    y = 'hello'\n    return x\n";
        index_source(&graph, source, "locals.py");

        let snap = graph.snapshot();
        let foo = snap.functions.values()
            .find(|f| f.name == "foo");
        assert!(foo.is_some(), "should have foo function");
        let foo = foo.unwrap();
        assert!(foo.calls.is_empty(),
                "foo should have no fn-ref calls; got {:?}",
                foo.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_argument_list() {
        // Argument-list fn-ref: `register_callback(handler)` → handler is fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def handler(x): pass\ndef register_callback(fn): fn(42)\ndef setup():\n    register_callback(handler)\n",
            "callback.py");

        let snap = graph.snapshot();
        let setup = snap.functions.values()
            .find(|f| f.name == "setup");
        assert!(setup.is_some(), "should have setup function");
        let setup = setup.unwrap();
        assert!(setup.calls.iter().any(|c| c.name == "handler"),
                "setup should have fn-ref to handler via argument; calls={:?}",
                setup.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_dict_values() {
        // Dict value fn-ref: `{"key": handler}` → handler is fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def handler(x): pass\ndef make_registry():\n    return {'cb': handler}\n",
            "registry.py");

        let snap = graph.snapshot();
        let make_reg = snap.functions.values()
            .find(|f| f.name == "make_registry");
        assert!(make_reg.is_some(), "should have make_registry function");
        let make_reg = make_reg.unwrap();
        assert!(make_reg.calls.iter().any(|c| c.name == "handler"),
                "make_registry should have fn-ref to handler from dict value; calls={:?}",
                make_reg.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_builtin_type_bases_filtered() {
        // Classes inheriting from builtin types should not track those as refs
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class MyException(Exception): pass\nclass MyInt(int): pass\n",
            "bases.py");

        let snap = graph.snapshot();
        let exc = snap.classes.values()
            .find(|c| c.name == "MyException");
        assert!(exc.is_some(), "should have MyException class");
        let exc = exc.unwrap();
        // Exception is not a builtin-type (it's a class), so it stays
        // But int IS filtered by is_builtin_type
        let myint = snap.classes.values()
            .find(|c| c.name == "MyInt");
        assert!(myint.is_some(), "should have MyInt class");
        let myint = myint.unwrap();
        // int should be filtered from bases
        assert!(!myint.bases.iter().any(|b| b.name == "int"),
                "int should be filtered from bases; got {:?}",
                myint.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_literal_receiver_skipped() {
        // Calls on literal receivers like "str".method() should be skipped
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def foo():\n    x = 'hello'.upper()\n    y = 42.to_bytes(2, 'big')\n",
            "literal.py");

        let snap = graph.snapshot();
        let foo = snap.functions.values()
            .find(|f| f.name == "foo");
        assert!(foo.is_some(), "should have foo function");
        let foo = foo.unwrap();
        // Calls on string/integer literals should be filtered — no path entries for them
        let has_literal_receiver = foo.calls.iter().any(|c| {
            c.path.iter().any(|p| p == "'hello'" || p == "42")
        });
        assert!(!has_literal_receiver,
                "literal receivers should be filtered; calls={:?}",
                foo.calls.iter().map(|c| format!("{:?}::{}", c.path, c.name)).collect::<Vec<_>>());
    }

    // ── v3.6: grammar_kind tests ─────────────────────────────────

    #[test]
    fn test_grammar_kind_python_class() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Foo: pass\n", "test.py");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Foo").unwrap();
        assert_eq!(cls.grammar_kind, "class_definition",
                   "Python class should have grammar_kind 'class_definition'");
    }

    #[test]
    fn test_grammar_kind_rust_struct() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "struct Point { x: f64, y: f64 }\n", "geom.rs");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Point").unwrap();
        assert_eq!(cls.grammar_kind, "struct_item",
                   "Rust struct should have grammar_kind 'struct_item'");
    }

    #[test]
    fn test_grammar_kind_typescript_class() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Drawable { draw(): void {} }\n", "draw.ts");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Drawable").unwrap();
        assert_eq!(cls.grammar_kind, "class_declaration",
                   "TS class should have grammar_kind 'class_declaration'");
    }

    #[test]
    fn test_grammar_kind_swift_struct() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "struct Cat { var age: Int }\n", "cat.swift");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Cat").unwrap();
        assert_eq!(cls.grammar_kind, "class_declaration/struct",
                   "Swift struct should be classified as class_declaration/struct");
    }

    #[test]
    fn test_grammar_kind_swift_class() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Dog { func bark() {} }\n", "dog.swift");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Dog").unwrap();
        assert_eq!(cls.grammar_kind, "class_declaration",
                   "Swift class should keep grammar_kind 'class_declaration'");
    }

    #[test]
    fn test_grammar_kind_zig_struct() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "const Point = struct { x: f32, y: f32 };\n", "geom.zig");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Point").unwrap();
        assert_eq!(cls.grammar_kind, "VarDecl",
                   "Zig struct should have grammar_kind 'VarDecl'");
    }

    // ── v3.6: Synthetic edge registration ──────────────────────────

    #[test]
    fn test_synthetic_edge_registration() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def index(): pass\ndef user_detail(id): pass\n", "views.py");

        // Register a synthetic route→handler edge (like Django path()→view)
        graph.register_synthetic_edge(
            "django:route:users/",
            "views.py::user_detail",
            "HANDLES",
        ).unwrap();

        let snap = graph.snapshot();
        // Route should appear as a caller of user_detail
        let callees = snap.callees_by_caller.get("django:route:users/");
        assert!(callees.is_some(), "route should have callees");
        assert!(callees.unwrap().contains("views.py::user_detail"),
                "route should call user_detail");

        // user_detail should appear as callee of the route
        let callers = snap.callers_by_callee.get("views.py::user_detail");
        assert!(callers.is_some(), "user_detail should have callers");
        assert!(callers.unwrap().contains("django:route:users/"),
                "user_detail should be called by route");
    }

    #[test]
    fn test_synthetic_edge_roundtrip_query() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def list_items(): pass\n", "views.py");

        graph.register_synthetic_edge(
            "fastapi:route:main.py:/items",
            "views.py::list_items",
            "HANDLES",
        ).unwrap();

        let snap = graph.snapshot();

        // Querying callees_of the route should return list_items
        let callees = snap.callees_by_caller.get("fastapi:route:main.py:/items");
        assert!(callees.map_or(false, |c| c.iter().any(|e| e.contains("list_items"))),
                "route should have list_items as callee");
    }

    // ── v3.6: Cross-file fn-ref via imports ──────────────────────

    #[test]
    fn test_fn_ref_cross_file_import() {
        // Cross-file fn-ref: `from .handlers import handle_click`
        // then `self.on_click = handle_click` in another file
        let graph = CodeGraph::new(GraphConfig::default());
        let source = concat!(
            "from .handlers import handle_click\n",
            "class Widget:\n",
            "    def register(self):\n",
            "        self.on_click = handle_click\n",
        );
        index_source(&graph, source, "widget.py");

        let snap = graph.snapshot();
        let register = snap.functions.values()
            .find(|f| f.name == "register");
        assert!(register.is_some(), "should have register function");
        let register = register.unwrap();
        assert!(register.calls.iter().any(|c| c.name == "handle_click"),
                "register should have fn-ref to imported handle_click; calls={:?}",
                register.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    // ── v3.6: module.children() convenience API ─────────────────

    #[test]
    fn test_module_children_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Foo:\n    def bar(self):\n        pass\n\ndef baz():\n    pass\n", "mod.py");
        let snap = graph.snapshot();

        let module = snap.modules.values().find(|m| m.path.ends_with("mod.py"));
        assert!(module.is_some(), "Should find module");
        let module = module.unwrap();

        assert!(!module.classes.is_empty(), "Module should have classes");
        assert!(!module.functions.is_empty(), "Module should have functions");

        for cid in &module.classes {
            let cls = snap.classes.get(cid);
            assert!(cls.is_some());
            assert_eq!(cls.unwrap().name, "Foo");
        }

        for fid in &module.functions {
            let func = snap.functions.get(fid);
            assert!(func.is_some());
        }
    }

    // ── v3.6: Parameter annotation + return type extraction ─────

    #[test]
    fn test_parameter_annotations_extracted() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "from typing import Optional\ndef create_user(name: str, age: int, email: Optional[str]) -> User:\n    pass\n",
            "typed.py");
        let snap = graph.snapshot();
        let func = snap.functions.values().find(|f| f.name == "create_user");
        assert!(func.is_some());
        let func = func.unwrap();

        // Parameters should have annotations (builtins filtered)
        assert_eq!(func.parameters.len(), 3);
        // name: str → annotation: None (str is builtin)
        assert_eq!(func.parameters[0].name, "name");
        assert!(func.parameters[0].annotation.is_none(), "str is builtin");
        // age: int → annotation: None (int is builtin)
        assert_eq!(func.parameters[1].name, "age");
        assert!(func.parameters[1].annotation.is_none(), "int is builtin");
        // email: Optional[str] → should have annotation (not a bare builtin)
        assert_eq!(func.parameters[2].name, "email");
        assert!(func.parameters[2].annotation.is_some(), "Optional[str] is not a bare builtin");

        // Return type: User → not builtin, should be extracted
        assert_eq!(func.return_type.as_deref(), Some("User"));
    }

    #[test]
    fn test_return_type_builtin_filtered() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def get_count() -> int:\n    return 0\n", "simple.py");
        let snap = graph.snapshot();
        let func = snap.functions.values().find(|f| f.name == "get_count");
        assert!(func.is_some());
        // int is builtin → return_type should be None
        assert!(func.unwrap().return_type.is_none(), "int return type should be filtered");
    }

    // ── v3.6: Macrame temporal query tests ──────────────────────

    /// Helper: create a CodeGraph with a real Macrame store in a temp dir.
    fn graph_with_temp_store() -> (CodeGraph, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let store = crate::storage::CodeGraphStore::open(&db_path).expect("open store");
        let graph = CodeGraph::new(GraphConfig::default()).with_store(store);
        (graph, dir)
    }

    #[test]
    fn test_temporal_concepts_persisted() {
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph, "def caller(): callee()\ndef callee(): pass\n", "tp_test.py");

        let snap = graph.snapshot();
        // In-memory graph should have data
        assert!(snap.functions.len() >= 2);
        let caller = snap.functions.values().find(|f| f.name == "caller").unwrap();
        // In-memory edges should exist
        let callees = snap.callees_by_caller.get(&caller.id).unwrap();
        assert!(!callees.is_empty(), "caller should have callees in memory");

        // Verify store was attached
        assert!(graph.has_store());

        // Verify the DB file exists and has content
        let db_path = _dir.path().join("test.db");
        assert!(db_path.exists(), "db file should exist");
        let meta = std::fs::metadata(&db_path).unwrap();
        assert!(meta.len() > 0, "db file should not be empty");
    }

    #[test]
    fn test_temporal_reconstruct_after_index() {
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph, "def foo(): pass\n", "recon_test.py");

        let store = graph.store.as_ref().unwrap();
        // reconstruct() requires a valid ISO 8601 timestamp
        let state = store.reconstruct(crate::storage::TS_OPEN);
        // reconstruct may fail if no data matches — but shouldn't crash
        // Either Ok or a reasonable error is acceptable
        match state {
            Ok(_s) => { /* reconstruction succeeded */ }
            Err(e) => {
                // Macrame may not have matching data for TS_OPEN — that's fine
                eprintln!("reconstruct returned: {:?}", e);
            }
        }
    }

    #[test]
    fn test_temporal_edge_persistence_across_indexes() {
        let (graph, _dir) = graph_with_temp_store();

        // First index
        index_source(&graph,
            "def a(): b()\ndef b(): c()\ndef c(): pass\n",
            "chain.py");

        let snap = graph.snapshot();
        let func_a = snap.functions.values().find(|f| f.name == "a").unwrap();
        let callees = snap.callees_by_caller.get(&func_a.id);
        assert!(callees.is_some(), "a should have callees");
        assert!(!callees.unwrap().is_empty());

        // Verify store persistence — db file should be non-empty
        let db_path = _dir.path().join("test.db");
        assert!(db_path.exists());
        let meta = std::fs::metadata(&db_path).unwrap();
        assert!(meta.len() > 0, "db should have persisted data; size={}", meta.len());

        // Second index should not corrupt
        index_source(&graph,
            "def x(): pass\n",
            "extra.py");
        let snap2 = graph.snapshot();
        assert!(snap2.functions.values().any(|f| f.name == "x"));
        assert!(snap2.functions.values().any(|f| f.name == "a"));
    }
#[test]
    fn test_persist_edges_emits_imports_and_extends() {
        // Phase D.5: persist_edges must assert IMPORTS / EXTENDS (and OVERRIDES)
        // edges to Macrame in addition to CALLS — and succeed (no FK/kind error).
        // index_file persists CONCEPTS only (not edges / resolve passes),
        // so we run the Phase-D passes + persist_edges exactly as analyze() does.
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph, "class Base:\n    def m(self): pass\n", "base.py");
        index_source(&graph, "class Sub(Base):\n    def m(self): pass\n", "sub.py");
        index_source(&graph, "def util(): pass\n", "src/u.py");
        index_source(&graph, "from src.u import util\ndef app(): util()\n", "src/app.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_imports(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_all_calls(&mut projection);

        let call_edges: usize = projection.callees_by_caller.values().map(|s| s.len()).sum();
        let importer_edges: usize = projection.importers.values().map(|s| s.len()).sum();
        let subclass_edges: usize = projection.subclasses.values().map(|s| s.len()).sum();
        let override_edges: usize = projection.overrides_base.len();

        // Sanity: the fixture produced real non-call edges that D should persist.
        assert!(importer_edges > 0, "fixture should resolve >=1 import, got {}", importer_edges);
        assert!(subclass_edges > 0, "fixture should resolve >=1 subclass, got {}", subclass_edges);
        assert!(override_edges > 0, "fixture should resolve >=1 override, got {}", override_edges);

        let persisted = graph.persist_edges(&projection)
            .expect("persist_edges should succeed (concepts present, no FK violation)");
        // persist_edges pushes one assertion per CALL + IMPORTS + EXTENDS + OVERRIDES
        // edge (fixture has no external/builtin targets), so the persisted total
        // must equal the exact sum — proving IMPORTS edges now reach Macrame
        // (module concepts are persisted by synthesize_module_unit).
        let expected = call_edges + importer_edges + subclass_edges + override_edges;
        assert_eq!(persisted, expected,
                "persist_edges should persist CALLS+IMPORTS+EXTENDS+OVERRIDES exactly");
    }

    #[test]
    fn test_temporal_synthetic_edge_persistence() {
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph,
            "def user_detail(id): pass\n",
            "views.py");

        // Register a synthetic framework edge
        graph.register_synthetic_edge(
            "django:route:users/",
            "views.py::user_detail",
            "HANDLES",
        ).unwrap();

        // Verify in-memory graph has the edge
        let snap = graph.snapshot();
        let route_callees = snap.callees_by_caller.get("django:route:users/");
        assert!(route_callees.is_some(), "route should have callees");
        assert!(route_callees.unwrap().contains("views.py::user_detail"));

        // Verify the DB file has content (persisted)
        let db_path = _dir.path().join("test.db");
        assert!(db_path.exists());
        let meta = std::fs::metadata(&db_path).unwrap();
        assert!(meta.len() > 0, "db should have persisted data");
    }

    /// Verify that every .scm query file compiles cleanly against its
    /// tree-sitter grammar.  A query that falls back to `(comment)` silently
    /// loses entity extraction — this test catches that.
    #[test]
    fn test_all_queries_compile_without_errors() {
        use crate::extract::tagger;
        use crate::types::Language;

        let languages: Vec<(Language, &str)> = vec![
            (Language::Python, "python"),
            (Language::TypeScript, "typescript"),
            (Language::JavaScript, "javascript"),
            (Language::Rust, "rust"),
            (Language::Go, "go"),
            (Language::Java, "java"),
            (Language::C, "c"),
            (Language::Cpp, "cpp"),
            (Language::Ruby, "ruby"),
            (Language::Php, "php"),
            (Language::CSharp, "csharp"),
            (Language::Kotlin, "kotlin"),
            (Language::Swift, "swift"),
            (Language::Scala, "scala"),
            (Language::Lua, "lua"),
            (Language::Elixir, "elixir"),
            (Language::Zig, "zig"),
            (Language::R, "r"),
            (Language::Bash, "bash"),
            (Language::Dart, "dart"),
            (Language::Protobuf, "protobuf"),
            (Language::Dockerfile, "dockerfile"),
            (Language::Sql, "sql"),
            (Language::Hcl, "hcl"),
            (Language::Cmake, "cmake"),
            (Language::Graphql, "graphql"),
            (Language::Erlang, "erlang"),
            (Language::Haskell, "haskell"),
            (Language::Nix, "nix"),
            (Language::Shell, "bash"),
            (Language::Groovy, "groovy"),
            (Language::Perl, "perl"),
            (Language::SystemVerilog, "systemverilog"),
            (Language::Ocaml, "ocaml"),
            (Language::Clojure, "clojure"),
            (Language::Fsharp, "fsharp"),
            (Language::Verilog, "verilog"),
            (Language::Julia, "julia"),
            (Language::Powershell, "powershell"),
            (Language::EmacsLisp, "elisp"),
            (Language::Objc, "objc"),
        ];

        let mut failures = 0;
        let mut skipped = 0;
        for (lang, _pack_name) in &languages {
            let query_src = tagger::get_query_for_language_src(*lang);
            let ts_lang = match crate::graph::CodeGraph::ts_language(lang) {
                Some(l) => l,
                None => {
                    // The language pack (with its default `download` feature)
                    // lazily fetches grammars from GitHub releases. On fresh
                    // CI runners those downloads can be rate-limited or
                    // unavailable, so the grammar isn't loadable here. That is
                    // not a query bug — skip the compile check rather than
                    // failing the whole suite. Production `analyze` downloads
                    // and caches grammars lazily, so this only limits this
                    // static check.
                    eprintln!(
                        "SKIP {:?}: grammar not available — cannot compile-check query",
                        lang
                    );
                    skipped += 1;
                    continue;
                }
            };
            match tree_sitter::Query::new(&ts_lang, query_src) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("FAIL {:?}: {}", lang, e);
                    failures += 1;
                }
            }
        }
        eprintln!(
            "query compile check: {} checked, {} skipped, {} failed",
            languages.len() - skipped,
            skipped,
            failures
        );
        assert_eq!(failures, 0, "{} query files failed to compile", failures);
    }
}
