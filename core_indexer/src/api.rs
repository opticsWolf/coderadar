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
    let snapshot = graph.snapshot();
    *crate::GLOBAL_GRAPH.write() = Some(graph);
    Ok(snapshot)
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

// ── Unit 6: engine-backed analyses and store reads ────────────────────────

fn entity_name(snap: &ProjectedGraph, entity_id: &str) -> Option<String> {
    if let Some(f) = snap.functions.get(entity_id) {
        return Some(f.name.clone());
    }
    if let Some(c) = snap.classes.get(entity_id) {
        return Some(c.name.clone());
    }
    snap.modules.get(entity_id).map(|m| m.name.clone())
}

/// Ranked dead-code findings, as JSON dicts in the Python binding's shape.
///
/// # Errors
/// Returns `InvalidRequest` for a confidence outside [0, 1].
pub fn dead_code(
    snap: &Arc<ProjectedGraph>,
    min_confidence: f64,
    include_test_reachable: bool,
    max_findings: usize,
) -> Result<Vec<Value>, String> {
    if !(0.0..=1.0).contains(&min_confidence) {
        return Err("InvalidRequest: min_confidence must be in [0.0, 1.0]".to_string());
    }
    let options = crate::graph::deadcode::DeadCodeOptions {
        include_test_only: include_test_reachable,
        root: crate::INDEXED_ROOT.read().clone(),
    };
    let findings = crate::graph::deadcode::detect_dead(snap, options);
    let mut rows = Vec::new();
    for f in findings
        .iter()
        .filter(|f| f.score >= min_confidence as f32)
        .take(max_findings)
    {
        let mut row = Map::new();
        insert(&mut row, "entity_id", f.entity_id.clone());
        insert(&mut row, "kind", f.kind.as_str());
        insert(&mut row, "tier", f.tier.as_str());
        insert(&mut row, "score", f.score);
        insert(&mut row, "removable_lines", f.removable_lines);
        insert(&mut row, "evidence", f.evidence.clone());
        if let Some(distance) = f.nearest_root_distance {
            insert(&mut row, "nearest_root_distance", distance as u32);
        }
        if let Some(name) = entity_name(snap, &f.entity_id) {
            insert(&mut row, "entity_name", name);
        }
        let (file, line) = match snap.functions.get(&f.entity_id) {
            Some(func) => {
                let file = snap
                    .modules
                    .get(&func.parent_module)
                    .map(|m| m.path.to_string_lossy().to_string())
                    .unwrap_or_else(|| func.id.split("::").next().unwrap_or_default().to_string());
                (file, func.line as u32)
            }
            None => (
                f.entity_id
                    .split("::")
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                0u32,
            ),
        };
        insert(&mut row, "file", file);
        insert(&mut row, "line", line);
        rows.push(Value::Object(row));
    }
    Ok(rows)
}

/// Code-smell findings, as JSON dicts in the Python binding's shape.
///
/// # Errors
/// Returns `InvalidRequest` for an unknown strictness profile.
pub fn smells(
    snap: &Arc<ProjectedGraph>,
    entity_id: Option<&str>,
    rule_id: Option<&str>,
    strictness: &str,
) -> Result<Vec<Value>, String> {
    let profile = crate::smells::profile::Strictness::parse(strictness)
        .map_err(|e| format!("InvalidRequest: {e}"))?;
    let findings = crate::smells::engine::SmellEngine::new().run_with_strictness(snap, profile);
    let mut rows = Vec::new();
    for finding in findings.iter() {
        if entity_id.is_some_and(|id| id != finding.entity_id) {
            continue;
        }
        if rule_id.is_some_and(|id| id != finding.rule_id) {
            continue;
        }
        let mut row = Map::new();
        insert(&mut row, "rule_id", finding.rule_id.clone());
        insert(&mut row, "entity_id", finding.entity_id.clone());
        insert(&mut row, "severity", finding.severity.as_str());
        insert(&mut row, "message", finding.message.clone());
        if let Some(name) = entity_name(snap, &finding.entity_id) {
            insert(&mut row, "entity_name", name);
        }
        let mut signals = Map::new();
        for (key, value) in &finding.signals {
            insert(&mut signals, key, *value);
        }
        insert(&mut row, "signals", signals);
        rows.push(Value::Object(row));
    }
    Ok(rows)
}

/// Clone groups, as JSON dicts in the Python binding's shape.
///
/// # Errors
/// Returns `InvalidRequest` for a similarity outside [0, 1] and `EngineError`
/// if detection panics internally.
pub fn clones(
    snap: &Arc<ProjectedGraph>,
    min_lines: usize,
    min_similarity: f64,
    max_groups: usize,
) -> Result<Vec<Value>, String> {
    if !(0.0..=1.0).contains(&min_similarity) {
        return Err("InvalidRequest: min_similarity must be in [0.0, 1.0]".to_string());
    }
    let options = crate::clones::CloneOptions {
        min_lines,
        min_similarity,
    };
    let detected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::clones::detect_clones(snap, options)
    }))
    .map_err(|_| {
        "EngineError: clone detection panicked internally (please report with min_lines/min_similarity); no results — not a clean scan".to_string()
    })?;
    let mut rows = Vec::new();
    for group in detected.iter().take(max_groups) {
        let mut row = Map::new();
        insert(&mut row, "clone_type", group.clone_type.as_str());
        insert(&mut row, "similarity", group.similarity);
        insert(&mut row, "confidence_tier", group.confidence_tier.as_str());
        row.insert(
            "reason".to_string(),
            group
                .reason
                .map(|r| Value::String(r.to_string()))
                .unwrap_or(Value::Null),
        );
        let mut sources: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        let mut instances = Vec::new();
        for instance in &group.instances {
            let source = sources.entry(instance.file.clone()).or_insert_with(|| {
                crate::graph::module_resolution::read_project_file(&instance.file).into_bytes()
            });
            let (start_line, start_col) = crate::types::line_col_at(source, instance.span.start);
            let (end_line, end_col) = crate::types::line_col_at(source, instance.span.end);
            let mut d = Map::new();
            insert(&mut d, "entity_id", instance.entity_id.clone());
            insert(&mut d, "file", instance.file.clone());
            insert(&mut d, "span_start", instance.span.start);
            insert(&mut d, "span_end", instance.span.end);
            insert(&mut d, "start_line", start_line);
            insert(&mut d, "start_col", start_col);
            insert(&mut d, "end_line", end_line);
            insert(&mut d, "end_col", end_col);
            instances.push(Value::Object(d));
        }
        insert(&mut row, "instances", instances);
        rows.push(Value::Object(row));
    }
    Ok(rows)
}

/// Scaffolding findings, as JSON dicts in the Python binding's shape.
///
/// # Errors
/// Returns `EngineError` when no project root has been indexed.
pub fn scaffolding(
    snap: &Arc<ProjectedGraph>,
    include_secrets: bool,
    max_findings: usize,
) -> Result<Vec<Value>, String> {
    let root = crate::INDEXED_ROOT
        .read()
        .clone()
        .ok_or_else(|| "EngineError: No graph loaded — run coderadar init first".to_string())?;
    let root = crate::fs::strip_verbatim_prefix(root);
    let cfg = crate::scaffold::ScaffoldConfig {
        include_secrets,
        ..Default::default()
    };
    let mut findings = crate::scaffold::scan_path(&root, &cfg);
    let (placeholders, stats) = crate::scaffold::scan_placeholder_bodies(snap);
    findings.extend(placeholders);
    let mut rows = Vec::new();
    for kind in ["placeholder-body", "secret", "comment-marker", "temp-file"] {
        for finding in findings
            .iter()
            .filter(|f| f.kind.as_str() == kind)
            .take(max_findings)
        {
            let mut row = Map::new();
            insert(&mut row, "kind", finding.kind.as_str());
            insert(
                &mut row,
                "file",
                crate::fs::display_path(&root, &finding.file),
            );
            insert(&mut row, "line", finding.line as u32);
            insert(&mut row, "label", finding.label.clone());
            insert(&mut row, "snippet", finding.snippet.clone());
            rows.push(Value::Object(row));
        }
    }
    let skipped = stats.skipped_no_module + stats.skipped_unreadable + stats.skipped_span;
    let mut stats_row = Map::new();
    insert(&mut stats_row, "kind", "scan-stats");
    insert(&mut stats_row, "file", "");
    insert(&mut stats_row, "line", 0u32);
    insert(
        &mut stats_row,
        "label",
        format!(
            "placeholder scan: {} functions, {} stubs, {} skipped (no-module {}, unreadable {}, span {}; {} via id fallback)",
            stats.functions,
            stats.stubs,
            skipped,
            stats.skipped_no_module,
            stats.skipped_unreadable,
            stats.skipped_span,
            stats.resolved_via_id_fallback
        ),
    );
    rows.push(Value::Object(stats_row));
    Ok(rows)
}

/// Raw blob bytes by digest, encoded as the wrapper's `$bytes` JSON marker.
///
/// # Errors
/// Returns `InvalidRequest` for a malformed digest and the engine's message
/// for a store failure.
pub fn blob_get(store: &CodeGraphStore, digest: &str) -> Result<Option<Value>, String> {
    if macrame::blob::validate_digest(digest).is_err() {
        return Err(format!(
            "InvalidRequest: malformed blob digest {digest:?}: expected 64 lowercase hex characters"
        ));
    }
    match store
        .blob_get(digest)
        .map_err(|e| format!("EngineError: {e:?}"))?
    {
        Some(bytes) => Ok(Some(json!({"$bytes": bytes}))),
        None => Ok(None),
    }
}

/// Archive cold blobs and ledger rows before a canonical UTC cutoff.
///
/// # Errors
/// Returns `InvalidRequest` for a bad timestamp and `EngineError` for a store
/// failure.
pub fn archive(store: &CodeGraphStore, cutoff: &str) -> Result<Value, String> {
    let ts = macrame::util::timestamp::normalize(cutoff).map_err(|e| {
        format!(
            "InvalidRequest: invalid timestamp {cutoff:?}: expected Macrame canonical UTC (YYYY-MM-DDTHH:MM:SS.ffffffZ); {e:?}"
        )
    })?;
    let report = store
        .archive(&ts)
        .map_err(|e| format!("EngineError: {e:?}"))?;
    Ok(json!({
        "links_archived": report.links_archived,
        "concepts_archived": report.concepts_archived,
        "log_entries_archived": report.log_entries_archived,
        "horizon": report.horizon,
        "blobs_archived": report.blobs_archived,
        "blobs_restored": report.blobs_restored,
        "blob_scan_bytes": report.blob_scan_bytes,
        "cutoff": ts,
    }))
}

// ── Unit 7b: store-level temporal primitives ─────────────────────────────

/// Normalize a free-form timestamp to Macrame's canonical UTC spelling.
///
/// # Errors
/// Returns the engine's `InvalidTimestamp` detail.
pub fn normalize_timestamp(value: &str) -> Result<String, String> {
    macrame::util::timestamp::normalize(value).map_err(|e| {
        format!(
            "InvalidRequest: invalid timestamp {value:?}: expected Macrame canonical UTC (YYYY-MM-DDTHH:MM:SS.ffffffZ); {e:?}"
        )
    })
}

/// Every lineage the ledger knows, trunk first.
///
/// # Errors
/// Returns the store's message when the ledger cannot be read.
pub fn branches(store: &CodeGraphStore) -> Result<Vec<Value>, String> {
    let infos = store
        .branches()
        .map_err(|e| format!("EngineError: branches() failed: {e:?}"))?;
    Ok(infos
        .into_iter()
        .map(|b| {
            json!({
                "name": b.name,
                "parent": b.parent,
                "forked_at": b.forked_at,
                "created_at": b.created_at,
            })
        })
        .collect())
}

/// The content spine of one file, plus versions reached through renames.
///
/// # Errors
/// Returns the store's message when the ledger cannot be read.
pub fn history_of(store: &CodeGraphStore, file: &str) -> Result<Value, String> {
    let module_id = crate::canonical_lookup_id(&format!("{file}::module"));
    let path = module_id
        .strip_suffix("::module")
        .unwrap_or(file)
        .to_string();
    let chain = store
        .history_of(&path)
        .map_err(|e| format!("EngineError: history_of({file:?}) failed: {e:?}"))?;
    let predecessors = store
        .rename_predecessors(&module_id)
        .map_err(|e| format!("EngineError: history_of({file:?}) rename chain failed: {e:?}"))?;
    let mut versions = Vec::new();
    for version in &chain {
        versions.push(json!({
            "digest": version.digest,
            "valid_from": version.valid_from,
            "valid_to": version.valid_to,
            "open": version.open,
            "parent_digest": version.parent_digest,
            "size_bytes": version.size_bytes,
            "line_count": version.line_count,
        }));
    }
    for link in predecessors.iter().rev() {
        let old_path = link
            .old_id
            .strip_suffix("::module")
            .unwrap_or(&link.old_path)
            .to_string();
        let older = store
            .history_of(&old_path)
            .map_err(|e| format!("EngineError: history_of({old_path:?}) failed: {e:?}"))?;
        for version in older {
            versions.push(json!({
                "digest": version.digest,
                "valid_from": version.valid_from,
                "valid_to": version.valid_to,
                "open": version.open,
                "parent_digest": version.parent_digest,
                "size_bytes": version.size_bytes,
                "line_count": version.line_count,
                "file": old_path,
                "via_rename": {
                    "from": link.old_id,
                    "to": link.new_id,
                    "commit": link.commit,
                    "similarity": link.similarity,
                },
            }));
        }
    }
    let count = versions.len();
    Ok(json!({"file": path, "versions": versions, "count": count}))
}

/// The store's own ingest cursors, horizons and completion counts.
///
/// # Errors
/// Returns `InvalidRequest` for an unreadable store and `EngineError` when
/// the report cannot be serialized.
pub fn ingest_status(store: &CodeGraphStore) -> Result<Value, String> {
    let report = crate::graph::ingest::store_status(store)
        .map_err(|e| format!("EngineError: store_status failed: {e}"))?;
    serde_json::to_value(report).map_err(|e| format!("EngineError: store_status report JSON: {e}"))
}

// ── Unit 7c: recorded-axis fold primitives ───────────────────────────────

fn arm_error(e: crate::graph::ingest::ReadArmError) -> String {
    match e {
        crate::graph::ingest::ReadArmError::Store(_) => format!("EngineError: {e}"),
        other => format!("InvalidRequest: {other}"),
    }
}

fn temporal_error(e: macrame::DbError) -> String {
    match e {
        macrame::DbError::UnknownBranch(name) => {
            format!("InvalidRequest: unknown branch {name:?}: fork it first")
        }
        macrame::DbError::InvalidBranchId(name) => {
            format!("InvalidRequest: invalid branch name {name:?}")
        }
        other => format!("EngineError: {other:?}"),
    }
}

fn resolved_read_json(r: &crate::graph::ingest::ResolvedRead) -> Value {
    json!({
        "fold_ts": r.fold_ts,
        "basis": r.basis,
        "sha": r.sha,
        "horizon": r.horizon,
        "dirty_at_commit": r.dirty_at_commit,
        "address": r.address,
        "parent_drift": r.parent_drift,
        "lineage": r.fold_branch,
    })
}

struct Fold {
    snap: Arc<ProjectedGraph>,
    predates: bool,
    resolved: crate::graph::ingest::ResolvedRead,
}

fn fold_projection(
    store: &CodeGraphStore,
    ts: &str,
    branch: &str,
    strict: bool,
    at: Option<&str>,
    valid: bool,
) -> Result<Fold, String> {
    let resolved = if valid {
        crate::graph::ingest::resolve_valid_read(store, branch, ts, at).map_err(arm_error)?
    } else {
        let lineage = if branch == "main" { "main" } else { branch };
        crate::graph::ingest::resolve_temporal_read(store, lineage, ts, strict, at)
            .map_err(arm_error)?
    };
    let state = if valid {
        store.reconstruct_valid(&resolved.fold_ts)
    } else {
        store.reconstruct_on(&resolved.fold_ts, &resolved.fold_branch)
    }
    .map_err(temporal_error)?;
    let predates = state.predates_recorded_history;
    let (projection, _) = crate::graph::cold_start::projection_from_state(&state).map_err(|e| {
        format!(
            "EngineError: projection at {:?} failed: {e}",
            resolved.fold_ts
        )
    })?;
    Ok(Fold {
        snap: Arc::new(projection),
        predates,
        resolved,
    })
}

/// Batched entity fold at one recorded instant.
///
/// # Errors
/// Returns the arm's refusal or a store failure.
pub fn lookup_at(
    store: &CodeGraphStore,
    ids: &[String],
    timestamp: &str,
    branch: &str,
    strict: bool,
    at: Option<&str>,
    valid: bool,
) -> Result<Value, String> {
    let ts = if at.is_none() {
        normalize_timestamp(timestamp)?
    } else {
        timestamp.to_string()
    };
    let fold = fold_projection(store, &ts, branch, strict, at, valid)?;
    let snap = fold.snap;
    let predates = fold.predates;
    let resolved = fold.resolved;
    let mut entities = Map::new();
    for id in ids {
        let canon = crate::canonical_lookup_id(id);
        let found = entity_json(&snap, id)
            .or_else(|| {
                if canon != *id {
                    entity_json(&snap, &canon)
                } else {
                    None
                }
            })
            .or_else(|| {
                crate::resolve_qualified_name(&snap, &canon)
                    .and_then(|resolved_id| entity_json(&snap, &resolved_id))
            });
        entities.insert(id.clone(), found.unwrap_or(Value::Null));
    }
    let mut out = Map::new();
    insert(&mut out, "entities", entities);
    insert(&mut out, "predates_recorded_history", predates);
    for (key, value) in resolved_read_json(&resolved)
        .as_object()
        .unwrap_or(&Map::new())
    {
        out.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(out))
}

type HashSpan = (String, Option<u64>, Option<u64>, Option<u64>);

fn hash_span(snap: &ProjectedGraph, id: &str) -> Option<HashSpan> {
    if let Some(f) = snap.functions.get(id) {
        return Some((
            f.id.clone(),
            Some(f.content_hash),
            Some(f.line as u64),
            Some(f.exit_line as u64),
        ));
    }
    if let Some(c) = snap.classes.get(id) {
        return Some((
            c.id.clone(),
            Some(c.content_hash),
            Some(c.line as u64),
            Some(c.exit_line as u64),
        ));
    }
    if let Some(m) = snap.modules.get(id) {
        return Some((m.id.clone(), Some(m.content_hash), None, None));
    }
    if let Some(i) = snap.imports.get(id) {
        return Some((i.id.clone(), None, Some(i.line as u64), None));
    }
    if let Some(k) = snap.constants.get(id) {
        return Some((k.id.clone(), None, None, None));
    }
    if let Some(t) = snap.type_aliases.get(id) {
        return Some((t.id.clone(), None, None, None));
    }
    snap.routes
        .get(id)
        .map(|r| (r.id.clone(), None, None, None))
}

/// Per-entity content hashes at one instant, keyed by requested id.
///
/// # Errors
/// Returns the engine's refusal for a bad stamp or lineage.
pub fn content_hashes_at(
    store: &CodeGraphStore,
    ids: &[String],
    timestamp: &str,
    branch: &str,
    valid: bool,
) -> Result<Value, String> {
    let ts = normalize_timestamp(timestamp)?;
    if valid && branch != "main" {
        return Err("InvalidRequest: valid-axis reads on a branch need U-2".to_string());
    }
    let fold = fold_projection(store, &ts, branch, false, None, valid)?;
    let snap = fold.snap;
    let predates = fold.predates;
    let mut hashes = Map::new();
    for id in ids {
        let canon = crate::canonical_lookup_id(id);
        let found = hash_span(&snap, id)
            .or_else(|| {
                if canon != *id {
                    hash_span(&snap, &canon)
                } else {
                    None
                }
            })
            .or_else(|| {
                crate::resolve_qualified_name(&snap, &canon).and_then(|r| hash_span(&snap, &r))
            });
        let value = match found {
            Some((actual, hash, start, end)) => json!({
                "id": actual,
                "content_hash": hash,
                "start_line": start,
                "end_line": end,
            }),
            None => Value::Null,
        };
        hashes.insert(id.clone(), value);
    }
    Ok(json!({"hashes": hashes, "predates_recorded_history": predates}))
}

/// Source refs for a set of entity ids at one instant.
///
/// # Errors
/// Returns the arm's refusal or a store failure.
pub fn source_refs_at(
    store: &CodeGraphStore,
    ids: &[String],
    timestamp: &str,
    branch: &str,
    strict: bool,
    at: Option<&str>,
    valid: bool,
) -> Result<Value, String> {
    let ts = if at.is_none() {
        normalize_timestamp(timestamp)?
    } else {
        timestamp.to_string()
    };
    let resolved = fold_projection(store, &ts, branch, strict, at, valid)?.resolved;
    let (refs, predates) = store
        .source_refs_at(ids, &resolved.fold_ts, &resolved.fold_branch)
        .map_err(temporal_error)?;
    let mut by_id = Map::new();
    for (id, r) in refs {
        by_id.insert(
            id,
            json!({
                "id": r.id,
                "file_path": r.file_path,
                "digest": r.digest,
                "span_start": r.span_start,
                "span_end": r.span_end,
                "has_span": r.has_span,
                "full_file": r.full_file,
                "start_line": r.start_line,
                "end_line": r.end_line,
                "handler_id": r.handler_id,
            }),
        );
    }
    let mut out = Map::new();
    insert(&mut out, "refs", by_id);
    insert(&mut out, "predates_recorded_history", predates);
    for (key, value) in resolved_read_json(&resolved)
        .as_object()
        .unwrap_or(&Map::new())
    {
        out.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(out))
}

/// Every recorded generation in a window, oldest first.
///
/// # Errors
/// Returns a bad-stamp or store error.
pub fn recorded_generations(
    store: &CodeGraphStore,
    from: Option<&str>,
    to: Option<&str>,
    branch: &str,
) -> Result<Vec<String>, String> {
    let lo = from.map(normalize_timestamp).transpose()?;
    let hi = to.map(normalize_timestamp).transpose()?;
    store
        .recorded_generations(lo.as_deref(), hi.as_deref(), branch)
        .map_err(|e| format!("EngineError: recorded_generations failed: {e:?}"))
}

/// Every concept id visible on a lineage.
///
/// # Errors
/// Returns the store's message for an unknown lineage.
pub fn visible_all_concept_ids(
    store: &CodeGraphStore,
    branch: &str,
) -> Result<Vec<String>, String> {
    store
        .visible_all_concept_ids(branch)
        .map_err(temporal_error)
}

/// Canonical id for a possibly old-spelled entity id.
pub fn canonical_id(id: &str) -> String {
    crate::canonical_lookup_id(id)
}

/// Every module id visible on a lineage — the universe `verify_history`
/// sweeps (modules only, not every concept kind).
///
/// # Errors
/// Returns the store's message for an unknown lineage.
pub fn visible_module_concept_ids(
    store: &CodeGraphStore,
    branch: &str,
) -> Result<Vec<String>, String> {
    store
        .visible_module_concept_ids(branch)
        .map_err(temporal_error)
}

// ── Unit 8: write-side ingest ────────────────────────────────────────────

fn ingest_error(e: crate::graph::ingest::IngestError) -> String {
    use crate::graph::ingest::IngestError;
    let user = match &e {
        IngestError::Git(crate::fs::git::GitError::UnknownBranch(_))
        | IngestError::Git(crate::fs::git::GitError::UnknownRevision(_))
        | IngestError::Git(crate::fs::git::GitError::NotAncestor(_))
        | IngestError::Git(crate::fs::git::GitError::NoCommonHistory { .. })
        | IngestError::Git(crate::fs::git::GitError::PathNotFound { .. })
        | IngestError::UnknownHighWater { .. }
        | IngestError::OutOfOrder { .. }
        | IngestError::NotDescendant { .. }
        | IngestError::IndexNowRefused { .. }
        | IngestError::TrunkBranchMismatch { .. }
        | IngestError::ForkMismatch { .. }
        | IngestError::LineageExists { .. }
        | IngestError::InvalidLineage { .. }
        | IngestError::TrunkRewritten { .. }
        | IngestError::BranchNowMerged { .. }
        | IngestError::WindowNeedsBelief { .. }
        | IngestError::WindowEndMismatch { .. }
        | IngestError::WindowOffChain { .. }
        | IngestError::WindowEmpty { .. }
        | IngestError::WindowStampsOverlap { .. } => true,
        IngestError::Store(_) => false,
        _ => true,
    };
    if user {
        format!("InvalidRequest: {e}")
    } else {
        format!("EngineError: {e}")
    }
}

/// Ingest git history into a commit-time store, trunk or branch lineage.
///
/// # Errors
/// Returns the ingest refusal or a store failure.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the Python ingest_history parameters"
)]
pub fn ingest_history(
    store_path: &str,
    repo: &str,
    branch: &str,
    from: Option<&str>,
    to: Option<&str>,
    dry_run: bool,
    lineage: Option<&str>,
    parent_ref: &str,
) -> Result<Value, String> {
    let effective = lineage.unwrap_or(branch);
    let report = if effective == "main" {
        crate::graph::ingest::ingest_history(store_path, repo, branch, from, to, dry_run)
    } else {
        crate::graph::ingest::ingest_branch_evolving(
            store_path, repo, branch, lineage, parent_ref, from, dry_run,
        )
    }
    .map_err(ingest_error)?;
    serde_json::to_value(report).map_err(|e| format!("EngineError: report JSON: {e}"))
}

/// Heal merge commits the original trunk walk skipped hollow.
///
/// # Errors
/// Returns the ingest refusal or a store failure.
pub fn reingest_merged_commits(
    store_path: &str,
    repo: &str,
    branch: &str,
    dry_run: bool,
) -> Result<Value, String> {
    let report = crate::graph::ingest::reingest_merged_commits(store_path, repo, branch, dry_run)
        .map_err(ingest_error)?;
    serde_json::to_value(report).map_err(|e| format!("EngineError: report JSON: {e}"))
}

/// Rebuild a lineage's commit index from its ledger completions.
///
/// # Errors
/// Returns a store-open refusal or an ingest failure.
pub fn backfill_commit_index(store_path: &str, branch: &str) -> Result<Value, String> {
    let store = CodeGraphStore::open(store_path).map_err(|e| {
        format!("InvalidRequest: backfill_commit_index cannot open {store_path:?}: {e:?}")
    })?;
    let lineage = if branch == "main" { "main" } else { branch };
    let report =
        crate::graph::ingest::backfill_commit_index(&store, lineage, true).map_err(ingest_error)?;
    serde_json::to_value(report).map_err(|e| format!("EngineError: report JSON: {e}"))
}

/// Rebuild commit messages from their git objects.
///
/// # Errors
/// Returns a repo/store refusal or an ingest failure.
pub fn backfill_commit_messages(store_path: &str, repo: &str) -> Result<Value, String> {
    let report = crate::graph::ingest::backfill_commit_messages(store_path, repo)
        .map_err(|e| format!("InvalidRequest: {e}"))?;
    serde_json::to_value(report).map_err(|e| format!("EngineError: report JSON: {e}"))
}

// ── Unit 9: mutation planners and single-file sync ───────────────────────

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::mutation::{MutationEngine, MutationPlan};

static PLANS: OnceLock<RwLock<HashMap<String, MutationPlan>>> = OnceLock::new();

fn plans() -> &'static RwLock<HashMap<String, MutationPlan>> {
    PLANS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn make_engine() -> MutationEngine {
    let engine = MutationEngine::new(crate::active_config().mutation.clone());
    match crate::INDEXED_ROOT.read().clone() {
        Some(root) => engine.with_project_root(root),
        None => engine,
    }
}

/// Canonical form of an entity id (full spelling, not just its file head).
pub fn canonical_entity_id(entity_id: &str) -> String {
    crate::canonical_entity_id(entity_id)
}

fn plan_json(plan: &MutationPlan) -> Value {
    let mut sources: HashMap<String, Vec<u8>> = HashMap::new();
    let edits: Vec<Value> = plan
        .edits
        .iter()
        .map(|e| {
            let source = sources.entry(e.file.clone()).or_insert_with(|| {
                crate::graph::module_resolution::read_project_file(&e.file).into_bytes()
            });
            let (line, col) = crate::types::line_col_at(source, e.span.start);
            let (end_line, end_col) = crate::types::line_col_at(source, e.span.end);
            json!({
                "file": e.file,
                "span_start": e.span.start,
                "span_end": e.span.end,
                "line": line,
                "col": col,
                "end_line": end_line,
                "end_col": end_col,
                "replacement": e.replacement,
                "expected_hash": e.expected_hash,
            })
        })
        .collect();
    let unverified: Vec<Value> = plan
        .unverified_sites
        .iter()
        .map(|u| {
            json!({
                "file": u.file,
                "line": u.line,
                "snippet": u.snippet,
                "reason": u.reason,
            })
        })
        .collect();
    json!({
        "id": plan.id,
        "tool": plan.tool,
        "diff_preview": plan.diff_preview,
        "affected_files": plan.affected_files,
        "warnings": plan.warnings,
        "unverified_sites": unverified,
        "edits": edits,
    })
}

fn stash(plan: MutationPlan) -> (String, Value) {
    let id = plan.id.clone();
    let value = plan_json(&plan);
    if let Ok(mut map) = plans().write() {
        map.insert(id.clone(), plan);
    }
    (id, value)
}

fn mutation_error(e: impl std::fmt::Debug) -> String {
    format!("MutationFailed: {e:?}")
}

/// Plan a body replacement. The plan is stashed for a later `apply_plan`.
///
/// # Errors
/// Returns the planner's refusal.
pub fn plan_replace_body(
    entity_id: &str,
    new_body: &str,
    expected_hash: Option<String>,
    dry_run: bool,
    snap: &ProjectedGraph,
) -> Result<(String, Value), String> {
    let plan = make_engine()
        .plan_body_replacement(entity_id, new_body, expected_hash, dry_run, snap)
        .map_err(mutation_error)?;
    Ok(stash(plan))
}

/// Plan a signature update. The plan is stashed for a later `apply_plan`.
///
/// # Errors
/// Returns the planner's refusal.
pub fn plan_update_signature(
    entity_id: &str,
    new_signature: &str,
    inject_defaults: bool,
    dry_run: bool,
    snap: &ProjectedGraph,
) -> Result<(String, Value), String> {
    let plan = make_engine()
        .plan_signature_update(
            entity_id,
            new_signature,
            &HashMap::new(),
            inject_defaults,
            dry_run,
            snap,
        )
        .map_err(mutation_error)?;
    Ok(stash(plan))
}

/// Plan a rename. The plan is stashed for a later `apply_plan`.
///
/// # Errors
/// Returns the planner's refusal.
pub fn plan_rename(
    entity_id: &str,
    new_name: &str,
    dry_run: bool,
    snap: &ProjectedGraph,
) -> Result<(String, Value), String> {
    let plan = make_engine()
        .plan_rename(entity_id, new_name, false, dry_run, snap)
        .map_err(mutation_error)?;
    Ok(stash(plan))
}

/// Plan a new entity. The plan is stashed for a later `apply_plan`.
///
/// # Errors
/// Returns the planner's refusal.
pub fn plan_create_entity(
    target_file: &str,
    anchor: &str,
    code: &str,
    dry_run: bool,
    snap: &ProjectedGraph,
) -> Result<(String, Value), String> {
    let plan = make_engine()
        .plan_create_entity(target_file, anchor, code, dry_run, snap)
        .map_err(mutation_error)?;
    Ok(stash(plan))
}

/// Apply a previously planned mutation, returning the engine's disk outcome.
///
/// # Errors
/// Returns `MutationFailed` when the plan is unknown or the write fails.
pub fn apply_plan(id: &str) -> Result<Value, String> {
    let plan = plans()
        .write()
        .map_err(|e| mutation_error(e.to_string()))?
        .remove(id)
        .ok_or_else(|| format!("MutationFailed: unknown plan {id:?}"))?;
    let result = make_engine().apply(&plan);
    serde_json::to_value(result).map_err(|e| mutation_error(e.to_string()))
}

/// Sync one file into the graph, from `content` or from disk.
///
/// # Errors
/// Returns the engine's update failure.
pub fn update_file(file_path: &str, content: Option<&str>) -> Result<Value, String> {
    let outcome = {
        let mut guard = crate::GLOBAL_GRAPH.write();
        let graph = guard
            .as_mut()
            .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
        graph
            .update_file(file_path, content, None)
            .map_err(|e| format!("EngineError: {e}"))?
    };
    let quality = match outcome.parse_quality {
        crate::types::ParseQuality::Clean => "clean",
        crate::types::ParseQuality::Partial => "partial",
        crate::types::ParseQuality::Tainted => "tainted",
        crate::types::ParseQuality::Deferred => "deferred",
    };
    Ok(json!({
        "fully_applied": outcome.parse_errors == 0,
        "entities_added": outcome.entities_added,
        "entities_removed": outcome.entities_removed,
        "affected_files": outcome.affected_files,
        "source_hash": outcome.source_hash,
        "update_error": outcome.update_error,
        "persistence_error": outcome.persistence_error,
        "blob_error": outcome.blob_error,
        "parse_quality": quality,
        "parse_errors": outcome.parse_errors,
        "elapsed_ms": outcome.elapsed_ms,
        "changed_symbols": outcome.changed_symbols.iter().map(|s| json!({
            "kind": s.kind,
            "operation": s.operation,
            "id": s.id,
            "name": s.name,
            "line": s.line,
        })).collect::<Vec<_>>(),
        "new_unresolved": outcome.new_unresolved,
        "newly_resolved": outcome.newly_resolved,
        "epoch_before": outcome.epoch_before,
        "epoch_after": outcome.epoch_after,
        "observed_at": outcome.observed_at,
        "source": outcome.source,
        "lineage": outcome.lineage,
        "blobs_stored": outcome.blobs_stored,
        "blobs_skipped_oversize": outcome.blobs_skipped_oversize,
        "blobs_skipped_excluded": outcome.blobs_skipped_excluded,
        "blobs_bytes": outcome.blobs_bytes,
    }))
}

// ── Unit 12: embedding-backed search (pure Rust) ─────────────────────────

fn cosine(query: &[f64], vector: &[f64]) -> f64 {
    if query.len() != vector.len() || query.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut q_norm = 0.0f64;
    let mut v_norm = 0.0f64;
    for (q, v) in query.iter().zip(vector.iter()) {
        dot += q * v;
        q_norm += q * q;
        v_norm += v * v;
    }
    if q_norm == 0.0 || v_norm == 0.0 {
        0.0
    } else {
        dot / (q_norm.sqrt() * v_norm.sqrt())
    }
}

fn materialize_scored(snap: &ProjectedGraph, scored: Vec<(f64, String)>, top_k: usize) -> Value {
    let mut scored = scored;
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);
    let rows: Vec<Value> = scored
        .into_iter()
        .filter_map(|(similarity, id)| {
            let mut row = entity_json(snap, &id)?;
            if let Some(map) = row.as_object_mut() {
                insert(map, "similarity", similarity);
            }
            Some(row)
        })
        .collect();
    json!(rows)
}

/// Search the persisted model table (or in-memory vectors) with a ready query vector.
pub fn search_similar_vec(query: &[f64], top_k: usize) -> Result<Value, String> {
    let guard = crate::GLOBAL_GRAPH.read();
    let graph = guard
        .as_ref()
        .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
    let snap = graph.snapshot();
    if let Some(store) = graph.store.as_ref() {
        let tables = store
            .model_tables_at_dim(query.len())
            .map_err(|e| format!("{e:?}"))?;
        if tables.len() > 1 {
            return Err(format!(
                "embedding dimension {} matches {} model tables — one model per store is the contract. Recompute from clean with a single model.",
                query.len(),
                tables.len()
            ));
        }
        if let Some(model) = tables.first() {
            let vectors = store
                .read_model_vectors(model)
                .map_err(|e| format!("{e:?}"))?;
            if vectors.dimension != query.len() {
                return Err(format!(
                    "embedding dimension mismatch: query is {}-dim but {} holds {}-dim vectors",
                    query.len(),
                    vectors.model_table,
                    vectors.dimension
                ));
            }
            let scored = vectors
                .rows
                .iter()
                .map(|(id, vector)| {
                    let vector: Vec<f64> = vector.iter().map(|v| *v as f64).collect();
                    (cosine(query, &vector), id.clone())
                })
                .collect();
            return Ok(materialize_scored(&snap, scored, top_k));
        }
    }
    let mut scored = Vec::new();
    for (id, entity) in &snap.functions {
        if !entity.embedding.vec.is_empty() {
            scored.push((cosine(query, &entity.embedding.vec), id.to_string()));
        }
    }
    for (id, entity) in &snap.classes {
        if !entity.embedding.vec.is_empty() {
            scored.push((cosine(query, &entity.embedding.vec), id.to_string()));
        }
    }
    for (id, entity) in &snap.modules {
        if !entity.embedding.vec.is_empty() {
            scored.push((cosine(query, &entity.embedding.vec), id.to_string()));
        }
    }
    for (id, entity) in &snap.imports {
        if !entity.embedding.vec.is_empty() {
            scored.push((cosine(query, &entity.embedding.vec), id.to_string()));
        }
    }
    for (id, entity) in &snap.constants {
        if !entity.embedding.vec.is_empty() {
            scored.push((cosine(query, &entity.embedding.vec), id.to_string()));
        }
    }
    for (id, entity) in &snap.type_aliases {
        if !entity.embedding.vec.is_empty() {
            scored.push((cosine(query, &entity.embedding.vec), id.to_string()));
        }
    }
    Ok(materialize_scored(&snap, scored, top_k))
}

/// Fuse keyword and vector ranks with reciprocal rank fusion.
pub fn search_hybrid_vec(
    model: &str,
    text: &str,
    query: &[f64],
    top_k: usize,
) -> Result<Value, String> {
    let guard = crate::GLOBAL_GRAPH.read();
    let graph = guard
        .as_ref()
        .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
    let snap = graph.snapshot();
    let store = graph.store.as_ref().ok_or_else(|| {
        "search_hybrid needs a stored graph (hybrid search reads the ledger's FTS and vector tables)".to_string()
    })?;
    let hits = store
        .search_hybrid(model, text, query, top_k)
        .map_err(|e| format!("{e:?}"))?;
    let rows: Vec<Value> = hits
        .into_iter()
        .filter_map(|hit| {
            let mut row = entity_json(&snap, &hit.concept_id)?;
            if let Some(map) = row.as_object_mut() {
                insert(map, "score", hit.score);
                insert(map, "vector_rank", hit.vector_rank);
                insert(map, "keyword_rank", hit.keyword_rank);
            }
            Some(row)
        })
        .collect();
    Ok(json!(rows))
}

/// Write a batch of vectors into the projection (one clone, not one per row).
pub fn set_embeddings(entries: Vec<(String, Vec<f64>, String)>) -> Result<Value, String> {
    let mut guard = crate::GLOBAL_GRAPH.write();
    let graph = guard
        .as_mut()
        .ok_or_else(|| "No graph loaded — run coderadar analyze first".to_string())?;
    let (applied, missing) = graph
        .set_embeddings_bulk(entries)
        .map_err(|e| e.to_string())?;
    Ok(json!({"applied": applied, "missing": missing}))
}

/// Clear every embedding vector (model-switch maintenance).
pub fn clear_embeddings() -> Result<Value, String> {
    let mut guard = crate::GLOBAL_GRAPH.write();
    let graph = guard
        .as_mut()
        .ok_or_else(|| "No graph loaded — run coderadar analyze first".to_string())?;
    Ok(json!({"cleared": graph.clear_all_embeddings()}))
}

/// Persist vector keys to the ledger's per-model table.
pub fn persist_embeddings(model: &str, entries: Vec<(String, String)>) -> Result<Value, String> {
    let guard = crate::GLOBAL_GRAPH.read();
    let graph = guard
        .as_ref()
        .ok_or_else(|| "No stored graph — vectors stay memory-only".to_string())?;
    let store = graph
        .store
        .as_ref()
        .ok_or_else(|| "No stored graph — vectors stay memory-only".to_string())?;
    let snap = graph.snapshot();
    let report = store
        .persist_projection_embeddings(model, &snap, &entries)
        .map_err(|e| format!("{e:?}"))?;
    Ok(json!({
        "persisted": report.persisted,
        "model": report.model_table,
        "dimension": report.dimension,
        "healed": report.healed
    }))
}

/// Collect `(id, embed_text, content_hash)` for every indexable entity.
pub fn embedding_targets() -> Result<Vec<(String, String, String)>, String> {
    let guard = crate::GLOBAL_GRAPH.read();
    let graph = guard
        .as_ref()
        .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
    let snap = graph.snapshot();
    let mut out = Vec::new();
    let mut collect = |id: &crate::types::EntityId, value: &Value| {
        let text = [
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            value
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            value
                .get("docstring")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ]
        .join(" ");
        let hash = value
            .get("content_hash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        out.push((id.to_string(), text, hash));
    };
    for id in snap.functions.keys() {
        if let Some(value) = entity_json(&snap, id) {
            collect(id, &value);
        }
    }
    for id in snap.classes.keys() {
        if let Some(value) = entity_json(&snap, id) {
            collect(id, &value);
        }
    }
    for id in snap.modules.keys() {
        if let Some(value) = entity_json(&snap, id) {
            collect(id, &value);
        }
    }
    for id in snap.imports.keys() {
        if let Some(value) = entity_json(&snap, id) {
            collect(id, &value);
        }
    }
    for id in snap.constants.keys() {
        if let Some(value) = entity_json(&snap, id) {
            collect(id, &value);
        }
    }
    for id in snap.type_aliases.keys() {
        if let Some(value) = entity_json(&snap, id) {
            collect(id, &value);
        }
    }
    Ok(out)
}

/// Sorted, de-duplicated unresolved call targets for one entity.
pub fn unresolved_targets(entity_id: &str) -> Vec<String> {
    let guard = crate::GLOBAL_GRAPH.read();
    let Some(graph) = guard.as_ref() else {
        return Vec::new();
    };
    let snap = graph.snapshot();
    crate::graph::CodeGraph::list_unresolved_targets(&snap, entity_id)
}

/// Ambiguous base-class findings as plain JSON rows.
pub fn ambiguous_base_details() -> Vec<Value> {
    let guard = crate::GLOBAL_GRAPH.read();
    let Some(graph) = guard.as_ref() else {
        return Vec::new();
    };
    let snap = graph.snapshot();
    snap.ambiguous_bases
        .iter()
        .map(|a| {
            json!({
                "class": a.class_name,
                "base": a.base_name,
                "candidates": a.candidates.clone(),
            })
        })
        .collect()
}

// ── Unit 14: git-sync primitives (pure Rust) ────────────────────────────

pub fn branch_plan(repo: &str, parent_ref: &str) -> Result<Vec<Value>, String> {
    let entries = crate::fs::git::branch_plan(repo, parent_ref)
        .map_err(|e| format!("branch plan failed: {e:?}"))?;
    Ok(entries
        .into_iter()
        .map(|e| json!({"name": e.name, "tip": e.tip, "merged": e.merged}))
        .collect())
}

pub fn git_head(repo: &str) -> Result<Value, String> {
    let head = crate::fs::git::head_info(repo).map_err(|e| format!("git head failed: {e:?}"))?;
    Ok(json!({
        "branch": head.branch,
        "sha": head.sha,
        "detached": head.branch.is_none() && head.sha.is_some(),
    }))
}

pub fn git_workdir_changes(repo: &str, sha: &str) -> Result<Vec<Value>, String> {
    let changes = crate::fs::git::workdir_changes(repo, sha)
        .map_err(|e| format!("git workdir changes failed: {e:?}"))?;
    Ok(changes
        .into_iter()
        .map(|c| json!({"path": c.path, "in_tree": c.in_tree, "in_workdir": c.in_workdir}))
        .collect())
}

pub fn git_blob_text(repo: &str, sha: &str, path: &str) -> Result<String, String> {
    let bytes = crate::fs::git::blob_bytes(repo, sha, std::path::Path::new(path))
        .map_err(|e| format!("git blob failed: {e:?}"))?;
    String::from_utf8(bytes).map_err(|_| format!("{path} at {sha} is not UTF-8"))
}

pub fn git_marks(store_path: &str) -> Result<Value, String> {
    let store =
        crate::storage::CodeGraphStore::open(std::path::Path::new(store_path)).map_err(|e| {
            format!("git_marks cannot open {store_path:?}: {e:?} — ingest_history builds stores")
        })?;
    let marks = store
        .git_marks()
        .map_err(|e| format!("cannot read sync marks: {e:?}"))?;
    Ok(json!({"checkout": marks.0, "head": marks.1, "dirty": marks.2}))
}

pub fn record_git_marks(
    store_path: &str,
    checkout: Option<&str>,
    head: Option<&str>,
    dirty: &str,
) -> Result<(), String> {
    let store = crate::storage::CodeGraphStore::open(std::path::Path::new(store_path))
        .map_err(|e| format!("record_git_marks cannot open {store_path:?}: {e:?}"))?;
    store
        .record_git_marks(checkout, head, dirty)
        .map_err(|e| format!("cannot write sync marks: {e:?}"))
}

pub fn record_branch_addresses(
    store_path: &str,
    entries: Vec<(String, String)>,
) -> Result<usize, String> {
    let store = crate::storage::CodeGraphStore::open(std::path::Path::new(store_path))
        .map_err(|e| format!("cannot open store {store_path:?}: {e:?}"))?;
    let mut written = 0usize;
    for (name, tip) in entries {
        match store.kv_get_sync(&crate::graph::ingest::commit_lineage_key(&tip)) {
            Ok(Some(_)) => {}
            _ => continue,
        }
        store
            .kv_put_sync(&crate::graph::ingest::branch_tip_key(&name), &tip)
            .map_err(|e| format!("{e:?}"))?;
        written += 1;
    }
    Ok(written)
}

/// Sync one file as an observation write (source + lineage preserved).
pub fn update_file_observed(
    file_path: &str,
    content: Option<&str>,
    source: &str,
    lineage: Option<&str>,
) -> Result<Value, String> {
    let observation = crate::storage::ObservationSource {
        source,
        tool: None,
        plan_id: None,
        attempt_id: None,
        lineage,
    };
    let outcome = {
        let guard = crate::GLOBAL_GRAPH.read();
        let graph = guard
            .as_ref()
            .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
        graph
            .update_file_observed(file_path, content, None, &observation)
            .map_err(|e| format!("EngineError: {e}"))?
    };
    let quality = match outcome.parse_quality {
        crate::types::ParseQuality::Clean => "clean",
        crate::types::ParseQuality::Partial => "partial",
        crate::types::ParseQuality::Tainted => "tainted",
        crate::types::ParseQuality::Deferred => "deferred",
    };
    Ok(json!({
        "fully_applied": outcome.parse_errors == 0,
        "source_hash": outcome.source_hash,
        "entities_added": outcome.entities_added,
        "entities_removed": outcome.entities_removed,
        "affected_files": outcome.affected_files,
        "parse_quality": quality,
        "parse_errors": outcome.parse_errors,
        "observed_at": outcome.observed_at,
    }))
}

/// Drop a file's entities as an observation on `lineage`.
pub fn remove_file_observed(file_path: &str, lineage: Option<&str>) -> Result<Value, String> {
    let canonical = crate::graph::module_resolution::canonical_file_form(file_path);
    let removed = {
        let guard = crate::GLOBAL_GRAPH.read();
        let graph = guard
            .as_ref()
            .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
        graph.remove_file_on(&canonical, lineage)
    };
    Ok(json!({"removed": removed.len(), "file": canonical}))
}

pub fn list_branches(repo: &str) -> Result<Vec<String>, String> {
    crate::fs::git::list_branches(repo).map_err(|e| format!("list_branches: {e:?}"))
}

/// Persist framework route nodes/edges and retire ghosts outside `scope`.
pub fn register_synthetic_routes(
    nodes: Vec<(String, String, String, String, Vec<String>, String)>,
    edges: Vec<(String, String, String)>,
    scope: &str,
) -> Result<Value, String> {
    let mut guard = crate::GLOBAL_GRAPH.write();
    let graph = guard
        .as_mut()
        .ok_or_else(|| "No graph loaded — run coderadar analyze first".to_string())?;
    let (routes_upserted, routes_retired, pairs_retired) =
        graph.register_synthetic_routes(nodes, edges, scope)?;
    Ok(json!({
        "ok": true,
        "routes_upserted": routes_upserted,
        "routes_retired": routes_retired,
        "pairs_retired": pairs_retired,
    }))
}

/// Stats plus the ledger stamp fields the Python CLI surfaces.
pub fn stats_full(snap: &ProjectedGraph) -> Value {
    let mut value = stats(snap);
    if let Some(map) = value.as_object_mut() {
        let revision = json!(*crate::LEDGER_REVISION.read());
        let graph_guard = crate::GLOBAL_GRAPH.read();
        let indexed_at = graph_guard
            .as_ref()
            .map(|graph| graph.indexed_at())
            .unwrap_or(0.0);
        map.insert("revision".into(), revision);
        map.insert("indexed_at".into(), json!(indexed_at));
        map.insert(
            "indexed_root".into(),
            json!(crate::indexed_root().to_string_lossy()),
        );
    }
    value
}

/// Inspect a Macrame store for load-blocking rows and retire safe leftovers.
///
/// Mirrors `ops.store_repair` without a Python interpreter: reports live v1
/// leftovers, live unreadable rows, edges closed by retirement and legacy
/// id spellings. The caller owns the `--delete` policy.
pub fn store_repair(db_path: &str) -> Result<Value, String> {
    use crate::storage::{classify_v2_concept, CodeGraphStore, V2ConceptClass};

    let store = CodeGraphStore::open(db_path)
        .map_err(|e| format!("store repair failed for {db_path}: {e:?}"))?;
    let live = store
        .live_concept_contents()
        .map_err(|e| format!("store repair failed for {db_path}: {e:?}"))?;
    let mut v1 = 0usize;
    let mut unreadable = 0usize;
    for (_, content) in &live {
        match classify_v2_concept(content) {
            V2ConceptClass::V1Leftover => v1 += 1,
            V2ConceptClass::Unreadable => unreadable += 1,
            _ => {}
        }
    }
    let (retired, edges) = store
        .retire_v1_leftovers()
        .map_err(|e| format!("store repair failed for {db_path}: {e:?}"))?;
    let legacy_ids = crate::legacy_spelled_concept_ids(&store)
        .map_err(|e| format!("store repair failed for {db_path}: {e:?}"))?
        .len();
    Ok(json!({
        "v1_found": v1,
        "v1_retired": retired,
        "edges_retired": edges,
        "unreadable_live": unreadable,
        "legacy_ids": legacy_ids,
    }))
}

/// Every call site in one function, with the resolver's verdict.
///
/// Mirrors the Python `call_sites` op as data. Returns `null` when the
/// entity is not a function.
pub fn call_sites(snap: &ProjectedGraph, entity_id: &str) -> Option<Value> {
    let id = canonical_entity_id(entity_id);
    let sites = crate::graph::CodeGraph::list_call_sites(snap, &id)?;
    Some(serde_json::json!(sites
        .iter()
        .map(|site| serde_json::json!({
            "name": site.name,
            "path": site.path,
            "line": site.line,
            "col": site.col,
            "status": site.status,
            "target": site.target,
            "reason": site.reason,
        }))
        .collect::<Vec<Value>>()))
}

/// Record the branch for bare observation writes on the loaded graph's store.
pub fn record_checkout(branch: Option<&str>) -> Result<(), String> {
    let guard = crate::GLOBAL_GRAPH.read();
    let graph = guard
        .as_ref()
        .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
    let store = graph
        .store
        .as_ref()
        .ok_or_else(|| "no store behind the loaded graph".to_string())?;
    store
        .kv_put_sync(
            crate::graph::ingest::CHECKOUT_KEY,
            branch.unwrap_or("").trim(),
        )
        .map_err(|e| format!("cannot record checkout: {e:?}"))
}

/// The engine's built-in exclusion baseline, used by `exclude list`.
pub fn default_excludes() -> Vec<String> {
    crate::DEFAULT_EXCLUDES
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}

/// Blame rows for one file: `{line, count, author, commit}`.
pub fn git_blame(repo: &str, file: &str) -> Result<Value, String> {
    let rows =
        crate::fs::git::blame_file(repo, file).map_err(|e| format!("git blame failed: {e:?}"))?;
    Ok(serde_json::json!(rows
        .iter()
        .map(|row| serde_json::json!({
            "line": row.line_number,
            "count": row.line_count,
            "author": row.author,
            "commit": row.commit,
        }))
        .collect::<Vec<Value>>()))
}

/// Whether the worktree has no uncommitted or untracked changes.
pub fn git_worktree_clean(repo: &str) -> Result<bool, String> {
    crate::fs::git::is_worktree_clean(repo).map_err(|e| format!("{e:?}"))
}

/// Repo-relative paths changed between two revisions.
pub fn git_changed_files(
    repo: &str,
    old_oid: Option<&str>,
    new_oid: Option<&str>,
) -> Result<Vec<String>, String> {
    crate::fs::git::changed_files_between(repo, old_oid, new_oid)
        .map_err(|e| format!("git diff failed: {e:?}"))
}
