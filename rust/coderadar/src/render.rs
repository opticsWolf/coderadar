//! Unit 3: text surfaces shared by the Rust CLI and MCP tools.
//!
//! Ports of the pure formatters in `render.py`; the text bar is normalized
//! (markup and whitespace), not byte-for-byte.

use serde_json::Value;

fn s(v: Option<&Value>, default: &str) -> String {
    match v {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(x)) => x.to_string(),
        Some(Value::Bool(x)) => x.to_string(),
        _ => default.to_string(),
    }
}

fn arr(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Array(a)) => a,
        _ => &[],
    }
}

/// A miss with "did you mean" candidates.
pub fn not_found(entity_id: &str, candidates: &[Value]) -> String {
    let mut lines = vec![format!("Entity `{entity_id}` not found.")];
    if candidates.is_empty() {
        lines.push("Try `coderadar_search` to locate it.".to_string());
    } else {
        lines.push("Did you mean:".to_string());
        for hit in candidates.iter().take(3) {
            lines.push(format!(
                "- `{}` — `{}`",
                s(hit.get("name"), "?"),
                s(hit.get("id"), "?")
            ));
        }
    }
    lines.join("\n")
}

/// An empty search, explaining what was tried.
pub fn search_miss(query: &str, kind: Option<&str>) -> String {
    let suffix = kind.map_or(String::new(), |k| format!(" (kind: {k})"));
    let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
    let mut lines = vec![format!("No results found for '{query}'{suffix}.")];
    if tokens.len() >= 2 {
        let shown = tokens
            .iter()
            .take(8)
            .map(|t| format!("`{t}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "Tokens are matched independently (OR) — none of {shown} occurs in any indexed entity's name, signature or docstring."
        ));
        lines.push(
            "Try a single token by itself, one that you expect as an identifier.".to_string(),
        );
    } else {
        lines.push("The token was matched against entity names, function signatures and docstrings and hit nothing.".to_string());
    }
    lines.push("Escape hatches: `coderadar_search_similar` or `coderadar_explore` with explicit `symbols`.".to_string());
    lines.join("\n")
}

/// Ranked search results.
pub fn search(query: &str, kind: Option<&str>, results: &[Value]) -> String {
    if results.is_empty() {
        return search_miss(query, kind);
    }
    let mut lines = vec![
        format!("## Search: `{query}`"),
        format!("Found {} result(s)", results.len()),
        String::new(),
    ];
    for (i, entity) in results.iter().enumerate() {
        lines.push(format!(
            "### {}. `{}` ({})",
            i + 1,
            s(entity.get("name"), "?"),
            s(entity.get("kind"), "?")
        ));
        lines.push(format!("- **ID:** `{}`", s(entity.get("id"), "?")));
        lines.push(format!("- **File:** `{}`", s(entity.get("file_path"), "?")));
        if let Some(line) = entity.get("start_line") {
            lines.push(format!("- **Line:** {line}"));
        }
        if let Some(doc) = entity.get("docstring").and_then(Value::as_str) {
            let shortened: String = doc.chars().take(200).collect();
            let ellipsis = if doc.chars().count() > 200 { "..." } else { "" };
            lines.push(format!("- **Docstring:** {shortened}{ellipsis}"));
        }
        if let Some(sig) = entity.get("signature") {
            lines.push(format!("- **Signature:** `{}`", plain(Some(sig))));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

fn caller_rows(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .map(|c| {
            format!(
                "- `{}` ({}) — `{}`",
                s(c.get("name"), "?"),
                s(c.get("kind"), "?"),
                s(c.get("id"), "?")
            )
        })
        .collect()
}

/// Direct callers list.
pub fn callers(entity_id: &str, results: &[Value]) -> String {
    let mut lines = vec![
        format!("## Callers of `{entity_id}` ({})", results.len()),
        String::new(),
    ];
    lines.extend(caller_rows(results));
    lines.join("\n")
}

/// Direct callees list.
pub fn callees(entity_id: &str, results: &[Value]) -> String {
    let mut lines = vec![
        format!("## Callees from `{entity_id}` ({})", results.len()),
        String::new(),
    ];
    lines.extend(caller_rows(results));
    lines.join("\n")
}

/// One entity's full record.
pub fn node(entity: &Value) -> String {
    let mut lines = vec![
        format!("## {}", s(entity.get("name"), "?")),
        String::new(),
        format!("- **ID:** `{}`", s(entity.get("id"), "?")),
        format!("- **Kind:** {}", s(entity.get("kind"), "?")),
        format!("- **File:** `{}`", s(entity.get("file_path"), "?")),
    ];
    if let (Some(start), Some(end)) = (entity.get("start_line"), entity.get("end_line")) {
        let a = start.as_u64().unwrap_or(0);
        let b = end.as_u64().unwrap_or(0);
        lines.push(format!(
            "- **Lines:** {a}–{b} ({lines} lines)",
            lines = b - a + 1
        ));
    }
    if let Some(doc) = entity.get("docstring").and_then(Value::as_str) {
        lines.push(format!("\n```\n{doc}\n```"));
    }
    if let Some(sig) = entity.get("signature") {
        lines.push(format!("\n**Signature:** `{sig}`"));
    }
    let decorators = arr(entity.get("decorators"));
    if !decorators.is_empty() {
        let names = decorators
            .iter()
            .map(|d| format!("`{d}`"))
            .collect::<Vec<_>>();
        lines.push(format!("\n**Decorators:** {}", names.join(", ")));
    }
    for (key, title) in [("callers", "Callers"), ("callees", "Callees")] {
        let items = arr(entity.get(key));
        if items.is_empty() {
            continue;
        }
        lines.push(format!("\n## {title} ({})", items.len()));
        for c in items.iter().take(15) {
            lines.push(format!(
                "- `{}` ({})",
                s(c.get("name").or_else(|| c.get("id")), "?"),
                s(c.get("kind"), "?")
            ));
        }
    }
    lines.join("\n")
}

/// Blast-radius report.
pub fn affected(entity_id: &str, result: &Value) -> String {
    let entity_name = s(result.get("entity").and_then(|e| e.get("name")), "?");
    let max_depth = s(result.get("max_depth"), "5");
    let mut total = 0usize;
    let mut depth_lines = Vec::new();
    if let Some(Value::Object(map)) = result.get("depths") {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in keys {
            let items = arr(map.get(key));
            total += items.len();
            depth_lines.push(format!("**Depth {key}** ({}):", items.len()));
            for e in items.iter().take(20) {
                depth_lines.push(format!(
                    "- `{}` ({}) — `{}`",
                    s(e.get("name"), "?"),
                    s(e.get("kind"), "?"),
                    s(e.get("id"), "?")
                ));
            }
            depth_lines.push(String::new());
        }
    }
    let mut lines = vec![
        format!("## Affected by `{entity_name}`"),
        String::new(),
        format!("Transitive impact for `{entity_id}` (max depth: {max_depth})"),
        format!("**Total dependents:** {total}"),
        String::new(),
    ];
    if total == 0 {
        lines.push("No dependents found. Nothing calls this entity.".to_string());
    } else {
        lines.extend(depth_lines);
    }
    lines.join("\n")
}

/// Breadth-first traversal report.
pub fn traverse(result: &Value) -> String {
    let entity_name = s(
        result.get("entity").and_then(|e| e.get("name")),
        &s(result.get("entity_id"), "?"),
    );
    let title = format!("## Traverse from `{entity_name}`");
    let rows = arr(result.get("results"));
    if rows.is_empty() {
        return format!(
            "{title}\n\nNo neighbors found (direction={}, max_depth={})",
            s(result.get("direction"), "both"),
            s(result.get("max_depth"), "3")
        );
    }
    let mut lines = vec![
        title,
        format!(
            "Direction: {}, max depth: {}, edge kinds: all",
            s(result.get("direction"), "both"),
            s(result.get("max_depth"), "3")
        ),
        format!("Found {} reachable entities", rows.len()),
        String::new(),
    ];
    for row in rows {
        lines.push(format!(
            "- `{}` ({}) — `{}`",
            s(row.get("name").or_else(|| row.get("id")), "?"),
            s(row.get("edge_type").or_else(|| row.get("kind")), "?"),
            s(row.get("id"), "?")
        ));
    }
    lines.join("\n")
}

/// Query result table in plain text.
pub fn query(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "No results.".to_string();
    }
    let mut lines = vec![format!("{} row(s)", rows.len())];
    for row in rows {
        lines.push(match row.get("id") {
            Some(id) => format!(
                "- `{id}` — {} ({})",
                s(row.get("name"), "?"),
                s(row.get("kind"), "?")
            ),
            None => serde_json::to_string(row).unwrap_or_default(),
        });
    }
    lines.join("\n")
}

/// Source grouped by file, then call relationships.
pub fn explore(result: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    for file in arr(result.get("files")) {
        let path = s(file.get("file_path"), "?");
        let entities = arr(file.get("entities"));
        let names = entities
            .iter()
            .take(10)
            .map(|e| format!("{}({})", s(e.get("name"), "?"), s(e.get("kind"), "?")))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("**{path}** — {names}"));
        lines.push(String::new());
        for entity in entities {
            if let Some(source) = entity.get("source").and_then(Value::as_str) {
                lines.push(source.trim_end().to_string());
                lines.push(String::new());
            }
        }
    }
    let relationships: Vec<String> = arr(result.get("relationships"))
        .iter()
        .map(|r| {
            if s(r.get("kind"), "") == "caller" {
                format!(
                    "- `{}` <-[caller]- `{}`",
                    s(r.get("other_name"), "?"),
                    s(r.get("entity_name"), "?")
                )
            } else {
                format!(
                    "- `{}` -[callee]-> `{}`",
                    s(r.get("entity_name"), "?"),
                    s(r.get("other_name"), "?")
                )
            }
        })
        .collect();
    if !relationships.is_empty() {
        lines.push("## Relationships".to_string());
        lines.extend(relationships);
    }
    if lines.is_empty() {
        "No source found for the named symbols.".to_string()
    } else {
        lines.join("\n")
    }
}

/// Open checklist items.
pub fn open_items(result: &Value) -> String {
    let items = arr(result.get("items"));
    if items.is_empty() {
        return "No open items under (whole store).".to_string();
    }
    let mut lines = vec![
        format!("## Open items: {}", s(result.get("scope"), "(whole store)")),
        format!("{} item(s)", items.len()),
        String::new(),
    ];
    let mut current = String::new();
    for item in items {
        let about = s(item.get("about"), "?");
        if about != current {
            current = about.clone();
            lines.push(format!("### `{current}`"));
        }
        lines.push(format!(
            "- line {}: {}  (`{}`)",
            s(item.get("line"), "?"),
            s(item.get("text"), ""),
            s(item.get("id"), "?")
        ));
    }
    lines.join("\n")
}

/// Project status.
pub fn status(result: &Value) -> String {
    let mut lines = vec![
        format!("## Project: `{}`", s(result.get("root"), "?")),
        String::new(),
    ];
    let config = s(result.get("config"), "");
    if config.is_empty() {
        lines.push("- **Config:** none".to_string());
    } else {
        lines.push(format!("- **Config:** `{config}`"));
    }
    let store = s(result.get("store"), "");
    if store.is_empty() {
        lines.push("- **Store:** none — run `coderadar init` to create one".to_string());
    } else {
        let fresh = if result.get("store_fresh").and_then(Value::as_bool) == Some(true) {
            " (fresh)"
        } else {
            ""
        };
        lines.push(format!("- **Store:** `{store}`{fresh}"));
    }
    if result.get("loaded").and_then(Value::as_bool) != Some(true) {
        lines.push("- **Index:** not loaded".to_string());
        return lines.join("\n");
    }
    lines.push("- **Index:** loaded".to_string());
    lines.push(String::new());
    if let Some(Value::Object(stats)) = result.get("stats") {
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
                lines.push(format!("- {}: {value}", key.replace('_', " ")));
            }
        }
    }
    lines.join("\n")
}

/// Reindex report.
pub fn reindex(result: &Value) -> String {
    let mut lines = vec!["## Reindex".to_string(), String::new()];
    for key in ["files_indexed", "entities_extracted", "extraction_failures"] {
        if let Some(value) = result.get(key) {
            lines.push(format!("- {}: {value}", key.replace('_', " ")));
        }
    }
    lines.join("\n")
}

fn plain(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(x)) => x.clone(),
        Some(x) => x.to_string(),
        None => "?".to_string(),
    }
}
