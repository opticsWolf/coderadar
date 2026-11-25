#![expect(
    dead_code,
    reason = "ported renderer kept complete even where a CLI path does not call it"
)]
#![expect(
    clippy::collapsible_if,
    reason = "explicit branches mirror the Python renderer"
)]
#![expect(
    clippy::trim_split_whitespace,
    reason = "token splitting matches Python re.split"
)]
#![expect(clippy::useless_format, reason = "kept for template symmetry")]
//! Exact pure renderers recorded from `py_agent/src/coderadar/render.py`.
//! These are the gated counterparts of the normalized CLI renderers in
//! `render.rs`; they never touch the graph.

use serde_json::{Map, Value};

pub fn py_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
        _ => true,
    }
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("'{s}'"),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("'{k}': {}", py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

fn unwrap_object(value: &Value) -> &Value {
    if let Some(map) = value.as_object() {
        if let Some(attrs) = map.get("attrs") {
            let known = map
                .get("$object")
                .and_then(Value::as_str)
                .is_some_and(|name| name.starts_with("Mutation"));
            if known && attrs.is_object() {
                return attrs;
            }
        }
    }
    value
}

fn object(value: &Value) -> Result<&Map<String, Value>, String> {
    unwrap_object(value).as_object().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not subscriptable",
            py_type(value)
        )
    })
}

fn field<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a Value, String> {
    map.get(key).ok_or_else(|| format!("KeyError: '{key}'"))
}

fn get_field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    match value {
        Value::Object(map) => field(map, key),
        other => Err(format!(
            "TypeError: '{}' object is not subscriptable",
            py_type(other)
        )),
    }
}

fn optional_field<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).filter(|v| !v.is_null())
}

fn value_string(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        other => Err(format!(
            "TypeError: expected string, got '{}'",
            py_type(other)
        )),
    }
}

fn value_string_field(map: &Map<String, Value>, key: &str) -> Result<String, String> {
    value_string(field(map, key)?)
}

fn value_i64(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::Bool(b) => i64::from(*b),
        _ => 0,
    }
}

fn array<'a>(value: &'a Value, key: &str) -> Result<Vec<&'a Value>, String> {
    match get_field(value, key)? {
        Value::Array(items) => Ok(items.iter().collect()),
        other => Err(format!(
            "TypeError: '{}' object is not iterable",
            py_type(other)
        )),
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn pad_right(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        text.to_string()
    } else {
        format!("{}{}", text, " ".repeat(width - len))
    }
}

fn pad_left(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        text.to_string()
    } else {
        format!("{}{}", " ".repeat(width - len), text)
    }
}

/// `not_found` (`render.py:18`).
pub fn not_found(
    entity_id: String,
    candidates: Vec<Value>,
    hint: String,
) -> Result<String, String> {
    let mut lines = vec![format!("Entity `{entity_id}` not found.")];
    if candidates.is_empty() {
        lines.push(format!("Try {hint} to locate it."));
        return Ok(lines.join("\n"));
    }
    lines.push("Did you mean:".to_string());
    for hit in candidates.iter().take(3) {
        let map = hit.as_object().ok_or_else(|| {
            format!(
                "AttributeError: '{}' object has no attribute 'get'",
                py_type(hit)
            )
        })?;
        lines.push(format!(
            "- `{}` — `{}`",
            py_str(map.get("name").unwrap_or(&Value::Null)),
            py_str(map.get("id").unwrap_or(&Value::Null))
        ));
    }
    Ok(lines.join("\n"))
}

/// `search_miss` (`render.py:30`).
pub fn search_miss(
    query: String,
    kind: Option<String>,
    search_similar: String,
    explore: String,
) -> Result<String, String> {
    let kind = kind.unwrap_or_default();
    let tokens: Vec<&str> = query
        .trim()
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .collect();
    let suffix = if kind.is_empty() {
        String::new()
    } else {
        format!(" (kind: {kind})")
    };
    let mut out = vec![format!("No results found for '{query}'{suffix}.")];
    if tokens.len() >= 2 {
        let shown = tokens
            .iter()
            .take(8)
            .map(|t| format!("`{t}`"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "Tokens are matched independently (OR) — none of {shown} occurs in any indexed entity's name, signature or docstring."
        ));
        out.push("Try a single token by itself, one that you expect as an identifier.".to_string());
    } else {
        out.push("The token was matched against entity names, function signatures and docstrings and hit nothing.".to_string());
    }
    out.push(format!(
        "Escape hatches: {search_similar} (semantic, embedding-based) or {explore} with explicit `symbols` for known names."
    ));
    Ok(out.join("\n"))
}

/// `diagnose` (`render.py:253`).
pub fn diagnose(result: Value) -> Result<String, String> {
    let map = result.as_object().ok_or_else(|| {
        format!(
            "AttributeError: '{}' object has no attribute 'get'",
            py_type(&result)
        )
    })?;
    let mut lines = vec!["Unresolved references:".to_string()];
    let unresolved = optional_field(map, "unresolved").and_then(Value::as_array);
    match unresolved {
        Some(rows) if rows.is_empty() => lines.push("  none".to_string()),
        None => lines.push("  none".to_string()),
        Some(rows) => {
            for row in rows {
                let row_map = row.as_object().ok_or_else(|| {
                    format!("TypeError: '{}' object is not subscriptable", py_type(row))
                })?;
                lines.push(format!(
                    "  `{}` → {}",
                    py_str(field(row_map, "id")?),
                    py_str(field(row_map, "targets")?)
                ));
            }
            lines.push(format!(
                "{} function(s) call targets the graph cannot follow",
                rows.len()
            ));
        }
    }
    lines.push("Ambiguous base classes:".to_string());
    let details = optional_field(map, "ambiguous_bases").and_then(Value::as_array);
    match details {
        Some(rows) if rows.is_empty() => lines.push("  none".to_string()),
        None => lines.push("  none".to_string()),
        Some(rows) => {
            for detail in rows {
                lines.push(format!("  {}", py_str(detail)));
            }
            let count = optional_field(map, "ambiguous_base_count")
                .map(value_i64)
                .unwrap_or(rows.len() as i64);
            lines.push(format!("{count} ambiguous"));
        }
    }
    Ok(lines.join("\n"))
}

/// `store_repair` (`render.py:280`).
pub fn store_repair(db_path: String, result: Value) -> Result<String, String> {
    if !result.is_object() && !result.is_null() && !result.is_array() && !result.is_string() {
        return Err(format!(
            "TypeError: argument of type '{}' is not iterable",
            py_type(&result)
        ));
    }
    let map = match &result {
        Value::Object(map) => map,
        _ => {
            return Err(format!(
                "TypeError: '{}' object is not subscriptable",
                py_type(&result)
            ));
        }
    };
    if let Some(missing) = map.get("missing") {
        return Ok(format!(
            "No store at {} — nothing to repair.",
            py_str(missing)
        ));
    }
    if let Some(deleted) = map.get("deleted") {
        return Ok(format!(
            "OK  Deleted {}; next analyze rebuilds from scratch.",
            py_str(deleted)
        ));
    }
    let mut lines = vec![
        format!("Store repair: {db_path}"),
        format!(
            "  v1 leftovers found:   {}",
            py_str(optional_field(map, "v1_found").unwrap_or(&Value::from(0)))
        ),
        format!(
            "  v1 leftovers retired: {}",
            py_str(optional_field(map, "v1_retired").unwrap_or(&Value::from(0)))
        ),
        format!(
            "  edges retired:        {}",
            py_str(optional_field(map, "edges_retired").unwrap_or(&Value::from(0)))
        ),
        format!(
            "  unreadable rows:      {}",
            py_str(optional_field(map, "unreadable_live").unwrap_or(&Value::from(0)))
        ),
    ];
    let legacy = optional_field(map, "legacy_ids")
        .map(value_i64)
        .unwrap_or(0);
    if legacy != 0 {
        lines.push(format!("  pre-0.10 id spellings: {legacy}"));
    }
    let unreadable = optional_field(map, "unreadable_live")
        .map(value_i64)
        .unwrap_or(0);
    if unreadable != 0 {
        lines.push(
            "Unreadable rows cannot be retired — if cold load still fails, re-run with --delete."
                .to_string(),
        );
    } else if legacy != 0 {
        lines.push(format!(
            "{legacy} concept id(s) predate the 0.10 canonical form (forward slashes, no './' prefix) — the store will not load until re-analyzed. Run `coderadar reindex --full` (or let cold start do it) to re-key them."
        ));
    } else if optional_field(map, "v1_found").map(value_i64).unwrap_or(0) == 0 {
        lines.push("OK  Store is clean — nothing to retire.".to_string());
    } else {
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

/// `stale_banner` (`render.py:435`).
pub fn stale_banner(stale_files: Value, referenced_paths: Vec<String>) -> Result<String, String> {
    let falsy = match &stale_files {
        Value::Null => true,
        Value::Bool(false) => true,
        Value::Number(n) => n.as_i64() == Some(0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    };
    if falsy {
        return Ok(String::new());
    }
    let rows = stale_files.as_array().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not iterable",
            py_type(&stale_files)
        )
    })?;
    let referenced: Vec<&str> = referenced_paths.iter().map(String::as_str).collect();
    let mut relevant = Vec::new();
    for file in rows {
        let map = file
            .as_object()
            .ok_or_else(|| format!("TypeError: '{}' object is not subscriptable", py_type(file)))?;
        let path = field(map, "path")?;
        let path = path
            .as_str()
            .ok_or_else(|| format!("TypeError: unhashable type: '{}'", py_type(path)))?;
        if referenced.contains(&path) {
            relevant.push(path.to_string());
        }
    }
    if relevant.is_empty() {
        return Ok(String::new());
    }
    let mut lines = vec![
        "⚠️ Some files referenced below were edited since the last index sync — their codegraph entries may be stale:".to_string(),
    ];
    for path in relevant {
        lines.push(format!("  - {path}"));
    }
    lines.push(
        "For accurate content of those specific files, Read them directly. Every file NOT listed above is fresh — still trust codegraph.".to_string(),
    );
    lines.push(String::new());
    Ok(lines.join("\n"))
}

/// `explore_usage` (`render.py:477`).
pub fn explore_usage(example: Option<String>) -> Result<String, String> {
    let example = example
        .unwrap_or_else(|| "coderadar_explore(query=\"User.save authenticate\")".to_string());
    Ok(format!(
        "Please provide symbol names or a question to explore. For example: {example}"
    ))
}

/// `explore_miss` (`render.py:484`).
pub fn explore_miss(
    names: Vec<String>,
    search: String,
    search_similar: String,
) -> Result<String, String> {
    let list = names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "Couldn't find {list} in the index. Each name was matched exactly, then as search tokens, against names, signatures and docstrings. Try a single well-known symbol, {search} with one token, or {search_similar} for semantic search."
    ))
}

const MAX_OUTPUT_CHARS: usize = 18_000;
const MAX_CHARS_PER_FILE: usize = 4_500;
const POINTER_HEADER: &str = "**Not shown above — explore these names for their source**";

/// `extract_path_from_header` (`render.py:639`).
pub fn extract_path_from_header(header: String) -> Result<String, String> {
    let rest = header.strip_prefix("**").unwrap_or(&header);
    Ok(rest.split("**").next().unwrap_or("").trim().to_string())
}

/// `trim_to_char_budget` (`render.py:644`).
pub fn trim_to_char_budget(body_lines: Vec<String>, max_chars: i64) -> Result<Vec<String>, String> {
    if max_chars <= 0 {
        return Ok(Vec::new());
    }
    let budget = max_chars as usize;
    let mut result = Vec::new();
    let mut used = 0usize;
    let total = body_lines.len();
    for line in body_lines {
        let cost = line.chars().count() + 1;
        if used + cost > budget {
            break;
        }
        result.push(line);
        used += cost;
    }
    if result.len() < total {
        result.push("...".to_string());
    }
    Ok(result)
}

/// `apply_output_budget` (`render.py:530`).
pub fn apply_output_budget(
    lines: Vec<String>,
    max_chars: Option<i64>,
    max_per_file: Option<i64>,
) -> Result<String, String> {
    let max_chars = if max_chars.is_none() && max_per_file.is_some() {
        return Err(
            "TypeError: '<=' not supported between instances of 'int' and 'NoneType'".to_string(),
        );
    } else {
        i128::from(max_chars.unwrap_or(MAX_OUTPUT_CHARS as i64))
    };
    let max_per_file = max_per_file.unwrap_or(MAX_CHARS_PER_FILE as i64).max(0) as usize;
    let text = lines.join("\n");
    if text.chars().count() as i128 <= max_chars {
        return Ok(text);
    }
    let mut file_sections: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut relationships: Vec<String> = Vec::new();
    let mut in_relationships = false;
    for line in lines {
        if line.starts_with("## Relationships") {
            in_relationships = true;
            if !current.is_empty() {
                file_sections.push(std::mem::take(&mut current));
            }
        }
        if in_relationships {
            relationships.push(line);
            continue;
        }
        if line.starts_with("**") && line.contains("** —") {
            if !current.is_empty() {
                file_sections.push(std::mem::take(&mut current));
            }
            current.push(line);
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        file_sections.push(current);
    }
    let mut preamble: Vec<String> = Vec::new();
    let mut sections: Vec<Vec<String>> = Vec::new();
    for section in file_sections {
        if section
            .first()
            .is_some_and(|h| h.starts_with("**") && h.contains("** —"))
        {
            sections.push(section);
        } else {
            preamble = section;
        }
    }
    let mut output = preamble;
    let mut remaining = max_chars - output.join("\n").chars().count() as i128;
    if !relationships.is_empty() {
        remaining -= relationships.join("\n").chars().count() as i128 + 2;
    }
    if remaining <= 0 {
        let mut fallback = vec![POINTER_HEADER.to_string(), String::new()];
        fallback.extend(relationships);
        return Ok(fallback.join("\n"));
    }
    let mut pointers = Vec::new();
    for section in sections {
        let sec_text = section.join("\n");
        if sec_text.chars().count() <= max_per_file {
            if sec_text.chars().count() as i128 <= remaining {
                output.extend(section);
                remaining -= sec_text.chars().count() as i128;
            } else if let Some(header) = section.first() {
                pointers.push(extract_path_from_header(header.clone())?);
            }
        } else if let Some(header) = section.first() {
            let header_len = header.chars().count();
            let body = trim_to_char_budget(
                section[1..].to_vec(),
                (max_per_file.saturating_sub(header_len + 1)) as i64,
            )?;
            let mut trimmed = vec![header.clone()];
            trimmed.extend(body);
            let trimmed_text = trimmed.join("\n");
            if trimmed_text.chars().count() as i128 <= remaining {
                output.extend(trimmed);
                remaining -= trimmed_text.chars().count() as i128;
            } else {
                pointers.push(extract_path_from_header(header.clone())?);
            }
        }
    }
    if !pointers.is_empty() {
        output.push(String::new());
        output.push(POINTER_HEADER.to_string());
        for pointer in pointers {
            output.push(format!("- {pointer}"));
        }
        output.push(String::new());
    }
    if !relationships.is_empty() {
        output.push(String::new());
        output.extend(relationships);
    }
    Ok(output.join("\n"))
}

/// `query_usage` (`render.py:708`).
pub fn query_usage() -> Result<String, String> {
    Ok([
        "Please provide a query. Examples:",
        "  - classes where inherits_from contains 'BaseModel'",
        "  - methods where is_async == true",
        "  - functions where name starts_with 'test_'",
        "  - imports where import_kind == 'from'",
        "Entities: modules, classes, functions, methods, constants, entities, imports, calls, fields. Full field reference: coderadar docs query-language",
    ]
    .join("\n"))
}

/// `fastembed_missing` (`render.py:783`).
pub fn fastembed_missing(compute: String) -> Result<String, String> {
    Ok(format!(
        "Semantic search requires `fastembed` to be installed. Run: pip install fastembed\nThen run {compute} to index all entities."
    ))
}

/// `no_embeddings` (`render.py:793`).
pub fn no_embeddings(compute: String, reindex: String) -> Result<String, String> {
    Ok(format!(
        "No embeddings found and auto-computation failed. Run {compute} first, or {reindex}."
    ))
}

fn why_lines(notes: &[Value], indent: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for note in notes {
        let map = note.as_object().ok_or_else(|| {
            format!(
                "AttributeError: '{}' object has no attribute 'get'",
                py_type(note)
            )
        })?;
        let body = map.get("body").and_then(Value::as_str).unwrap_or("").trim();
        let first = body.lines().next().unwrap_or("(empty)");
        let head = if first.chars().count() > 140 {
            format!("{}...", truncate_chars(first, 140))
        } else {
            first.to_string()
        };
        let mark = if map.get("kind").and_then(Value::as_str) == Some("annotation") {
            format!(
                " ({} open)",
                py_str(map.get("open").unwrap_or(&Value::from(0)))
            )
        } else {
            String::new()
        };
        out.push(format!(
            "{indent}- **Why:** `{id}`{mark} — {head}",
            id = py_str(map.get("id").unwrap_or(&Value::from("?")))
        ));
    }
    Ok(out)
}

/// `search_similar` (`render.py:744`).
pub fn search_similar(query: String, results: Vec<Value>) -> Result<String, String> {
    if results.is_empty() {
        return Ok(format!(
            "No semantically similar results found for '{query}'."
        ));
    }
    let mut lines = vec![
        format!("## Semantic Search: `{query}`"),
        format!("Found {} result(s)", results.len()),
        String::new(),
    ];
    for (index, row) in results.iter().enumerate() {
        let map = row.as_object().ok_or_else(|| {
            format!(
                "AttributeError: '{}' object has no attribute 'get'",
                py_type(row)
            )
        })?;
        let name = map.get("name").and_then(Value::as_str).unwrap_or("?");
        let kind = map.get("kind").and_then(Value::as_str).unwrap_or("?");
        let similarity = map.get("similarity").and_then(Value::as_f64).unwrap_or(0.0);
        lines.push(format!(
            "{}. `{name}` ({kind}) — similarity {similarity:.3}",
            index + 1
        ));
        let file = map.get("file_path").and_then(Value::as_str).unwrap_or("?");
        lines.push(format!("   File: `{file}`"));
        if let Some(doc) = map.get("docstring").and_then(Value::as_str) {
            if !doc.is_empty() {
                let short = truncate_chars(doc, 120);
                let suffix = if doc.chars().count() > 120 { "..." } else { "" };
                lines.push(format!("   {short}{suffix}"));
            }
        }
        if let Some(Value::Array(notes)) = map.get("notes") {
            lines.extend(why_lines(notes, "   ")?);
        }
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

/// `search_hybrid` (`render.py:764`).
pub fn search_hybrid(query: String, results: Value) -> Result<String, String> {
    let rows = match &results {
        Value::Null | Value::Bool(false) => &[][..],
        Value::Number(n) if n.as_i64() == Some(0) => &[][..],
        Value::String(s) if s.is_empty() => &[][..],
        Value::Array(a) if a.is_empty() => &[][..],
        Value::Object(m) if m.is_empty() => &[][..],
        Value::Array(a) => a.as_slice(),
        other => {
            return Err(format!(
                "AttributeError: '{}' object has no attribute 'get'",
                py_type(other)
            ));
        }
    };
    if rows.is_empty() {
        return Ok(format!("No hybrid results found for '{query}'."));
    }
    let mut lines = vec![
        format!("## Hybrid Search: `{query}`"),
        format!("Found {} result(s)", rows.len()),
        String::new(),
    ];
    for (index, row) in rows.iter().enumerate() {
        let map = row.as_object().ok_or_else(|| {
            format!(
                "AttributeError: '{}' object has no attribute 'get'",
                py_type(row)
            )
        })?;
        let name = map
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| map.get("id").and_then(Value::as_str))
            .unwrap_or("?");
        let kind = map.get("kind").and_then(Value::as_str).unwrap_or("?");
        let score = map.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let mut arms = Vec::new();
        if let Some(rank) = map.get("vector_rank") {
            if !rank.is_null() {
                arms.push(format!("vector #{}", py_str(rank)));
            }
        }
        if let Some(rank) = map.get("keyword_rank") {
            if !rank.is_null() {
                arms.push(format!("keyword #{}", py_str(rank)));
            }
        }
        lines.push(format!(
            "{}. `{name}` ({kind}) — RRF {score:.4} ({})",
            index + 1,
            if arms.is_empty() {
                "no arm".to_string()
            } else {
                arms.join(", ")
            }
        ));
        let file = map.get("file_path").and_then(Value::as_str).unwrap_or("?");
        lines.push(format!("   File: `{file}`"));
        if let Some(Value::Array(notes)) = map.get("notes") {
            lines.extend(why_lines(notes, "   ")?);
        }
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

/// `compute_embeddings` (`render.py:802`).
pub fn compute_embeddings(metrics: Value, search_similar: String) -> Result<String, String> {
    let map = metrics.as_object().ok_or_else(|| {
        format!(
            "AttributeError: '{}' object has no attribute 'get'",
            py_type(&metrics)
        )
    })?;
    let num = |key: &str| py_str(map.get(key).unwrap_or(&Value::from(0)));
    Ok(format!(
        "## Embeddings Complete\n\n- **Generated:** {}\n- **Cached (unchanged):** {}\n- **Total entities:** {}\n- **Errors:** {}\n\nSemantic search ({}) is now available.",
        num("generated"),
        num("cached"),
        num("total"),
        num("errors"),
        search_similar
    ))
}

/// `ingest_status` (`render.py:886`).
pub fn ingest_status(result: Value) -> Result<String, String> {
    let map = result.as_object().ok_or_else(|| {
        format!(
            "AttributeError: '{}' object has no attribute 'get'",
            py_type(&result)
        )
    })?;
    let fmt = map.get("format").is_some_and(truthy);
    let mut out = vec![
        "## Ingest state".to_string(),
        String::new(),
        format!(
            "format           {}",
            if fmt {
                "commit-time (P5b.1+)"
            } else {
                "analyze-only"
            }
        ),
        String::new(),
    ];
    let rows = map.get("lineages").and_then(Value::as_array);
    if rows.is_none_or(|r| r.is_empty()) {
        out.push("No lineage is recorded: ingest a branch's history first.".to_string());
        return Ok(out.join("\n"));
    }
    for row in rows.map(|r| r.as_slice()).unwrap_or_default() {
        let row = row
            .as_object()
            .ok_or_else(|| format!("TypeError: '{}' object is not subscriptable", py_type(row)))?;
        let commits = row.get("commits").map(value_i64).unwrap_or(0);
        let completions = row.get("completions").map(value_i64).unwrap_or(0);
        let name = row
            .get("name")
            .map(py_str)
            .unwrap_or_else(|| "?".to_string());
        let mut head = format!(
            "{}  gen {}  commits {}/{}",
            name,
            row.get("generation")
                .map(py_str)
                .unwrap_or_else(|| "1".to_string()),
            commits,
            completions
        );
        let address_only = row
            .get("address_only")
            .is_some_and(|v| v.as_bool() == Some(true));
        if address_only {
            let tip = row
                .get("tip")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "?".to_string());
            head.push_str(&format!("  address -> {}", truncate_chars(&tip, 12)));
        } else {
            if let Some(cursor) = row.get("cursor").filter(|v| truthy(v)) {
                head.push_str(&format!("  cursor {}", truncate_chars(&py_str(cursor), 12)));
            }
            if let Some(horizon) = row.get("horizon").filter(|v| truthy(v)) {
                head.push_str(&format!("  horizon {}", py_str(horizon)));
            }
        }
        if let Some(lineage) = row.get("lineage").and_then(Value::as_str) {
            if lineage != name {
                head.push_str(&format!("  (lineage {lineage})"));
            }
        }
        out.push(head);
        if !address_only && completions < commits {
            let lineage = row
                .get("lineage")
                .or_else(|| row.get("name"))
                .map(py_str)
                .unwrap_or_else(|| "?".to_string());
            out.push(format!(
                "  incomplete: {} commit(s) lack a completion mark - run backfill_commit_index for {lineage}",
                commits - completions
            ));
        }
    }
    Ok(out.join("\n"))
}

/// `export_annotations` (`render.py:1012`).
pub fn export_annotations(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let mut out = vec![
        format!("format           {}", py_str(field(map, "format")?)),
        format!("rows             {}", py_str(field(map, "rows")?)),
        format!("versions         {}", py_str(field(map, "versions")?)),
        format!(
            "path             {}",
            map.get("path")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "(stdout)".to_string())
        ),
    ];
    if let Some(kinds) = map.get("kinds").and_then(Value::as_object) {
        if !kinds.is_empty() {
            out.push(format!(
                "kinds            {}",
                kinds
                    .iter()
                    .map(|(k, v)| format!("{k}={}", py_str(v)))
                    .collect::<Vec<_>>()
                    .join("  ")
            ));
        }
    }
    Ok(out.join("\n"))
}

/// `import_annotations` (`render.py:1031`).
pub fn import_annotations(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let mut out = vec![
        format!("rows             {}", py_str(field(map, "rows")?)),
        format!("versions         {}", py_str(field(map, "versions")?)),
        format!("imported         {}", py_str(field(map, "imported")?)),
        format!("verified         {}", py_str(field(map, "verified")?)),
    ];
    if let Some(store) = map.get("store") {
        out.push(format!("store            {}", py_str(store)));
    }
    let skipped = optional_field(map, "skipped").and_then(Value::as_array);
    out.push(format!("skipped          {}", skipped.map_or(0, Vec::len)));
    for id in skipped.map(|s| s.as_slice()).unwrap_or_default() {
        out.push(format!("  {}", py_str(id)));
    }
    Ok(out.join("\n"))
}

/// `rebuild` (`render.py:1048`).
pub fn rebuild(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let mut out = vec![
        format!("rebuilt          {}", py_str(field(map, "repo")?)),
        format!("store            {}", py_str(field(map, "store")?)),
        format!(
            "backup           {}",
            map.get("backup")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "(no store to keep)".to_string())
        ),
        format!(
            "commits          ingested={}  skipped={}  failed_files={}  elapsed={}s",
            py_str(field(map, "commits_ingested")?),
            py_str(field(map, "commits_skipped")?),
            py_str(field(map, "files_failed")?),
            py_str(field(map, "elapsed_secs")?)
        ),
        format!(
            "branches         {}",
            field(map, "branches")?
                .as_array()
                .map(|a| a.iter().map(py_str).collect::<Vec<_>>().join(", "))
                .unwrap_or_default()
        ),
    ];
    if let Some(export) = map.get("export").and_then(Value::as_object) {
        out.push(format!(
            "export           {} row(s) / {} version(s) -> {}",
            py_str(field(export, "rows")?),
            py_str(field(export, "versions")?),
            py_str(field(export, "path")?)
        ));
    }
    if let Some(imported) = map.get("imported").and_then(Value::as_object) {
        let skipped = imported
            .get("skipped")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        out.push(format!(
            "imported         {} row(s), verified {}, skipped {}",
            py_str(field(imported, "imported")?),
            py_str(field(imported, "verified")?),
            skipped
        ));
    }
    let embedded = map
        .get("embeddings")
        .and_then(Value::as_object)
        .and_then(|m| m.get("embedded"))
        .map(value_i64)
        .unwrap_or(0);
    if embedded == 0 {
        out.push(
            "embeddings       skipped (run compute-embeddings to restore vectors)".to_string(),
        );
    }
    Ok(out.join("\n"))
}

/// `open_items` (`render.py:1083`).
pub fn open_items(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let items = field(map, "items")?.as_array().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not iterable",
            py_type(field(map, "items").unwrap_or(&Value::Null))
        )
    })?;
    let scope = map
        .get("scope")
        .filter(|v| !v.is_null())
        .map(py_str)
        .unwrap_or_else(|| "(whole store)".to_string());
    if items.is_empty() {
        return Ok(format!("No open items under {scope}."));
    }
    let mut lines = vec![
        format!("## Open items: {scope}"),
        format!("{} item(s)", items.len()),
        String::new(),
    ];
    let mut current: Option<String> = None;
    for item in items {
        let item = item
            .as_object()
            .ok_or_else(|| format!("TypeError: '{}' object is not subscriptable", py_type(item)))?;
        let about = value_string_field(item, "about")?;
        if current.as_deref() != Some(about.as_str()) {
            current = Some(about.clone());
            lines.push(format!("### `{about}`"));
        }
        lines.push(format!(
            "- line {}: {}  (`{}`)",
            py_str(field(item, "line")?),
            py_str(field(item, "text")?),
            py_str(field(item, "id")?)
        ));
    }
    Ok(lines.join("\n"))
}

/// `annotation_versions` (`render.py:1106`).
pub fn annotation_versions(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let versions = field(map, "versions")?
        .as_array()
        .ok_or_else(|| format!("TypeError: list indices must be integers or slices, not str"))?;
    let subject = map
        .get("target")
        .or_else(|| map.get("id"))
        .map(py_str)
        .unwrap_or_else(|| "?".to_string());
    let mut out = vec![
        format!("subject          {subject}"),
        format!("versions         {}", py_str(field(map, "count")?)),
        String::new(),
        format!(
            "{} {} {} {} {}",
            pad_right("id", 34),
            pad_left("open", 4),
            pad_right("valid_from", 28),
            pad_right("valid_to", 28),
            "state"
        ),
    ];
    for version in versions {
        let version = version.as_object().ok_or_else(|| {
            format!(
                "TypeError: '{}' object is not subscriptable",
                py_type(version)
            )
        })?;
        let state = if field(version, "current")?.as_bool().unwrap_or(false) {
            "current"
        } else if field(version, "retired")?.as_bool().unwrap_or(false) {
            "retired"
        } else {
            "superseded"
        };
        out.push(format!(
            "{} {} {} {} {}",
            pad_right(&truncate_chars(&py_str(field(version, "id")?), 34), 34),
            pad_left(&py_str(field(version, "open")?), 4),
            pad_right(
                &truncate_chars(&py_str(field(version, "valid_from")?), 28),
                28
            ),
            pad_right(
                &truncate_chars(&py_str(field(version, "valid_to")?), 28),
                28
            ),
            state
        ));
    }
    Ok(out.join("\n"))
}

/// `annotation_stored` (`render.py:1136`).
pub fn annotation_stored(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let action = if map.contains_key("repaired") && map.contains_key("about") {
        "minted"
    } else if map
        .get("revised")
        .is_some_and(|v| v.as_bool() == Some(true))
    {
        "revised"
    } else if map.contains_key("revised") {
        "unchanged"
    } else if map
        .get("retired")
        .is_some_and(|v| v.as_bool() == Some(true))
    {
        "retired"
    } else {
        "already retired"
    };
    let id = field(map, "id")?;
    let mut lines = vec![format!("{} {}", pad_right(action, 16), py_str(id))];
    if let Some(open) = map.get("open") {
        lines.push(format!("open             {}", py_str(open)));
    }
    if map
        .get("repaired")
        .is_some_and(|v| v.as_bool() == Some(true))
    {
        lines
            .push("repaired         input repaired by the linter (stored canonically)".to_string());
    }
    Ok(lines.join("\n"))
}

/// `ingest_history` (`render.py:1159`).
pub fn ingest_history(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let dry = field(map, "dry_run")?.as_bool().unwrap_or(false);
    let mut out = vec![
        format!(
            "branch           {}  (lineage {})",
            py_str(field(map, "branch")?),
            py_str(field(map, "lineage")?)
        ),
        format!(
            "from             {}",
            optional_field(map, "from")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "(root)".to_string())
        ),
        format!(
            "fork             {}",
            map.get("fork_point")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "(trunk)".to_string())
        ),
        format!(
            "head             {}",
            map.get("head")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "(unchanged)".to_string())
        ),
        format!(
            "belief           {}",
            map.get("belief_start")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "(none)".to_string())
        ),
        format!(
            "commits          ingested={}  skipped={}  ghosts={}{}",
            py_str(field(map, "commits_ingested")?),
            field(map, "commits_skipped")?
                .as_array()
                .map_or(0, Vec::len),
            py_str(field(map, "ghost_commits")?),
            if dry { "  DRY RUN" } else { "" }
        ),
        format!(
            "files            parsed={}  failed={}  excluded={}",
            py_str(field(map, "files_parsed")?),
            field(map, "files_failed")?.as_array().map_or(0, Vec::len),
            py_str(field(map, "files_excluded")?)
        ),
        format!(
            "blobs            stored={}  deduped={}  stamps_nudged={}",
            py_str(field(map, "blobs_stored")?),
            py_str(field(map, "blobs_deduped")?),
            py_str(field(map, "stamps_nudged")?)
        ),
        format!(
            "elapsed          {:.1}s",
            field(map, "elapsed_secs")?.as_f64().unwrap_or(0.0)
        ),
    ];
    if let Some(window) = map.get("window").and_then(Value::as_object) {
        if !window.is_empty() {
            out.push(String::new());
            out.push(format!(
                "backfill window  ({}, {})  commits={}  edges_skipped={}",
                window
                    .get("from")
                    .filter(|v| !v.is_null())
                    .map(py_str)
                    .unwrap_or_else(|| "(root)".to_string()),
                py_str(field(window, "to")?),
                py_str(field(window, "commits")?),
                py_str(field(window, "edges_skipped")?)
            ));
        }
    }
    let skipped = field(map, "commits_skipped")?
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default();
    if !skipped.is_empty() {
        out.push("\nskipped commits:".to_string());
        for item in skipped {
            let item = item.as_object().ok_or_else(|| {
                format!("TypeError: '{}' object is not subscriptable", py_type(item))
            })?;
            out.push(format!(
                "  {}  {}",
                truncate_chars(&py_str(field(item, "sha")?), 12),
                py_str(field(item, "reason")?)
            ));
        }
    }
    let failed = field(map, "files_failed")?
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default();
    if !failed.is_empty() {
        out.push("\nfailed files:".to_string());
        for item in failed {
            let item = item.as_object().ok_or_else(|| {
                format!("TypeError: '{}' object is not subscriptable", py_type(item))
            })?;
            out.push(format!(
                "  {}  {}  {}",
                truncate_chars(&py_str(field(item, "sha")?), 12),
                py_str(field(item, "path")?),
                py_str(field(item, "reason")?)
            ));
        }
    }
    Ok(out.join("\n"))
}

/// `migrate_to_commit_time` (`render.py:1211`).
pub fn migrate_to_commit_time(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let mut out = vec![
        format!("migrated         {}", py_str(field(map, "repo")?)),
        format!("store            {}", py_str(field(map, "store")?)),
        format!("parent           {}", py_str(field(map, "parent_ref")?)),
        format!(
            "commits          ingested={}  skipped={}  failed_files={}  elapsed={}s",
            py_str(field(map, "commits_ingested")?),
            py_str(field(map, "commits_skipped")?),
            py_str(field(map, "files_failed")?),
            py_str(field(map, "elapsed_secs")?)
        ),
    ];
    let branches = field(map, "branches")?.as_array().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not iterable",
            py_type(field(map, "branches").unwrap_or(&Value::Null))
        )
    })?;
    let reports = field(map, "branch_reports")?.as_object().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not subscriptable",
            py_type(field(map, "branch_reports").unwrap_or(&Value::Null))
        )
    })?;
    for branch in branches {
        let name = py_str(branch);
        let report = field(reports, &name)?.as_object().ok_or_else(|| {
            format!(
                "TypeError: '{}' object is not subscriptable",
                py_type(field(reports, &name).unwrap_or(&Value::Null))
            )
        })?;
        let head = field(report, "head")?;
        let head = if head.is_null() {
            "(unchanged)".to_string()
        } else {
            py_str(head)
        };
        out.push(format!(
            "  {}  ingested={}  skipped={}  head={}",
            name,
            py_str(field(report, "commits_ingested")?),
            field(report, "commits_skipped")?
                .as_array()
                .map_or(0, Vec::len),
            truncate_chars(&head, 12)
        ));
    }
    let embeddings = field(map, "embeddings")?.as_object().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not subscriptable",
            py_type(field(map, "embeddings").unwrap_or(&Value::Null))
        )
    })?;
    if field(embeddings, "embedded")?.as_bool().unwrap_or(false) {
        out.push(format!(
            "embeddings       generated={}  cached={}  total={}  persisted={}",
            py_str(field(embeddings, "generated")?),
            py_str(field(embeddings, "cached")?),
            py_str(field(embeddings, "total")?),
            py_str(field(embeddings, "persisted")?)
        ));
    } else {
        out.push("embeddings       skipped".to_string());
    }
    let verify_value = field(map, "verify")?;
    if verify_value.is_null() {
        out.push("verify           skipped".to_string());
        return Ok(out.join(
            "
",
        ));
    }
    let coverage = verify_value.as_object().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not subscriptable",
            py_type(verify_value)
        )
    })?;
    if coverage.is_empty() {
        out.push("verify           skipped".to_string());
    } else {
        for (branch, verdict) in coverage {
            let verdict = verdict.as_object().ok_or_else(|| {
                format!(
                    "TypeError: '{}' object is not subscriptable",
                    py_type(verdict)
                )
            })?;
            out.push(format!(
                "verify[{branch}]    {}% over {} generations",
                py_str(field(verdict, "byte_coverage_pct")?),
                py_str(field(verdict, "total_generations")?)
            ));
        }
    }
    Ok(out.join("\n"))
}

fn field_failure(value: Option<&Value>) -> Option<String> {
    let value = value.filter(|v| !v.is_null())?;
    match unwrap_object(value) {
        Value::Object(map) => Some(format!(
            "{}/{} — {}",
            map.get("stage")
                .map(py_str)
                .unwrap_or_else(|| "?".to_string()),
            map.get("code")
                .map(py_str)
                .unwrap_or_else(|| "?".to_string()),
            map.get("message").map(py_str).unwrap_or_default()
        )),
        _ => Some(format!("?/? — {}", py_str(value))),
    }
}

fn field_list<'a>(value: &'a Value, key: &str) -> Vec<&'a Value> {
    match unwrap_object(value) {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn field_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    unwrap_object(value).as_object().and_then(|m| m.get(key))
}

/// `mutation_plan` (`render.py:1385`).
pub fn mutation_plan(plan: Value) -> Result<String, String> {
    let tool = field_value(&plan, "tool").ok_or_else(|| {
        format!(
            "AttributeError: '{}' object has no attribute 'tool'",
            py_type(&plan)
        )
    })?;
    let mut lines = vec![
        format!("## Mutation Plan: `{}` (DRY RUN)", py_str(tool)),
        String::new(),
        format!(
            "- **Plan ID:** `{}`",
            py_str(field_value(&plan, "id").unwrap_or(&Value::from("?")))
        ),
        format!(
            "- **Affected files:** {}",
            field_value(&plan, "affected_files")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        ),
    ];
    if let Some(diff) = field_value(&plan, "diff_preview").and_then(Value::as_str) {
        if !diff.is_empty() {
            lines.push(String::new());
            lines.push("### Diff Preview".to_string());
            lines.push("```diff".to_string());
            let rows: Vec<&str> = diff.split('\n').collect();
            for row in rows.iter().take(60) {
                lines.push((*row).to_string());
            }
            if rows.len() > 60 {
                lines.push("...".to_string());
            }
            lines.push("```".to_string());
        }
    }
    let sites = field_value(&plan, "unverified_sites")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default();
    if !sites.is_empty() {
        lines.push(String::new());
        lines.push(format!("⚠️ **WARNING: {} call site(s) could not be verified/rewritten. Manual review required.**", sites.len()));
        for site in sites.iter().take(10) {
            if let Some(map) = site.as_object() {
                let where_ = format!(
                    "{}:{}",
                    map.get("file")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("line")
                        .map(py_str)
                        .unwrap_or_else(|| "0".to_string())
                );
                let snippet = map
                    .get("snippet")
                    .filter(|v| !v.is_null())
                    .map(|v| format!(" (`{}`)", py_str(v)))
                    .unwrap_or_default();
                lines.push(format!(
                    "- `{where_}` — {}{snippet}",
                    map.get("reason").map(py_str).unwrap_or_default()
                ));
            } else {
                lines.push(format!("- `{}`", py_str(site)));
            }
        }
    }
    let warnings = field_value(&plan, "warnings")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default();
    if !warnings.is_empty() {
        lines.push(String::new());
        lines.push("### Warnings".to_string());
        for warning in warnings {
            lines.push(format!("- ⚠ {}", py_str(warning)));
        }
    }
    Ok(lines.join("\n"))
}

/// `mutation_error` (`render.py:1345`).
pub fn mutation_error(raw: String, update_file: String) -> Result<String, String> {
    let short = truncate_chars(&raw, 300);
    let lower = raw.to_lowercase();
    if raw.contains("StaleIndex") || lower.contains("stale") {
        return Ok(format!(
            "## Mutation Rejected — Stale Index\n\nThe file changed on disk after it was indexed, so the planned span no longer lines up. Nothing was written.\n\n**Next step:** run {update_file} on the file (or re-analyze), then retry the mutation.\n\n<details>Raw error: `{short}`</details>"
        ));
    }
    if raw.contains("RejectedPolicy") || lower.contains("policy") {
        return Ok(format!(
            "## Mutation Rejected — Policy\n\nThe target path is outside the `[mutation] allow` list. Nothing was written.\n\n**Next step:** pick a target under an allowed root, or ask the user to extend the allow list.\n\n<details>Raw error: `{short}`</details>"
        ));
    }
    if raw.contains("SpanOutOfBounds") || lower.contains("out of bounds") {
        return Ok(format!(
            "## Mutation Failed — Span Mismatch\n\nThe computed edit span fell outside the file — likely a stale concept or an off-by-one in the planner. Nothing was written (rollback confirmed).\n\n**Next step:** update the file in the graph and retry; if it persists, report it with the entity id.\n\n<details>Raw error: `{short}`</details>"
        ));
    }
    if raw.contains("ParseError") || lower.contains("syntax") {
        return Ok(format!(
            "## Mutation Failed — Syntax\n\nThe edited file did not re-parse, so the change was rolled back. Nothing was written.\n\n<details>Raw error: `{short}`</details>"
        ));
    }
    Ok(format!("Mutation failed: {raw}"))
}

/// `mutation_applied` (`render.py:1440`).
pub fn mutation_applied(result: Value, unverified_sites: Option<Value>) -> Result<String, String> {
    let status = field_value(&result, "status")
        .map(py_str)
        .unwrap_or_default();
    let files_written = field_list(&result, "files_written");
    let syntax_errors = field_list(&result, "syntax_errors");
    let file_outcomes = field_list(&result, "files");
    let sync = field_value(&result, "sync");
    let sync_state = sync.and_then(|v| field_value(v, "state"));
    let applied = status == "Applied";
    let graph_updated = applied
        && (sync.is_none() || sync_state.is_some_and(|v| v.as_str() == Some("Synchronized")));
    let header = match status.as_str() {
        "Applied" => "## Mutation Applied".to_string(),
        "RolledBack" => "## Mutation Rolled Back".to_string(),
        "RejectedStale" => "## Mutation Rejected — Stale".to_string(),
        "RejectedPolicy" => "## Mutation Rejected — Policy".to_string(),
        "ApplyIncomplete" => "## Mutation Incomplete — Graph Sync Failed".to_string(),
        "RollbackIncomplete" => "## Mutation Incomplete — Rollback Incomplete".to_string(),
        _ => format!(
            "## Mutation Result — {}",
            if status.is_empty() {
                "Unknown"
            } else {
                &status
            }
        ),
    };
    let mut lines = vec![header, String::new()];
    lines.push(format!("- **Status:** {status}"));
    lines.push(format!(
        "- **File written:** {}",
        if files_written.is_empty() {
            "no"
        } else {
            "yes"
        }
    ));
    if graph_updated {
        lines.push("- **Graph updated:** yes".to_string());
    } else if applied || status == "ApplyIncomplete" || status == "RollbackIncomplete" {
        lines.push("- **Graph updated:** no — synchronization did not complete".to_string());
    } else {
        lines.push("- **Graph updated:** no — nothing changed".to_string());
    }
    if !files_written.is_empty() {
        lines.push(format!("- **Files written:** {}", files_written.len()));
        for file in &files_written {
            lines.push(format!("  - `{}`", py_str(file)));
        }
    }
    let failure =
        field_failure(field_value(&result, "failure").or_else(|| field_value(&result, "primary")));
    if let Some(failure) = failure {
        lines.push(format!("- **Failure:** {failure}"));
    }
    if !file_outcomes.is_empty() {
        lines.push(format!("- **File outcomes:** {}", file_outcomes.len()));
        for entry in &file_outcomes {
            let state = field_value(entry, "state")
                .map(py_str)
                .unwrap_or_else(|| "?".to_string());
            let name = field_value(entry, "file")
                .map(py_str)
                .unwrap_or_else(|| "?".to_string());
            let detail = field_failure(field_value(entry, "error"));
            let recovery = field_value(entry, "recovery_path").filter(|v| !v.is_null());
            let mut text = format!("  - `{name}` — {state}");
            if let Some(detail) = detail {
                text.push_str(&format!(" ({detail})"));
            }
            if let Some(recovery) = recovery {
                text.push_str(&format!(" [recovery: `{}`]", py_str(recovery)));
            }
            lines.push(text);
        }
    }
    if let (Some(sync), Some(sync_state)) = (sync, sync_state) {
        lines.push(format!("- **Sync:** {}", py_str(sync_state)));
        for error in field_list(sync, "errors").iter().take(5) {
            if let Some(map) = error.as_object() {
                lines.push(format!(
                    "  - `{}` — {}/{} — {}",
                    map.get("file")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("stage")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("code")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("message").map(py_str).unwrap_or_default()
                ));
            } else {
                lines.push(format!("  - {}", py_str(error)));
            }
        }
        for warning in field_list(sync, "warnings").iter().take(5) {
            if let Some(map) = warning.as_object() {
                lines.push(format!(
                    "  - warning `{}` — {}/{} — {}",
                    map.get("file")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("stage")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("code")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("message").map(py_str).unwrap_or_default()
                ));
            } else {
                lines.push(format!("  - warning: {}", py_str(warning)));
            }
        }
    }
    if !syntax_errors.is_empty() {
        lines.push(format!("- **Syntax errors:** {}", syntax_errors.len()));
        for error in syntax_errors.iter().take(5) {
            if let Some(map) = error.as_object() {
                lines.push(format!(
                    "  - `{}:{}:{}` — {}",
                    map.get("file")
                        .map(py_str)
                        .unwrap_or_else(|| "?".to_string()),
                    map.get("line")
                        .map(py_str)
                        .unwrap_or_else(|| "0".to_string()),
                    map.get("column")
                        .map(py_str)
                        .unwrap_or_else(|| "0".to_string()),
                    map.get("message").map(py_str).unwrap_or_default()
                ));
            } else {
                lines.push(format!("  - {}", py_str(error)));
            }
        }
    }
    let mut recovery_paths: Vec<String> = Vec::new();
    for entry in &file_outcomes {
        if let Some(path) = field_value(entry, "recovery_path").filter(|v| !v.is_null()) {
            let path = py_str(path);
            if !recovery_paths.contains(&path) {
                recovery_paths.push(path);
            }
        }
    }
    if let Some(backup) = field_value(&result, "backup_path").filter(|v| !v.is_null()) {
        let backup = py_str(backup);
        if !recovery_paths.contains(&backup) {
            recovery_paths.push(backup.clone());
        }
        lines.push(format!("- **Backup:** `{backup}`"));
    }
    if !recovery_paths.is_empty() && (status == "ApplyIncomplete" || status == "RollbackIncomplete")
    {
        lines.push(format!("- **Recovery paths:** {}", recovery_paths.len()));
        for path in recovery_paths {
            lines.push(format!("  - `{path}`"));
        }
    }
    if !applied {
        lines.push(String::new());
        match status.as_str() {
            "RejectedStale" => lines.push(
                "The file changed since planning — call the plan tool again to re-read the current content, then apply the fresh plan.".to_string(),
            ),
            "ApplyIncomplete" => lines.push(
                "The valid source was left in place, but graph or persistence synchronization did not complete. Repair the named sync problem with `coderadar_update_file`/`coderadar_reindex`; do not blindly apply the old plan.".to_string(),
            ),
            "RollbackIncomplete" => lines.push(
                "Compensation or restored-state synchronization could not be proven complete. Stop automatic retries, inspect the per-file states and retained recovery paths above, recover manually, then resync.".to_string(),
            ),
            _ => lines.push("No changes were kept. Address the reason above and re-plan.".to_string()),
        }
    }
    if let Some(sites) = unverified_sites {
        let rows = match sites {
            Value::Array(rows) => rows,
            Value::Null => Vec::new(),
            _ => return Err("KeyError: slice(None, 10, None)".to_string()),
        };
        if !rows.is_empty() {
            lines.push(String::new());
            lines.push(format!("⚠️ **WARNING: {} call site(s) could not be verified/rewritten. Manual review required.**", rows.len()));
            for site in rows.iter().take(10) {
                lines.push(format!("- `{}`", py_str(site)));
            }
        }
    }
    Ok(lines.join("\n"))
}

/// `edit` (`render.py:1336`).
pub fn edit(outcome: Value, apply_hint: String) -> Result<String, String> {
    let map = object(&outcome)?;
    let note_value = map.get("note").filter(|v| !v.is_null());
    let note = match note_value {
        Some(value) => {
            let text = py_str(value);
            if text.is_empty() {
                String::new()
            } else {
                format!("Note: {text}\n\n")
            }
        }
        None => String::new(),
    };
    let plan = field(map, "plan")?.clone();
    let result = field(map, "result")?;
    if result.is_null() {
        let sites = unwrap_object(&plan)
            .as_object()
            .and_then(|m| m.get("unverified_sites"))
            .cloned();
        return Ok(format!(
            "{}{}\n**To apply:** {apply_hint}.",
            note,
            mutation_plan_with_sites(&plan, sites)?
        ));
    }
    let sites = unwrap_object(&plan)
        .as_object()
        .and_then(|m| m.get("unverified_sites"))
        .cloned();
    Ok(format!(
        "{}{}",
        note,
        mutation_applied(result.clone(), sites)?
    ))
}

fn mutation_plan_with_sites(plan: &Value, sites: Option<Value>) -> Result<String, String> {
    if let Some(sites) = sites {
        let mut value = plan.clone();
        if let Some(map) = value.as_object_mut() {
            map.insert("unverified_sites".to_string(), sites);
        }
        mutation_plan(value)
    } else {
        mutation_plan(plan.clone())
    }
}

/// `reindex` (`render.py:1578`).
pub fn reindex(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let stats = field(map, "stats")?.as_object().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not subscriptable",
            py_type(field(map, "stats").unwrap_or(&Value::Null))
        )
    })?;
    let mut lines = vec![
        "## Reindex Complete".to_string(),
        String::new(),
        format!(
            "- **Files:** {}",
            py_str(stats.get("file_count").unwrap_or(&Value::from(0)))
        ),
        format!(
            "- **Modules:** {}",
            py_str(stats.get("modules").unwrap_or(&Value::from(0)))
        ),
        format!(
            "- **Classes:** {}",
            py_str(stats.get("classes").unwrap_or(&Value::from(0)))
        ),
        format!(
            "- **Functions:** {}",
            py_str(stats.get("functions").unwrap_or(&Value::from(0)))
        ),
        format!(
            "- **Call edges:** {}",
            py_str(stats.get("call_edges").unwrap_or(&Value::from(0)))
        ),
    ];
    if let Some(embeddings) = map.get("embeddings").and_then(Value::as_object) {
        lines.push(String::new());
        lines.push(format!(
            "- **Embeddings generated:** {}",
            py_str(embeddings.get("generated").unwrap_or(&Value::from(0)))
        ));
        lines.push(format!(
            "- **Embeddings cached:** {}",
            py_str(embeddings.get("cached").unwrap_or(&Value::from(0)))
        ));
    } else if let Some(error) = map.get("embeddings_error") {
        lines.push(String::new());
        lines.push(format!("- **Embeddings:** failed — {}", py_str(error)));
    }
    if let Some(error) = map.get("edge_persist_error") {
        lines.extend([
            String::new(),
            "> **Edges are INCOMPLETE for this generation.** Edge persistence failed part way through, so some edges were never written (concepts and blobs are unaffected). Re-run the reindex.".to_string(),
            String::new(),
            format!("> Engine said: `{}`", py_str(error)),
        ]);
    }
    Ok(lines.join("\n"))
}

/// `status` (`render.py:1641`).
pub fn status(result: Value) -> Result<String, String> {
    let map = object(&result)?;
    let mut lines = vec![
        format!("## Project: `{}`", py_str(field(map, "root")?)),
        String::new(),
    ];
    let config = field(map, "config")?;
    if config.is_null() {
        lines.push("- **Config:** none".to_string());
    } else {
        lines.push(format!("- **Config:** `{}`", py_str(config)));
    }
    let store = field(map, "store")?;
    if store.is_null() {
        lines.push("- **Store:** none — run `coderadar init` to create one".to_string());
    } else {
        let state = match map.get("store_fresh") {
            Some(Value::Bool(true)) => " (fresh)".to_string(),
            Some(Value::Bool(false)) => " (older than some source files)".to_string(),
            _ => String::new(),
        };
        lines.push(format!("- **Store:** `{}`{state}", py_str(store)));
    }
    let stats = field(map, "stats")?;
    if !field(map, "loaded")?.as_bool().unwrap_or(false) || stats.is_null() {
        lines.push("- **Index:** not loaded".to_string());
        return Ok(lines.join("\n"));
    }
    let age = map.get("age_seconds").and_then(Value::as_f64);
    let when = match age {
        None => String::new(),
        Some(seconds) if seconds >= 60.0 => format!(", synced {:.0} min ago", seconds / 60.0),
        Some(_) => ", synced just now".to_string(),
    };
    lines.push(format!("- **Index:** loaded{when}"));
    lines.push(String::new());
    let empty_stats = Map::new();
    let stats = stats.as_object().unwrap_or(&empty_stats);
    for key in [
        "file_count",
        "modules",
        "classes",
        "functions",
        "imports",
        "constants",
        "call_edges",
    ] {
        if let Some(value) = stats.get(key) {
            lines.push(format!("- {}: {}", key.replace('_', " "), py_str(value)));
        }
    }
    Ok(lines.join("\n"))
}

/// `changes_between` (`render.py:1260`).
pub fn changes_between(result: Value, per_class: i64) -> Result<String, String> {
    let map = object(&result)?;
    let counts = field(map, "counts")?.as_object().ok_or_else(|| {
        format!(
            "TypeError: '{}' object is not subscriptable",
            py_type(field(map, "counts").unwrap_or(&Value::Null))
        )
    })?;
    let scope = field(map, "scope")?;
    let normalized = field(map, "normalized_ids")?.as_bool().unwrap_or(false);
    let mut head = vec![
        format!(
            "store            {}  (branch {})",
            py_str(field(map, "store")?),
            py_str(field(map, "branch")?)
        ),
        format!("t1               {}", py_str(field(map, "t1")?)),
        format!("t2               {}", py_str(field(map, "t2")?)),
        if scope.is_null() {
            "scope            (whole universe)".to_string()
        } else {
            format!("scope            {}", py_str(scope))
        },
        format!(
            "changed          added={}  removed={}  modified={}  moved={}  uncompared={}  unmodified={}",
            py_str(field(counts, "added")?),
            py_str(field(counts, "removed")?),
            py_str(field(counts, "modified")?),
            py_str(field(counts, "moved")?),
            py_str(field(counts, "uncompared")?),
            py_str(field(map, "unmodified")?)
        ),
        if normalized {
            format!(
                "ids              normalized across H-11 respelling: {} merged groups",
                py_str(field(map, "normalized_groups")?)
            )
        } else {
            "ids              single spelling (no H-11 merge)".to_string()
        },
    ];
    let limit = per_class.max(0) as usize;
    let fmt_span = |entry: &Map<String, Value>, key: &str| -> String {
        match entry.get(key) {
            Some(Value::Array(span)) if span.len() >= 2 => {
                format!("  @{}-{}", py_str(&span[0]), py_str(&span[1]))
            }
            _ => String::new(),
        }
    };
    let sections: [(&str, &str, &str); 5] = [
        ("added", "added", "span"),
        ("removed", "removed", "span"),
        ("modified", "modified", "span_after"),
        ("moved", "moved", "span_after"),
        ("uncompared", "uncompared", ""),
    ];
    for (label, key, span_key) in sections {
        let items = field(map, key)?.as_array().ok_or_else(|| {
            format!(
                "TypeError: '{}' object is not iterable",
                py_type(field(map, key).unwrap_or(&Value::Null))
            )
        })?;
        head.push(format!("\n{label} ({}):", items.len()));
        for item in items.iter().take(limit) {
            let entry = item.as_object().ok_or_else(|| {
                format!("TypeError: '{}' object is not subscriptable", py_type(item))
            })?;
            let line = match label {
                "added" => format!(
                    "  + {}{}",
                    py_str(field(entry, "id")?),
                    fmt_span(entry, span_key)
                ),
                "removed" => format!(
                    "  - {}{}",
                    py_str(field(entry, "id")?),
                    fmt_span(entry, span_key)
                ),
                "modified" => format!(
                    "  ~ {}  {} -> {}{}",
                    py_str(field(entry, "id")?),
                    py_str(field(entry, "hash_before")?),
                    py_str(field(entry, "hash_after")?),
                    fmt_span(entry, span_key)
                ),
                "moved" => match entry.get("id_before") {
                    Some(before) => format!(
                        "  > {} -> {}{}",
                        py_str(before),
                        py_str(field(entry, "id_after")?),
                        fmt_span(entry, span_key)
                    ),
                    None => format!(
                        "  > {}{}",
                        py_str(field(entry, "id")?),
                        fmt_span(entry, span_key)
                    ),
                },
                _ => format!("  ? {}", py_str(field(entry, "id")?)),
            };
            head.push(line);
        }
        if items.len() > limit {
            head.push(format!("  ... and {} more", items.len() - limit));
        }
    }
    Ok(head.join("\n"))
}
