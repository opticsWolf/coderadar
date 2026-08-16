use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::CodeGraph;
use super::module_resolution::{find_module_by_dotted_name, normalize_path_str};
use crate::types::*;

impl CodeGraph {
    /// Close removed entities in the persistent ledger.
    ///
    /// Removal used to touch the in-memory projection only. The store kept
    /// every concept and edge open forever, so `as_of` for any past instant
    /// answered with a set that only ever grew — a deleted function stayed
    /// "currently true" in a database whose whole point is knowing when
    /// things stopped being true.
    ///
    /// Best-effort by design: the projection is the source of truth for
    /// queries, and a ledger write that fails must not fail the update. It is
    /// reported, not swallowed.
    pub(crate) fn retire_in_store<'a>(&self, ids: impl IntoIterator<Item = &'a EntityId>) {
        let store = match &self.store {
            Some(store) => store,
            None => return,
        };
        let ids: Vec<String> = ids.into_iter().cloned().collect();
        if ids.is_empty() {
            return;
        }
        if let Err(e) = store.retire_entities(&ids) {
            eprintln!("Warning: could not retire {} removed entities: {:?}", ids.len(), e);
        }
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
            self.retire_in_store(removed.iter());
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

        self.retire_in_store(removed.iter());
        removed
    }

    /// Drop a deleted file from the graph entirely.
    ///
    /// `update_file` cannot express this: it re-reads the file, and a file
    /// that is gone has no content to diff against. Without this the watcher
    /// had nothing to call on a deletion, so removed files stayed in the
    /// projection, in the ledger, and in every query result until the next
    /// full `analyze`.
    ///
    /// Returns the ids that went away.
    pub fn remove_file(&self, file_path: &str) -> Vec<EntityId> {
        let normalized = normalize_path_str(file_path);
        let mut projection = (*self.snapshot()).clone();
        let removed = self.remove_file_entities(&mut projection, &normalized);
        // The file→module mapping outlives the module otherwise, and a
        // recreated file would resolve to an id that is no longer there.
        projection.file_to_modules.remove(&PathBuf::from(&normalized));
        projection.file_to_modules.remove(&PathBuf::from(file_path));
        self.commit_projection(projection);
        removed.into_iter().collect()
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

        // Map normalized id → the exact spelling the projection already stores.
        //
        // `analyze` walks with OS separators and records `.\p.py::f`; this path
        // normalizes first and extracts `p.py::f`. Inserting under the extracted
        // spelling left the walker's entry untouched, so every updated entity
        // was duplicated and its id — the handle agents hold across calls —
        // silently forked. Reuse the stored spelling instead.
        let mut existing_ids: std::collections::HashMap<EntityId, EntityId> =
            std::collections::HashMap::new();
        {
            let mut note = |id: &EntityId| {
                let normalized = normalize_path_str(id);
                if normalized.starts_with(&normalized_file_path) {
                    existing_ids.entry(normalized).or_insert_with(|| id.clone());
                }
            };
            for id in projection.functions.keys() { note(id); }
            for id in projection.classes.keys() { note(id); }
            for id in projection.imports.keys() { note(id); }
            for id in projection.constants.keys() { note(id); }
            for id in projection.type_aliases.keys() { note(id); }
            for id in projection.modules.keys() { note(id); }
        }
        let canonical = |id: &str| -> EntityId {
            existing_ids
                .get(&normalize_path_str(id))
                .cloned()
                .unwrap_or_else(|| id.to_string())
        };

        // Keyed by normalized id so the lookup in step 4 can find it — it was
        // keyed raw and looked up normalized, so every function missed and was
        // re-inserted, defeating the diff.
        let old_hashes: std::collections::HashMap<EntityId, (EntityId, u64, u64)> = projection
            .functions
            .iter()
            .filter(|(id, _)| normalize_path_str(id).starts_with(&normalized_file_path))
            .map(|(id, f)| {
                (normalize_path_str(id), (id.clone(), f.signature_hash, f.body_hash))
            })
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
        // Kept so the ledger can be told what went away; the counter alone
        // says how many, which is not enough to retire them.
        let mut removed_ids: Vec<EntityId> = Vec::new();

        // 3. Remove entities that don't exist in new units
        let remove_entity = |id: &str,
                             proj: &mut ProjectedGraph,
                             removed: &mut usize,
                             removed_ids: &mut Vec<EntityId>| {
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
            removed_ids.push(id.to_string());
            *removed += 1;
        };

        let module_id = canonical(&format!("{}::module", file_path));

        for (_, (stored_id, _, _)) in old_hashes.iter().filter(|(n, _)| !new_funcs.contains(*n)) {
            remove_entity(stored_id, projection, &mut removed, &mut removed_ids);
        }
        for id in old_classes.iter().filter(|id| !new_classes.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed, &mut removed_ids);
        }
        for id in old_imports.iter().filter(|id| !new_imports.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed, &mut removed_ids);
        }
        for id in old_constants.iter().filter(|id| !new_constants.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed, &mut removed_ids);
        }
        for id in old_aliases.iter().filter(|id| !new_aliases.contains(&normalize_id(id))) {
            remove_entity(id, projection, &mut removed, &mut removed_ids);
        }

        // 4. Insert new entities + re-insert changed ones (hash mismatch)
        for unit in units {
            let id = unit.entity_id();
            let needs_insert = match unit {
                ExtractedUnit::Function(f) => {
                    old_hashes.get(&normalize_id(&f.id)).map_or(true, |(_, sig, body)| {
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

            // Remove the old version first (for modified entities). Use the id
            // the projection actually stores it under, not the normalized one —
            // removing a spelling that is not there leaves a duplicate behind.
            let norm_id = canonical(&id);
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
                    let entity_id = canonical(&f.id);
                    let func = Function::from_extracted(
                        f, entity_id.clone(), module_id.clone(),
                        f.parent_class.as_deref().map(canonical));
                    projection.functions.insert(entity_id, Arc::new(func));
                    inserted += 1;
                }
                ExtractedUnit::Class(c) => {
                    let entity_id = canonical(&c.id);
                    let class = Class::from_extracted(
                        c, entity_id.clone(), module_id.clone(),
                        c.parent_class.as_deref().map(canonical));
                    projection.classes.insert(entity_id, Arc::new(class));
                    inserted += 1;
                }
                ExtractedUnit::Import(i) => {
                    let entity_id = canonical(&i.id);
                    let import = Import {
                        id: entity_id.clone(), raw: i.raw.clone(),
                        kind: i.kind.clone(),
                        resolution: ImportResolution::Unresolved,
                        line: i.line, is_type_only: i.is_type_only,
                        name_span: i.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.imports.insert(entity_id.clone(), Arc::new(import));
                    if let Some(mod_arc) = projection.modules.get(&module_id) {
                        let mut m = (**mod_arc).clone();
                        m.imports.push(entity_id);
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
                    let entity_id = canonical(&k.id);
                    let constant = Constant {
                        id: entity_id.clone(), name: k.name.clone(),
                        annotation: k.annotation.clone(), source: k.source,
                        default_value: k.default_value.clone(),
                        span: k.span, name_span: k.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.constants.insert(entity_id, Arc::new(constant));
                    inserted += 1;
                }
                ExtractedUnit::TypeAlias(ta) => {
                    let entity_id = canonical(&ta.id);
                    let alias = TypeAlias {
                        id: entity_id.clone(), name: ta.name.clone(),
                        target: ta.target.clone(), source: ta.source,
                        span: ta.span, name_span: ta.name_span,
                        embedding: EmbeddingVec::default(),                    };
                    projection.type_aliases.insert(entity_id, Arc::new(alias));
                    inserted += 1;
                }
                _ => {}
            }
        }

        self.retire_in_store(removed_ids.iter());

        (inserted, removed)
    }

    /// Update a single file in-place: re-index and diff against the current graph.
    ///
    /// The report used to be fabricated at the FFI boundary — `parse_quality:
    /// "clean"`, `parse_errors: 0`, `elapsed_ms: 0.0` regardless of what
    /// happened — which made the Python and MCP failure branches keyed off
    /// `parse_quality` dead code. It is measured here now.
    pub fn update_file(
        &self,
        file_path: &str,
        content: Option<&str>,
        _force: Option<bool>,
    ) -> Result<UpdateOutcome, String> {
        let started = std::time::Instant::now();
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
        units.insert(0, Self::synthesize_module_unit(file_path, &lang, &source, root_node));

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
        // Scoped: an edit to one file must not re-assert the project's whole
        // edge set (plan §1.2).
        let _ = self.persist_edges_scoped(&projection, Some(file_path));
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

        Ok(UpdateOutcome {
            entities_added: new_count,
            entities_removed: removed_count,
            affected_files,
            parse_quality: crate::extract::node_quality(root_node),
            parse_errors: crate::extract::count_parse_errors(root_node),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
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
        // Carried onto the projected Module below. synthesize_module_unit reads
        // both off the parsed tree, so a file tree-sitter had to recover from
        // is recorded as Partial rather than reported clean.
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
            parse_quality: module_quality,
            file_version: 1,
            content_hash: module_content_hash,
                        embedding: EmbeddingVec::default(),        };
        projection.modules.insert(module_id.clone(), Arc::new(module));
        projection.file_to_modules
            .entry(PathBuf::from(file_path))
            .or_default()
            .push(module_id);
    }
}
