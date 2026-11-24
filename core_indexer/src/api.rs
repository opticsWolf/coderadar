//! Pure-Rust façade over the engine for the ported Rust application.
//!
//! The PyO3 bindings in `lib.rs` build Python dictionaries. This module
//! exposes the same reads as `serde_json::Value` so `rust/coderadar` can link
//! the engine as an rlib without a Python interpreter in the middle (M-FFI:
//! no function round-trips through Python).

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::graph::CodeGraph;
use crate::query::exec::{execute_query, QueryValue};
use crate::query::grammar::parse_query;
use crate::storage::{now_iso8601, CodeGraphStore};
use crate::types::{Function, ProjectedGraph};

/// Load a Macrame store into the same projection the Python cold start uses.
///
/// # Errors
/// Returns a message when the store cannot be opened, predates the canonical
/// id format, or its ledger cannot be reconstructed.
pub fn load_projection(db_path: &str, root: Option<&str>) -> Result<Arc<ProjectedGraph>, String> {
    use crate::graph::cold_start::projection_from_state;

    let store = CodeGraphStore::open(db_path)
        .map_err(|e| format!("failed to open Macrame store {db_path}: {e:?}"))?;
    let legacy = crate::legacy_spelled_concept_ids(&store)
        .map_err(|e| format!("failed to inspect Macrame store {db_path}: {e:?}"))?;
    if !legacy.is_empty() {
        return Err(format!(
            "store {db_path} predates the 0.10 canonical id format: {} live id(s) are misspelled; re-analyze",
            legacy.len()
        ));
    }
    let state = store
        .reconstruct(&now_iso8601())
        .map_err(|e| format!("ledger reconstruct failed for {db_path}: {e:?}"))?;
    let (mut projection, _stats) = projection_from_state(&state)?;
    let config = crate::active_config();
    let graph = CodeGraph::new((*config).clone()).with_store(store);
    if let Some(r) = root {
        projection.path_maps = crate::graph::path_maps::PathMaps::discover(r, &config.project, &[]);
    }
    graph.resolve_imports(&mut projection);
    graph.populate_class_methods(&mut projection);
    graph.compute_all_mro(&mut projection);
    graph.resolve_class_hierarchy(&mut projection);
    graph.resolve_overrides(&mut projection);
    graph.resolve_decorators(&mut projection);
    graph.resolve_instantiations(&mut projection);
    graph.resolve_references(&mut projection);
    graph.commit_projection(projection);
    if let Some(r) = root {
        *crate::INDEXED_ROOT.write() =
            Some(std::fs::canonicalize(r).unwrap_or_else(|_| std::path::PathBuf::from(r)));
    }
    Ok(graph.snapshot())
}

fn insert(map: &mut Map<String, Value>, key: &str, value: impl Into<Value>) {
    map.insert(key.to_string(), value.into());
}

/// The live entity as a JSON object, in the shape the Python bindings return.
pub fn entity_json(snap: &ProjectedGraph, entity_id: &str) -> Option<Value> {
    if let Some(f) = snap.functions.get(entity_id) {
        let mut d = Map::new();
        insert(&mut d, "id", f.id.clone());
        insert(&mut d, "name", f.name.clone());
        insert(&mut d, "references", f.resolved_refs.clone());
        insert(
            &mut d,
            "kind",
            match f.kind {
                crate::types::FunctionKind::Free
                | crate::types::FunctionKind::StaticMethod
                | crate::types::FunctionKind::ClassMethod => "function",
                _ => "method",
            },
        );
        if let Some((file, _)) = f.id.rsplit_once("::") {
            insert(&mut d, "file_path", file);
        }
        insert(&mut d, "parent_module", f.parent_module.clone());
        if let Some(pc) = &f.parent_class {
            insert(&mut d, "parent_id", pc.clone());
        }
        if let Some(doc) = &f.docstring {
            insert(&mut d, "docstring", doc.clone());
        }
        if let Some(ret) = &f.return_type {
            insert(&mut d, "return_type", ret.clone());
        }
        insert(&mut d, "line", f.line);
        insert(&mut d, "end_line", f.exit_line);
        insert(&mut d, "start_line", f.line);
        insert(&mut d, "decorators", f.decorators.clone());
        insert(&mut d, "is_async", f.is_async);
        insert(&mut d, "is_generator", f.is_generator);
        insert(&mut d, "span_start", f.span.start);
        insert(&mut d, "span_end", f.span.end);
        insert(&mut d, "name_span_start", f.name_span.start);
        insert(&mut d, "name_span_end", f.name_span.end);
        insert(&mut d, "has_embedding", !f.embedding.vec.is_empty());
        insert(&mut d, "embedding_hash", f.embedding.hash.clone());
        let params: Vec<String> = f
            .parameters
            .iter()
            .map(|p| {
                let mut s = p.name.clone();
                if let Some(a) = &p.annotation {
                    s.push_str(": ");
                    s.push_str(a);
                }
                if let Some(v) = &p.default_value {
                    s.push_str(" = ");
                    s.push_str(v);
                }
                s
            })
            .collect();
        let keyword = crate::ffi_convert::declaration_keyword(&f.id);
        let sig = if keyword.is_empty() {
            format!("{}({})", f.name, params.join(", "))
        } else {
            format!("{} {}({})", keyword, f.name, params.join(", "))
        };
        let sig = match &f.return_type {
            Some(ret) => format!("{sig} -> {ret}"),
            None => sig,
        };
        insert(&mut d, "signature", sig);
        if let Some(m) = snap.modules.get(&f.parent_module) {
            insert(&mut d, "file_path", m.path.to_string_lossy().to_string());
        }
        return Some(Value::Object(d));
    }
    if let Some(c) = snap.classes.get(entity_id) {
        let mut d = Map::new();
        insert(&mut d, "id", c.id.clone());
        insert(&mut d, "name", c.name.clone());
        insert(&mut d, "grammar_kind", c.grammar_kind.clone());
        insert(&mut d, "kind", "class");
        insert(&mut d, "parent_module", c.parent_module.clone());
        if let Some((file, _)) = c.id.rsplit_once("::") {
            insert(&mut d, "file_path", file);
        }
        if let Some(pc) = &c.parent_class {
            insert(&mut d, "parent_id", pc.clone());
        }
        if let Some(doc) = &c.docstring {
            insert(&mut d, "docstring", doc.clone());
        }
        insert(&mut d, "line", c.line);
        insert(&mut d, "end_line", c.exit_line);
        insert(&mut d, "start_line", c.line);
        insert(&mut d, "decorators", c.decorators.clone());
        insert(&mut d, "span_start", c.span.start);
        insert(&mut d, "span_end", c.span.end);
        insert(
            &mut d,
            "bases",
            c.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>(),
        );
        insert(&mut d, "has_embedding", !c.embedding.vec.is_empty());
        if let Some(m) = snap.modules.get(&c.parent_module) {
            insert(&mut d, "file_path", m.path.to_string_lossy().to_string());
        }
        return Some(Value::Object(d));
    }
    if let Some(m) = snap.modules.get(entity_id) {
        let mut d = Map::new();
        insert(&mut d, "id", m.id.clone());
        insert(&mut d, "name", m.name.clone());
        insert(&mut d, "kind", "module");
        insert(&mut d, "file_path", m.path.to_string_lossy().to_string());
        insert(&mut d, "language", format!("{:?}", m.language));
        insert(&mut d, "parse_quality", format!("{:?}", m.parse_quality));
        insert(&mut d, "classes", m.classes.clone());
        insert(&mut d, "functions", m.functions.clone());
        insert(&mut d, "imports", m.imports.clone());
        insert(&mut d, "constants", m.constants.clone());
        insert(&mut d, "type_aliases", m.type_aliases.clone());
        insert(&mut d, "file_version", m.file_version);
        insert(&mut d, "has_embedding", !m.embedding.vec.is_empty());
        return Some(Value::Object(d));
    }
    if let Some(i) = snap.imports.get(entity_id) {
        let mut d = Map::new();
        insert(&mut d, "id", i.id.clone());
        insert(&mut d, "name", i.raw.clone());
        insert(&mut d, "kind", "import");
        insert(&mut d, "line", i.line);
        insert(&mut d, "start_line", i.line);
        return Some(Value::Object(d));
    }
    if let Some(k) = snap.constants.get(entity_id) {
        let mut d = Map::new();
        insert(&mut d, "id", k.id.clone());
        insert(&mut d, "name", k.name.clone());
        insert(&mut d, "kind", "constant");
        if let Some(a) = &k.annotation {
            insert(&mut d, "annotation", a.clone());
        }
        insert(&mut d, "span_start", k.span.start);
        return Some(Value::Object(d));
    }
    if let Some(t) = snap.type_aliases.get(entity_id) {
        let mut d = Map::new();
        insert(&mut d, "id", t.id.clone());
        insert(&mut d, "name", t.name.clone());
        insert(&mut d, "kind", "type_alias");
        insert(&mut d, "target", t.target.clone());
        return Some(Value::Object(d));
    }
    if let Some(r) = snap.routes.get(entity_id) {
        let mut d = Map::new();
        insert(&mut d, "id", r.id.clone());
        insert(&mut d, "name", r.pattern.clone());
        insert(&mut d, "kind", "route");
        insert(&mut d, "file_path", r.file_path.clone());
        insert(&mut d, "handler", r.handler_id.clone());
        insert(&mut d, "methods", r.methods.clone());
        insert(&mut d, "framework", r.framework.clone());
        return Some(Value::Object(d));
    }
    None
}

/// A sparse row for a call-graph id with no concept row (external/builtin).
pub fn unresolved_json(entity_id: &str) -> Value {
    let name = entity_id.rsplit("::").next().unwrap_or(entity_id);
    json!({
        "id": entity_id,
        "name": name,
        "kind": crate::ffi_convert::unresolved_ref_kind(entity_id),
    })
}

/// Look up an entity by any accepted spelling, including dotted names.
pub fn lookup(snap: &ProjectedGraph, entity_id: &str) -> Option<Value> {
    let id = crate::canonical_lookup_id(entity_id);
    if let Some(found) = entity_json(snap, &id) {
        return Some(found);
    }
    let resolved = crate::resolve_qualified_name(snap, &id)?;
    entity_json(snap, &resolved)
}

fn signature_text(f: &Function) -> Option<String> {
    crate::function_signature_text(f)
}

/// Keyword search over names, signatures and docstrings (OR semantics).
pub fn search(
    snap: &ProjectedGraph,
    query: &str,
    top_k: usize,
    kind: Option<&str>,
) -> Result<Vec<Value>, String> {
    const KINDS: &[&str] = &[
        "function",
        "class",
        "type_alias",
        "constant",
        "module",
        "import",
        "route",
    ];
    let kind_filter = kind.map(|k| k.to_lowercase());
    if let Some(k) = &kind_filter {
        if !KINDS.contains(&k.as_str()) {
            return Err(format!(
                "unknown kind `{k}` (expected: function | class | type_alias | constant | module | import | route)"
            ));
        }
    }
    let wants = |wanted: &str| kind_filter.is_none() || kind_filter.as_deref() == Some(wanted);
    let tokens = crate::search_tokens(query);
    let mut scored: Vec<(usize, Value)> = Vec::new();

    macro_rules! scan {
        ($kind:literal, $map:expr, $field:ident, $weight:expr, $sig:expr, $doc:expr) => {
            if wants($kind) {
                for (id, e) in $map.iter() {
                    let name = e.$field.to_lowercase();
                    let sig = $sig(e).map(|s| s.to_lowercase());
                    let doc = $doc(e).map(|s| s.to_lowercase());
                    let score =
                        crate::token_entity_score(&name, sig.as_deref(), doc.as_deref(), &tokens);
                    if score > 0 {
                        if let Some(row) = entity_json(snap, id) {
                            scored.push((score * $weight, row));
                        }
                    }
                }
            }
        };
    }
    let doc_f = |f: &Function| f.docstring.clone();
    let doc_c = |c: &crate::types::Class| c.docstring.clone();
    let no_sig_c = |_c: &crate::types::Class| -> Option<String> { None };
    let no_op_t = |_e: &crate::types::TypeAlias| -> Option<String> { None };
    let no_op_k = |_e: &crate::types::Constant| -> Option<String> { None };
    let no_op_m = |_e: &crate::types::Module| -> Option<String> { None };
    let no_op_i = |_e: &crate::types::Import| -> Option<String> { None };
    let no_op_r = |_e: &crate::types::Route| -> Option<String> { None };
    scan!("function", snap.functions, name, 10, signature_text, doc_f);
    scan!("class", snap.classes, name, 10, no_sig_c, doc_c);
    scan!("type_alias", snap.type_aliases, name, 10, no_op_t, no_op_t);
    scan!("constant", snap.constants, name, 9, no_op_k, no_op_k);
    scan!("module", snap.modules, name, 9, no_op_m, no_op_m);
    scan!("import", snap.imports, raw, 8, no_op_i, no_op_i);
    scan!("route", snap.routes, pattern, 10, no_op_r, no_op_r);

    scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    scored.truncate(top_k);
    Ok(scored.into_iter().map(|(_, row)| row).collect())
}

fn ids_to_rows(snap: &ProjectedGraph, ids: impl IntoIterator<Item = String>) -> Vec<Value> {
    ids.into_iter()
        .map(|id| entity_json(snap, &id).unwrap_or_else(|| unresolved_json(&id)))
        .collect()
}

/// Direct callers (reverse call index).
pub fn callers(snap: &ProjectedGraph, entity_id: &str) -> Vec<Value> {
    let id = crate::canonical_lookup_id(entity_id);
    let ids: Vec<String> = snap
        .callers_by_callee
        .get(id.as_str())
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    ids_to_rows(snap, ids)
}

/// Direct callees (forward call index).
pub fn callees(snap: &ProjectedGraph, entity_id: &str) -> Vec<Value> {
    let id = crate::canonical_lookup_id(entity_id);
    let ids: Vec<String> = snap
        .callees_by_caller
        .get(id.as_str())
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    ids_to_rows(snap, ids)
}

/// Graph statistics, matching `graph_stats()`'s public keys.
pub fn stats(snap: &ProjectedGraph) -> Value {
    let has_row = |id: &str| entity_json(snap, id).is_some();
    let mut asserted = 0usize;
    let mut external = 0usize;
    let mut unresolved = 0usize;
    let mut route_edges = 0usize;
    for (caller, targets) in &snap.callees_by_caller {
        let is_route = snap.routes.contains_key(caller.as_str());
        for callee in targets {
            if is_route {
                route_edges += 1;
            } else if callee.starts_with("external::") {
                external += 1;
            } else if has_row(callee) {
                asserted += 1;
            } else {
                unresolved += 1;
            }
        }
    }
    let total: usize = snap.callees_by_caller.values().map(|s| s.len()).sum();
    json!({
        "modules": snap.modules.len(),
        "classes": snap.classes.len(),
        "functions": snap.functions.len(),
        "imports": snap.imports.len(),
        "constants": snap.constants.len(),
        "type_aliases": snap.type_aliases.len(),
        "routes": snap.routes.len(),
        "route_edges": route_edges,
        "file_count": snap.file_to_modules.len(),
        "call_edges": total,
        "asserted_call_edges": asserted,
        "external_call_edges": external,
        "unresolved_call_edges": unresolved,
    })
}

fn query_value(v: QueryValue) -> Value {
    match v {
        QueryValue::String(s) => Value::String(s),
        QueryValue::Int(i) => json!(i),
        QueryValue::Float(f) => json!(f),
        QueryValue::Bool(b) => Value::Bool(b),
        QueryValue::List(l) => Value::Array(l.into_iter().map(query_value).collect()),
        QueryValue::Null => Value::Null,
    }
}

/// Run a Pest query against the projection, returning JSON rows.
pub fn query(snap: &ProjectedGraph, text: &str) -> Result<Vec<Value>, String> {
    let parsed = parse_query(text).map_err(|e| e.to_string())?;
    Ok(execute_query(snap, &parsed)
        .into_iter()
        .map(|row| {
            Value::Object(
                row.fields
                    .into_iter()
                    .map(|(k, v)| (k, query_value(v)))
                    .collect(),
            )
        })
        .collect())
}

/// One breadth-first traversal step for the edge kinds the CLI exposes.
pub fn traverse(
    snap: &ProjectedGraph,
    entity_id: &str,
    direction: &str,
    kinds: &[String],
    max_depth: usize,
) -> Vec<Value> {
    let core_dir = match direction {
        "out" | "downstream" => "out",
        "in" | "upstream" => "in",
        _ => "both",
    };
    let wants = |kind: &str| kinds.is_empty() || kinds.iter().any(|k| k == kind);
    let mut out: Vec<Value> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut frontier: Vec<(String, usize, String)> = Vec::new();
    let root = crate::canonical_lookup_id(entity_id);
    seen.insert(root.clone());
    frontier.push((root, 0, "self".to_string()));

    while let Some((id, depth, _edge)) = frontier.pop() {
        if depth >= max_depth {
            continue;
        }
        let mut next: Vec<(String, String)> = Vec::new();
        let mut push = |targets: &BTreeSet<String>, kind: &str| {
            if wants(kind) {
                for t in targets {
                    next.push((t.clone(), kind.to_string()));
                }
            }
        };
        if core_dir != "out" {
            push(
                &snap
                    .callers_by_callee
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "calls",
            );
            push(
                &snap.importers.get(id.as_str()).cloned().unwrap_or_default(),
                "imports",
            );
            push(
                &snap
                    .subclasses
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "extends",
            );
            push(
                &snap
                    .implementers
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "implements",
            );
            push(
                &snap
                    .decorated_by
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "decorates",
            );
            push(
                &snap
                    .instantiated_by
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "instantiates",
            );
            push(
                &snap
                    .referenced_by
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "references",
            );
            push(
                &snap
                    .overridden_by
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "overrides",
            );
        }
        if core_dir != "in" {
            push(
                &snap
                    .callees_by_caller
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "calls",
            );
            push(
                &snap
                    .imports_by_importer
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                "imports",
            );
            if wants("extends") || wants("inherits") {
                if let Some(c) = snap.classes.get(id.as_str()) {
                    for t in c.resolved_bases.iter().chain(c.resolved_interfaces.iter()) {
                        next.push((t.clone(), "extends".to_string()));
                    }
                }
            }
            if wants("decorates") {
                if let Some(f) = snap.functions.get(id.as_str()) {
                    for t in &f.resolved_decorators {
                        next.push((t.clone(), "decorates".to_string()));
                    }
                }
            }
            if let Some(base) = snap.overrides_base.get(id.as_str()) {
                if wants("overrides") {
                    next.push((base.clone(), "overrides".to_string()));
                }
            }
        }
        for (target, edge_kind) in next {
            if seen.insert(target.clone()) {
                let mut row =
                    entity_json(snap, &target).unwrap_or_else(|| unresolved_json(&target));
                if let Value::Object(ref mut map) = row {
                    insert(map, "depth", depth + 1);
                    insert(map, "edge_type", edge_kind.clone());
                }
                out.push(row);
                frontier.push((target, depth + 1, edge_kind));
            }
        }
    }
    out
}

/// Index a project from source and make it the engine's loaded graph.
///
/// # Errors
/// Returns the engine's message when the walk or persist fails.
pub fn analyze(root: &str, create_store: bool, exclude: &[String]) -> Result<Value, String> {
    let outcome = crate::analyze_inner(root, create_store, exclude)?;
    Ok(json!({
        "files_indexed": outcome.files_indexed,
        "entities_extracted": outcome.total_entities,
        "extraction_failures": outcome.failures,
        "panicked_workers": outcome.panicked_workers,
        "blobs_stored": outcome.blobs_stored,
        "blobs_skipped_oversize": outcome.blobs_skipped_oversize,
        "blobs_skipped_excluded": outcome.blobs_skipped_excluded,
        "blobs_bytes": outcome.blobs_bytes,
    }))
}
