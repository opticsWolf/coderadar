use super::CodeGraph;
use crate::types::*;
use macrame::graph::EdgeAssertion;

/// Ledger dedup key for an edge triple. `\u{0}` cannot appear in entity ids
/// or edge kinds, so the join is collision-free.
fn edge_key(source: &str, target: &str, kind: &str) -> String {
    format!("{source}\u{0}{target}\u{0}{kind}")
}

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
            // Insert-if-absent: the post-cascade v2 flush owns every id it
            // knows, so pre-cascade v1 rows exist only so a crashed run
            // leaves partial progress. Re-writing v1 for known ids would
            // alternate with v2 forever (different JSON shapes, notably
            // `resolved_calls`), minting two versions per analyze. New ids
            // still flush immediately; the v2 flush heals them to v2 shape
            // in the same run, or the next one after a crash.
            let fresh: Vec<crate::types::ExtractedUnit> = if units.is_empty() {
                Vec::new()
            } else {
                let ids: Vec<String> = units.iter().map(|u| u.entity_id()).collect();
                let absent = store.absent_concept_ids(&ids);
                units
                    .iter()
                    .filter(|u| absent.contains(u.entity_id().as_str()))
                    .cloned()
                    .collect()
            };
            if !fresh.is_empty() {
                store.upsert_entities(&fresh, file_path, language)?;
            }
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
        self.persist_edges_scoped(projection, None)
    }

    /// Persist edges, optionally only those touching one file.
    ///
    /// `update_file` used to call the unscoped form, so editing one function
    /// re-asserted every CALLS, IMPORTS, EXTENDS and OVERRIDES edge in the
    /// project with a fresh `valid_from` — tens of thousands of writes per
    /// save on a 558-file tree. Each duplicate is also a new version that
    /// `as_of` has to reconstruct through, so the ledger grew without bound
    /// and got slower as it grew.
    ///
    /// An edge is in scope when *either* endpoint lives in the changed file:
    /// a call into an edited function changed as surely as a call out of one.
    /// This mirrors `resolve_calls_scoped`, which already made the same split
    /// for resolution.
    ///
    /// Idempotent on the ledger: an in-scope edge that already has an open
    /// interval is not re-asserted. CodeRadar edges carry no properties and a
    /// constant weight, so the open interval is the same fact — re-asserting
    /// would only add a version `as_of` has to replay through, and with a
    /// fresh per-run `valid_from` it would abort on macrame's
    /// single-open-interval guard. Removal is not this path's job: deleted
    /// entities retire their edges via `retire_entities`.
    ///
    /// Returns the number of in-scope, FK-safe (non-external, no symbolic
    /// heuristic target) edges in the projection — whether newly asserted or
    /// already open — so the count is a property of the projection, not of
    /// the ledger's prior state.
    pub fn persist_edges_scoped(
        &self,
        projection: &ProjectedGraph,
        scope_file: Option<&str>,
    ) -> Result<usize, macrame::DbError> {
        use super::module_resolution::normalize_path_str;

        let store = match self.store.as_ref() {
            Some(s) => s,
            None => return Ok(0),
        };

        // Entity ids are `<file>::<name>`, so the file is a prefix — but the
        // projection stores whichever separators the walker saw, hence the
        // normalization on both sides.
        let scope_prefix = scope_file.map(|f| format!("{}::", normalize_path_str(f)));
        let in_scope = |a: &str, b: &str| match &scope_prefix {
            None => true,
            Some(prefix) => {
                normalize_path_str(a).starts_with(prefix.as_str())
                    || normalize_path_str(b).starts_with(prefix.as_str())
            }
        };

        let mut edge_count = 0usize;
        let mut batch: Vec<macrame::graph::EdgeAssertion> = Vec::new();
        let ts_now = crate::storage::now_iso8601();

        // FK safety: the ledger enforces REFERENCES(concepts(id) ON DELETE
        // CASCADE) on both endpoints, and the concept flush (which runs
        // first) wrote exactly the entities in these maps. Edges whose
        // endpoint is not one of them must never be asserted. That is the
        // general case behind `external::…` / `builtins.…` callees — and it
        // also covers the call resolver's symbolic heuristic targets (a
        // capitalized receiver like `Date.now()` becomes the pseudo-target
        // `Date::now`, which is never a concept id). Before this check such
        // edges aborted the whole all-or-nothing batch with a FOREIGN KEY
        // error and were silently lost (`let _ =` at the call site) — which
        // is how stores with concepts but zero edges got built.
        let valid: std::collections::HashSet<&str> = projection
            .modules
            .keys()
            .chain(projection.functions.keys())
            .chain(projection.classes.keys())
            .chain(projection.imports.keys())
            .chain(projection.constants.keys())
            .chain(projection.type_aliases.keys())
            .map(String::as_str)
            .collect();
        let is_persistable = |id: &str| valid.contains(id);

        // Triples that already hold an open interval — skipped below so a
        // re-persist is a no-op instead of a single-open abort.
        let open: std::collections::HashSet<String> = store
            .open_edge_triples(ts_now.as_str())?
            .into_iter()
            .map(|(s, t, k)| edge_key(&s, &t, &k))
            .collect();

        let dangling = |a: &str, b: &str| !is_persistable(a) || !is_persistable(b);

        for (caller, callees) in projection.callees_by_caller.iter() {
            for callee in callees.iter() {
                if !in_scope(caller, callee) {
                    continue;
                }
                // External/builtin callees and symbolic heuristic targets
                // have no concept row — assert only FK-safe edges.
                if dangling(caller, callee) {
                    continue;
                }
                if open.contains(&edge_key(caller, callee, "CALLS")) {
                    edge_count += 1;
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
                if !in_scope(importer, target_mod) {
                    continue;
                }
                if dangling(importer, target_mod) {
                    continue;
                }
                if open.contains(&edge_key(importer, target_mod, "IMPORTS")) {
                    edge_count += 1;
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
        // concrete class ids (resolve_class_hierarchy discards externals);
        // the persistable check below is the FK guard.
        for (cid, class) in projection.classes.iter() {
            for base_id in class.resolved_bases.iter() {
                if !in_scope(cid, base_id) {
                    continue;
                }
                if dangling(cid, base_id) {
                    continue;
                }
                if open.contains(&edge_key(cid, base_id, "EXTENDS")) {
                    edge_count += 1;
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
            if !in_scope(override_fid, base_fid) {
                continue;
            }
            if dangling(override_fid, base_fid) {
                continue;
            }
            if open.contains(&edge_key(override_fid, base_fid, "OVERRIDES")) {
                edge_count += 1;
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

        // Persist to Macrame if store attached. Re-registering a synthetic
        // edge that is already open is skipped (same fact), otherwise the
        // re-run would abort on the single-open guard and the error would be
        // silently dropped below. Two boundary guards, both enforced here so
        // no caller can trip them:
        //
        //  * macrame's edge-type validator accepts only `[A-Z0-9]+`, so an
        //    underscore kind (`DEPENDS_ON`) would fail the whole
        //    all-or-nothing batch and lose every edge in it. The underscore
        //    is stripped at this boundary; the in-memory indices are
        //    kind-agnostic, so nothing else changes.
        //  * `links` has FKs to `concepts` on both endpoints, so edges whose
        //    endpoints are not persisted concepts (e.g. `django:route:...`
        //    route nodes) cannot be asserted; the batch drops the failing
        //    write rather than failing the registration (best-effort, as
        //    before).
        if let Some(store) = self.store.as_ref() {
            let ts_now = crate::storage::now_iso8601();
            // Unreadable ledger → assert everything (the pre-existing
            // best-effort behaviour); the assert error is dropped below.
            let open: std::collections::HashSet<String> = store
                .open_edge_triples(ts_now.as_str())
                .unwrap_or_default()
                .into_iter()
                .map(|(s, t, k)| edge_key(&s, &t, &k))
                .collect();
            let batch: Vec<_> = edges
                .iter()
                .map(|(source_id, target_id, kind)| {
                    let kind = kind.replace('_', "");
                    (source_id.clone(), target_id.clone(), kind)
                })
                .filter(|(source_id, target_id, kind)| {
                    !open.contains(&edge_key(source_id, target_id, kind))
                })
                .map(|(source_id, target_id, kind)| {
                    macrame::graph::EdgeAssertion::new(
                        source_id.as_str(), target_id.as_str(), kind.as_str())
                        .valid_from(ts_now.as_str())
                        .weight(1.0)
                })
                .collect();
            if !batch.is_empty() {
                let _ = store.assert_edges_bulk(batch);
            }
        }

        Ok(edges.len())
    }
}
