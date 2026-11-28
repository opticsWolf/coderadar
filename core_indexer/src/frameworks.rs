//! Pure-Rust framework resolution heuristics.
//!
//! Moved out of the PyO3/CLI crate so resolver classes, CLI commands and MCP
//! tools all share one engine implementation (M-FFI-TRANSLATES).

use serde_json::{json, Value};

fn route_segments(value: &Value) -> Vec<String> {
    let inner = value.get("$tuple").unwrap_or(value);
    match inner {
        Value::Array(rows) => rows
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Port of `canonical_route_pattern` (`py_agent/src/coderadar/resolvers/resolution.py:81`).
pub fn canonical_route_pattern(path: String) -> Result<Value, String> {
    let segments: Vec<String> = path
        .trim_matches('/')
        .split('/')
        .map(|seg| {
            if seg.starts_with('<') || seg.starts_with(':') || seg.starts_with('{') || seg == "*" {
                "*".to_string()
            } else {
                seg.to_string()
            }
        })
        .collect();
    Ok(json!({"$tuple": segments}))
}

/// Port of `route_patterns_match` (`py_agent/src/coderadar/resolvers/resolution.py:99`).
pub fn route_patterns_match(query: String, candidate: String) -> Result<bool, String> {
    let left = route_segments(&canonical_route_pattern(query)?);
    let right = route_segments(&canonical_route_pattern(candidate)?);
    Ok(left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a == b || a == "*" || b == "*"))
}

const FRAMEWORK_ORDER: &[&str] = &[
    "django",
    "flask",
    "fastapi",
    "go",
    "actix",
    "express",
    "springboot",
    "laravel",
    "aspnet",
    "rails",
    "nestjs",
    "vue-router",
    "react-router",
];

fn preferred_dirs(framework: &str) -> &'static [&'static str] {
    match framework {
        "django" => &["models", "views", "forms"],
        "actix" => &["handler", "handlers", "routes", "services", "service"],
        "express" => &[
            "routes",
            "controllers",
            "handlers",
            "api",
            "middleware",
            "router",
            "services",
            "models",
            "utils",
            "lib",
            "helpers",
        ],
        "go" => &[
            "handler",
            "handlers",
            "api",
            "routes",
            "controller",
            "controllers",
            "service",
            "services",
            "repository",
            "store",
            "pkg",
            "middleware",
            "middlewares",
            "model",
            "models",
            "entity",
            "entities",
            "domain",
        ],
        "springboot" => &[
            "controller",
            "controllers",
            "api",
            "handler",
            "resource",
            "endpoint",
            "service",
            "services",
            "repository",
            "component",
            "manager",
            "facade",
        ],
        "laravel" => &[
            "Http/Controllers",
            "Controllers",
            "routes",
            "handlers",
            "api",
            "Services",
            "Repositories",
            "Models",
            "Providers",
            "Middleware",
        ],
        "aspnet" => &[
            "Controllers",
            "Controllers/Api",
            "Endpoints",
            "Handlers",
            "Services",
            "Repositories",
            "Data",
            "Models",
            "Middleware",
        ],
        "rails" => &[
            "app/models",
            "app/controllers",
            "app/services",
            "models",
            "controllers",
            "app/helpers",
            "lib",
            "config",
        ],
        "nestjs" => &[
            "controllers",
            "services",
            "providers",
            "modules",
            "guards",
            "interceptors",
        ],
        "vue-router" => &[
            "router",
            "routes",
            "views",
            "pages",
            "components",
            "layouts",
        ],
        "react-router" => &["routes", "pages", "components", "layouts", "views", "app"],
        _ => &[],
    }
}

fn path_in_dir(file_path: &str, dir: &str) -> bool {
    file_path.contains(&format!("/{dir}/")) || file_path.contains(&format!("\\{dir}\\"))
}

fn with_confidence(row: &Value, confidence: f64) -> Value {
    let mut row = row.clone();
    if let Some(map) = row.as_object_mut() {
        map.insert("confidence".into(), json!(confidence));
    }
    row
}

pub fn claims_reference(name: String, framework: String) -> Result<bool, String> {
    let parts = name.rsplit('.').next().unwrap_or(&name);
    let ok = match framework.as_str() {
        "django" => {
            parts.ends_with("Model")
                || parts.ends_with("View")
                || parts.ends_with("Form")
                || name == "objects"
                || name == "DoesNotExist"
                || name == "MultipleObjectsReturned"
        }
        "flask" => {
            let lower = parts.to_lowercase();
            ["blueprint", "route", "flask", "current_app", "g"]
                .iter()
                .any(|kw| lower.contains(kw))
        }
        "fastapi" => {
            let lower = parts.to_lowercase();
            [
                "fastapi",
                "apirouter",
                "depends",
                "query",
                "path_param",
                "body_param",
                "header_param",
            ]
            .iter()
            .any(|kw| lower.contains(kw))
        }
        "go" => {
            parts.ends_with("Handler")
                || parts.ends_with("Service")
                || parts.ends_with("Repository")
                || parts.ends_with("Store")
                || parts.ends_with("Middleware")
                || name.starts_with("Handle")
                || name.starts_with("Auth")
                || name.starts_with("Log")
        }
        "actix" => {
            parts.ends_with("_handler")
                || parts.ends_with("_service")
                || name.starts_with("handle_")
                || name.starts_with("get_")
                || name.starts_with("post_")
                || name.starts_with("put_")
                || name.starts_with("delete_")
        }
        "express" => {
            parts.ends_with("Controller")
                || parts.ends_with("Handler")
                || parts.ends_with("Middleware")
                || parts.ends_with("Service")
                || parts.ends_with("Router")
                || name.starts_with("handle")
                || name.starts_with("get")
                || name.starts_with("post")
                || name.starts_with("put")
                || name.starts_with("delete")
        }
        "springboot" => {
            parts.ends_with("Controller")
                || parts.ends_with("Service")
                || parts.ends_with("Repository")
                || parts.ends_with("Component")
                || parts.ends_with("Handler")
                || parts.ends_with("Resource")
                || parts.ends_with("Facade")
                || parts.contains("ServiceImpl")
        }
        "laravel" => {
            parts.ends_with("Controller")
                || parts.ends_with("Service")
                || parts.ends_with("Repository")
                || parts.ends_with("Provider")
                || parts.ends_with("Middleware")
        }
        "aspnet" => {
            parts.ends_with("Controller")
                || parts.ends_with("Service")
                || parts.ends_with("Repository")
                || parts.ends_with("Handler")
                || parts.ends_with("Endpoint")
                || parts.ends_with("Middleware")
        }
        "rails" => {
            parts.ends_with("Controller") || parts.ends_with("Model") || parts.ends_with("Record")
        }
        "nestjs" => {
            parts.ends_with("Controller")
                || parts.ends_with("Service")
                || parts.ends_with("Module")
                || parts.ends_with("Guard")
                || parts.ends_with("Interceptor")
                || parts.ends_with("Provider")
        }
        "vue-router" => name.contains("View") || name.contains("Page") || name.contains("Layout"),
        "react-router" => {
            parts.ends_with("Page")
                || parts.ends_with("View")
                || parts.ends_with("Route")
                || parts.ends_with("Layout")
                || parts.ends_with("Screen")
        }
        other => return Err(format!("unknown framework `{other}`")),
    };
    Ok(ok)
}

pub fn resolve_one(name: &str, framework: &str, candidates: &[Value]) -> Option<Value> {
    if candidates.is_empty() {
        return None;
    }
    if framework == "django" {
        let parts = name.rsplit('.').next().unwrap_or(name);
        let (suffix, hint) = if parts.ends_with("Model") {
            ("Model", "models")
        } else if parts.ends_with("View") {
            ("View", "views")
        } else if parts.ends_with("Form") {
            ("Form", "forms")
        } else {
            return None;
        };
        let base = if parts == suffix {
            parts
        } else {
            &parts[..parts.len() - suffix.len()]
        };
        for candidate in candidates {
            let candidate_name = candidate
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let file_path = candidate
                .get("file_path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // Python's `prefer_in_dir` uses a substring hint, so
            // `app/models.py` matches `models` even though it is a file,
            // not a directory component.
            if candidate_name == base && file_path.contains(hint) {
                return Some(with_confidence(candidate, 0.85));
            }
        }
        return None;
    }
    if framework == "flask" || framework == "fastapi" {
        return None;
    }
    for candidate in candidates {
        let file_path = candidate
            .get("file_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        for dir in preferred_dirs(framework) {
            if path_in_dir(file_path, dir) {
                return Some(with_confidence(candidate, 0.85));
            }
        }
    }
    Some(with_confidence(&candidates[0], 0.65))
}

pub fn resolve_reference(
    name: String,
    candidates: Vec<Value>,
    limit: i64,
) -> Result<Value, String> {
    let mut rows: Vec<Value> = Vec::new();
    for framework in FRAMEWORK_ORDER {
        if !claims_reference(name.clone(), (*framework).to_string())? {
            continue;
        }
        if let Some(resolved) = resolve_one(&name, framework, &candidates) {
            let mut row = resolved;
            if let Some(map) = row.as_object_mut() {
                map.entry("resolved_by".to_string())
                    .or_insert_with(|| json!(framework));
                map.entry("confidence".to_string()).or_insert(json!(0.7));
            }
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| {
        b.get("confidence")
            .and_then(Value::as_f64)
            .partial_cmp(&a.get("confidence").and_then(Value::as_f64))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut seen: Vec<String> = Vec::new();
    let mut deduped = Vec::new();
    for row in rows {
        let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || !seen.iter().any(|value| value == id) {
            if !id.is_empty() {
                seen.push(id.to_string());
            }
            deduped.push(row);
        }
    }
    deduped.truncate(limit.max(0) as usize);
    Ok(Value::Array(deduped))
}

/// Mirror of `resolution.resolve_route` over supplied route/handler rows.
pub fn resolve_route(
    path: String,
    route_candidates: Vec<Value>,
    callees: Vec<Value>,
    limit: i64,
) -> Result<Value, String> {
    let mut rows = Vec::new();
    for route in route_candidates {
        if route.get("kind").and_then(Value::as_str) != Some("route") {
            continue;
        }
        let name = route
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !route_patterns_match(path.clone(), name.to_string())? {
            continue;
        }
        for handler in &callees {
            let kind = handler
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(kind, "function" | "method" | "struct") {
                continue;
            }
            let mut row = handler.clone();
            if let Some(map) = row.as_object_mut() {
                map.entry("resolved_by".to_string())
                    .or_insert_with(|| json!("route-resolution"));
                map.entry("confidence".to_string()).or_insert(json!(0.9));
                map.entry("route".to_string())
                    .or_insert_with(|| route.clone());
            }
            rows.push(row);
        }
    }
    rows.truncate(limit.max(0) as usize);
    Ok(Value::Array(rows))
}

/// Names accepted by the framework resolver family.
pub fn framework_names() -> Vec<&'static str> {
    FRAMEWORK_ORDER.to_vec()
}
