use super::CodeGraph;
use crate::storage::{now_iso8601, route_content, v2_upsert};
use crate::types::*;
use macrame::graph::EdgeAssertion;
use std::sync::Arc;

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
    pub fn persist_edges(&self, projection: &ProjectedGraph) -> Result<usize, macrame::DbError> {
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
    /// Returns the number of in-scope, FK-safe, non-synthetic (non-external,
    /// no symbolic heuristic target, not a registered synthetic pair) edges
    /// in the projection — whether newly asserted or already open — so the
    /// count is a property of the projection, not of the ledger's prior state.
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
            // §1.3: route nodes are concepts, so route→handler edges persist
            // (previously the FK check dropped every edge from a route).
            .chain(projection.routes.keys())
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
                // R2-2: synthetic pairs carry their own ledger rows under
                // the synthetic kind (see register_synthetic_edges_bulk).
                // Re-asserting them here as CALLS gave every synthetic edge
                // a second, structural life that restore could no longer
                // distinguish from a natural call.
                if projection
                    .synthetic_edges
                    .contains(&(caller.clone(), callee.clone()))
                {
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
            projection
                .callees_by_caller
                .entry(source_id.clone())
                .or_default()
                .insert(target_id.clone());
            projection
                .callers_by_callee
                .entry(target_id.clone())
                .or_default()
                .insert(source_id.clone());
            // Issue 8: track synthetic pairs apart from natural CALLS so
            // scoped re-resolution preserves them instead of clearing.
            projection
                .synthetic_edges
                .insert((source_id.clone(), target_id.clone()));
        }
        self.commit_projection(projection);

        // Persist to Macrame if store attached. Re-registering a synthetic
        // edge that is already open is skipped (same fact), otherwise the
        // re-run would abort on the single-open guard and the error would be
        // silently dropped below. Macrame 0.18 accepts namespaced kinds, so
        // preserve the caller's semantic edge type rather than flattening it.
        // `links` still has FKs to `concepts` on both endpoints, so edges whose
        // endpoints are not persisted concepts (e.g. `django:route:...` route
        // nodes) cannot be asserted; the batch drops those best-effort.
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
                .filter(|(source_id, target_id, kind)| {
                    !open.contains(&edge_key(source_id, target_id, kind))
                })
                .map(|(source_id, target_id, kind)| {
                    (source_id.clone(), target_id.clone(), kind.clone())
                })
                .map(|(source_id, target_id, kind)| {
                    macrame::graph::EdgeAssertion::new(
                        source_id.as_str(),
                        target_id.as_str(),
                        kind.as_str(),
                    )
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

    /// Register framework routes (§1.3, DR-10): nodes + route→handler edges
    /// with scoped diff-retire, in one projection commit.
    ///
    /// `nodes` are `(id, pattern, file_path, handler_id, methods,
    /// framework)`; `edges` are `(source_id, target_id, kind)` with
    /// route-id sources. `scope` is `"full"` (whole-tree extraction: any
    /// live route concept absent from `nodes` is a ghost) or a root-relative
    /// file path (only that file's routes are reconciled).
    ///
    /// Ledger writes happen only with a store attached (memory-only
    /// otherwise — same best-effort rule as the synthetic-edges path).
    /// Unchanged re-runs write nothing: concept upserts go through
    /// `filter_unchanged` and already-open triples are skipped via the
    /// single-open guard read.
    ///
    /// Returns `(routes_upserted, routes_retired, pairs_retired)`.
    pub fn register_synthetic_routes(
        &self,
        nodes: Vec<(String, String, String, String, Vec<String>, String)>,
        edges: Vec<(String, String, String)>,
        scope: &str,
    ) -> Result<(usize, usize, usize), String> {
        let ts_now = now_iso8601();
        let new_ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.0.as_str()).collect();
        let new_pairs: std::collections::HashSet<(&str, &str, &str)> = edges
            .iter()
            .map(|e| (e.0.as_str(), e.1.as_str(), e.2.as_str()))
            .collect();
        // In-scope route ids: the fresh set plus live ledger ghosts (a
        // vanished route is in the ledger but in neither the fresh set nor
        // — after a full analyze — the projection).
        let mut scope_ids: std::collections::HashSet<String> =
            new_ids.iter().map(|s| s.to_string()).collect();
        if let Some(store) = self.store.as_ref() {
            let live: Vec<String> = if scope == "full" {
                store.live_route_ids()
            } else {
                store.live_route_ids_for_file(scope)
            }
            .map_err(|e| format!("route ghost scan failed: {e:?}"))?;
            scope_ids.extend(live);
        } else if scope != "full" {
            // Storeless: ghosts can only hide in the projection.
            let snap = self.snapshot();
            scope_ids.extend(
                snap.routes
                    .values()
                    .filter(|r| r.file_path == scope)
                    .map(|r| r.id.clone()),
            );
        }

        // 1. Upsert route concepts (no-op when unchanged).
        let mut upserted = 0usize;
        if let Some(store) = self.store.as_ref() {
            let concepts: Vec<macrame::ConceptUpsert> = nodes
                .iter()
                .map(|(id, pattern, file, handler, methods, framework)| {
                    v2_upsert(
                        id.as_str(),
                        pattern.as_str(),
                        route_content(
                            pattern.as_str(),
                            file.as_str(),
                            handler.as_str(),
                            methods,
                            framework.as_str(),
                        ),
                        &ts_now,
                        None,
                    )
                })
                .collect();
            upserted = store
                .upsert_concepts_bulk(&concepts)
                .map_err(|e| format!("route concept upsert failed: {e:?}"))?;
        }

        // 2. Retire ghost route concepts (cascades their edges).
        let ghosts: Vec<String> = scope_ids
            .iter()
            .filter(|id| !new_ids.contains(id.as_str()))
            .cloned()
            .collect();
        let mut retired_routes = 0usize;
        if !ghosts.is_empty() {
            if let Some(store) = self.store.as_ref() {
                retired_routes = store
                    .retire_entities(&ghosts)
                    .map_err(|e| format!("route retire failed: {e:?}"))?
                    .0;
            }
        }

        // 3. Assert new edges (single-open guard: re-runs are no-ops).
        if let Some(store) = self.store.as_ref() {
            let open: std::collections::HashSet<String> = store
                .open_edge_triples(ts_now.as_str())
                .unwrap_or_default()
                .into_iter()
                .map(|(s, t, k)| edge_key(&s, &t, &k))
                .collect();
            let batch: Vec<EdgeAssertion> = edges
                .iter()
                .filter(|(s, t, k)| !open.contains(&edge_key(s, t, k)))
                .map(|(s, t, k)| {
                    EdgeAssertion::new(s.as_str(), t.as_str(), k.as_str())
                        .valid_from(ts_now.as_str())
                        .weight(1.0)
                })
                .collect();
            if !batch.is_empty() {
                let _ = store.assert_edges_bulk(batch);
            }
            // 4. Close in-scope open synthetic triples the fresh set no
            // longer believes (handler renamed, edge kind changed).
            let stale: Vec<(String, String, String, String)> = store
                .open_synthetic_triples(ts_now.as_str())
                .unwrap_or_default()
                .into_iter()
                .filter(|(s, t, k, _)| {
                    scope_ids.contains(s)
                        && !new_pairs.contains(&(s.as_str(), t.as_str(), k.as_str()))
                })
                .collect();
            if !stale.is_empty() {
                let _ = store.retire_synthetic_pairs(&stale, ts_now.as_str());
            }
        }

        // 5. One projection commit — but only when something actually
        // changed. The hook runs on every `update_file`, and an unconditional
        // commit would advance the epoch (and `indexed_at`) on updates that
        // touch no route, breaking the epoch-continuity reports pin.
        let mut changed = upserted > 0 || retired_routes > 0;
        let mut projection = (*self.snapshot()).clone();
        for (id, pattern, file, handler, methods, framework) in &nodes {
            let route = Arc::new(Route {
                id: id.clone(),
                pattern: pattern.clone(),
                file_path: file.clone(),
                handler_id: handler.clone(),
                methods: methods.clone(),
                framework: framework.clone(),
            });
            if projection.routes.get(id) != Some(&route) {
                projection.routes.insert(id.clone(), route);
                changed = true;
            }
        }
        for id in &ghosts {
            projection.routes.remove(id);
        }
        // Drop in-scope index pairs the fresh set no longer believes. A
        // surviving route keeps its surviving pairs; a ghost loses all of
        // them (its ledger rows died in step 2 via the retire cascade).
        let drop_pair = |projection: &mut ProjectedGraph, source: &str, target: &str| {
            if let Some(targets) = projection.callees_by_caller.get_mut(source) {
                targets.remove(target);
            }
            if let Some(callers) = projection.callers_by_callee.get_mut(target) {
                callers.remove(source);
            }
            projection
                .synthetic_edges
                .remove(&(source.to_string(), target.to_string()));
        };
        let mut index_stale = 0usize;
        for source in &scope_ids {
            let targets: Vec<String> = projection
                .callees_by_caller
                .get(source)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            for target in targets {
                // Only synthetic pairs are ours: a route id can never be a
                // structural caller (tree-sitter emits no such edges), but
                // the guard keeps the hook from touching CALLS rows even if
                // that invariant ever breaks.
                if !projection
                    .synthetic_edges
                    .contains(&(source.clone(), target.clone()))
                {
                    continue;
                }
                if edges.iter().any(|(s, t, _)| s == source && t == &target) {
                    continue;
                }
                drop_pair(&mut projection, source, &target);
                index_stale += 1;
            }
        }
        // Insert fresh pairs (new ones only — re-inserting an existing
        // pair must not count as a change).
        for (source, target, _kind) in &edges {
            let pair_new = !projection
                .callees_by_caller
                .get(source)
                .is_some_and(|s| s.contains(target));
            projection
                .callees_by_caller
                .entry(source.clone())
                .or_default()
                .insert(target.clone());
            projection
                .callers_by_callee
                .entry(target.clone())
                .or_default()
                .insert(source.clone());
            projection
                .synthetic_edges
                .insert((source.clone(), target.clone()));
            changed |= pair_new;
        }
        changed |= index_stale > 0;
        if changed {
            self.commit_projection(projection);
        }

        Ok((upserted, retired_routes, index_stale))
    }
}
