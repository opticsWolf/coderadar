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
    let mut results: Vec<Value> =
        core_indexer::api::search(&snap, probe, limit.clamp(1, 20) as usize, None)?
            .into_iter()
            .map(|mut hit| {
                if let Value::Object(ref mut map) = hit {
                    map.insert("confidence".to_string(), json!(0.6));
                    map.insert("resolved_by".to_string(), json!("search"));
                }
                hit
            })
            .collect();
    if mode == "route" {
        results.retain(|hit| {
            hit.get("kind").and_then(Value::as_str) == Some("route")
                || hit
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.contains(probe))
        });
    }
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
        let source = read_source(entity);
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

/// Line-numbered source for one entity, read from the indexed root.
fn read_source(entity: &Value) -> Option<String> {
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
        out.push_str(&format!(
            "{}	{}
",
            from + offset + 1,
            line
        ));
    }
    Some(out)
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
}
