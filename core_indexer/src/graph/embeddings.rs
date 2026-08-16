use std::sync::Arc;

use super::CodeGraph;
use crate::types::*;

impl CodeGraph {
    /// Store an embedding vector on any entity type.
    ///
    /// One call clones the whole projection, so embedding a project one entity
    /// at a time is O(N²) in Arc clones. Prefer [`set_embeddings_bulk`] for
    /// more than a handful of entities — this is the single-entity case of it.
    ///
    /// [`set_embeddings_bulk`]: CodeGraph::set_embeddings_bulk
    pub fn set_embedding(
        &self,
        entity_id: &str,
        embedding: &[f64],
        content_hash: &str,
    ) -> Result<(), String> {
        let entries = vec![(
            entity_id.to_string(),
            embedding.to_vec(),
            content_hash.to_string(),
        )];
        let (_, missing) = self.set_embeddings_bulk(entries);
        match missing.into_iter().next() {
            Some(id) => Err(format!("Entity not found: {}", id)),
            None => Ok(()),
        }
    }

    /// Store many embeddings against a single projection clone.
    ///
    /// Returns `(applied, missing_ids)`. `compute_embeddings` looped
    /// `set_embedding` over every entity, cloning the entire `ProjectedGraph`
    /// once per entity — 10,000 full clones on a 10k-entity project, which is
    /// the dominant cost of `coderadar init --with-embeddings`.
    pub fn set_embeddings_bulk(
        &self,
        entries: Vec<(String, Vec<f64>, String)>,
    ) -> (usize, Vec<String>) {
        if entries.is_empty() {
            return (0, vec![]);
        }
        let mut projection = (*self.snapshot()).clone();
        let mut applied = 0usize;
        let mut missing = Vec::new();

        for (entity_id, embedding, content_hash) in entries {
            let emb = EmbeddingVec { vec: embedding, hash: content_hash };
            if let Some(e) = projection.functions.get_mut(&entity_id) {
                Arc::make_mut(e).embedding = emb;
            } else if let Some(e) = projection.classes.get_mut(&entity_id) {
                Arc::make_mut(e).embedding = emb;
            } else if let Some(e) = projection.modules.get_mut(&entity_id) {
                Arc::make_mut(e).embedding = emb;
            } else if let Some(e) = projection.imports.get_mut(&entity_id) {
                Arc::make_mut(e).embedding = emb;
            } else if let Some(e) = projection.constants.get_mut(&entity_id) {
                Arc::make_mut(e).embedding = emb;
            } else if let Some(e) = projection.type_aliases.get_mut(&entity_id) {
                Arc::make_mut(e).embedding = emb;
            } else {
                missing.push(entity_id);
                continue;
            }
            applied += 1;
        }

        self.commit_projection(projection);
        (applied, missing)
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
        self.set_module_star_exports_bulk(vec![(module_id.to_string(), names)]);
    }

    /// Set `__all__` for many modules against a single projection clone.
    ///
    /// `analyze()` calls this once per module with `__all__`; one clone per
    /// module is the same O(N²) shape as [`set_embeddings_bulk`].
    ///
    /// [`set_embeddings_bulk`]: CodeGraph::set_embeddings_bulk
    pub fn set_module_star_exports_bulk(&self, entries: Vec<(String, Vec<String>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }
        let mut projection = (*self.snapshot()).clone();
        let mut applied = 0usize;
        for (module_id, names) in entries {
            if let Some(module) = projection.modules.get_mut(&module_id) {
                Arc::make_mut(module).star_exports = Some(names);
                applied += 1;
            }
        }
        self.commit_projection(projection);
        applied
    }
}
