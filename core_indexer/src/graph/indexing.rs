use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::CodeGraph;
use crate::types::*;

impl CodeGraph {
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
    ///
    /// `root` is the parsed tree's root, so the module carries the file's real
    /// parse outcome: tree-sitter recovers from syntax errors rather than
    /// failing, so a file that does not parse still produces entities.
    pub(crate) fn synthesize_module_unit(
        file_path: &str,
        language: &Language,
        source: &str,
        root: tree_sitter::Node,
    ) -> ExtractedUnit {
        let stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        ExtractedUnit::Module(ExtractedModule {
            id: format!("{}::module", file_path),
            name: stem.to_string(),
            path: PathBuf::from(file_path),
            language: language.clone(),
            parse_quality: crate::extract::node_quality(root),
            content_hash: crate::extract::hash_span(source, 0, source.len()),
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
        units.insert(0, Self::synthesize_module_unit(file_path, language, source, root_node));

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
        // Carried onto the projected Module below — see insert_extracted.
        let mut module_quality = ParseQuality::Clean;
        let mut module_content_hash = 0u64;

        for unit in units {
            match unit {
                ExtractedUnit::Module(m) => {
                    module_quality = m.parse_quality;
                    module_content_hash = m.content_hash;
                }
                ExtractedUnit::Class(c) => {
                    let class = Class::from_extracted(
                        c, c.id.clone(), module_id.clone(), c.parent_class.clone());
                    projection.classes.insert(class.id.clone(), Arc::new(class));
                    module_classes.push(c.id.clone());
                }
                ExtractedUnit::Function(f) => {
                    let func = Function::from_extracted(
                        f, f.id.clone(), module_id.clone(), f.parent_class.clone());
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
            parse_quality: module_quality, file_version: 1, content_hash: module_content_hash,
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
        units.insert(0, Self::synthesize_module_unit(file_path, language, source, root_node));

        // Phase 3: Insert into ProjectedGraph
        let count = units.len();
        let mut projection = (*self.snapshot()).clone();
        self.insert_extracted(&mut projection, &units, file_path, language);
        self.commit_projection(projection);

        Ok((count, units))
    }
}
