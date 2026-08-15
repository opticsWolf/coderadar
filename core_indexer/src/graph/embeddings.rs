use std::sync::Arc;

use super::CodeGraph;
use crate::types::*;

impl CodeGraph {
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
}
