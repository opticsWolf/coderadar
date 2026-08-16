use super::CodeGraph;
use crate::types::*;
use macrame::graph::EdgeAssertion;

impl CodeGraph {
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
            Ok(0)
        }
    }

    /// Persist call edges from the projection to the Macrame store.
    pub fn persist_edges(
        &self,
        projection: &ProjectedGraph,
    ) -> Result<usize, macrame::DbError> {
        let store = match self.store.as_ref() {
            Some(s) => s,
            None => return Ok(0),
        };

        let mut edge_count = 0usize;
        let mut batch: Vec<macrame::graph::EdgeAssertion> = Vec::new();
        let ts_now = crate::storage::now_iso8601();

        for (caller, callees) in projection.callees_by_caller.iter() {
            for callee in callees.iter() {
                // Skip edges where target is external/builtin (no concept entry)
                // and edges where the target would violate FK constraints
                if callee.starts_with("external::") || callee.starts_with("builtins.") {
                    continue;
                }
                batch.push(
                    EdgeAssertion::new(caller.as_str(), callee.as_str(), "CALLS")
                        .valid_from(ts_now.as_str())
                        .weight(1.0),
                );
                edge_count += 1;
                if batch.len() >= 200 {
                    store.assert_edges_bulk(std::mem::take(&mut batch))?;
                }
            }
        }

        // IMPORTS edges: importer module → imported target module.
        // Direction adopted project-wide: importer depends on target, so
        // source=importer, target=imported (the import “dependency”).
        // Modules are now persisted as Macrame concepts (synthesize_module_unit
        // prepends a Module unit in extract_only / index_file_inner / update_file),
        // so the FK target exists. External targets are still skipped.
        for (target_mod, importer_mods) in projection.importers.iter() {
            for importer in importer_mods.iter() {
                if importer.starts_with("external::")
                    || target_mod.starts_with("external::")
                {
                    continue;
                }
                batch.push(
                    EdgeAssertion::new(importer.as_str(), target_mod.as_str(), "IMPORTS")
                        .valid_from(ts_now.as_str())
                        .weight(1.0),
                );
                edge_count += 1;
                if batch.len() >= 200 {
                    store.assert_edges_bulk(std::mem::take(&mut batch))?;
                }
            }
        }

        // EXTENDS edges: subclass → base. `resolved_bases` holds only
        // concrete class ids (resolve_class_hierarchy discards externals),
        // so the FK guard is defensive.
        for (cid, class) in projection.classes.iter() {
            for base_id in class.resolved_bases.iter() {
                if cid.starts_with("external::") || base_id.starts_with("external::") {
                    continue;
                }
                batch.push(
                    EdgeAssertion::new(cid.as_str(), base_id.as_str(), "EXTENDS")
                        .valid_from(ts_now.as_str())
                        .weight(1.0),
                );
                edge_count += 1;
                if batch.len() >= 200 {
                    store.assert_edges_bulk(std::mem::take(&mut batch))?;
                }
            }
        }

        // OVERRIDES edges: override method → base method.
        for (override_fid, base_fid) in projection.overrides_base.iter() {
            if override_fid.starts_with("external::") || base_fid.starts_with("external::") {
                continue;
            }
            batch.push(
                EdgeAssertion::new(override_fid.as_str(), base_fid.as_str(), "OVERRIDES")
                    .valid_from(ts_now.as_str())
                    .weight(1.0),
            );
            edge_count += 1;
            if batch.len() >= 200 {
                store.assert_edges_bulk(std::mem::take(&mut batch))?;
            }
        }

        if !batch.is_empty() {
            store.assert_edges_bulk(batch)?;
        }

        Ok(edge_count)
    }

    /// v3.6: Register a synthetic edge from a framework resolver.
    ///
    /// Framework resolvers (Django, Flask, FastAPI) produce edges like
    /// route→handler, router→viewset, app→middleware. These are not
    /// tree-sitter-extracted but are merged into the graph so agents
    /// can trace them via callers_of / callees_of / explore.
    pub fn register_synthetic_edge(
        &self,
        source_id: &str,
        target_id: &str,
        kind: &str,
    ) -> Result<(), String> {
        self.register_synthetic_edges_bulk(vec![(
            source_id.to_string(),
            target_id.to_string(),
            kind.to_string(),
        )])
        .map(|_| ())
    }

    /// Register many synthetic edges against a single projection clone.
    ///
    /// The 13 framework resolvers call this once per route/handler edge, and
    /// each single-edge call used to clone the whole `ProjectedGraph`.
    /// Returns the number of edges registered.
    pub fn register_synthetic_edges_bulk(
        &self,
        edges: Vec<(String, String, String)>,
    ) -> Result<usize, String> {
        if edges.is_empty() {
            return Ok(0);
        }
        let mut projection = (*self.snapshot()).clone();
        for (source_id, target_id, _kind) in &edges {
            projection.callees_by_caller
                .entry(source_id.clone())
                .or_default()
                .insert(target_id.clone());
            projection.callers_by_callee
                .entry(target_id.clone())
                .or_default()
                .insert(source_id.clone());
        }
        self.commit_projection(projection);

        // Persist to Macrame if store attached
        if let Some(store) = self.store.as_ref() {
            let ts_now = crate::storage::now_iso8601();
            let batch: Vec<_> = edges
                .iter()
                .map(|(source_id, target_id, kind)| {
                    macrame::graph::EdgeAssertion::new(
                        source_id.as_str(), target_id.as_str(), kind.as_str())
                        .valid_from(ts_now.as_str())
                        .weight(1.0)
                })
                .collect();
            let _ = store.assert_edges_bulk(batch);
        }

        Ok(edges.len())
    }
}
