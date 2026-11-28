//! The Rust port of `py_agent/src/coderadar/ops.py` (read/query vertical).
//!
//! Every function returns the plain JSON-shaped value the Python operation
//! returned, or an `Err("Kind: message")` that the PyO3 wrapper maps back to
//! the operation's exception. The graph backend is a separate Rust crate
//! (`core_indexer`); until a store is loaded the error paths are the
//! executable specification recorded in `cases.jsonl`.

#![forbid(unsafe_code)]
#![expect(
    clippy::collapsible_if,
    reason = "nested guards mirror the Python original"
)]

use fancy_regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::{Component, MAIN_SEPARATOR, Path};
use std::sync::OnceLock;

const NO_GRAPH: &str = "RuntimeError: No graph loaded — run coderadar init first";
const NO_INDEX: &str = "NoIndex: No graph loaded — run coderadar init first";
const SEARCH_KINDS: [&str; 7] = [
    "function",
    "class",
    "type_alias",
    "constant",
    "module",
    "import",
    "route",
];

// ── Backend seam ────────────────────────────────────────────────────────────
// The read vertical never touches a file directly: it asks the graph backend.
// The first port step characterises the no-store behaviour; the CLI/MCP units
// replace these with calls into `core_indexer`'s pure Rust API.

fn backend_loaded() -> bool {
    crate::state::projection().is_some()
}

fn graph() -> Result<std::sync::Arc<core_indexer::types::ProjectedGraph>, String> {
    crate::state::projection().ok_or_else(|| NO_GRAPH.to_string())
}

fn backend_lookup(entity_id: &str) -> Result<Option<Value>, String> {
    let snap = graph()?;
    Ok(core_indexer::api::lookup(&snap, entity_id))
}

fn backend_text_search(query: &str, top_k: i64, kind: Option<&str>) -> Result<Vec<Value>, String> {
    let snap = graph()?;
    core_indexer::api::search(&snap, query, top_k.clamp(0, 100) as usize, kind)
}

fn backend_callers(entity_id: &str) -> Result<Vec<Value>, String> {
    let snap = graph()?;
    Ok(core_indexer::api::callers(&snap, entity_id))
}

fn backend_callees(entity_id: &str) -> Result<Vec<Value>, String> {
    let snap = graph()?;
    Ok(core_indexer::api::callees(&snap, entity_id))
}

fn require_index() -> Result<(), String> {
    if backend_loaded() {
        Ok(())
    } else {
        Err(NO_INDEX.to_string())
    }
}

fn not_found(entity_id: &str) -> String {
    format!("NotFound: Entity `{entity_id}` not found.")
}

/// Python's type name for a JSON-shaped value, for the one `display_file`
/// error path the original has (calling `.get` on a non-dict).
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(py_str).collect();
            format!("[{}]", body.join(", "))
        }
        Value::Object(map) => {
            let body: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("'{}': {}", k, py_str(v)))
                .collect();
            format!("{{{}}}", body.join(", "))
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The Windows volume (`C:`, `D:`, a UNC share) a path starts on. `None`
/// everywhere else, where `os.path.relpath` has no cross-volume failure.
fn volume(path: &Path) -> Option<String> {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => Some(prefix.as_os_str().to_string_lossy().into_owned()),
        _ => None,
    }
}

/// Port of `friendly_entity_id` (`py_agent/src/coderadar/ops.py:201`).
pub fn friendly_entity_id(entity_id: String) -> Result<String, String> {
    let friendly = entity_id.replace('\\', "/");
    Ok(friendly.strip_prefix("./").unwrap_or(&friendly).to_string())
}

/// Port of `canonical_entity_id` (`py_agent/src/coderadar/ops.py:215`).
pub fn canonical_entity_id(entity_id: String) -> Result<String, String> {
    if backend_lookup(&entity_id)?.is_some() {
        return Ok(entity_id);
    }

    let mut candidates: Vec<String> = Vec::new();
    let path = Path::new(&entity_id);
    if path.is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            let cwd = core_indexer::fs::strip_verbatim_prefix(cwd);
            if let Some(rel) = pathdiff::diff_paths(path, &cwd) {
                if !rel.is_absolute() && volume(&rel) == volume(path) {
                    let rel = rel.to_string_lossy().to_string();
                    candidates.push(format!(".{MAIN_SEPARATOR}{rel}"));
                    candidates.push(rel);
                }
            }
        }
    }

    let base = entity_id.replace('\\', "/");
    let base = base.strip_prefix("./").unwrap_or(&base);
    for (sep, prefix) in [("/", "./"), ("\\", ".\\")] {
        let body = base.replace('/', sep);
        candidates.push(body.clone());
        candidates.push(format!("{prefix}{body}"));
    }

    for candidate in candidates {
        if !candidate.is_empty() && backend_lookup(&candidate)?.is_some() {
            return Ok(candidate);
        }
    }
    Ok(entity_id)
}

/// Port of `display_file` (`py_agent/src/coderadar/ops.py:258`).
pub fn display_file(d: Value) -> Result<String, String> {
    let Value::Object(map) = &d else {
        return Err(format!(
            "AttributeError: '{}' object has no attribute 'get'",
            py_type(&d)
        ));
    };
    for key in ["file_path", "file", "path"] {
        if let Some(v) = map.get(key) {
            if truthy(v) {
                return Ok(py_str(v));
            }
        }
    }
    let id = map
        .get("id")
        .or_else(|| map.get("entity_id"))
        .map(py_str)
        .unwrap_or_default();
    if let Some((file, _)) = id.split_once("::") {
        return Ok(file.to_string());
    }
    Ok("?".to_string())
}

fn find_entity_inner(entity_id: &str) -> Result<Option<Value>, String> {
    let canonical = canonical_entity_id(entity_id.to_string())?;
    if let Some(found) = backend_lookup(&canonical)? {
        return Ok(Some(found));
    }
    // Rename-chain forwarding is a store-backed read; with no store the
    // Python original returns None here (it swallows ImportError/RuntimeError
    // around `rename_chain`).
    Ok(None)
}

/// Port of `find_entity` (`py_agent/src/coderadar/ops.py:275`).
pub fn find_entity(entity_id: String) -> Result<Option<Value>, String> {
    find_entity_inner(&entity_id)
}

/// Port of `callers` (`py_agent/src/coderadar/ops.py:339`).
pub fn callers(entity_id: String) -> Result<Vec<Value>, String> {
    backend_callers(&entity_id)
}

/// Port of `callees` (`py_agent/src/coderadar/ops.py:350`).
pub fn callees(entity_id: String) -> Result<Vec<Value>, String> {
    backend_callees(&entity_id)
}

/// Port of `suggest_entities` (`py_agent/src/coderadar/ops.py:359`).
pub fn suggest_entities(entity_id: String, limit: i64) -> Result<Vec<Value>, String> {
    let hint = entity_id
        .rsplit_once("::")
        .map_or(entity_id.as_str(), |(_, tail)| tail)
        .rsplit_once('.')
        .map_or_else(
            || {
                entity_id
                    .rsplit_once("::")
                    .map_or(entity_id.as_str(), |(_, t)| t)
            },
            |(_, tail)| tail,
        );
    // The original's `range(len(hint), 2, -1)` never enters for a hint of
    // length 0..=2, so no backend call is made and `[]` is returned.
    if hint.chars().count() <= 2 {
        return Ok(Vec::new());
    }
    for length in (3..=hint.chars().count()).rev() {
        let probe: String = hint.chars().take(length).collect();
        let hits: Vec<Value> = backend_text_search(&probe, 5, None)?
            .into_iter()
            .filter(|hit| {
                hit.get("name").is_some_and(truthy)
                    && hit.get("id").and_then(Value::as_str) != Some(entity_id.as_str())
            })
            .collect();
        if !hits.is_empty() {
            let take = limit.clamp(0, i64::MAX) as usize;
            return Ok(hits.into_iter().take(take).collect());
        }
    }
    Ok(Vec::new())
}

/// Port of `search` (`py_agent/src/coderadar/ops.py:392`).
pub fn search(query: String, kind: Option<String>, top_k: i64) -> Result<Vec<Value>, String> {
    require_index()?;
    if query.trim().is_empty() {
        return Err("InvalidRequest: Please provide a query to search for.".to_string());
    }
    if let Some(ref k) = kind {
        if !SEARCH_KINDS.contains(&k.to_lowercase().as_str()) {
            return Err(format!(
                "InvalidRequest: Unknown kind `{k}` (expected: {}).",
                SEARCH_KINDS.join(" | ")
            ));
        }
    }
    let hits = backend_text_search(&query, top_k.min(20), kind.as_deref())?;
    let take = top_k.clamp(0, i64::MAX) as usize;
    Ok(hits.into_iter().take(take).collect())
}

/// Port of `search_symbols` (`py_agent/src/coderadar/ops.py:414`).
pub fn search_symbols(query: String, top_k: i64, raw: bool) -> Result<Vec<Value>, String> {
    require_index()?;
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let snap = graph()?;
    // `raw` selects FTS5 power syntax in the Python engine; the Rust facade
    // uses the same ranked token matcher as `search` (escaped by design).
    let _ = raw;
    let hits = core_indexer::api::search(&snap, &query, top_k.clamp(1, 50) as usize, None)?;
    Ok(hits
        .into_iter()
        .enumerate()
        .map(|(i, hit)| {
            json!({"id": hit.get("id").cloned().unwrap_or(Value::Null), "rank": -(i as i64) - 1})
        })
        .collect())
}

fn entity_or_raise(entity_id: &str) -> Result<Value, String> {
    find_entity_inner(entity_id)?.ok_or_else(|| not_found(entity_id))
}

/// Port of `node` (`py_agent/src/coderadar/ops.py:448`).
pub fn node(entity_id: String, include_neighbors: bool) -> Result<Value, String> {
    require_index()?;
    let mut entity = entity_or_raise(&entity_id)?;
    if include_neighbors {
        if let Value::Object(ref mut map) = entity {
            map.insert(
                "callers".to_string(),
                Value::Array(backend_callers(&entity_id)?),
            );
            map.insert(
                "callees".to_string(),
                Value::Array(backend_callees(&entity_id)?),
            );
        }
    }
    Ok(entity)
}

/// Port of `affected` (`py_agent/src/coderadar/ops.py:458`).
pub fn affected(entity_id: String, max_depth: i64) -> Result<Value, String> {
    require_index()?;
    let entity = entity_or_raise(&entity_id)?;
    let mut depths: Vec<(i64, Vec<Value>)> = Vec::new();
    let mut visited = std::collections::HashSet::from([entity_id.clone()]);
    let mut queue = std::collections::VecDeque::from([(entity_id.clone(), 0_i64)]);
    let limit = max_depth.min(20);
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= limit {
            continue;
        }
        for caller in backend_callers(&current)? {
            let cid = caller
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !cid.is_empty() && visited.insert(cid.clone()) {
                depths.push((depth + 1, vec![caller]));
                queue.push_back((cid, depth + 1));
            }
        }
    }
    depths.sort_by_key(|(d, _)| *d);
    let mut tree = Map::new();
    for (d, entities) in depths {
        tree.entry(d.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(Value::Array(rows)) = tree.get_mut(&d.to_string()) {
            rows.extend(entities);
        }
    }
    Ok(json!({
        "entity": entity,
        "max_depth": max_depth,
        "depths": tree,
        "central_ids": [],
    }))
}

/// Port of `module_children` (`py_agent/src/coderadar/ops.py:512`).
pub fn module_children(module_id: String) -> Result<Value, String> {
    require_index()?;
    if module_id.trim().is_empty() {
        return Err(
            "InvalidRequest: Please provide a module ID (e.g. 'src/main.py::module').".to_string(),
        );
    }
    let module_id = canonical_entity_id(module_id)?;
    let snap = graph()?;
    let Some(module) = snap.modules.get(module_id.as_str()) else {
        return Err(format!(
            "NotFound: Module `{module_id}` not found or error: not in graph"
        ));
    };
    let rows = |ids: &[String]| -> Vec<Value> {
        ids.iter()
            .map(|id| {
                let entity = core_indexer::api::entity_json(&snap, id);
                json!({
                    "name": entity.as_ref().and_then(|e| e.get("name")).cloned().unwrap_or(Value::Null),
                    "id": id,
                    "line": entity.as_ref().and_then(|e| e.get("line")).cloned().unwrap_or(Value::Null),
                })
            })
            .collect()
    };
    Ok(json!({
        "module_id": module_id,
        "classes": rows(&module.classes),
        "functions": rows(&module.functions),
        "imports": rows(&module.imports),
        "constants": rows(&module.constants),
    }))
}

/// Port of `resolve` (`py_agent/src/coderadar/ops.py:531`).
pub fn resolve(name: String, limit: i64) -> Result<Value, String> {
    require_index()?;
    if name.trim().is_empty() {
        return Err("InvalidRequest: Please provide a name or path to resolve.".to_string());
    }
    let mode = if name.starts_with('/') {
        "route"
    } else {
        "reference"
    };
    let snap = graph()?;
    let probe = name.trim_start_matches('/');
    let mut results: Vec<Value> = Vec::new();
    if mode == "route" {
        let static_segments: Vec<&str> = name
            .trim_matches('/')
            .split('/')
            .filter(|segment| {
                !matches!(segment.chars().next(), Some('<' | ':' | '{')) && *segment != "*"
            })
            .collect();
        let query = if static_segments.is_empty() {
            name.clone()
        } else {
            static_segments.join(" ")
        };
        let route_candidates = core_indexer::api::search(
            &snap,
            &query,
            limit.clamp(1, 20).saturating_mul(2) as usize,
            None,
        )?;
        for route in route_candidates {
            let route_id = route.get("id").and_then(Value::as_str).unwrap_or_default();
            let route_name = route
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !route_patterns_match(name.clone(), route_name.to_string())? {
                continue;
            }
            let handlers = core_indexer::api::callees(&snap, route_id);
            let found = framework_resolve_route(name.clone(), vec![route], handlers, limit)?;
            if let Value::Array(rows) = found {
                results.extend(rows);
            }
        }
    } else {
        results = core_indexer::api::search(&snap, probe, limit.clamp(1, 20) as usize, None)?
            .into_iter()
            .map(|mut hit| {
                if let Value::Object(ref mut map) = hit {
                    map.insert("confidence".to_string(), json!(0.6));
                    map.insert("resolved_by".to_string(), json!("search"));
                }
                hit
            })
            .collect();
    }
    let mut seen: Vec<String> = Vec::new();
    results.retain(|hit| {
        let id = hit.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || seen.iter().any(|value| value == id) {
            false
        } else {
            seen.push(id.to_string());
            true
        }
    });
    results.truncate(limit.max(0) as usize);
    Ok(json!({"name": name, "mode": mode, "results": results}))
}

fn arity_re() -> Result<Regex, String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    if let Some(re) = RE.get() {
        return Ok(re.clone());
    }
    let re = Regex::new(r"\b([A-Za-z_][\w@]*)/(\d{1,3})\b")
        .map_err(|e| format!("EngineError: invalid arity pattern: {e}"))?;
    let _ = RE.set(re.clone());
    Ok(re)
}

fn module_fn_re() -> Result<Regex, String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    if let Some(re) = RE.get() {
        return Ok(re.clone());
    }
    let re = Regex::new(
        r"(^|[\s,()\[\]]) (?!(?:kind|lang|language|path|name):) ([A-Za-z_][\w@]*):([A-Za-z_][\w@]*)(?=$|[\s,()\]])"
            .replace(" ", "").as_str(),
    )
    .map_err(|e| format!("EngineError: invalid module/function pattern: {e}"))?;
    let _ = RE.set(re.clone());
    Ok(re)
}

/// Port of `normalize_query_spelling` (`py_agent/src/coderadar/ops.py:567`).
pub fn normalize_query_spelling(query: String) -> Result<String, String> {
    let arity = arity_re()?;
    let stripped = arity.replace_all(&query, "$1");
    let module_fn = module_fn_re()?;
    let dotted = module_fn.replace_all(&stripped, "$1$2.$3");
    Ok(dotted.into_owned())
}

/// Port of `parse_names` (`py_agent/src/coderadar/ops.py:584`).
pub fn parse_names(query: String, symbols: Option<Vec<String>>) -> Result<Vec<String>, String> {
    if let Some(names) = symbols.filter(|n| !n.is_empty()) {
        return Ok(names
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect());
    }
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let normalized = normalize_query_spelling(query)?;
    let re = Regex::new(r"[,;\s]+").map_err(|e| format!("EngineError: split pattern: {e}"))?;
    let parts: Vec<String> = re
        .split(&normalized)
        .map(|part| {
            part.map(|p| p.trim().to_string())
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parts
        .into_iter()
        .filter(|part| !part.is_empty() && part.chars().count() > 1)
        .collect())
}

/// Port of `resolve_names` (`py_agent/src/coderadar/ops.py:600`).
pub fn resolve_names(names: Vec<String>) -> Result<Vec<Value>, String> {
    let mut results: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if let Some(entity) = find_entity_inner(&name)? {
            let id = entity.get("id").and_then(Value::as_str).unwrap_or_default();
            if seen.insert(id.to_string()) {
                results.push(entity);
            }
            continue;
        }
        for hit in backend_text_search(&name, 3, None)? {
            let id = hit.get("id").and_then(Value::as_str).unwrap_or_default();
            if seen.insert(id.to_string()) {
                results.push(hit);
            }
        }
    }
    Ok(results)
}

/// Port of `explore` (`py_agent/src/coderadar/ops.py:742`).
pub fn explore(
    query: String,
    symbols: Option<Vec<String>>,
    direction: String,
    max_files: i64,
) -> Result<Value, String> {
    require_index()?;
    let names = parse_names(query, symbols)?;
    if names.is_empty() {
        return Err(
            "InvalidRequest: Please provide symbol names or a question to explore.".to_string(),
        );
    }
    let resolved = resolve_names(names.clone())?;
    if resolved.is_empty() {
        return Err(format!("NotFound: Entity `{}` not found.", names.join(" ")));
    }
    let snap = graph()?;
    let mut by_file: Vec<(String, Vec<Value>)> = Vec::new();
    for entity in &resolved {
        let file = entity
            .get("file_path")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let mut enriched = entity.clone();
        let source = source_lines(entity);
        if let Value::Object(ref mut map) = enriched {
            map.insert(
                "source".to_string(),
                source.map(Value::String).unwrap_or(Value::Null),
            );
        }
        if let Some((_, entities)) = by_file.iter_mut().find(|(path, _)| *path == file) {
            entities.push(enriched);
        } else {
            by_file.push((file, vec![enriched]));
        }
    }
    let mut files = Vec::new();
    for (file, entities) in by_file.iter().take(max_files.clamp(0, 100) as usize) {
        files.push(json!({"file_path": file, "entities": entities}));
    }
    let mut relationships = Vec::new();
    for entity in &resolved {
        let eid = entity.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = entity.get("name").and_then(Value::as_str).unwrap_or(eid);
        if direction == "upstream" || direction == "both" {
            for other in backend_callers(eid)?.into_iter().take(5) {
                relationships.push(json!({
                    "kind": "caller", "entity_id": eid, "entity_name": name,
                    "other_id": other.get("id").cloned().unwrap_or(Value::Null),
                    "other_name": other.get("name").cloned().unwrap_or(Value::Null),
                }));
            }
        }
        if direction == "downstream" || direction == "both" {
            for other in backend_callees(eid)?.into_iter().take(5) {
                relationships.push(json!({
                    "kind": "callee", "entity_id": eid, "entity_name": name,
                    "other_id": other.get("id").cloned().unwrap_or(Value::Null),
                    "other_name": other.get("name").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }
    let _ = snap;
    Ok(json!({
        "names": names,
        "files": files,
        "relationships": relationships,
        "stale_files": [],
    }))
}

/// Port of `traverse` (`py_agent/src/coderadar/ops.py:805`).
pub fn traverse(
    entity_id: String,
    direction: String,
    edge_kinds: Option<Vec<String>>,
    max_depth: i64,
) -> Result<Value, String> {
    require_index()?;
    if entity_id.trim().is_empty() {
        return Err("InvalidRequest: Please provide an entity ID to traverse from.".to_string());
    }
    let direction = match direction.as_str() {
        "out" => "downstream",
        "in" => "upstream",
        other => other,
    };
    if !["downstream", "upstream", "both"].contains(&direction) {
        return Err(format!(
            "InvalidRequest: Unknown direction `{direction}` (expected: downstream | upstream | both)."
        ));
    }
    let entity = entity_or_raise(&entity_id)?;
    let canonical = canonical_entity_id(entity_id)?;
    let snap = graph()?;
    let kinds = edge_kinds.unwrap_or_default();
    let core_direction = match direction {
        "upstream" => "in",
        "downstream" => "out",
        _ => "both",
    };
    let results = core_indexer::api::traverse(
        &snap,
        &canonical,
        core_direction,
        &kinds,
        max_depth.clamp(0, 10) as usize,
    );
    Ok(json!({
        "entity": entity,
        "entity_id": canonical,
        "direction": direction,
        "max_depth": max_depth.min(10),
        "edge_kinds": kinds,
        "results": results,
        "unresolved": 0,
    }))
}

/// Port of `query` (`py_agent/src/coderadar/ops.py:859`).
pub fn query(query: String) -> Result<Vec<Value>, String> {
    if query.trim().is_empty() {
        return Err("InvalidRequest: Please provide a query.".to_string());
    }
    require_index()?;
    let snap = graph()?;
    core_indexer::api::query(&snap, &query).map_err(|e| format!("InvalidRequest: {e}"))
}

/// Port of `canonical_file_path` (`py_agent/src/coderadar/ops.py:3615`).
pub fn canonical_file_path(file_path: String) -> Result<String, String> {
    let path = Path::new(&file_path);
    if path.is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            let cwd = core_indexer::fs::strip_verbatim_prefix(cwd);
            if let Some(rel) = pathdiff::diff_paths(path, &cwd) {
                // Python's os.path.relpath raises ValueError across volumes;
                // pathdiff instead hands back the absolute path. Preserve the
                // original spelling in that case, like the Python original.
                if volume(&cwd) != volume(path) {
                    return Ok(file_path);
                }
                return Ok(format!(".{MAIN_SEPARATOR}{}", rel.to_string_lossy()));
            }
        }
        return Ok(file_path);
    }
    if file_path.starts_with("./") || file_path.starts_with(".\\") {
        return Ok(file_path);
    }
    Ok(format!(".{MAIN_SEPARATOR}{file_path}"))
}

// ── Unit 6: engine-backed wrappers, analyses and edits ───────────────────────

const NO_GRAPH_ENGINE: &str = "EngineError: No graph loaded — run coderadar init first";

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_stamp(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != 27 {
        return false;
    }
    let digits = [
        0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22, 23, 24, 25,
    ];
    if !digits.iter().all(|&i| b[i].is_ascii_digit()) {
        return false;
    }
    [4, 7, 13, 16, 19, 26]
        .iter()
        .all(|&i| b[i] == b'-' || b[i] == b'T' || b[i] == b':' || b[i] == b'.' || b[i] == b'Z')
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && b[26] == b'Z'
}

/// Port of `blob_get` (`py_agent/src/coderadar/ops.py:618`).
pub fn blob_get(digest: String) -> Result<Option<Value>, String> {
    if !valid_digest(&digest) {
        return Err(format!(
            "InvalidRequest: malformed blob digest {:?}: expected 64 lowercase hex characters",
            digest
        ));
    }
    if !backend_loaded() {
        return Err(NO_GRAPH_ENGINE.to_string());
    }
    let store = crate::state::store()?;
    core_indexer::api::blob_get(&store, &digest)
}

/// `[retention] archive_after_days` → canonical cutoff stamp.
fn retention_cutoff() -> Result<String, String> {
    let days = core_indexer::api::active_config_json()
        .get("retention")
        .and_then(|section| section.get("archive_after_days"))
        .and_then(Value::as_i64);
    core_indexer::api::retention_cutoff(days)
}

/// Port of `archive` (`py_agent/src/coderadar/ops.py:642`).
pub fn archive(cutoff: Option<String>) -> Result<Value, String> {
    let cutoff = match cutoff {
        Some(value) => value,
        None => retention_cutoff()?,
    };
    if !valid_stamp(&cutoff) {
        return Err(format!(
            "InvalidRequest: invalid timestamp {:?}: expected Macrame canonical UTC (YYYY-MM-DDTHH:MM:SS.ffffffZ); InvalidTimestamp {{ value: {:?}, reason: {:?} }}",
            cutoff, cutoff, "expected YYYY-MM-DDTHH:MM:SS.ffffffZ"
        ));
    }
    if !backend_loaded() {
        return Err(NO_GRAPH_ENGINE.to_string());
    }
    let store = crate::state::store()?;
    core_indexer::api::archive(&store, &cutoff)
}

/// Line-numbered source for one entity, read from the indexed root.
fn source_lines(entity: &Value) -> Option<String> {
    let file = entity.get("file_path")?.as_str()?;
    let root = crate::state::root()?;
    let path = root.join(file);
    let text = std::fs::read_to_string(path).ok()?;
    let start = entity
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1) as usize;
    let end = entity
        .get("end_line")
        .and_then(Value::as_u64)
        .unwrap_or(start as u64) as usize;
    let lines: Vec<&str> = text.lines().collect();
    if start == 0 || start > lines.len() {
        return None;
    }
    let from = start - 1;
    let to = end.min(lines.len());
    let mut out = String::new();
    for (offset, line) in lines[from..to].iter().enumerate() {
        out.push_str(&format!("{}\t{}\n", from + offset + 1, line));
    }
    Some(out)
}

/// Port of `read_source` (`py_agent/src/coderadar/ops.py:693`).
pub fn read_source(entity: Value) -> Result<Option<String>, String> {
    if !entity.is_object() {
        return Err(format!(
            "AttributeError: '{}' object has no attribute 'get'",
            py_type(&entity)
        ));
    }
    Ok(source_lines(&entity))
}

/// Port of `stale_files` (`py_agent/src/coderadar/ops.py:717`).
pub fn stale_files(file_paths: Vec<String>) -> Result<Vec<Value>, String> {
    let Some(snap) = crate::state::projection() else {
        return Ok(Vec::new());
    };
    let stats = core_indexer::api::stats(&snap);
    let Some(indexed_at) = stats.get("indexed_at").and_then(Value::as_f64) else {
        return Ok(Vec::new());
    };
    if indexed_at == 0.0 {
        return Ok(Vec::new());
    }
    let mut stale = Vec::new();
    for path in file_paths {
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(seconds) = modified.duration_since(std::time::UNIX_EPOCH) {
                    let mtime = seconds.as_secs_f64();
                    if mtime > indexed_at {
                        stale.push(json!({"path": path, "mtime": mtime}));
                    }
                }
            }
        }
    }
    Ok(stale)
}

/// Port of `dead_code` (`py_agent/src/coderadar/ops.py:3272`).
pub fn dead_code(
    min_confidence: f64,
    include_test_reachable: bool,
    max_findings: i64,
) -> Result<Vec<Value>, String> {
    require_index()?;
    let max = usize::try_from(max_findings)
        .map_err(|_| "InvalidRequest: max_findings must be non-negative".to_string())?;
    let snap = graph()?;
    core_indexer::api::dead_code(&snap, min_confidence, include_test_reachable, max)
}

/// Port of `get_smells` (`py_agent/src/coderadar/ops.py:3287`).
pub fn get_smells(
    entity_id: Option<String>,
    rule_id: Option<String>,
    strictness: String,
) -> Result<Vec<Value>, String> {
    require_index()?;
    let snap = graph()?;
    core_indexer::api::smells(&snap, entity_id.as_deref(), rule_id.as_deref(), &strictness)
}

/// Port of `find_clones` (`py_agent/src/coderadar/ops.py:3300`).
pub fn find_clones(
    min_lines: i64,
    min_similarity: f64,
    max_groups: i64,
) -> Result<Vec<Value>, String> {
    require_index()?;
    let min_lines = usize::try_from(min_lines)
        .map_err(|_| "InvalidRequest: min_lines must be non-negative".to_string())?;
    let max_groups = usize::try_from(max_groups)
        .map_err(|_| "InvalidRequest: max_groups must be non-negative".to_string())?;
    let snap = graph()?;
    core_indexer::api::clones(&snap, min_lines, min_similarity, max_groups)
}

/// Port of `diagnose` (`py_agent/src/coderadar/ops.py:3313`).
// INFALLIBLE: the native analysis symbols are statically linked into the Rust
// binary, so the original's `NoExtension` import failure cannot occur here.
pub fn diagnose(unresolved: bool, low_confidence: bool) -> Result<Value, String> {
    let snap = match graph() {
        Ok(snap) => snap,
        Err(_) => {
            return Ok(json!({
                "unresolved": [],
                "ambiguous_bases": [],
                "ambiguous_base_count": 0
            }));
        }
    };
    let mut rows = Vec::new();
    if unresolved {
        let functions = core_indexer::api::search(&snap, "", 1000, Some("function"))?;
        for function in functions {
            let id = function
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let targets = core_indexer::api::unresolved_targets(id);
            if !targets.is_empty() {
                rows.push(json!({
                    "id": id,
                    "targets": targets.join(", "),
                }));
            }
        }
        rows.sort_by(|a, b| {
            let len = |v: &Value| v.get("targets").and_then(Value::as_str).map_or(0, str::len);
            len(b).cmp(&len(a))
        });
    }
    let mut bases = Vec::new();
    let mut base_count = 0usize;
    if low_confidence {
        bases = core_indexer::api::ambiguous_base_details();
        base_count = bases.len();
    }
    Ok(json!({
        "unresolved": rows,
        "ambiguous_bases": bases,
        "ambiguous_base_count": base_count,
    }))
}

/// Port of `find_scaffolding` (`py_agent/src/coderadar/ops.py:3475`).
pub fn find_scaffolding(include_secrets: bool, max_findings: i64) -> Result<Vec<Value>, String> {
    require_index()?;
    let max = usize::try_from(max_findings)
        .map_err(|_| "InvalidRequest: max_findings must be non-negative".to_string())?;
    let snap = graph()?;
    core_indexer::api::scaffolding(&snap, include_secrets, max)
}

/// Port of `replace_body` (`py_agent/src/coderadar/ops.py:3506`).
pub fn replace_body(
    entity_id: String,
    new_body: String,
    expected_hash: Option<String>,
    dry_run: bool,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    require_index()?;
    let _ = graph_arg;
    let snap = graph()?;
    let id = core_indexer::api::canonical_entity_id(&entity_id);
    let (plan_id, plan) =
        core_indexer::api::plan_replace_body(&id, &new_body, expected_hash, dry_run, &snap)?;
    let result = if dry_run {
        Value::Null
    } else {
        core_indexer::api::apply_plan(&plan_id)?
    };
    Ok(json!({"plan": plan, "result": result, "note": ""}))
}

/// Port of `update_signature` (`py_agent/src/coderadar/ops.py:3520`).
pub fn update_signature(
    entity_id: String,
    new_signature: String,
    inject_defaults: bool,
    dry_run: bool,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    require_index()?;
    let _ = graph_arg;
    let snap = graph()?;
    let id = core_indexer::api::canonical_entity_id(&entity_id);
    let (plan_id, plan) = core_indexer::api::plan_update_signature(
        &id,
        &new_signature,
        inject_defaults,
        dry_run,
        &snap,
    )?;
    let result = if dry_run {
        Value::Null
    } else {
        core_indexer::api::apply_plan(&plan_id)?
    };
    Ok(json!({"plan": plan, "result": result, "note": ""}))
}

/// Port of `rename` (`py_agent/src/coderadar/ops.py:3533`).
pub fn rename(
    entity_id: String,
    new_name: String,
    dry_run: bool,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    require_index()?;
    let _ = graph_arg;
    let snap = graph()?;
    let id = core_indexer::api::canonical_entity_id(&entity_id);
    let (plan_id, plan) = core_indexer::api::plan_rename(&id, &new_name, dry_run, &snap)?;
    let result = if dry_run {
        Value::Null
    } else {
        core_indexer::api::apply_plan(&plan_id)?
    };
    Ok(json!({"plan": plan, "result": result, "note": ""}))
}

fn indent(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                line.to_string()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Port of `render_entity_code` (`py_agent/src/coderadar/ops.py:3541`).
pub fn render_entity_code(
    language: String,
    kind: String,
    name: String,
    body: String,
    decorators: Option<Vec<String>>,
    signature: String,
) -> Result<String, String> {
    let lang = language.to_lowercase();
    let kind_norm = if kind.is_empty() {
        "function".to_string()
    } else {
        kind.to_lowercase()
    };
    let body = body.trim_end_matches('\n');
    let dec = decorators.unwrap_or_default().join("\n");
    let dec_block = if dec.is_empty() {
        String::new()
    } else {
        format!("{dec}\n")
    };
    let sig = signature.trim();

    if ["function", "method", "fn"].contains(&kind_norm.as_str()) {
        if !sig.is_empty() {
            if ["python", "py"].contains(&lang.as_str()) {
                let header = if sig.ends_with(':') {
                    sig.to_string()
                } else {
                    format!("{sig}:")
                };
                let inner = {
                    let rendered = indent(body, 4);
                    if rendered.is_empty() {
                        "    pass".to_string()
                    } else {
                        rendered
                    }
                };
                return Ok(format!("{dec_block}{header}\n{inner}\n"));
            }
            if ["ruby", "rb"].contains(&lang.as_str()) {
                return Ok(format!("{dec_block}{sig}\n{body}\nend\n"));
            }
            return Ok(format!("{dec_block}{sig} {{\n{body}\n}}\n"));
        }
        if ["python", "py"].contains(&lang.as_str()) {
            return Ok(format!("{dec_block}def {name}():\n{}\n", indent(body, 4)));
        }
        if ["rust", "rs"].contains(&lang.as_str()) {
            return Ok(format!("{dec_block}pub fn {name}() {{\n{body}\n}}\n"));
        }
        if lang == "go" {
            return Ok(format!("{dec_block}func {name}() {{\n{body}\n}}\n"));
        }
        if ["javascript", "typescript", "js", "ts", "jsx", "tsx"].contains(&lang.as_str()) {
            return Ok(format!("{dec_block}function {name}() {{\n{body}\n}}\n"));
        }
        if lang == "java" || ["csharp", "cs"].contains(&lang.as_str()) {
            return Ok(format!("{dec_block}public void {name}() {{\n{body}\n}}\n"));
        }
        if lang == "php" {
            return Ok(format!("{dec_block}function {name}() {{\n{body}\n}}\n"));
        }
        if ["ruby", "rb"].contains(&lang.as_str()) {
            return Ok(format!("{dec_block}def {name}\n{body}\nend\n"));
        }
        return Ok(format!("{dec_block}{name}() {{\n{body}\n}}\n"));
    }

    if ["class", "struct"].contains(&kind_norm.as_str()) {
        if ["python", "py"].contains(&lang.as_str()) {
            let inner = {
                let rendered = indent(body, 4);
                if rendered.is_empty() {
                    "    pass".to_string()
                } else {
                    rendered
                }
            };
            return Ok(format!("{dec_block}class {name}:\n{inner}\n"));
        }
        if ["ruby", "rb"].contains(&lang.as_str()) {
            return Ok(format!("{dec_block}class {name}\n{body}\nend\n"));
        }
        return Ok(format!("{dec_block}class {name} {{\n{body}\n}}\n"));
    }

    if ["constant", "variable", "const", "var"].contains(&kind_norm.as_str()) {
        if ["python", "py"].contains(&lang.as_str()) {
            return Ok(format!(
                "{dec_block}{name} = {}\n",
                if body.is_empty() { "None" } else { body }
            ));
        }
        if lang == "go" {
            return Ok(format!(
                "{dec_block}const {name} = {}\n",
                if body.is_empty() { "nil" } else { body }
            ));
        }
        if ["javascript", "typescript", "js", "ts"].contains(&lang.as_str()) {
            return Ok(format!(
                "{dec_block}const {name} = {};\n",
                if body.is_empty() { "null" } else { body }
            ));
        }
        return Ok(format!(
            "{dec_block}{name} = {}\n",
            if body.is_empty() { "null" } else { body }
        ));
    }

    if body.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{body}\n"))
    }
}

/// Port of `create_entity` (`py_agent/src/coderadar/ops.py:3632`).
pub fn create_entity(
    file_path: String,
    language: String,
    kind: String,
    name: String,
    body: String,
    decorators: Option<Vec<String>>,
    anchor: String,
    signature: Option<String>,
    dry_run: bool,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    require_index()?;
    let _ = graph_arg;
    let code = render_entity_code(
        language,
        kind.clone(),
        name,
        body,
        decorators,
        signature.clone().unwrap_or_default(),
    )?;
    if code.trim().is_empty() {
        return Err(
            "InvalidRequest: Cannot render entity: provide a non-empty body or kind.".to_string(),
        );
    }
    let note = if signature.is_some()
        && !["function", "method", "fn"].contains(&kind.to_lowercase().as_str())
    {
        format!(
            "`signature` is only used for function-like kinds; it was ignored for kind '{kind}'."
        )
    } else {
        String::new()
    };
    let snap = graph()?;
    let anchor_norm = if anchor.is_empty() {
        "end".to_string()
    } else if anchor == "top" || anchor == "end" {
        anchor
    } else {
        core_indexer::api::canonical_entity_id(&anchor)
    };
    let target = canonical_file_path(file_path)?;
    let (plan_id, plan) =
        core_indexer::api::plan_create_entity(&target, &anchor_norm, &code, dry_run, &snap)?;
    let result = if dry_run {
        Value::Null
    } else {
        core_indexer::api::apply_plan(&plan_id)?
    };
    Ok(json!({"plan": plan, "result": result, "note": note}))
}

/// Port of `update_file` (`py_agent/src/coderadar/ops.py:3724`).
pub fn update_file(
    file_path: String,
    content: Option<String>,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    if file_path.trim().is_empty() {
        return Err("InvalidRequest: Please provide a file path.".to_string());
    }
    require_index()?;
    let _ = graph_arg;
    core_indexer::api::update_file(&file_path, content.as_deref())
}

fn round_even(value: f64) -> f64 {
    let floor = value.floor();
    let frac = value - floor;
    if frac > 0.5 {
        floor + 1.0
    } else if frac < 0.5 || (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    }
}

fn round2_even(value: f64) -> f64 {
    round_even(value * 100.0) / 100.0
}

fn sample_generations(stamps: &[String], n: i64) -> Vec<String> {
    if n <= 0 || stamps.len() as i64 <= n {
        return stamps.to_vec();
    }
    if n == 1 {
        return vec![stamps[stamps.len() - 1].clone()];
    }
    let step = (stamps.len() - 1) as f64 / (n - 1) as f64;
    let mut picked = std::collections::BTreeSet::new();
    picked.insert(0usize);
    picked.insert(stamps.len() - 1);
    for i in 0..n {
        picked.insert(round_even(i as f64 * step) as usize);
    }
    picked
        .into_iter()
        .filter_map(|i| stamps.get(i).cloned())
        .collect()
}

fn fold_store_path() -> (String, String) {
    let root = crate::state::root()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let db = crate::state::store_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".coderadar/store/coderadar.db".to_string());
    let store = if Path::new(&db).is_absolute() {
        db
    } else {
        Path::new(&root).join(db).to_string_lossy().to_string()
    };
    (root, store)
}

fn get_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn get_bool(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn get_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn span_of(entry: &Value) -> Option<Value> {
    match (get_u64(entry, "start_line"), get_u64(entry, "end_line")) {
        (Some(start), Some(end)) => Some(json!([start, end])),
        _ => None,
    }
}

fn hex_of(value: Option<u64>) -> Value {
    value
        .map(|v| json!(format!("{v:x}")))
        .unwrap_or(Value::Null)
}

fn side_entry(raws: &[String], hashes: &Value) -> Option<Value> {
    let found: Vec<(String, Value)> = raws
        .iter()
        .filter_map(|raw| {
            hashes
                .get(raw)
                .filter(|v| !v.is_null())
                .cloned()
                .map(|entry| (raw.clone(), entry))
        })
        .collect();
    if found.is_empty() {
        return None;
    }
    for (raw, entry) in &found {
        if get_str(entry, "id").as_deref() == Some(raw.as_str()) {
            return Some(entry.clone());
        }
    }
    Some(found[0].1.clone())
}

// ── Unit 7: temporal reads (gated surface) ────────────────────────────────

/// Port of `branches` (`py_agent/src/coderadar/ops.py:1199`).
pub fn branches(graph_arg: Option<Value>) -> Result<Vec<Value>, String> {
    require_index()?;
    let _ = graph_arg;
    let store = crate::state::store()?;
    core_indexer::api::branches(&store)
}

/// Port of `as_of` (`py_agent/src/coderadar/ops.py:1219`).
#[expect(
    clippy::too_many_arguments,
    reason = "the Python API has these keyword parameters"
)]
pub fn as_of(
    timestamp: String,
    query: String,
    symbols: Option<Vec<String>>,
    branch: String,
    strict: bool,
    at: Option<String>,
    valid: bool,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    if timestamp.is_empty() && at.is_none() {
        return Err(
            "InvalidRequest: Please provide an ISO 8601 timestamp (e.g. '2025-01-15T10:00:00Z')."
                .to_string(),
        );
    }
    require_index()?;
    let names = parse_names(query, symbols)?;
    if names.is_empty() {
        let stamp = if at.is_some() {
            String::new()
        } else {
            core_indexer::api::normalize_timestamp(&timestamp)?
        };
        return Ok(json!({
            "timestamp": stamp,
            "fold_ts": Value::Null,
            "names": [],
            "entities": {},
            "predates_recorded_history": false,
            "basis": Value::Null,
            "horizon": Value::Null,
            "commit": Value::Null,
            "dirty_at_commit": [],
            "address": false,
            "parent_drift": 0,
        }));
    }
    let _ = graph_arg;
    let store = crate::state::store()?;
    let batch = core_indexer::api::lookup_at(
        &store,
        &names,
        &timestamp,
        &branch,
        strict,
        at.as_deref(),
        valid,
    )?;
    let stamp = if at.is_some() {
        String::new()
    } else {
        core_indexer::api::normalize_timestamp(&timestamp)?
    };
    let fold_ts = batch
        .get("fold_ts")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| stamp.clone());
    let mut entities = Map::new();
    if let Some(found) = batch.get("entities").and_then(Value::as_object) {
        for name in &names {
            entities.insert(
                name.clone(),
                found.get(name).cloned().unwrap_or(Value::Null),
            );
        }
    }
    Ok(json!({
        "timestamp": stamp,
        "fold_ts": fold_ts,
        "names": names,
        "entities": entities,
        "predates_recorded_history": get_bool(&batch, "predates_recorded_history"),
        "basis": batch.get("basis").cloned().unwrap_or(json!("recorded")),
        "horizon": batch.get("horizon").cloned().unwrap_or(Value::Null),
        "commit": batch.get("sha").cloned().unwrap_or(Value::Null),
        "dirty_at_commit": batch.get("dirty_at_commit").cloned().unwrap_or(json!([])),
        "address": get_bool(&batch, "address"),
        "parent_drift": batch.get("parent_drift").and_then(Value::as_u64).unwrap_or(0),
    }))
}

/// Port of `verify_history` (`py_agent/src/coderadar/ops.py:1343`).
#[expect(
    clippy::too_many_arguments,
    reason = "the Python API has these keyword parameters"
)]
pub fn verify_history(
    from_ts: Option<String>,
    to_ts: Option<String>,
    generations: i64,
    all: bool,
    branch: String,
    offenders: i64,
    max_generations: Option<i64>,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    require_index()?;
    let _ = graph_arg;
    if generations < 1 {
        return Err(format!(
            "InvalidRequest: generations must be a positive integer, got {generations}."
        ));
    }
    if offenders < 0 {
        return Err(format!(
            "InvalidRequest: offenders must be a non-negative integer, got {offenders}."
        ));
    }
    let store = crate::state::store()?;
    let lo = from_ts
        .as_deref()
        .map(core_indexer::api::normalize_timestamp)
        .transpose()?;
    let hi = to_ts
        .as_deref()
        .map(core_indexer::api::normalize_timestamp)
        .transpose()?;
    if let (Some(lo), Some(hi)) = (&lo, &hi) {
        if lo > hi {
            return Err(format!(
                "InvalidRequest: empty window: from {:?} sorts after to {:?} once normalized.",
                from_ts, to_ts
            ));
        }
    }
    let stamps =
        core_indexer::api::recorded_generations(&store, lo.as_deref(), hi.as_deref(), &branch)?;
    let ever = core_indexer::api::visible_module_concept_ids(&store, &branch)?;
    if stamps.is_empty() {
        let suffix = if branch != "main" {
            format!(" for branch '{branch}'")
        } else {
            String::new()
        };
        return Err(format!(
            "InvalidRequest: no recorded generations in that window{suffix}; the ledger has nothing to verify there."
        ));
    }
    let picked = if all {
        stamps.clone()
    } else {
        sample_generations(&stamps, generations)
    };
    if let Some(max) = max_generations {
        if picked.len() as i64 > max {
            return Err(format!(
                "InvalidRequest: {} generations exceed max_generations={max}; sample with a smaller generations count or a narrower window.",
                picked.len()
            ));
        }
    }
    let mut verdicts: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut gens = Vec::new();
    let mut offender_all: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for ts in &picked {
        let batch =
            core_indexer::api::source_refs_at(&store, &ever, ts, &branch, false, None, false)?;
        let refs = batch
            .get("refs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let described: Vec<(String, Option<String>)> = refs
            .iter()
            .map(|(id, value)| {
                (
                    id.clone(),
                    get_str(value, "digest").filter(|d| !d.is_empty()),
                )
            })
            .collect();
        let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        let mut full: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (id, digest) in described {
            let outcome = match digest {
                None => "no_digest".to_string(),
                Some(digest) => match verdicts.get(&digest) {
                    Some(v) => v.clone(),
                    None => {
                        let verdict = match core_indexer::api::blob_get(&store, &digest) {
                            Err(_) => "digest_malformed".to_string(),
                            Ok(None) => "unresolvable".to_string(),
                            Ok(Some(Value::Object(map))) => match map.get("$bytes") {
                                Some(Value::Array(bytes)) => {
                                    let raw: Vec<u8> = bytes
                                        .iter()
                                        .filter_map(Value::as_u64)
                                        .map(|b| b as u8)
                                        .collect();
                                    let mut hasher = Sha256::new();
                                    hasher.update(&raw);
                                    let hex = format!("{:x}", hasher.finalize());
                                    if hex == digest {
                                        "ok".to_string()
                                    } else {
                                        "digest_mismatch".to_string()
                                    }
                                }
                                _ => "unresolvable".to_string(),
                            },
                            Ok(_) => "unresolvable".to_string(),
                        };
                        verdicts.insert(digest, verdict.clone());
                        verdict
                    }
                },
            };
            *counts.entry(outcome.clone()).or_insert(0) += 1;
            if outcome != "ok" {
                full.entry(outcome.clone()).or_default().push(id.clone());
                offender_all.entry(outcome).or_default().insert(id);
            }
        }
        let cap = offenders.max(0) as usize;
        let offenders_map: std::collections::BTreeMap<String, Value> = full
            .into_iter()
            .map(|(k, v)| (k, json!(v.into_iter().take(cap).collect::<Vec<_>>())))
            .collect();
        gens.push(json!({
            "timestamp": ts,
            "modules": counts.values().sum::<u64>(),
            "counts": counts,
            "offenders": offenders_map,
        }));
    }
    let mut totals: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for one in &gens {
        if let Some(counts) = one.get("counts").and_then(Value::as_object) {
            for (key, value) in counts {
                *totals.entry(key.clone()).or_insert(0) += value.as_u64().unwrap_or(0);
            }
        }
    }
    let covered = totals.get("ok").copied().unwrap_or(0);
    let total_files: u64 = totals.values().sum();
    let (root, store_path) = fold_store_path();
    let cap = offenders.max(0) as usize;
    let offender_sample: std::collections::BTreeMap<String, Value> = offender_all
        .iter()
        .map(|(k, v)| {
            let rows: Vec<String> = v.iter().take(cap).cloned().collect();
            (k.clone(), json!(rows))
        })
        .collect();
    let offender_distinct: std::collections::BTreeMap<String, usize> = offender_all
        .iter()
        .map(|(k, v)| (k.clone(), v.len()))
        .collect();
    Ok(json!({
        "store": store_path,
        "root": root,
        "branch": branch,
        "window": [stamps[0], stamps[stamps.len() - 1]],
        "total_generations": stamps.len(),
        "sampled": picked.len(),
        "offenders_per_outcome": offenders,
        "generations": gens,
        "totals": totals,
        "byte_coverage_pct": if total_files == 0 { 0.0 } else { round2_even(covered as f64 / total_files as f64 * 100.0) },
        "distinct_digests": verdicts.len(),
        "offender_sample": offender_sample,
        "offender_distinct": offender_distinct,
    }))
}

/// Port of `history_of` (`py_agent/src/coderadar/ops.py:1496`).
pub fn history_of(file: String, graph_arg: Option<Value>) -> Result<Value, String> {
    if file.trim().is_empty() {
        return Err("InvalidRequest: Please provide a file path (e.g. 'pkg/mod.py').".to_string());
    }
    let Some(_snap) = crate::state::projection() else {
        return Err(NO_INDEX.to_string());
    };
    let store = crate::state::store()?;
    let _ = graph_arg;
    core_indexer::api::history_of(&store, &file)
}

/// Port of `changes_between` (`py_agent/src/coderadar/ops.py:3040`).
pub fn changes_between(
    t1: String,
    t2: String,
    scope: Option<String>,
    branch: String,
    valid: bool,
    graph_arg: Option<Value>,
) -> Result<Value, String> {
    require_index()?;
    let _ = graph_arg;
    let store = crate::state::store()?;
    let start = core_indexer::api::normalize_timestamp(&t1)?;
    let end = core_indexer::api::normalize_timestamp(&t2)?;
    if start > end {
        return Err(format!(
            "InvalidRequest: empty window: t1 {t1:?} sorts after t2 {t2:?} once normalized."
        ));
    }
    let ever = core_indexer::api::visible_all_concept_ids(&store, &branch)?;
    let mut groups: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for raw in &ever {
        groups
            .entry(core_indexer::api::canonical_id(raw))
            .or_default()
            .push(raw.clone());
    }
    let normalized_groups = groups
        .values()
        .filter(|raws| {
            let set: std::collections::BTreeSet<&String> = raws.iter().collect();
            set.len() > 1
        })
        .count();
    if let Some(scope) = &scope {
        if scope.contains("::") {
            let cscope = core_indexer::api::canonical_id(scope);
            groups.retain(|cid, _| cid == &cscope || cid.starts_with(&cscope));
        } else {
            let head = scope.replace('\\', "/");
            groups.retain(|cid, _| cid.split("::").next().unwrap_or_default() == head);
        }
    }
    let raws: Vec<String> = groups.values().flatten().cloned().collect();
    let first = core_indexer::api::content_hashes_at(&store, &raws, &start, &branch, valid)?;
    let second = core_indexer::api::content_hashes_at(&store, &raws, &end, &branch, valid)?;
    let first_hashes = first
        .get("hashes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let second_hashes = second
        .get("hashes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    let mut moved = Vec::new();
    let mut uncompared = Vec::new();
    let mut t1_only: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut t2_only: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut unmodified = 0u64;
    for (cid, raws) in &groups {
        let before = side_entry(raws, &Value::Object(first_hashes.clone()));
        let after = side_entry(raws, &Value::Object(second_hashes.clone()));
        match (before, after) {
            (Some(before), Some(after)) => {
                let hb = get_u64(&before, "content_hash");
                let ha = get_u64(&after, "content_hash");
                let display = get_str(&after, "id").unwrap_or_default();
                if hb.is_none() && ha.is_none() {
                    uncompared.push(json!({"id": display}));
                } else if hb.is_some() && ha == hb {
                    let sb = span_of(&before);
                    let sa = span_of(&after);
                    if sb.is_some() && sa.is_some() && sb != sa {
                        moved.push(json!({
                            "id": display,
                            "content_hash": hex_of(ha),
                            "span_before": sb,
                            "span_after": sa,
                        }));
                    } else {
                        unmodified += 1;
                    }
                } else {
                    modified.push(json!({
                        "id": display,
                        "hash_before": hex_of(hb),
                        "hash_after": hex_of(ha),
                        "span_before": span_of(&before),
                        "span_after": span_of(&after),
                    }));
                }
            }
            (Some(before), None) => {
                t1_only.insert(cid.clone(), before);
            }
            (None, Some(after)) => {
                t2_only.insert(cid.clone(), after);
            }
            (None, None) => {}
        }
    }
    let mut by_hash_before: std::collections::BTreeMap<u64, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut by_hash_after: std::collections::BTreeMap<u64, Vec<String>> =
        std::collections::BTreeMap::new();
    for (cid, entry) in &t1_only {
        if let Some(hash) = get_u64(entry, "content_hash") {
            by_hash_before.entry(hash).or_default().push(cid.clone());
        }
    }
    for (cid, entry) in &t2_only {
        if let Some(hash) = get_u64(entry, "content_hash") {
            by_hash_after.entry(hash).or_default().push(cid.clone());
        }
    }
    let mut paired: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (digest, cids) in &by_hash_before {
        if let Some(others) = by_hash_after.get(digest) {
            if cids.len() == 1 && others.len() == 1 && cids[0] != others[0] {
                let cid_b = &cids[0];
                let cid_a = &others[0];
                paired.insert(cid_b.clone());
                paired.insert(cid_a.clone());
                let entry_b = t1_only.get(cid_b).cloned().unwrap_or(json!({}));
                let entry_a = t2_only.get(cid_a).cloned().unwrap_or(json!({}));
                moved.push(json!({
                    "id_before": get_str(&entry_b, "id"),
                    "id_after": get_str(&entry_a, "id"),
                    "content_hash": hex_of(Some(*digest)),
                    "span_before": span_of(&entry_b),
                    "span_after": span_of(&entry_a),
                }));
            }
        }
    }
    for (cid, entry) in &t1_only {
        if paired.contains(cid) {
            continue;
        }
        removed.push(json!({
            "id": get_str(entry, "id"),
            "content_hash": hex_of(get_u64(entry, "content_hash")),
            "span": span_of(entry),
        }));
    }
    for (cid, entry) in &t2_only {
        if paired.contains(cid) {
            continue;
        }
        added.push(json!({
            "id": get_str(entry, "id"),
            "content_hash": hex_of(get_u64(entry, "content_hash")),
            "span": span_of(entry),
        }));
    }
    let (root, store_path) = fold_store_path();
    Ok(json!({
        "store": store_path,
        "root": root,
        "branch": branch,
        "t1": start,
        "t2": end,
        "scope": scope,
        "normalized_ids": normalized_groups > 0,
        "normalized_groups": normalized_groups,
        "added": added,
        "removed": removed,
        "modified": modified,
        "moved": moved,
        "uncompared": uncompared,
        "counts": {
            "added": added.len(),
            "removed": removed.len(),
            "modified": modified.len(),
            "moved": moved.len(),
            "uncompared": uncompared.len(),
        },
        "unmodified": unmodified,
        "predates": {
            "t1": get_bool(&first, "predates_recorded_history"),
            "t2": get_bool(&second, "predates_recorded_history"),
        },
    }))
}

/// Port of `ingest_status` (`py_agent/src/coderadar/ops.py:2551`).
pub fn ingest_status(store_path: Option<String>) -> Result<Value, String> {
    let Some(path) =
        store_path.or_else(|| crate::state::store_path().map(|p| p.to_string_lossy().to_string()))
    else {
        return Err(
            "InvalidRequest: ingest_status needs a store_path (or a graph carrying one)."
                .to_string(),
        );
    };
    if !std::path::Path::new(&path).exists() {
        return Err(format!(
            "InvalidRequest: no store at {path} — ingest a branch's history first."
        ));
    }
    let store = core_indexer::storage::CodeGraphStore::open(&path)
        .map_err(|e| format!("InvalidRequest: cannot open {path}: {e:?}"))?;
    core_indexer::api::ingest_status(&store)
}

// ── Unit 8: write-side ingest ────────────────────────────────────────────

fn write_store_path(store_path: Option<String>) -> Result<String, String> {
    store_path
        .or_else(|| crate::state::store_path().map(|p| p.to_string_lossy().to_string()))
        .ok_or_else(|| {
            "InvalidRequest: a store_path is required (no graph carrying one is loaded)."
                .to_string()
        })
}

/// Port of `ingest_history` (`py_agent/src/coderadar/ops.py:2024`).
#[expect(
    clippy::too_many_arguments,
    reason = "the Python API has these ingest parameters"
)]
pub fn ingest_history(
    repo: String,
    branch: String,
    from_sha: Option<String>,
    dry_run: bool,
    lineage: Option<String>,
    parent_ref: String,
    to_sha: Option<String>,
    store_path: Option<String>,
) -> Result<Value, String> {
    if repo.trim().is_empty() {
        return Err("InvalidRequest: Please provide a repository path.".to_string());
    }
    if branch.trim().is_empty() {
        return Err("InvalidRequest: Please provide a git branch (e.g. 'main').".to_string());
    }
    if parent_ref.trim().is_empty() {
        return Err("InvalidRequest: Please provide a parent ref (default 'main').".to_string());
    }
    let store = write_store_path(store_path)?;
    core_indexer::api::ingest_history(
        &store,
        &repo,
        &branch,
        from_sha.as_deref(),
        to_sha.as_deref(),
        dry_run,
        lineage.as_deref(),
        &parent_ref,
    )
}

/// Port of `reingest_merged_commits` (`py_agent/src/coderadar/ops.py:2425`).
pub fn reingest_merged_commits(
    repo: String,
    branch: String,
    dry_run: bool,
    store_path: Option<String>,
) -> Result<Value, String> {
    if repo.trim().is_empty() {
        return Err("InvalidRequest: Please provide a repository path.".to_string());
    }
    let store = write_store_path(store_path)?;
    core_indexer::api::reingest_merged_commits(&store, &repo, &branch, dry_run)
}

/// Port of `backfill_commit_index` (`py_agent/src/coderadar/ops.py:2511`).
pub fn backfill_commit_index(branch: String, store_path: Option<String>) -> Result<Value, String> {
    let store = write_store_path(store_path)?;
    core_indexer::api::backfill_commit_index(&store, &branch)
}

/// Port of `backfill_commit_messages` (`py_agent/src/coderadar/ops.py:2994`).
pub fn backfill_commit_messages(repo: String, store_path: Option<String>) -> Result<Value, String> {
    if repo.trim().is_empty() {
        return Err("InvalidRequest: Please provide a repository path.".to_string());
    }
    let store = write_store_path(store_path)?;
    core_indexer::api::backfill_commit_messages(&store, &repo)
}

// ── Unit 10: shared resolver helpers ──────────────────────────────────────

fn quoted_names(text: &str) -> Result<Vec<String>, String> {
    let re = Regex::new(r#"['"]([^'"]*)['"]"#).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for caps in re.captures_iter(text) {
        let caps = caps.map_err(|e| e.to_string())?;
        if let Some(name) = caps.get(1) {
            out.push(name.as_str().to_string());
        }
    }
    Ok(out)
}

/// Blank out comments and string literals so a docstring example is not
/// mistaken for a real `__all__` assignment. Byte lengths are preserved so
/// offsets into the masked text also index the original source.
fn mask_python(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            let quote = bytes[i];
            let triple = i + 2 < bytes.len() && bytes[i + 1] == quote && bytes[i + 2] == quote;
            let width = if triple { 3 } else { 1 };
            for byte in out.iter_mut().skip(i).take(width) {
                *byte = b' ';
            }
            i += width;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    out[i] = b' ';
                    i += 1;
                    if i < bytes.len() && bytes[i] != b'\n' {
                        out[i] = b' ';
                        i += 1;
                    }
                    continue;
                }
                if triple
                    && i + 2 < bytes.len()
                    && bytes[i] == quote
                    && bytes[i + 1] == quote
                    && bytes[i + 2] == quote
                {
                    for byte in out.iter_mut().skip(i).take(3) {
                        *byte = b' ';
                    }
                    i += 3;
                    break;
                }
                if !triple && bytes[i] == quote {
                    out[i] = b' ';
                    i += 1;
                    break;
                }
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn bracket_end(masked: &str, open: usize) -> usize {
    let bytes = masked.as_bytes();
    let mut depth = 0usize;
    for (offset, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return offset;
                }
            }
            _ => {}
        }
    }
    bytes.len()
}

/// Port of `extract_all_exports` (`py_agent/src/coderadar/resolvers/exports.py:18`).
pub fn extract_all_exports(source: String) -> Result<Option<Vec<String>>, String> {
    let masked = mask_python(&source);
    let list = Regex::new(r"(?s)__all__\s*(?:=|\+=)\s*\[|__all__\.extend\(\s*\[")
        .map_err(|e| e.to_string())?;
    let append = Regex::new(r"__all__\.append\(\s*").map_err(|e| e.to_string())?;
    let mut found: Vec<(usize, String)> = Vec::new();
    for caps in list.captures_iter(&masked) {
        let caps = caps.map_err(|e| e.to_string())?;
        let whole = caps.get(0).ok_or("missing __all__ match")?;
        let open = whole.end() - 1;
        let close = bracket_end(&masked, open);
        for name in quoted_names(&source[open..close])? {
            found.push((whole.start(), name));
        }
    }
    for caps in append.captures_iter(&masked) {
        let caps = caps.map_err(|e| e.to_string())?;
        let whole = caps.get(0).ok_or("missing __all__.append match")?;
        let bytes = source.as_bytes();
        let mut at = whole.end();
        let masked_bytes = masked.as_bytes();
        while at > 0 && masked_bytes[at - 1].is_ascii_whitespace() {
            at -= 1;
        }
        if at >= bytes.len() || (bytes[at] != b'\'' && bytes[at] != b'"') {
            continue;
        }
        let quote = bytes[at];
        let mut end = at + 1;
        while end < bytes.len() {
            if bytes[end] == b'\\' {
                end += 2;
                continue;
            }
            if bytes[end] == quote {
                break;
            }
            end += 1;
        }
        if end < bytes.len() {
            found.push((whole.start(), source[at + 1..end].to_string()));
        }
    }
    if found.is_empty() {
        return Ok(None);
    }
    found.sort_by_key(|(position, _)| *position);
    let mut names = Vec::new();
    for (_, name) in found {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(Some(names))
}

/// Port of `canonical_route_pattern` (`py_agent/src/coderadar/resolvers/resolution.py:81`).
pub fn canonical_route_pattern(path: String) -> Result<Value, String> {
    core_indexer::frameworks::canonical_route_pattern(path)
}

/// Port of `route_patterns_match` (`py_agent/src/coderadar/resolvers/resolution.py:99`).
pub fn route_patterns_match(query: String, candidate: String) -> Result<bool, String> {
    core_indexer::frameworks::route_patterns_match(query, candidate)
}

/// Port of `prefer_in_dir` (`py_agent/src/coderadar/resolvers/resolution.py:160`).
pub fn prefer_in_dir(
    candidates: Vec<Value>,
    name: String,
    dir_hint: String,
    confidence: f64,
) -> Result<Option<Value>, String> {
    for candidate in &candidates {
        let Some(object) = candidate.as_object() else {
            return Err(format!(
                "AttributeError: '{}' object has no attribute 'get'",
                py_type(candidate)
            ));
        };
        let candidate_name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let file = object
            .get("file_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if candidate_name == name && file.contains(&dir_hint) {
            let mut result = object.clone();
            result.insert("confidence".to_string(), json!(confidence));
            return Ok(Some(Value::Object(result)));
        }
    }
    for candidate in &candidates {
        let Some(object) = candidate.as_object() else {
            return Err(format!(
                "AttributeError: '{}' object has no attribute 'get'",
                py_type(candidate)
            ));
        };
        let candidate_name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if candidate_name == name {
            let mut result = object.clone();
            result.insert("confidence".to_string(), json!(confidence - 0.15));
            return Ok(Some(Value::Object(result)));
        }
    }
    Ok(None)
}

// ── Unit 11: exact render surfaces ───────────────────────────────────────
pub fn annotation_stored(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::annotation_stored(result)
}

pub fn annotation_versions(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::annotation_versions(result)
}

pub fn apply_output_budget(
    lines: Vec<String>,
    max_chars: Option<i64>,
    max_per_file: Option<i64>,
) -> Result<String, String> {
    crate::render_remaining::apply_output_budget(lines, max_chars, max_per_file)
}

pub fn compute_embeddings(
    metrics: serde_json::Value,
    search_similar: String,
) -> Result<String, String> {
    crate::render_remaining::compute_embeddings(metrics, search_similar)
}

pub fn render_diagnose(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::diagnose(result)
}

pub fn edit(outcome: serde_json::Value, apply_hint: String) -> Result<String, String> {
    crate::render_remaining::edit(outcome, apply_hint)
}

pub fn explore_miss(
    names: Vec<String>,
    search: String,
    search_similar: String,
) -> Result<String, String> {
    crate::render_remaining::explore_miss(names, search, search_similar)
}

pub fn explore_usage(example: Option<String>) -> Result<String, String> {
    crate::render_remaining::explore_usage(example)
}

pub fn export_annotations(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::export_annotations(result)
}

pub fn extract_path_from_header(header: String) -> Result<String, String> {
    crate::render_remaining::extract_path_from_header(header)
}

pub fn fastembed_missing(compute: String) -> Result<String, String> {
    crate::render_remaining::fastembed_missing(compute)
}

pub fn import_annotations(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::import_annotations(result)
}

pub fn render_ingest_history(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::ingest_history(result)
}

pub fn render_ingest_status(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::ingest_status(result)
}

pub fn migrate_to_commit_time(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::migrate_to_commit_time(result)
}

pub fn mutation_applied(
    result: serde_json::Value,
    unverified_sites: Option<serde_json::Value>,
) -> Result<String, String> {
    crate::render_remaining::mutation_applied(result, unverified_sites)
}

pub fn mutation_error(raw: String, update_file: String) -> Result<String, String> {
    crate::render_remaining::mutation_error(raw, update_file)
}

pub fn mutation_plan(plan: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::mutation_plan(plan)
}

pub fn no_embeddings(compute: String, reindex: String) -> Result<String, String> {
    crate::render_remaining::no_embeddings(compute, reindex)
}

pub fn render_not_found(
    entity_id: String,
    candidates: Vec<serde_json::Value>,
    hint: String,
) -> Result<String, String> {
    crate::render_remaining::not_found(entity_id, candidates, hint)
}

pub fn open_items(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::open_items(result)
}

pub fn query_usage() -> Result<String, String> {
    crate::render_remaining::query_usage()
}

pub fn rebuild(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::rebuild(result)
}

pub fn reindex(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::reindex(result)
}

pub fn search_hybrid(query: String, results: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::search_hybrid(query, results)
}

pub fn search_miss(
    query: String,
    kind: Option<String>,
    search_similar: String,
    explore: String,
) -> Result<String, String> {
    crate::render_remaining::search_miss(query, kind, search_similar, explore)
}

pub fn search_similar(query: String, results: Vec<serde_json::Value>) -> Result<String, String> {
    crate::render_remaining::search_similar(query, results)
}

pub fn stale_banner(
    stale_files: serde_json::Value,
    referenced_paths: Vec<String>,
) -> Result<String, String> {
    crate::render_remaining::stale_banner(stale_files, referenced_paths)
}

pub fn status(result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::status(result)
}

pub fn store_repair(db_path: String, result: serde_json::Value) -> Result<String, String> {
    crate::render_remaining::store_repair(db_path, result)
}

pub fn trim_to_char_budget(body_lines: Vec<String>, max_chars: i64) -> Result<Vec<String>, String> {
    crate::render_remaining::trim_to_char_budget(body_lines, max_chars)
}

// ── Unit 12: pure-Rust embedding lifecycle and vector search ─────────────

fn embedding_config() -> (String, usize) {
    let config = core_indexer::api::active_config_json();
    let model = config
        .get("embedding")
        .and_then(|section| section.get("model"))
        .and_then(Value::as_str)
        .unwrap_or("BAAI/bge-small-en-v1.5")
        .to_string();
    let dimension = config
        .get("embedding")
        .and_then(|section| section.get("dimension"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(384);
    (model, dimension)
}

fn embedding_batch_size() -> usize {
    core_indexer::api::active_config_json()
        .get("embedding")
        .and_then(|section| section.get("batch_size"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(32)
}

fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f64>>, String> {
    let (model, dimension) = embedding_config();
    core_indexer::api::embed_texts_checked(
        &model,
        texts,
        embedding_batch_size(),
        dimension,
        deterministic_embeddings(),
    )
}

fn deterministic_embeddings() -> bool {
    std::env::var("CODERADAR_DETERMINISTIC_EMBEDDINGS").as_deref() == Ok("1")
}

fn embed_text(text: &str, _dimension: usize) -> Result<Vec<f64>, String> {
    embed_texts(&[text.to_string()])?
        .into_iter()
        .next()
        .ok_or_else(|| "EngineError: fastembed produced no vector".to_string())
}

fn query_vector(query: &Value, dimension: usize) -> Result<Vec<f64>, String> {
    match query {
        Value::String(text) => {
            if text.trim().is_empty() {
                Err(
                    "InvalidRequest: Please provide a natural-language query for semantic search."
                        .to_string(),
                )
            } else {
                embed_text(text, dimension)
            }
        }
        Value::Array(rows) => {
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let value = row.as_f64().ok_or_else(|| {
                    "InvalidRequest: embedding vectors must contain numbers".to_string()
                })?;
                out.push(value);
            }
            if out.is_empty() {
                Err("InvalidRequest: Please provide a query vector.".to_string())
            } else {
                Ok(out)
            }
        }
        _ => Err("InvalidRequest: query must be text or a vector".to_string()),
    }
}

/// Pure-Rust counterpart of `ops.compute_embeddings`.
pub fn compute_embeddings_op(
    model_name: Option<String>,
    batch_size: i64,
    recompute: bool,
) -> Result<Value, String> {
    require_index()?;
    let (configured, _dimension) = embedding_config();
    let model = model_name.unwrap_or(configured);
    if recompute {
        core_indexer::api::clear_embeddings()?;
    }
    let targets = core_indexer::api::embedding_targets()?;
    let texts: Vec<String> = targets.iter().map(|(_, text, _)| text.clone()).collect();
    let vectors = embed_texts(&texts)?;
    let entries: Vec<(String, Vec<f64>, String)> = targets
        .iter()
        .zip(vectors)
        .map(|((id, _, hash), vector)| (id.clone(), vector, hash.clone()))
        .collect();
    let report = core_indexer::api::set_embeddings(entries.clone())?;
    let persisted = core_indexer::api::persist_embeddings(
        &model,
        entries
            .iter()
            .map(|(id, _, hash)| (id.clone(), hash.clone()))
            .collect(),
    )
    .and_then(|value| {
        value
            .get("persisted")
            .and_then(Value::as_u64)
            .map(|n| n as i64)
            .ok_or_else(|| "persist_embeddings returned no count".to_string())
    })
    .unwrap_or(0);
    let _ = batch_size;
    Ok(json!({
        "generated": report.get("applied").and_then(Value::as_i64).unwrap_or(0),
        "cached": 0,
        "total": targets.len(),
        "errors": report.get("missing").and_then(Value::as_array).map_or(0, Vec::len),
        "persisted": persisted,
    }))
}

/// Pure-Rust counterpart of `ops.search_similar`.
pub fn search_similar_op(query: Value, top_k: i64) -> Result<Value, String> {
    let (_, dimension) = embedding_config();
    let vector = query_vector(&query, dimension)?;
    core_indexer::api::search_similar_vec(&vector, top_k.max(0) as usize)
}

/// Pure-Rust counterpart of `ops.search_hybrid`.
pub fn search_hybrid_op(query: Value, top_k: i64, query_text: String) -> Result<Value, String> {
    let (configured, dimension) = embedding_config();
    let (text, vector) = match &query {
        Value::String(value) => {
            if value.trim().is_empty() {
                return Err(
                    "InvalidRequest: Please provide a natural-language query for hybrid search."
                        .to_string(),
                );
            }
            (value.clone(), embed_text(value, dimension)?)
        }
        other => (query_text, query_vector(other, dimension)?),
    };
    if text.trim().is_empty() && vector.is_empty() {
        return Err(
            "InvalidRequest: Hybrid search needs query text, a query vector, or both.".to_string(),
        );
    }
    core_indexer::api::search_hybrid_vec(&configured, &text, &vector, top_k.max(0) as usize)
}

// ── Unit 37: cold-start fast path ───────────────────────────────────────

// The engine owns the load-vs-analyze decision and the staleness walk; this
// crate only keeps its CLI state in step. NOTE: Rust's full analyze is
// already 35-190x faster than Python's, so this path exists for caller-edge
// parity and can be removed once those callers use `api::analyze` directly.
pub fn coldstart_build_graph_op(
    root: &std::path::Path,
    create_store: bool,
) -> Result<Value, String> {
    let report = core_indexer::api::coldstart_build_graph(&root.to_string_lossy(), create_store)?;
    if let Some(store) = report.get("store").and_then(Value::as_str) {
        let _ = crate::state::load_from(std::path::Path::new(store), root);
    }
    Ok(report)
}

/// Bring the current project's index up to date, returning an ops-shaped report.
pub fn reindex_op(root: &std::path::Path) -> Result<Value, String> {
    reindex_with_op(root, false, false)
}

/// Reindex with the flags the MCP tool advertises.
///
/// `full` walks the whole tree through `analyze`; the default replays only
/// stale files through the cold-start fast path. `with_embeddings` refreshes
/// vectors after the graph is current.
///
/// # Errors
/// Returns the engine error when the walk, the replay, or the embedding pass
/// fails.
pub fn reindex_with_op(
    root: &std::path::Path,
    with_embeddings: bool,
    full: bool,
) -> Result<Value, String> {
    let report = if full {
        let report = analyze_with_routes(&root.to_string_lossy(), false, &[])?;
        crate::state::clear();
        crate::state::load(root)?;
        report
    } else {
        coldstart_build_graph_op(root, false)?
    };
    let stats = crate::state::projection()
        .map(|snap| core_indexer::api::stats(&snap))
        .unwrap_or(Value::Null);
    let mut value = json!({
        "files_indexed": report.get("files_indexed").cloned().unwrap_or(json!(0)),
        "entities_extracted": report.get("entities_extracted").cloned().unwrap_or(json!(0)),
        "extraction_failures": report.get("extraction_failures").cloned().unwrap_or(json!(0)),
        "stats": stats,
        "coldstart": report,
    });
    if with_embeddings {
        value["embeddings"] = compute_embeddings_op(None, 32, false)?;
    }
    Ok(value)
}

// ── Unit 14: git sync and commit-time migration ─────────────────────────

fn is_excluded_sync_path(path: &str) -> bool {
    path.split('/')
        .any(|part| part == ".coderadar" || part == ".git")
}

fn file_digest(repo: &std::path::Path, path: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(repo.join(path)).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(format!("{digest:x}"))
}

fn sync_ingest(
    report: &mut serde_json::Map<String, Value>,
    repo: &str,
    branch: &str,
    parent_ref: &str,
    store: &str,
) -> Option<Value> {
    match ingest_history(
        repo.to_string(),
        branch.to_string(),
        None,
        false,
        None,
        parent_ref.to_string(),
        None,
        Some(store.to_string()),
    ) {
        Ok(done) => {
            if done
                .get("commits_ingested")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0
            {
                report
                    .get_mut("ingested")
                    .and_then(Value::as_array_mut)
                    .expect("report.ingested is always an array")
                    .push(json!({
                        "branch": branch,
                        "lineage": done.get("lineage").cloned().unwrap_or(Value::Null),
                        "commits_ingested": done.get("commits_ingested").cloned().unwrap_or(json!(0)),
                        "generation": done.get("generation").cloned().unwrap_or(Value::Null),
                    }));
            }
            Some(done)
        }
        Err(reason) => {
            report
                .get_mut("refusals")
                .and_then(Value::as_array_mut)
                .expect("report.refusals is always an array")
                .push(json!({"branch": branch, "reason": reason}));
            None
        }
    }
}

/// Port of `ops.sync_git` (`py_agent/src/coderadar/ops.py:2140`).
pub fn sync_git(
    repo: String,
    store_path: Option<String>,
    parent_ref: String,
    catch_up: bool,
    source: String,
) -> Result<Value, String> {
    if repo.trim().is_empty() {
        return Err("InvalidRequest: Please provide a repository path.".to_string());
    }
    let store = write_store_path(store_path)?;
    let head = core_indexer::api::git_head(&repo).map_err(|e| format!("InvalidRequest: {e}"))?;
    let branches = core_indexer::api::branch_plan(&repo, &parent_ref)
        .map_err(|e| format!("EngineError: {e}"))?;
    let marks = core_indexer::api::git_marks(&store).map_err(|e| format!("EngineError: {e}"))?;

    let mut report = serde_json::Map::new();
    report.insert("repo".into(), json!(repo));
    report.insert("store".into(), json!(store));
    report.insert("head".into(), head.clone());
    report.insert(
        "checkout".into(),
        head.get("branch").cloned().unwrap_or(Value::Null),
    );
    report.insert(
        "previous".into(),
        json!({
            "checkout": marks.get("checkout").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null),
            "head": marks.get("head").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null),
        }),
    );
    report.insert("head_moved".into(), json!(false));
    report.insert("checkout_changed".into(), json!(false));
    report.insert("ingested".into(), json!([]));
    report.insert("refusals".into(), json!([]));
    report.insert("addresses_recorded".into(), json!(0));
    report.insert("observations".into(), json!([]));
    report.insert("skipped".into(), json!([]));
    report.insert("dirty".into(), json!([]));

    let sha = head.get("sha").and_then(Value::as_str).unwrap_or_default();
    if sha.is_empty() {
        core_indexer::api::record_git_marks(&store, None, None, "{}")
            .map_err(|e| format!("EngineError: {e}"))?;
        report.insert("empty".into(), json!(true));
        return Ok(Value::Object(report));
    }
    report.insert("empty".into(), json!(false));

    let trunk_tip = branches
        .iter()
        .find(|b| b.get("name").and_then(Value::as_str) == Some(parent_ref.as_str()))
        .and_then(|b| b.get("tip").and_then(Value::as_str))
        .unwrap_or(sha)
        .to_string();
    sync_ingest(&mut report, &repo, &parent_ref, &parent_ref, &store);

    let mut picked: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for branch in &branches {
        let name = branch
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let tip = branch
            .get("tip")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if branch
            .get("merged")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || tip == trunk_tip
            || name.ends_with("/HEAD")
        {
            continue;
        }
        let replace = match picked.get(tip) {
            None => true,
            Some(current) => current.contains('/') && !name.contains('/'),
        };
        if replace {
            picked.insert(tip.to_string(), name.to_string());
        }
    }
    for (_, name) in picked {
        sync_ingest(&mut report, &repo, &name, &parent_ref, &store);
    }

    let merged: Vec<(String, String)> = branches
        .iter()
        .filter(|b| b.get("merged").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|b| {
            Some((
                b.get("name").and_then(Value::as_str)?.to_string(),
                b.get("tip").and_then(Value::as_str)?.to_string(),
            ))
        })
        .collect();
    match core_indexer::api::record_branch_addresses(&store, merged) {
        Ok(count) => {
            report.insert("addresses_recorded".into(), json!(count));
        }
        Err(reason) => {
            report
                .get_mut("skipped")
                .and_then(Value::as_array_mut)
                .expect("report.skipped is always an array")
                .push(json!({"file": "", "reason": format!("addresses: {reason}")}));
        }
    }

    let prev_checkout = report
        .get("previous")
        .and_then(|v| v.get("checkout"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let prev_head = report
        .get("previous")
        .and_then(|v| v.get("head"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let new_checkout = head
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let same_lineage = if prev_checkout.is_empty() {
        "main"
    } else {
        &prev_checkout
    } == if new_checkout.is_empty() {
        "main"
    } else {
        &new_checkout
    };
    report.insert(
        "head_moved".into(),
        json!(!prev_head.is_empty() && prev_head != sha),
    );
    report.insert(
        "checkout_changed".into(),
        json!(!prev_head.is_empty() && !same_lineage),
    );

    let repo_root = std::path::Path::new(&repo);
    let changes: Vec<Value> = core_indexer::api::git_workdir_changes(&repo, sha)
        .map_err(|e| format!("EngineError: {e}"))?
        .into_iter()
        .filter(|c| {
            c.get("path")
                .and_then(Value::as_str)
                .is_some_and(|p| !is_excluded_sync_path(p))
        })
        .collect();
    let recorded: std::collections::BTreeMap<String, Option<String>> =
        serde_json::from_str(marks.get("dirty").and_then(Value::as_str).unwrap_or("{}"))
            .unwrap_or_default();
    let mut current: std::collections::BTreeMap<String, Option<String>> =
        std::collections::BTreeMap::new();
    for change in &changes {
        let path = change
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let digest = if change
            .get("in_workdir")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            file_digest(repo_root, path)
        } else {
            None
        };
        current.insert(path.to_string(), digest);
    }
    report.insert(
        "dirty".into(),
        json!(current.keys().cloned().collect::<Vec<_>>()),
    );

    let sweep_ready = crate::state::projection().is_some();
    if !sweep_ready {
        report
            .get_mut("skipped")
            .and_then(Value::as_array_mut)
            .expect("report.skipped is always an array")
            .push(json!({
                "file": "",
                "reason": "no project loaded: the sweep writes through update_file/remove_file, which need the loaded graph"
            }));
    } else if new_checkout.is_empty() {
        report
            .get_mut("skipped")
            .and_then(Value::as_array_mut)
            .expect("report.skipped is always an array")
            .push(json!({
                "file": "",
                "reason": "detached HEAD: there is no lineage to record the working tree's edits on"
            }));
    } else {
        let checkout_changed = report
            .get("checkout_changed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let head_moved = report
            .get("head_moved")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if checkout_changed || catch_up || head_moved {
            let (left, right, src, reset_all) = if checkout_changed {
                (
                    if prev_checkout.is_empty() {
                        "main".to_string()
                    } else {
                        prev_checkout.clone()
                    },
                    new_checkout.clone(),
                    "checkout".to_string(),
                    true,
                )
            } else {
                (
                    new_checkout.clone(),
                    new_checkout.clone(),
                    source.clone(),
                    false,
                )
            };
            if !prev_head.is_empty() {
                for path in recorded.keys() {
                    if !reset_all && current.contains_key(path) {
                        continue;
                    }
                    let content = core_indexer::api::git_blob_text(&repo, &prev_head, path).ok();
                    observe(&mut report, path, content, &left, &src);
                }
            }
            for change in &changes {
                let path = change
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let in_workdir = change
                    .get("in_workdir")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if in_workdir {
                    if !reset_all
                        && recorded.get(path).and_then(|v| v.as_deref())
                            == current.get(path).and_then(|v| v.as_deref())
                        && recorded.contains_key(path)
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(repo_root.join(path)).ok();
                    match text {
                        Some(text) => observe(&mut report, path, Some(text), &right, &src),
                        None => report
                            .get_mut("skipped")
                            .and_then(Value::as_array_mut)
                            .expect("report.skipped is always an array")
                            .push(json!({"file": path, "reason": "not readable as UTF-8"})),
                    }
                } else {
                    observe(&mut report, path, None, &right, &src);
                }
            }
        }
    }

    let mut dirty_json = serde_json::Map::new();
    for (path, digest) in &current {
        if let Some(digest) = digest {
            dirty_json.insert(path.clone(), json!(digest));
        }
    }
    let dirty = serde_json::to_string(&Value::Object(dirty_json))
        .map_err(|e| format!("EngineError: cannot serialize dirty set: {e}"))?;
    core_indexer::api::record_git_marks(
        &store,
        if new_checkout.is_empty() {
            None
        } else {
            Some(&new_checkout)
        },
        Some(sha),
        &dirty,
    )
    .map_err(|e| format!("EngineError: {e}"))?;
    Ok(Value::Object(report))
}

fn observe(
    report: &mut serde_json::Map<String, Value>,
    path: &str,
    content: Option<String>,
    lineage: &str,
    source: &str,
) {
    let removed = content.is_none();
    let result = match content {
        None => core_indexer::api::remove_file_observed(path, Some(lineage)),
        Some(text) => {
            core_indexer::api::update_file_observed(path, Some(&text), source, Some(lineage))
        }
    };
    match result {
        Ok(value) => {
            let observed_at = value
                .get("observed_at")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut row = serde_json::Map::new();
            row.insert("file".into(), json!(path));
            row.insert("lineage".into(), json!(lineage));
            row.insert("source".into(), json!(source));
            if removed {
                row.insert("removed".into(), json!(true));
            } else {
                row.insert("observed_at".into(), json!(observed_at));
                row.insert("removed".into(), json!(false));
            }
            report
                .get_mut("observations")
                .and_then(Value::as_array_mut)
                .expect("report.observations is always an array")
                .push(Value::Object(row));
        }
        Err(reason) => {
            report
                .get_mut("skipped")
                .and_then(Value::as_array_mut)
                .expect("report.skipped is always an array")
                .push(json!({"file": path, "reason": reason}));
        }
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Port of `ops.migrate_to_commit_time` (`py_agent/src/coderadar/ops.py:2659`).
pub fn migrate_to_commit_time_op(
    repo: String,
    store: String,
    branches: Option<Vec<String>>,
    parent_ref: String,
    embed: bool,
    verify: bool,
) -> Result<Value, String> {
    if repo.trim().is_empty() {
        return Err("InvalidRequest: Please provide a git repository path.".to_string());
    }
    if store.trim().is_empty() {
        return Err(
            "InvalidRequest: Please provide a destination store path (a new file; the old store is never touched)."
                .to_string(),
        );
    }
    if parent_ref.trim().is_empty() {
        return Err("InvalidRequest: Please provide a parent ref (default 'main').".to_string());
    }
    let started = std::time::Instant::now();
    if let Some(parent) = std::path::Path::new(&store).parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "InvalidRequest: cannot create store directory {}: {e}",
                    parent.display()
                )
            })?;
        }
    }
    let listed =
        core_indexer::api::list_branches(&repo).map_err(|e| format!("InvalidRequest: {e}"))?;
    let plan = if listed.iter().any(|b| b == "main") {
        core_indexer::api::branch_plan(&repo, &parent_ref)
            .map_err(|e| format!("InvalidRequest: {e}"))?
    } else {
        Vec::new()
    };
    let (ordered, addresses) = match branches {
        Some(mut explicit) => {
            if listed.iter().any(|b| b == "main") && !explicit.iter().any(|b| b == "main") {
                explicit.insert(0, "main".to_string());
            }
            (explicit, std::collections::BTreeMap::new())
        }
        None => {
            if !listed.iter().any(|b| b == "main") {
                return Err(format!(
                    "InvalidRequest: repository has no 'main' branch: pass branches=[...] explicitly (local branches: {}).",
                    if listed.is_empty() {
                        "(none)".to_string()
                    } else {
                        listed.join(", ")
                    }
                ));
            }
            let mut lineages: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            let mut addresses: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            for branch in &plan {
                let name = branch
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let tip = branch
                    .get("tip")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if branch
                    .get("merged")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || name.ends_with("/HEAD")
                {
                    addresses.insert(name.to_string(), tip.to_string());
                } else if lineages
                    .get(tip)
                    .is_none_or(|current| current.contains('/') && !name.contains('/'))
                {
                    lineages.insert(tip.to_string(), name.to_string());
                }
            }
            let mut ordered: Vec<String> = vec![parent_ref.clone()];
            for (_, name) in lineages {
                if name != parent_ref {
                    ordered.push(name);
                }
            }
            (ordered, addresses)
        }
    };

    let mut branch_reports = serde_json::Map::new();
    for branch in &ordered {
        let done = ingest_history(
            repo.clone(),
            branch.clone(),
            None,
            false,
            None,
            parent_ref.clone(),
            None,
            Some(store.clone()),
        )?;
        branch_reports.insert(branch.clone(), done);
    }
    let entries: Vec<(String, String)> = addresses
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let addresses_recorded = if entries.is_empty() {
        0
    } else {
        core_indexer::api::record_branch_addresses(&store, entries)
            .map_err(|e| format!("EngineError: {e}"))?
    };
    let merge_heal =
        reingest_merged_commits(repo.clone(), parent_ref.clone(), false, Some(store.clone()))?;
    let mut commits_skipped = 0i64;
    let mut files_failed = 0i64;
    for report in branch_reports.values() {
        for (key, total) in [
            ("commits_skipped", &mut commits_skipped),
            ("files_failed", &mut files_failed),
        ] {
            *total += report
                .get(key)
                .and_then(Value::as_array)
                .map_or(report.get(key).and_then(Value::as_i64).unwrap_or(0), |a| {
                    a.len() as i64
                });
        }
    }
    let commits_ingested: i64 = branch_reports
        .values()
        .filter_map(|r| r.get("commits_ingested").and_then(Value::as_i64))
        .sum();
    crate::state::load(std::path::Path::new(&repo))?;
    let embeddings = if embed {
        let report = compute_embeddings_op(None, 32, false)?;
        json!({"embedded": true, "embedded_count": report.get("generated").cloned().unwrap_or(json!(0))})
    } else {
        json!({"embedded": false})
    };
    let coverage = if verify {
        let mut rows = serde_json::Map::new();
        for branch in &ordered {
            let verdict = verify_history(None, None, 0, true, branch.clone(), 0, None, None)?;
            rows.insert(
                branch.clone(),
                json!({
                    "byte_coverage_pct": verdict.get("byte_coverage_pct").cloned().unwrap_or(Value::Null),
                    "totals": verdict.get("totals").cloned().unwrap_or(Value::Null),
                    "total_generations": verdict.get("total_generations").cloned().unwrap_or(Value::Null),
                }),
            );
        }
        Value::Object(rows)
    } else {
        Value::Null
    };
    Ok(json!({
        "repo": repo,
        "store": store,
        "parent_ref": parent_ref,
        "branches": ordered,
        "branch_reports": Value::Object(branch_reports),
        "commits_ingested": commits_ingested,
        "commits_skipped": commits_skipped,
        "files_failed": files_failed,
        "addresses": Value::Object(addresses.into_iter().map(|(k,v)|(k,json!(v))).collect()),
        "addresses_recorded": addresses_recorded,
        "merge_heal": merge_heal,
        "embeddings": embeddings,
        "verify": coverage,
        "elapsed_secs": round3(started.elapsed().as_secs_f64()),
    }))
}

// ── Unit 15: framework route extraction ─────────────────────────────────

fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(map) => match map.keys().next().map(String::as_str) {
            Some("$tuple") => "tuple",
            Some("$set") => "set",
            Some("$bytes") => "bytes",
            _ => "dict",
        },
    }
}

fn root_arg(value: &Value) -> Result<String, String> {
    match value {
        Value::String(path) => Ok(path.clone()),
        other => Err(format!(
            "TypeError: argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
            py_type_name(other)
        )),
    }
}

fn empty_report() -> Value {
    json!({
        "routes": 0,
        "handlers": 0,
        "frameworks": [],
        "routes_upserted": 0,
        "routes_retired": 0,
        "pairs_retired": 0,
    })
}

fn register_routes(
    nodes: Vec<(String, String, String, String, Vec<String>, String)>,
    edges: Vec<(String, String, String)>,
    scope: &str,
) -> (i64, i64, i64) {
    match core_indexer::api::register_synthetic_routes(nodes, edges, scope) {
        Ok(report) => (
            report
                .get("routes_upserted")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            report
                .get("routes_retired")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            report
                .get("pairs_retired")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        ),
        Err(_) => (0, 0, 0),
    }
}

fn fill_report(
    report: &mut Value,
    routes: i64,
    handlers: i64,
    frameworks: Vec<String>,
    registration: (i64, i64, i64),
) {
    if let Some(map) = report.as_object_mut() {
        map.insert("routes".into(), json!(routes));
        map.insert("handlers".into(), json!(handlers));
        map.insert("frameworks".into(), json!(frameworks));
        map.insert("routes_upserted".into(), json!(registration.0));
        map.insert("routes_retired".into(), json!(registration.1));
        map.insert("pairs_retired".into(), json!(registration.2));
    }
}

fn detect_frameworks(source: &str) -> Vec<String> {
    let lower = source.to_lowercase();
    let mut found: Vec<String> = Vec::new();
    // The Python Flask/FastAPI detectors are intentionally broad: any
    // mention of the framework in a project file opts the resolver in.
    if lower.contains("flask") {
        found.push("flask".to_string());
    }
    if lower.contains("fastapi") {
        found.push("fastapi".to_string());
    }
    if lower.contains("laravel/framework") {
        found.push("laravel".to_string());
    }
    for line in source.lines() {
        let raw = line.trim_start();
        let trimmed = raw.to_lowercase();
        let marker = if trimmed.starts_with("from django") || trimmed.starts_with("import django") {
            Some("django")
        } else if trimmed.contains("require('express')") || trimmed.starts_with("from 'express'") {
            Some("express")
        } else if trimmed.starts_with("from '@nestjs")
            || line.starts_with("@Controller(")
            || line.starts_with("@Module(")
            || (line.starts_with('{') && trimmed.contains("\"@nestjs/core\""))
        {
            Some("nestjs")
        } else if trimmed.starts_with("import org.springframework") {
            Some("springboot")
        } else if trimmed.starts_with("from 'react-router")
            || (line.starts_with('<') && trimmed.contains("route"))
            || (line.starts_with('{') && trimmed.contains("\"react-router-dom\""))
        {
            Some("react-router")
        } else if trimmed.starts_with("from 'vue-router")
            || (line.starts_with("import") && trimmed.contains("from 'vue-router'"))
            || (line.starts_with('{') && trimmed.contains("\"vue-router\""))
        {
            Some("vue-router")
        } else if trimmed.starts_with("rails.application.routes")
            || trimmed.starts_with("get '/")
            || line.starts_with("gem 'rails'")
            || line.starts_with("has_many ")
            || line.starts_with("belongs_to ")
            || line.starts_with("has_one ")
            || (line.starts_with("class ")
                && (trimmed.contains("applicationrecord")
                    || trimmed.contains("applicationcontroller")))
        {
            Some("rails")
        } else if line.starts_with("Route::get(") || line.starts_with("Route::post(") {
            Some("laravel")
        } else if trimmed.starts_with("using microsoft.aspnetcore")
            || trimmed.starts_with("<project sdk=\"microsoft.net.sdk.web")
            || line.starts_with("[ApiController")
            || line.starts_with("[HttpPost")
            || line.starts_with("[HttpGet")
        {
            Some("aspnet")
        } else if trimmed.starts_with("@restcontroller")
            || trimmed.starts_with("@springbootapplication")
        {
            Some("springboot")
        } else if trimmed.starts_with("use actix_web") || line.starts_with("actix-web") {
            Some("actix")
        } else if trimmed.starts_with("\"net/http\"")
            || trimmed.starts_with("http.handlefunc")
            || (line.starts_with("import") && trimmed.contains("gin-gonic/gin"))
        {
            Some("go")
        } else {
            None
        };
        if let Some(name) = marker {
            if !found.iter().any(|n| n == name) {
                found.push(name.to_string());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Project-level tells (pom.xml, composer.json, csproj, package.json, ...).
fn detect_root_frameworks(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if path.extension().and_then(|ext| ext.to_str()) == Some("go") {
            if !found.contains(&"go".to_string()) {
                found.push("go".to_string());
            }
        }
        for framework in detect_frameworks(&text) {
            if !found.contains(&framework) {
                found.push(framework);
            }
        }
    }
    found
}

fn walk_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn visit(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "__pycache__" || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                visit(&path, out);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| {
                    matches!(
                        ext,
                        "py" | "js"
                            | "ts"
                            | "jsx"
                            | "tsx"
                            | "java"
                            | "go"
                            | "php"
                            | "rb"
                            | "cs"
                            | "rs"
                    )
                })
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out.sort();
    out
}

fn unquote(raw: &str) -> Option<String> {
    let mut trimmed = raw.trim();
    if trimmed.len() > 1
        && matches!(
            trimmed.as_bytes()[0],
            b'r' | b'R' | b'f' | b'F' | b'u' | b'U' | b'b' | b'B'
        )
        && matches!(trimmed.as_bytes()[1], b'\'' | b'"' | b'`')
    {
        trimmed = &trimmed[1..];
    }
    let first = trimmed.chars().next()?;
    if first != '\'' && first != '"' && first != '`' {
        return None;
    }
    let rest = &trimmed[first.len_utf8()..];
    let end = rest.find(first)?;
    Some(rest[..end].to_string())
}

fn first_arg(raw: &str) -> Option<String> {
    let open = raw.find('(')?;
    unquote(&raw[open + 1..])
}

fn second_token(raw: &str) -> String {
    let open = raw.find('(').unwrap_or(0);
    raw[open + 1..]
        .split(',')
        .nth(1)
        .unwrap_or_default()
        .trim()
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
        .to_string()
}

fn quoted_values(raw: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut chars = raw.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch != '\'' && ch != '"' && ch != '`' {
            continue;
        }
        let quote = ch;
        let mut value = String::new();
        for (_, next) in chars.by_ref() {
            if next == quote {
                break;
            }
            value.push(next);
        }
        if !value.is_empty() {
            values.push(value);
        }
    }
    values
}

/// Generic route patterns shared by the resolver family. Each match yields
/// `(pattern, handler, methods, framework)`.
fn extract_routes(source: &str, rel_posix: &str, is_js: bool) -> (Vec<Route>, Vec<RouteEdge>) {
    let mut out = Vec::new();
    let mut extra_edges: Vec<RouteEdge> = Vec::new();
    let mut spring_base = String::new();
    let mut aspnet_base = String::new();
    let mut nest_base = String::new();
    let mut in_nest_module = false;
    let is_go = std::path::Path::new(rel_posix)
        .extension()
        .and_then(|ext| ext.to_str())
        == Some("go");
    let is_rust = std::path::Path::new(rel_posix)
        .extension()
        .and_then(|ext| ext.to_str())
        == Some("rs");
    let mut in_docstring = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
            let quote = if trimmed.starts_with("\"\"\"") {
                "\"\"\""
            } else {
                "'''"
            };
            if !trimmed[quote.len()..].contains(quote) {
                in_docstring = !in_docstring;
            }
            continue;
        }
        if in_docstring
            || (trimmed.starts_with('#') && !trimmed.starts_with("#["))
            || trimmed.starts_with("//")
            || trimmed.starts_with('*')
        {
            continue;
        }
        let lower = trimmed.to_lowercase();

        // Python/TS decorators and receiver calls: @app.route('/x'), @router.get.
        let decorator = lower.contains(".route(")
            || lower.contains(".get(")
            || lower.contains(".post(")
            || lower.contains(".put(")
            || lower.contains(".delete(")
            || lower.contains(".patch(");
        let decorator = decorator
            && !lower.contains("route::")
            && (trimmed.starts_with('@') || is_js || lower.contains("=>"));
        if decorator {
            if let Some(pattern) = first_arg(trimmed) {
                let method = if lower.contains(".post(") {
                    "POST"
                } else if lower.contains(".put(") {
                    "PUT"
                } else if lower.contains(".delete(") {
                    "DELETE"
                } else if lower.contains(".patch(") {
                    "PATCH"
                } else {
                    "GET"
                };
                let handler = source
                    .lines()
                    .skip_while(|candidate| !std::ptr::eq(*candidate, trimmed))
                    .skip(1)
                    .find(|candidate| candidate.contains("def "))
                    .and_then(|candidate| {
                        candidate.rsplit_once("def ").map(|(_, tail)| {
                            tail.split('(')
                                .next()
                                .unwrap_or_default()
                                .trim()
                                .to_string()
                        })
                    })
                    .unwrap_or_else(|| second_token(trimmed));
                let fw = if lower.contains("django") {
                    "django"
                } else if lower.contains("fastapi") {
                    "fastapi"
                } else if lower.contains("express") {
                    "express"
                } else {
                    "flask"
                };
                out.push((pattern, handler, vec![method.to_string()], fw.to_string()));
            }
        }

        // Django url patterns.
        if lower.starts_with("path(") || lower.starts_with("url(") || lower.starts_with("re_path(")
        {
            if let Some(pattern) = first_arg(trimmed) {
                out.push((
                    pattern,
                    second_token(trimmed),
                    vec!["GET".to_string()],
                    "django".to_string(),
                ));
            }
        }

        // Laravel resource controllers expand to their five canonical routes.
        if lower.contains("route::resource(") {
            let name = first_arg(trimmed).unwrap_or_default();
            let controller = trimmed
                .split("resource(")
                .nth(1)
                .unwrap_or_default()
                .split(',')
                .nth(1)
                .unwrap_or_default()
                .trim()
                .split("::")
                .next()
                .unwrap_or_default()
                .to_string();
            let base = format!("/{name}");
            for (method, suffix, action) in [
                ("GET", "", "index"),
                ("GET", "/{id}", "show"),
                ("POST", "", "store"),
                ("PUT", "/{id}", "update"),
                ("DELETE", "/{id}", "destroy"),
            ] {
                let handler = if controller.is_empty() {
                    action.to_string()
                } else {
                    format!("{controller}.{action}")
                };
                out.push((
                    format!("{base}{suffix}"),
                    handler,
                    vec![method.to_string()],
                    "laravel".to_string(),
                ));
            }
        }

        // Laravel facades.
        if lower.contains("route::get(")
            || lower.contains("route::post(")
            || lower.contains("route::put(")
            || lower.contains("route::delete(")
            || lower.contains("route::patch(")
            || lower.contains("route::any(")
        {
            if let Some(pattern) = first_arg(trimmed) {
                let method = if lower.contains("post(") {
                    "POST"
                } else {
                    "GET"
                };
                out.push((
                    pattern,
                    second_token(trimmed),
                    vec![method.to_string()],
                    "laravel".to_string(),
                ));
            }
        }

        // Spring Boot mappings (class-level base path + method annotations).
        if lower.starts_with("@requestmapping(") {
            let base = first_arg(trimmed).unwrap_or_default();
            spring_base = base;
        }
        if trimmed.starts_with('@')
            && lower.contains("mapping")
            && !lower.contains("requestmapping")
        {
            let method = if lower.contains("postmapping") {
                "POST"
            } else if lower.contains("putmapping") {
                "PUT"
            } else if lower.contains("deletemapping") {
                "DELETE"
            } else {
                "GET"
            };
            let suffix = first_arg(trimmed).unwrap_or_default();
            let path = if suffix.is_empty() {
                spring_base.clone()
            } else if spring_base.is_empty() {
                suffix
            } else {
                format!("{}{}", spring_base.trim_end_matches('/'), suffix)
            };
            if !path.is_empty() {
                let handler = trimmed.trim().to_string();
                out.push((
                    path,
                    handler,
                    vec![method.to_string()],
                    "springboot".to_string(),
                ));
            }
        }

        // ASP.NET attributes (class-level [Route] + method attributes).
        if lower.starts_with("[route(") {
            let base = first_arg(trimmed).unwrap_or_default();
            aspnet_base = format!("/{}", base.trim_start_matches('/'));
        }
        if trimmed.starts_with('[') && lower.contains("httpget")
            || trimmed.starts_with('[') && lower.contains("httppost")
        {
            let method = if lower.contains("httppost") {
                "POST"
            } else {
                "GET"
            };
            let suffix = first_arg(trimmed).unwrap_or_default();
            let path = if suffix.is_empty() {
                aspnet_base.clone()
            } else if aspnet_base.is_empty() {
                suffix
            } else {
                format!("{}{}", aspnet_base.trim_end_matches('/'), suffix)
            };
            if !path.is_empty() {
                let handler = trimmed.trim().to_string();
                out.push((
                    path,
                    handler,
                    vec![method.to_string()],
                    "aspnet".to_string(),
                ));
            }
        }

        // Go net/http handlers.
        if lower.contains("handlefunc(") {
            if let Some(pattern) = first_arg(trimmed) {
                out.push((
                    pattern,
                    second_token(trimmed),
                    vec!["GET".to_string()],
                    "go".to_string(),
                ));
            }
        }

        // JS/TS router objects: path: '/x'.
        if lower.contains("path:") {
            if let Some(pos) = trimmed.find(':') {
                if let Some(pattern) = unquote(&trimmed[pos + 1..]) {
                    let fw = if lower.contains("router") || lower.contains("route") {
                        "react-router"
                    } else {
                        "vue-router"
                    };
                    let component = trimmed
                        .split("component:")
                        .nth(1)
                        .unwrap_or_default()
                        .trim()
                        .trim_end_matches(',')
                        .to_string();
                    out.push((pattern, component, vec!["GET".to_string()], fw.to_string()));
                }
            }
        }
        // NestJS controller routes.
        if lower.starts_with("@controller(") {
            nest_base = first_arg(trimmed).unwrap_or_default();
        }
        if trimmed.starts_with('@')
            && matches!(lower.as_str(), lower if lower.starts_with("@get(") || lower.starts_with("@post(") || lower.starts_with("@put(") || lower.starts_with("@delete(") || lower.starts_with("@get()") || lower.starts_with("@post()"))
        {
            let method = if lower.starts_with("@post") {
                "POST"
            } else if lower.starts_with("@put") {
                "PUT"
            } else if lower.starts_with("@delete") {
                "DELETE"
            } else {
                "GET"
            };
            let suffix = first_arg(trimmed).unwrap_or_default();
            let path = if suffix.is_empty() {
                nest_base.clone()
            } else if nest_base.is_empty() {
                suffix
            } else {
                format!("{}{}", nest_base.trim_end_matches('/'), suffix)
            };
            if !path.is_empty() {
                out.push((
                    path,
                    trimmed.trim().to_string(),
                    vec![method.to_string()],
                    "nestjs".to_string(),
                ));
            }
        }

        // Rails model associations are synthetic nodes in the Ruby resolver.
        for kind in ["has_many", "belongs_to", "has_one"] {
            if lower.starts_with(&format!("{kind} ")) {
                let name = trimmed
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .trim_start_matches(':')
                    .to_string();
                if !name.is_empty() {
                    out.push((
                        name.clone(),
                        name,
                        vec![kind.to_uppercase()],
                        "rails".to_string(),
                    ));
                }
            }
        }

        // React Router JSX/data routes.
        if trimmed.starts_with("<route") || lower.contains("<route ") {
            let component = trimmed
                .split("element={<")
                .nth(1)
                .and_then(|tail| tail.split(['}', ' ', '/']).next())
                .unwrap_or_default()
                .to_string();
            if let Some(pos) = lower.find("path=") {
                if let Some(pattern) = unquote(&trimmed[pos + 5..]) {
                    out.push((
                        pattern,
                        component,
                        vec!["GET".to_string()],
                        "react-router".to_string(),
                    ));
                }
            } else if lower.contains("index ") || lower.contains("index>") {
                out.push((
                    "index".to_string(),
                    component,
                    vec!["GET".to_string()],
                    "react-router".to_string(),
                ));
            }
        }
        // Link navigation targets are routes too.
        if lower.starts_with("<link to=") || lower.starts_with("<navlink to=") {
            if let Some(pos) = lower.find("to=") {
                if let Some(pattern) = unquote(&trimmed[pos + 3..]) {
                    out.push((
                        pattern,
                        String::new(),
                        vec!["GET".to_string()],
                        "react-router".to_string(),
                    ));
                }
            }
        }

        // Go Gin / Echo style method calls.
        if is_go {
            for (needle, method) in [
                (".get(", "GET"),
                (".post(", "POST"),
                (".put(", "PUT"),
                (".delete(", "DELETE"),
                (".patch(", "PATCH"),
            ] {
                if lower.contains(needle) {
                    if let Some(pattern) = first_arg(trimmed) {
                        out.push((
                            pattern,
                            second_token(trimmed),
                            vec![method.to_string()],
                            "go".to_string(),
                        ));
                    }
                    break;
                }
            }
        }

        // Actix attribute macros and cfg.route(...) registrations.
        if is_rust {
            for (needle, method) in [
                ("#[get(", "GET"),
                ("#[post(", "POST"),
                ("#[put(", "PUT"),
                ("#[delete(", "DELETE"),
            ] {
                if lower.starts_with(needle) {
                    if let Some(pattern) = unquote(&trimmed[needle.len()..]) {
                        out.push((
                            pattern,
                            trimmed.trim().to_string(),
                            vec![method.to_string()],
                            "actix".to_string(),
                        ));
                    }
                    break;
                }
            }
            let mut rest = trimmed;
            while let Some(pos) = rest.find(".route(") {
                let after = &rest[pos + ".route(".len()..];
                let segment = after;
                if let Some(pattern) = unquote(segment) {
                    let handler = segment
                        .split(".to(")
                        .nth(1)
                        .unwrap_or_default()
                        .split(')')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    out.push((
                        pattern,
                        handler,
                        vec!["GET".to_string()],
                        "actix".to_string(),
                    ));
                }
                rest = after;
            }
        }

        // Express middleware: app.use(fn) or app.use('/path', fn, router).
        if is_js && lower.contains(".use(") {
            let after = trimmed.split_once(".use(").map_or("", |(_, tail)| tail);
            let path = unquote(after).unwrap_or_else(|| "/*".to_string());
            let handler_part = if path == "/*" {
                after.to_string()
            } else {
                let quote = after.chars().next().unwrap_or('"');
                after[1..]
                    .find(quote)
                    .map_or(String::new(), |i| after[i + 2..].to_string())
            };
            let handlers: Vec<String> = handler_part
                .trim_start_matches([',', '"', '\'', '`'])
                .split(',')
                .map(|value| value.trim().trim_end_matches([')', ';']).to_string())
                .filter(|value| !value.is_empty())
                .collect();
            if let Some(first) = handlers.first() {
                out.push((
                    path.clone(),
                    first.clone(),
                    vec!["USE".to_string()],
                    "express".to_string(),
                ));
                for handler in handlers.iter().skip(1) {
                    extra_edges.push((
                        format!("{rel_posix}::express:route:USE:{path}"),
                        format!("{rel_posix}::{handler}"),
                        "synthetic:express:handles".to_string(),
                    ));
                }
            }
        }

        // Rails controller/model callbacks are synthetic callback nodes.
        for kind in [
            "before_action",
            "after_action",
            "around_action",
            "skip_before_action",
            "skip_after_action",
            "before_save",
            "after_save",
            "before_create",
            "after_create",
            "before_validation",
            "after_validation",
        ] {
            if lower.starts_with(kind) {
                let method = trimmed
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .trim_start_matches(':')
                    .to_string();
                if !method.is_empty() {
                    out.push((
                        format!("{kind} :{method}"),
                        method,
                        vec![kind.to_uppercase()],
                        "rails".to_string(),
                    ));
                }
                break;
            }
        }

        // NestJS @Module declarations contribute provider/controller edges.
        if lower.contains("@module(") {
            in_nest_module = true;
        }
        if in_nest_module {
            for key in ["controllers", "providers", "imports", "exports"] {
                if let Some(pos) = lower.find(&format!("{key}:")) {
                    let after = &trimmed[pos + key.len() + 1..];
                    if let Some(open) = after.find('[') {
                        if let Some(close) = after[open + 1..].find(']') {
                            for value in after[open + 1..open + 1 + close].split(',') {
                                let reference = value.trim().trim_matches([' ', '\'', '"']);
                                if !reference.is_empty() {
                                    let kind = key.trim_end_matches('s');
                                    extra_edges.push((
                                        format!("{rel_posix}::module"),
                                        format!("{rel_posix}::{reference}"),
                                        format!("synthetic:nestjs:{kind}"),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            if lower.contains("})") {
                in_nest_module = false;
            }
        }

        // ASP.NET minimal APIs.
        if lower.contains("mapget(") || lower.contains("mappost(") {
            if let Some(pattern) = first_arg(trimmed) {
                let method = if lower.contains("mappost(") {
                    "POST"
                } else {
                    "GET"
                };
                out.push((
                    pattern,
                    second_token(trimmed),
                    vec![method.to_string()],
                    "aspnet".to_string(),
                ));
            }
        }

        // Framework-specific edges that carry no synthetic route node.
        if lower.contains("register_blueprint(") {
            if let Some(open) = trimmed.find('(') {
                let target = trimmed[open + 1..]
                    .split(')')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !target.is_empty() {
                    extra_edges.push((
                        format!("{rel_posix}::app"),
                        format!("{rel_posix}::{target}"),
                        "synthetic:flask:registers".to_string(),
                    ));
                }
            }
        }
        if lower.contains("add_resource(") {
            let resource = trimmed
                .split('(')
                .nth(1)
                .unwrap_or_default()
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            for path in quoted_values(trimmed) {
                extra_edges.push((
                    format!("{rel_posix}::flask:route:{path}"),
                    format!("{rel_posix}::{resource}"),
                    "synthetic:flask:handles".to_string(),
                ));
            }
        }
        if lower.contains("router.register(") {
            let values = quoted_values(trimmed);
            let viewset = trimmed
                .split("register(")
                .nth(1)
                .unwrap_or_default()
                .split(',')
                .nth(1)
                .unwrap_or_default()
                .trim()
                .to_string();
            if let Some(prefix) = values.first() {
                extra_edges.push((
                    format!("{rel_posix}::django:route:{prefix}"),
                    format!("{rel_posix}::{viewset}"),
                    "synthetic:django:handles".to_string(),
                ));
            }
        }
        if lower.contains("depends(") {
            let handler = trimmed
                .rsplit_once("def ")
                .map(|(_, tail)| {
                    tail.split('(')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string()
                })
                .unwrap_or_default();
            let dependency = trimmed
                .split("Depends(")
                .nth(1)
                .unwrap_or_default()
                .split(')')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            if !handler.is_empty() && !dependency.is_empty() {
                extra_edges.push((
                    format!("{rel_posix}::{handler}"),
                    format!("{rel_posix}::{dependency}"),
                    "synthetic:fastapi:depends_on".to_string(),
                ));
            }
        }
    }
    (out, extra_edges)
}

type Route = (String, String, Vec<String>, String);
type RouteNode = (String, String, String, String, Vec<String>, String);
type RouteEdge = (String, String, String);

fn collect_extractions(rel_posix: &str, routes: Vec<Route>) -> (Vec<RouteNode>, Vec<RouteEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for (pattern, handler, methods, framework) in routes {
        let id = format!("{rel_posix}::{framework}:route:{pattern}");
        nodes.push((
            id.clone(),
            pattern,
            rel_posix.to_string(),
            handler.clone(),
            methods,
            framework.clone(),
        ));
        if !handler.is_empty() {
            edges.push((
                id,
                format!("{rel_posix}::{handler}"),
                format!("synthetic:{framework}:handles"),
            ));
        }
    }
    (nodes, edges)
}

/// Whole-tree framework extraction (`framework.py::run_framework_extraction`).
pub fn run_framework_extraction(root: serde_json::Value) -> Result<serde_json::Value, String> {
    let root = root_arg(&root)?;
    let root_path = std::path::Path::new(&root);
    let mut frameworks: Vec<String> = Vec::new();
    let mut sources: Vec<(String, String)> = Vec::new();
    for file in walk_sources(root_path) {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(rel) = file.strip_prefix(root_path) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        for framework in detect_frameworks(&source) {
            if !frameworks.contains(&framework) {
                frameworks.push(framework);
            }
        }
        sources.push((rel, source));
    }
    frameworks.sort();
    if frameworks.is_empty() {
        let registration = register_routes(Vec::new(), Vec::new(), "full");
        let mut report = empty_report();
        fill_report(&mut report, 0, 0, frameworks, registration);
        return Ok(report);
    }
    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();
    let mut routes = 0i64;
    let mut handlers = 0i64;
    for (rel, source) in sources {
        let is_js = std::path::Path::new(&rel)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "js" | "ts" | "jsx" | "tsx"));
        let (found_routes, extra_edges) = extract_routes(&source, &rel, is_js);
        let (nodes, mut edges) = collect_extractions(&rel, found_routes);
        edges.extend(extra_edges);
        routes += nodes.len() as i64;
        handlers += edges.len() as i64;
        all_nodes.extend(nodes);
        all_edges.extend(edges);
    }
    let registration = register_routes(all_nodes, all_edges, "full");
    let mut report = empty_report();
    fill_report(&mut report, routes, handlers, frameworks, registration);
    Ok(report)
}

/// Full analyze plus the DR-10 framework-route hook.
///
/// Python's `analyze` runs extraction best-effort after the Rust index so a
/// fresh-session `resolve('/health')` can see persisted routes. Extraction
/// failures never fail the index; the analyze report stays byte-identical.
pub fn analyze_with_routes(
    root: &str,
    create_store: bool,
    exclude: &[String],
) -> Result<serde_json::Value, String> {
    let report = core_indexer::api::analyze(root, create_store, exclude)?;
    let _ = run_framework_extraction(serde_json::json!(root));
    Ok(report)
}

/// One-file framework extraction (`framework.py::refresh_framework_file`).
pub fn refresh_framework_file(
    root: serde_json::Value,
    rel_posix: String,
    content: Option<String>,
) -> Result<serde_json::Value, String> {
    let root = root_arg(&root)?;
    let source = content
        .or_else(|| std::fs::read_to_string(std::path::Path::new(&root).join(&rel_posix)).ok());
    let Some(source) = source else {
        let registration = register_routes(Vec::new(), Vec::new(), &rel_posix);
        let mut report = empty_report();
        fill_report(&mut report, 0, 0, Vec::new(), registration);
        return Ok(report);
    };
    let mut frameworks = detect_frameworks(&source);
    for framework in detect_root_frameworks(std::path::Path::new(&root)) {
        if !frameworks.contains(&framework) {
            frameworks.push(framework);
        }
    }
    frameworks.sort();
    let is_js = std::path::Path::new(&rel_posix)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "js" | "ts" | "jsx" | "tsx"));
    let (found_routes, extra_edges) = extract_routes(&source, &rel_posix, is_js);
    let (nodes, mut edges) = collect_extractions(&rel_posix, found_routes);
    edges.extend(extra_edges);
    let routes = nodes.len() as i64;
    let handlers = edges.len() as i64;
    let registration = register_routes(nodes, edges, &rel_posix);
    let mut report = empty_report();
    fill_report(&mut report, routes, handlers, frameworks, registration);
    Ok(report)
}

// ── Unit 21: framework reference-resolution heuristics ──────────────────

// ── Unit 21/46: framework reference-resolution wrappers ─────────────────

/// Mirror of a resolver's `claims_reference` naming convention.
pub fn framework_claims_reference(name: String, framework: String) -> Result<bool, String> {
    core_indexer::frameworks::claims_reference(name, framework)
}

/// Whether a project uses a framework, checked in pure Rust.
pub fn framework_detect(root: String, framework: String) -> Result<bool, String> {
    let valid = core_indexer::frameworks::framework_names();
    if !valid.iter().any(|name| *name == framework) {
        return Err(format!("InvalidRequest: unknown framework `{framework}`"));
    }
    let root = std::path::PathBuf::from(root);
    if detect_root_frameworks(&root)
        .iter()
        .any(|found| found == &framework)
    {
        return Ok(true);
    }
    for file in walk_sources(&root) {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        if detect_frameworks(&source)
            .iter()
            .any(|found| found == &framework)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Pure one-file extraction for a single resolver class.
pub fn framework_extract(
    file_path: String,
    source: String,
    framework: String,
) -> Result<Value, String> {
    let valid = core_indexer::frameworks::framework_names();
    if !valid.iter().any(|name| *name == framework) {
        return Err(format!("InvalidRequest: unknown framework `{framework}`"));
    }
    let is_js = std::path::Path::new(&file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "js" | "ts" | "jsx" | "tsx"));
    let (routes, extra_edges) = extract_routes(&source, &file_path, is_js);
    let (nodes, edges) = collect_extractions(
        &file_path,
        routes
            .into_iter()
            .filter(|(_, _, _, found)| found == &framework)
            .collect(),
    );
    let metadata = |found: &str| {
        json!({
            "synthesized_by": found,
            "framework": found,
        })
    };
    let node_rows: Vec<Value> = nodes
        .into_iter()
        .map(|(id, name, file, handler, methods, found)| {
            let mut meta = metadata(&found);
            if let Some(map) = meta.as_object_mut() {
                map.insert("pattern".into(), json!(name));
                map.insert("handler".into(), json!(handler));
                map.insert("methods".into(), json!(methods));
            }
            json!({
                "id": id,
                "name": name,
                "kind": "route",
                "file_path": file,
                "metadata": meta,
            })
        })
        .collect();
    let mut edge_rows: Vec<Value> = edges
        .into_iter()
        .map(|(source_id, target_id, kind)| {
            json!({
                "source_id": source_id,
                "target_id": target_id,
                "kind": kind.rsplit(':').next().unwrap_or(&kind),
                "metadata": metadata(&framework),
            })
        })
        .collect();
    let prefix = format!("synthetic:{framework}:");
    for (source_id, target_id, kind) in extra_edges {
        if !kind.starts_with(&prefix) {
            continue;
        }
        edge_rows.push(json!({
            "source_id": source_id,
            "target_id": target_id,
            "kind": kind.strip_prefix(&prefix).unwrap_or(&kind),
            "metadata": metadata(&framework),
        }));
    }
    Ok(json!({
        "file_path": file_path,
        "nodes": node_rows,
        "edges": edge_rows,
    }))
}

/// Resolve one framework's naming convention against supplied candidates.
pub fn framework_resolve_one(
    name: String,
    framework: String,
    candidates: Vec<Value>,
) -> Result<Value, String> {
    Ok(
        core_indexer::frameworks::resolve_one(&name, &framework, &candidates)
            .unwrap_or(Value::Null),
    )
}
pub fn framework_candidates_op(query: String, limit: i64) -> Result<Vec<Value>, String> {
    require_index()?;
    let snap = graph()?;
    core_indexer::api::search(&snap, &query, limit.clamp(1, 20) as usize, None)
}

/// Mirror of `resolution.resolve_reference` with search results supplied as
/// Mirror of `resolution.resolve_reference` with search results supplied as
/// data (the Python original takes a callable searcher).
pub fn framework_resolve_reference(
    name: String,
    candidates: Vec<Value>,
    limit: i64,
) -> Result<Value, String> {
    core_indexer::frameworks::resolve_reference(name, candidates, limit)
}

/// Mirror of `resolution.resolve_route` over supplied route/handler rows.
pub fn framework_resolve_route(
    path: String,
    route_candidates: Vec<Value>,
    callees: Vec<Value>,
    limit: i64,
) -> Result<Value, String> {
    core_indexer::frameworks::resolve_route(path, route_candidates, callees, limit)
}

/// Ops-shaped `status`: what is indexed and how fresh the store is.
pub fn status_op(root: Option<String>) -> Result<Value, String> {
    let root_path = root
        .map(std::path::PathBuf::from)
        .or_else(crate::state::root)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let config = root_path.join(".coderadar.toml");
    let store_dir = root_path.join(".coderadar").join("store");
    let db = store_dir.join("coderadar.db");
    let loaded = crate::state::projection().is_some();
    let modified = std::fs::metadata(&db).ok().and_then(|m| m.modified().ok());
    let age_seconds = modified
        .and_then(|instant| std::time::SystemTime::now().duration_since(instant).ok())
        .map(|d| d.as_secs_f64());
    let store_fresh = modified.map(|stamp| !has_newer_source(&root_path, stamp));
    let stats = crate::state::projection()
        .map(|snap| core_indexer::api::stats_full(&snap))
        .unwrap_or(Value::Null);
    Ok(json!({
        "root": root_path.to_string_lossy(),
        "config": if config.is_file() { Some(config.to_string_lossy().to_string()) } else { None },
        "store": if store_dir.is_dir() { Some(store_dir.to_string_lossy().to_string()) } else { None },
        "loaded": loaded,
        "stats": stats,
        "age_seconds": age_seconds,
        "store_fresh": store_fresh,
    }))
}

fn has_newer_source(root: &std::path::Path, stamp: std::time::SystemTime) -> bool {
    fn walk(dir: &std::path::Path, stamp: std::time::SystemTime) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if matches!(name, ".git" | ".coderadar" | "target" | "__pycache__") {
                    continue;
                }
                if walk(&path, stamp) {
                    return true;
                }
            } else if path.is_file()
                && path
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .is_some_and(|m| m > stamp)
            {
                return true;
            }
        }
        false
    }
    walk(root, stamp)
}

/// Ops-shaped store rebuild: move the old store aside, replay git history,
/// restore annotations, and optionally embed the final state.
///
/// # Errors
/// Refuses when a graph is loaded, when the repository has no usable
/// lineage selection, or when any replay/read-back step fails.
pub fn rebuild_op(
    repo: String,
    store_path: Option<String>,
    branches: Option<Vec<String>>,
    parent_ref: String,
    export_path: Option<String>,
    on_conflict: String,
    embed: bool,
) -> Result<Value, String> {
    use std::collections::BTreeMap;
    use std::time::Instant;

    if repo.trim().is_empty() {
        return Err("InvalidRequest: Please provide a git repository path.".to_string());
    }
    if crate::state::projection().is_some() {
        return Err(
            "InvalidRequest: a graph is loaded in this process, and it holds the store this rebuild moves; run the rebuild in a fresh process".to_string(),
        );
    }
    if on_conflict != "error" && on_conflict != "skip" {
        return Err(format!(
            "InvalidRequest: on_conflict={on_conflict:?}: expected 'error' or 'skip'"
        ));
    }

    let started = Instant::now();
    let repo_path = std::path::Path::new(&repo);
    let repo = core_indexer::fs::strip_verbatim_prefix(
        std::fs::canonicalize(repo_path)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(repo_path)),
    )
    .to_string_lossy()
    .into_owned();
    let store = store_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(&repo).join(".coderadar/store/coderadar.db"));
    if let Some(parent) = store.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("InvalidRequest: cannot create {}: {e}", parent.display()))?;
    }

    let backup = if store.exists() {
        let now = core_indexer::storage::now_iso8601();
        let head: String = now.chars().take(19).collect();
        let stamp = format!("{}Z", head.replace(['-', ':'], ""));
        let mut candidate = store.with_file_name(format!(
            "{}.pre-rebuild-{}",
            store
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("coderadar.db"),
            stamp
        ));
        let mut n = 1;
        while candidate.exists() {
            n += 1;
            candidate = store.with_file_name(format!(
                "{}.pre-rebuild-{}-{}",
                store
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("coderadar.db"),
                stamp,
                n
            ));
        }
        std::fs::rename(&store, &candidate).map_err(|e| {
            format!(
                "InvalidRequest: cannot move {} aside ({e}): a process holds it open",
                store.display()
            )
        })?;
        for suffix in ["-wal", "-shm"] {
            let name = format!(
                "{}{}",
                store
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("coderadar.db"),
                suffix
            );
            let side = store.with_file_name(name);
            if side.exists() {
                let target = candidate.with_file_name(format!(
                    "{}{}",
                    candidate
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("backup"),
                    suffix
                ));
                let _ = std::fs::rename(side, target);
            }
        }
        Some(candidate)
    } else {
        None
    };

    let export_path = export_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            store.with_file_name(format!(
                "{}.annotations.json",
                store
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("coderadar.db")
            ))
        });
    let mut export: Option<Value> = None;
    if let Some(backup) = &backup {
        let opened = core_indexer::storage::CodeGraphStore::open(backup.to_string_lossy().as_ref())
            .map_err(|e| format!("EngineError: {e:?}"))?;
        let document = core_indexer::annotations::export_all(&opened)
            .map_err(|e| format!("EngineError: {e:?}"))?;
        drop(opened);
        let value = serde_json::to_value(document).map_err(|e| format!("EngineError: {e}"))?;
        let raw = serde_json::to_string(&value).map_err(|e| format!("EngineError: {e}"))?;
        let rows = value
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let versions = rows
            .iter()
            .map(|row| {
                row.get("versions")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            })
            .sum::<usize>();
        if let Some(parent) = export_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        std::fs::write(&export_path, &raw).map_err(|e| e.to_string())?;
        export = Some(json!({
            "format": value.get("format").cloned().unwrap_or(Value::Null),
            "exported_at": value.get("exported_at").cloned().unwrap_or(Value::Null),
            "rows": rows.len(),
            "versions": versions,
            "path": export_path.to_string_lossy(),
        }));
    }

    let listed = core_indexer::api::list_branches(&repo)?;
    let plan = if listed.iter().any(|name| name == "main") {
        core_indexer::api::branch_plan(&repo, &parent_ref)?
    } else {
        Vec::new()
    };
    let mut addresses: BTreeMap<String, String> = BTreeMap::new();
    let ordered: Vec<String> = match branches {
        Some(names) => {
            if names.is_empty() {
                return Err("InvalidRequest: branches=[] is empty: nothing to build.".to_string());
            }
            let mut result = names;
            if listed.iter().any(|name| name == "main") && !result.iter().any(|name| name == "main")
            {
                result.insert(0, "main".to_string());
            }
            for entry in &plan {
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let tip = entry.get("tip").and_then(Value::as_str).unwrap_or_default();
                if !result.iter().any(|value| value == name) && !tip.is_empty() {
                    addresses.insert(name.to_string(), tip.to_string());
                }
            }
            result
        }
        None => {
            if !listed.iter().any(|name| name == "main") {
                return Err(format!(
                    "InvalidRequest: repository has no 'main' branch: pass --branches explicitly (local branches: {}).",
                    if listed.is_empty() {
                        "(none)".to_string()
                    } else {
                        listed.join(", ")
                    }
                ));
            }
            let mut chosen: BTreeMap<String, String> = BTreeMap::new();
            let mut sorted = plan.clone();
            sorted.sort_by(|a, b| {
                let an = a.get("name").and_then(Value::as_str).unwrap_or_default();
                let bn = b.get("name").and_then(Value::as_str).unwrap_or_default();
                (an.matches('/').count(), an).cmp(&(bn.matches('/').count(), bn))
            });
            for entry in sorted {
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let tip = entry.get("tip").and_then(Value::as_str).unwrap_or_default();
                if name == "main" || tip.is_empty() {
                    continue;
                }
                if entry
                    .get("merged")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || chosen.contains_key(tip)
                {
                    addresses.insert(name.to_string(), tip.to_string());
                    continue;
                }
                chosen.insert(tip.to_string(), name.to_string());
            }
            let mut result = vec!["main".to_string()];
            result.extend(chosen.into_values());
            result
        }
    };

    let store_s = store.to_string_lossy().into_owned();
    let mut branch_reports = serde_json::Map::new();
    for branch in &ordered {
        let report = ingest_history(
            repo.clone(),
            branch.clone(),
            None,
            false,
            None,
            parent_ref.clone(),
            None,
            Some(store_s.clone()),
        )
        .map_err(|e| {
            format!(
                "{e} — the old store is at {:?}{}",
                backup.as_ref().map(|p| p.to_string_lossy().into_owned()),
                export
                    .as_ref()
                    .and_then(|v| v.get("path"))
                    .and_then(Value::as_str)
                    .map(|p| format!(" and the export at {p:?}"))
                    .unwrap_or_default()
            )
        })?;
        branch_reports.insert(branch.clone(), report);
    }

    let address_entries: Vec<(String, String)> = addresses
        .iter()
        .map(|(name, tip)| (name.clone(), tip.clone()))
        .collect();
    let addresses_recorded = if address_entries.is_empty() {
        0
    } else {
        core_indexer::api::record_branch_addresses(&store_s, address_entries)?
    };
    let merge_heal = reingest_merged_commits(
        repo.clone(),
        "main".to_string(),
        false,
        Some(store_s.clone()),
    )?;

    let mut imported: Option<Value> = None;
    if let Some(export) = &export {
        let payload = std::fs::read_to_string(
            export
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        let opened = core_indexer::storage::CodeGraphStore::open(store.to_string_lossy().as_ref())
            .map_err(|e| format!("EngineError: {e:?}"))?;
        let report = core_indexer::annotations::import_all(&opened, &payload, &on_conflict)
            .map_err(|e| format!("EngineError: {e:?}"))?;
        drop(opened);
        imported = Some(serde_json::to_value(report).map_err(|e| format!("EngineError: {e}"))?);
    }
    let verified = imported.as_ref().is_some_and(|value| {
        value.get("verified").and_then(Value::as_u64)
            == value.get("imported").and_then(Value::as_u64)
    });
    if imported.is_some() && !verified {
        return Err(format!(
            "EngineError: the import did not verify its own rows: the rebuilt store is incomplete; the old store is at {:?}",
            backup.as_ref().map(|p| p.to_string_lossy().into_owned())
        ));
    }

    let mut embeddings = json!({"embedded": false});
    if embed {
        crate::state::load_from(&store, std::path::Path::new(&repo))?;
        let report = compute_embeddings_op(None, 32, false)?;
        embeddings = json!({"embedded": true, "report": report});
        crate::state::clear();
    }

    let commits_ingested: u64 = branch_reports
        .values()
        .filter_map(|v| v.get("commits_ingested"))
        .filter_map(Value::as_u64)
        .sum();
    let commits_skipped: usize = branch_reports
        .values()
        .filter_map(|v| v.get("commits_skipped"))
        .filter_map(Value::as_array)
        .map(Vec::len)
        .sum();
    let files_failed: usize = branch_reports
        .values()
        .filter_map(|v| v.get("files_failed"))
        .filter_map(Value::as_array)
        .map(Vec::len)
        .sum();

    Ok(json!({
        "repo": repo,
        "store": store_s,
        "backup": backup.map(|p| p.to_string_lossy().into_owned()),
        "export": export,
        "parent_ref": parent_ref,
        "branches": ordered,
        "branch_reports": branch_reports,
        "addresses": addresses,
        "addresses_recorded": addresses_recorded,
        "commits_ingested": commits_ingested,
        "commits_skipped": commits_skipped,
        "files_failed": files_failed,
        "merge_heal": merge_heal,
        "imported": imported,
        "verified": verified,
        "embeddings": embeddings,
        "elapsed_secs": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
    }))
}

/// Embed texts with the activated model, for parity checks and callers that
/// need vectors directly.
///
/// # Errors
/// Returns the engine message when the model cannot load or the vectors have
/// the wrong width.
pub fn embed_texts_op(texts: Vec<String>, model: Option<String>) -> Result<Value, String> {
    let (configured, dimension) = embedding_config();
    let vectors = core_indexer::api::embed_texts_checked(
        &model.unwrap_or(configured),
        &texts,
        embedding_batch_size(),
        dimension,
        deterministic_embeddings(),
    )?;
    serde_json::to_value(vectors).map_err(|e| format!("EngineError: {e}"))
}

/// The activated config as JSON, for thin facade helpers.
pub fn config_op() -> Value {
    core_indexer::api::active_config_json()
}

/// Fold a façade batch of `update_file` calls into one engine report.
///
/// # Errors
/// Returns the engine message when a length or update error occurs.
pub fn batch_update_op(files: Vec<String>, contents: Vec<Option<String>>) -> Result<Value, String> {
    core_indexer::api::batch_update(&files, &contents)
}

/// Poll watched paths once from a previously returned `stamps` object.
pub fn watch_changes_op(
    paths: Vec<String>,
    previous: Value,
    max_bytes: Option<i64>,
) -> Result<Value, String> {
    let limit = max_bytes
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(1_048_576);
    Ok(core_indexer::api::watch_changes(&paths, &previous, limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_ids_strip_prefixes_and_separators() {
        assert_eq!(
            friendly_entity_id(".\\a\\b.py::f".into()).unwrap(),
            "a/b.py::f"
        );
        assert_eq!(friendly_entity_id("./a.py::f".into()).unwrap(), "a.py::f");
    }

    #[test]
    fn spelling_normalization_matches_language_notation() {
        assert_eq!(
            normalize_query_spelling("GenServer:handle_call/3 spawn/1".into()).unwrap(),
            "GenServer.handle_call spawn"
        );
        assert_eq!(
            normalize_query_spelling("kind:function lang:python User".into()).unwrap(),
            "kind:function lang:python User"
        );
    }

    #[test]
    fn parse_names_splits_and_filters() {
        assert_eq!(
            parse_names("{name} has {count} items".into(), None).unwrap(),
            vec!["{name}", "has", "{count}", "items"]
        );
        assert_eq!(parse_names("a".into(), None).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn display_file_derives_from_id() {
        let d = json!({"id": "a.py::f"});
        assert_eq!(display_file(d).unwrap(), "a.py");
    }

    #[test]
    fn short_suggestion_hints_never_touch_the_backend() {
        assert!(suggest_entities("a".into(), 3).unwrap().is_empty());
    }
    #[test]
    fn framework_claims_follow_resolver_conventions() {
        assert!(framework_claims_reference("UserModel".into(), "django".into()).unwrap());
        assert!(framework_claims_reference("UserController".into(), "aspnet".into()).unwrap());
        assert!(!framework_claims_reference("userHandler".into(), "django".into()).unwrap());
    }

    #[test]
    fn framework_resolve_prefers_conventional_directory() {
        let candidates = json!([
            {"id": "a.py::UserController", "name": "UserController", "file_path": "src/other/a.py"},
            {"id": "b.py::UserController", "name": "UserController", "file_path": "src/controllers/b.py"},
        ]);
        let rows = framework_resolve_reference(
            "UserController".into(),
            candidates.as_array().cloned().unwrap_or_default(),
            5,
        )
        .unwrap();
        let first = rows.get(0).expect("at least one resolved row");
        assert_eq!(
            first.get("id").and_then(Value::as_str),
            Some("b.py::UserController")
        );
        assert_eq!(first.get("confidence").and_then(Value::as_f64), Some(0.85));
    }

    #[test]
    fn framework_route_resolution_attaches_handler() {
        let route = json!({"id": "app.py::flask:route:/users/<id>", "name": "/users/<id>", "kind": "route"});
        let handler = json!({"id": "app.py::get_user", "name": "get_user", "kind": "function"});
        let rows =
            framework_resolve_route("/users/:id".into(), vec![route.clone()], vec![handler], 5)
                .unwrap();
        let first = rows.get(0).expect("route handler resolved");
        assert_eq!(
            first.get("resolved_by").and_then(Value::as_str),
            Some("route-resolution")
        );
        assert_eq!(first.get("route").and_then(Value::as_str), None);
        assert!(first.get("route").is_some());
    }
}
