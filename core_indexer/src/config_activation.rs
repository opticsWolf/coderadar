//! Pure-Rust `.coderadar.toml` activation.
//!
//! Python's `config.activate_config` validates with pydantic and pushes the
//! resulting dict into `set_config`. The Rust binary has no Python layer, so
//! the engine owns the whole path: read the TOML, keep only schema-known keys,
//! validate types, build `GraphConfig`, and publish it as `ACTIVE_CONFIG`.
//!
//! `ffi_config::set_config` uses the same `apply_config` mapping, so the
//! Python and Rust front doors cannot drift (M-FFI-TRANSLATES).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use crate::graph;
use crate::{active_config, ACTIVE_CONFIG};

/// The result of mapping one config object onto `GraphConfig`.
#[derive(Clone, Debug)]
pub struct ActivationReport {
    /// Dotted paths and the values that actually landed.
    pub applied: Vec<(String, Value)>,
    /// Dotted paths that map to nothing in the engine.
    pub ignored: Vec<String>,
    /// The configuration now in force.
    pub config: graph::GraphConfig,
}

fn section<'a>(
    parent: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    match parent.get(key) {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map)),
        Some(_) => Err(format!("config section '{key}' must be a table")),
    }
}

fn typed<T: DeserializeOwned>(
    section: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<T>, String> {
    match section.get(key) {
        None => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| format!("config '{path}': {error}")),
    }
}

/// Apply a config object and publish it process-wide.
///
/// # Errors
/// Returns a dotted-path error when a section is not a table or a leaf has
/// the wrong type. A broken config never partially mutates `ACTIVE_CONFIG`.
pub fn apply_config(value: &Value) -> Result<ActivationReport, String> {
    let root = match value {
        Value::Object(map) => map,
        _ => return Err("config must be a table".to_string()),
    };
    let mut config = graph::GraphConfig::default();
    let mut applied: Vec<(String, Value)> = Vec::new();
    let mut consumed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    macro_rules! take {
        ($section:expr, $key:literal, $path:literal, $target:expr, $ty:ty) => {{
            if let Some(parsed) = typed::<$ty>($section, $key, $path)? {
                let out = serde_json::to_value(&parsed)
                    .map_err(|error| format!("config '{}': {}", $path, error))?;
                applied.push(($path.to_string(), out));
                $target = parsed;
            }
            consumed.insert($path.to_string());
        }};
    }

    if let Some(proj) = section(root, "project")? {
        take!(
            proj,
            "roots",
            "project.roots",
            config.project.roots,
            Vec<String>
        );
        take!(
            proj,
            "exclude",
            "project.exclude",
            config.project.exclude,
            Vec<String>
        );
    }

    if let Some(db) = section(root, "database")? {
        take!(db, "path", "database.path", config.database.path, String);
        take!(
            db,
            "store_source_blobs",
            "database.store_source_blobs",
            config.database.store_source_blobs,
            bool
        );
        if let Some(extra) = typed::<Vec<String>>(db, "blob_exclude", "database.blob_exclude")? {
            let mut merged = config.database.blob_exclude.clone();
            for pattern in extra {
                if !merged.contains(&pattern) {
                    merged.push(pattern);
                }
            }
            merged.sort();
            applied.push((
                "database.blob_exclude".to_string(),
                serde_json::to_value(&merged)
                    .map_err(|error| format!("config 'database.blob_exclude': {error}"))?,
            ));
            config.database.blob_exclude = merged;
            consumed.insert("database.blob_exclude".to_string());
        }
    }

    if let Some(res) = section(root, "resolution")? {
        take!(
            res,
            "min_confidence",
            "resolution.min_confidence",
            config.resolution.min_confidence,
            f32
        );
        if let Some(ig) = section(res, "import_graph")? {
            take!(
                ig,
                "max_import_depth",
                "resolution.import_graph.max_import_depth",
                config.import_graph.max_import_depth,
                usize
            );
            take!(
                ig,
                "include_same_package",
                "resolution.import_graph.include_same_package",
                config.import_graph.include_same_package,
                bool
            );
            take!(
                ig,
                "max_wildcard_hops",
                "resolution.import_graph.max_wildcard_hops",
                config.import_graph.max_wildcard_hops,
                u8
            );
        }
        if let Some(sig) = section(res, "signature")? {
            take!(
                sig,
                "min_score",
                "resolution.signature.min_score",
                config.signature.min_score,
                f32
            );
            take!(
                sig,
                "name_weight",
                "resolution.signature.name_weight",
                config.signature.name_weight,
                f32
            );
            take!(
                sig,
                "arity_weight",
                "resolution.signature.arity_weight",
                config.signature.arity_weight,
                f32
            );
            take!(
                sig,
                "proximity_weight",
                "resolution.signature.proximity_weight",
                config.signature.proximity_weight,
                f32
            );
            take!(
                sig,
                "ambiguous_name_ceiling",
                "resolution.signature.ambiguous_name_ceiling",
                config.signature.ambiguous_name_ceiling,
                usize
            );
        }
        if let Some(lsp) = section(res, "lsp")? {
            take!(
                lsp,
                "enabled",
                "resolution.lsp.enabled",
                config.resolution.lsp.enabled,
                bool
            );
            take!(
                lsp,
                "result_ttl_s",
                "resolution.lsp.result_ttl_s",
                config.resolution.lsp.result_ttl_s,
                u64
            );
            take!(
                lsp,
                "idle_timeout_s",
                "resolution.lsp.idle_timeout_s",
                config.resolution.lsp.idle_timeout_s,
                u64
            );
            take!(
                lsp,
                "timeout_ms",
                "resolution.lsp.timeout_ms",
                config.resolution.lsp.timeout_ms,
                u64
            );
            take!(
                lsp,
                "override_threshold",
                "resolution.lsp.override_threshold",
                config.resolution.lsp.override_threshold,
                f32
            );
            take!(
                lsp,
                "servers",
                "resolution.lsp.servers",
                config.resolution.lsp.servers,
                BTreeMap<String, String>
            );
        }
    }

    if let Some(embed) = section(root, "embedding")? {
        take!(
            embed,
            "model",
            "embedding.model",
            config.embedding.model,
            String
        );
        take!(
            embed,
            "dimension",
            "embedding.dimension",
            config.embedding.dimension,
            usize
        );
        take!(
            embed,
            "max_body_tokens",
            "embedding.max_body_tokens",
            config.embedding.max_body_tokens,
            usize
        );
        take!(
            embed,
            "batch_size",
            "embedding.batch_size",
            config.embedding.batch_size,
            usize
        );
    }

    if let Some(watch) = section(root, "watch")? {
        take!(
            watch,
            "debounce_ms",
            "watch.debounce_ms",
            config.watch.debounce_ms,
            u64
        );
        take!(
            watch,
            "max_file_size_bytes",
            "watch.max_file_size_bytes",
            config.watch.max_file_size_bytes,
            u64
        );
    }

    if let Some(retention) = section(root, "retention")? {
        take!(
            retention,
            "archive_after_days",
            "retention.archive_after_days",
            config.retention.archive_after_days,
            Option<i64>
        );
    }

    if let Some(an) = section(root, "analysis")? {
        take!(
            an,
            "use_cfg_metrics",
            "analysis.use_cfg_metrics",
            config.analysis.use_cfg_metrics,
            bool
        );
    }

    if let Some(m) = section(root, "mutation")? {
        take!(
            m,
            "enabled",
            "mutation.enabled",
            config.mutation.enabled,
            bool
        );
        take!(
            m,
            "default_dry_run",
            "mutation.default_dry_run",
            config.mutation.default_dry_run,
            bool
        );
        take!(
            m,
            "max_files_per_plan",
            "mutation.max_files_per_plan",
            config.mutation.max_files_per_plan,
            usize
        );
        take!(
            m,
            "max_edits_per_plan",
            "mutation.max_edits_per_plan",
            config.mutation.max_edits_per_plan,
            usize
        );
        take!(
            m,
            "max_body_tokens",
            "mutation.max_body_tokens",
            config.mutation.max_body_tokens,
            usize
        );
        take!(
            m,
            "backup_retention_hours",
            "mutation.backup_retention_hours",
            config.mutation.backup_retention_hours,
            u64
        );
        take!(
            m,
            "post_verify",
            "mutation.post_verify",
            config.mutation.post_verify,
            bool
        );
        take!(
            m,
            "max_repair_attempts",
            "mutation.max_repair_attempts",
            config.mutation.max_repair_attempts,
            u32
        );
        take!(
            m,
            "require_clean_git",
            "mutation.require_clean_git",
            config.mutation.require_clean_git,
            bool
        );
        take!(
            m,
            "allow",
            "mutation.allow",
            config.mutation.allow,
            Vec<String>
        );
        take!(
            m,
            "deny",
            "mutation.deny",
            config.mutation.deny,
            Vec<String>
        );
    }

    if let Some(q) = section(root, "query")? {
        take!(
            q,
            "max_depth",
            "query.max_depth",
            config.query.max_depth,
            usize
        );
        take!(
            q,
            "default_top_k",
            "query.default_top_k",
            config.query.default_top_k,
            usize
        );
        take!(
            q,
            "cache_ttl_seconds",
            "query.cache_ttl_seconds",
            config.query.cache_ttl_seconds,
            u64
        );
        take!(
            q,
            "cache_max_size",
            "query.cache_max_size",
            config.query.cache_max_size,
            usize
        );
        take!(
            q,
            "use_rust_graph_for_traversal",
            "query.use_rust_graph_for_traversal",
            config.query.use_rust_graph_for_traversal,
            bool
        );
    }

    let mut leaves = Vec::new();
    leaf_paths(root, "", &mut leaves);
    let mut ignored: Vec<String> = leaves
        .into_iter()
        .filter(|path| !consumed.contains(path))
        .collect();
    ignored.sort();

    *ACTIVE_CONFIG.write() = Arc::new(config.clone());
    Ok(ActivationReport {
        applied,
        ignored,
        config,
    })
}

fn leaf_paths(map: &Map<String, Value>, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Object(sub) => leaf_paths(sub, &path, out),
            _ => out.push(path),
        }
    }
}

// Pydantic's default is to ignore unknown model keys. `activate_config` reads
// raw TOML, so it filters against the same schema before calling `apply_config`;
// `set_config` deliberately does not filter, because its contract is to name
// unmapped keys.
#[derive(Clone, Copy)]
enum Schema {
    Leaf,
    Table(&'static [(&'static str, Schema)]),
}

const IMPORT_GRAPH_SCHEMA: &[(&str, Schema)] = &[
    ("max_import_depth", Schema::Leaf),
    ("include_same_package", Schema::Leaf),
    ("max_wildcard_hops", Schema::Leaf),
];
const SIGNATURE_SCHEMA: &[(&str, Schema)] = &[
    ("min_score", Schema::Leaf),
    ("name_weight", Schema::Leaf),
    ("arity_weight", Schema::Leaf),
    ("proximity_weight", Schema::Leaf),
    ("ambiguous_name_ceiling", Schema::Leaf),
];
const LSP_SCHEMA: &[(&str, Schema)] = &[
    ("enabled", Schema::Leaf),
    ("result_ttl_s", Schema::Leaf),
    ("idle_timeout_s", Schema::Leaf),
    ("timeout_ms", Schema::Leaf),
    ("override_threshold", Schema::Leaf),
    ("servers", Schema::Leaf),
];
const RESOLUTION_SCHEMA: &[(&str, Schema)] = &[
    ("min_confidence", Schema::Leaf),
    ("import_graph", Schema::Table(IMPORT_GRAPH_SCHEMA)),
    ("signature", Schema::Table(SIGNATURE_SCHEMA)),
    ("lsp", Schema::Table(LSP_SCHEMA)),
];
const EMBEDDING_SCHEMA: &[(&str, Schema)] = &[
    ("model", Schema::Leaf),
    ("dimension", Schema::Leaf),
    ("max_body_tokens", Schema::Leaf),
    ("batch_size", Schema::Leaf),
];
const DATABASE_SCHEMA: &[(&str, Schema)] = &[
    ("path", Schema::Leaf),
    ("store_source_blobs", Schema::Leaf),
    ("blob_exclude", Schema::Leaf),
];
const MUTATION_SCHEMA: &[(&str, Schema)] = &[
    ("enabled", Schema::Leaf),
    ("default_dry_run", Schema::Leaf),
    ("max_files_per_plan", Schema::Leaf),
    ("max_edits_per_plan", Schema::Leaf),
    ("max_body_tokens", Schema::Leaf),
    ("backup_retention_hours", Schema::Leaf),
    ("post_verify", Schema::Leaf),
    ("max_repair_attempts", Schema::Leaf),
    ("require_clean_git", Schema::Leaf),
    ("allow", Schema::Leaf),
    ("deny", Schema::Leaf),
];
const RETENTION_SCHEMA: &[(&str, Schema)] = &[("archive_after_days", Schema::Leaf)];
const QUERY_SCHEMA: &[(&str, Schema)] = &[
    ("max_depth", Schema::Leaf),
    ("default_top_k", Schema::Leaf),
    ("cache_ttl_seconds", Schema::Leaf),
    ("cache_max_size", Schema::Leaf),
    ("use_rust_graph_for_traversal", Schema::Leaf),
];
const WATCH_SCHEMA: &[(&str, Schema)] = &[
    ("debounce_ms", Schema::Leaf),
    ("max_file_size_bytes", Schema::Leaf),
];
const ROOT_SCHEMA: &[(&str, Schema)] = &[
    (
        "project",
        Schema::Table(&[("roots", Schema::Leaf), ("exclude", Schema::Leaf)]),
    ),
    ("resolution", Schema::Table(RESOLUTION_SCHEMA)),
    ("embedding", Schema::Table(EMBEDDING_SCHEMA)),
    ("database", Schema::Table(DATABASE_SCHEMA)),
    ("mutation", Schema::Table(MUTATION_SCHEMA)),
    ("retention", Schema::Table(RETENTION_SCHEMA)),
    ("query", Schema::Table(QUERY_SCHEMA)),
    ("watch", Schema::Table(WATCH_SCHEMA)),
    (
        "analysis",
        Schema::Table(&[("use_cfg_metrics", Schema::Leaf)]),
    ),
];

fn filter_by_schema(src: &Map<String, Value>, schema: &[(&str, Schema)]) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, kind) in schema {
        let Some(value) = src.get(*key) else {
            continue;
        };
        match (kind, value) {
            (Schema::Leaf, _) => {
                out.insert((*key).to_string(), value.clone());
            }
            (Schema::Table(sub), Value::Object(map)) => {
                out.insert(
                    (*key).to_string(),
                    Value::Object(filter_by_schema(map, sub)),
                );
            }
            (Schema::Table(_), _) => {
                // Keep the bad value so `apply_config` reports the named path.
                out.insert((*key).to_string(), value.clone());
            }
        }
    }
    out
}

/// Read and activate `<root>/.coderadar.toml`.
///
/// A missing file applies defaults. A broken file applies defaults and reports
/// `config_error` instead of failing startup, matching `activate_config`'s
/// best-effort callers.
///
/// # Errors
/// Only type errors from a usable file are returned; I/O failures are
/// reported in the JSON result as `config_error`.
pub fn activate_root(root: &Path) -> Result<Value, String> {
    let path = root.join(".coderadar.toml");
    let mut config_present = false;
    let mut config_error: Option<String> = None;
    let raw = if !path.exists() {
        json!({})
    } else {
        config_present = true;
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<toml::Value>(&text) {
                Ok(doc) => serde_json::to_value(doc)
                    .map_err(|error| format!("config '{}': {error}", path.display()))?,
                Err(error) => {
                    config_error = Some(format!("{}: {error}", path.display()));
                    json!({})
                }
            },
            Err(error) => {
                config_error = Some(format!("{}: {error}", path.display()));
                json!({})
            }
        }
    };
    let filtered = match raw {
        Value::Object(map) => Value::Object(filter_by_schema(&map, ROOT_SCHEMA)),
        other => other,
    };
    let report = match apply_config(&filtered) {
        Ok(report) => report,
        Err(error) => {
            if config_error.is_none() {
                config_error = Some(error);
            }
            apply_config(&json!({}))?
        }
    };
    Ok(report_json(
        &report,
        config_present,
        config_error.as_deref(),
    ))
}

fn report_json(report: &ActivationReport, present: bool, error: Option<&str>) -> Value {
    let mut applied = Map::new();
    for (path, value) in &report.applied {
        applied.insert(path.clone(), value.clone());
    }
    json!({
        "applied": Value::Object(applied),
        "ignored": report.ignored,
        "config_present": present,
        "config_error": error,
    })
}

/// The live `GraphConfig` as JSON, for pure-Rust callers.
pub fn active_json() -> Value {
    config_json(&active_config())
}

fn config_json(config: &graph::GraphConfig) -> Value {
    let mut servers = Map::new();
    for (key, value) in &config.resolution.lsp.servers {
        servers.insert(key.clone(), Value::String(value.clone()));
    }
    json!({
        "project": {
            "roots": &config.project.roots,
            "exclude": &config.project.exclude,
        },
        "database": {
            "path": &config.database.path,
            "store_source_blobs": config.database.store_source_blobs,
            "blob_exclude": &config.database.blob_exclude,
        },
        "resolution": {
            "min_confidence": config.resolution.min_confidence,
            "import_graph": {
                "max_import_depth": config.import_graph.max_import_depth,
                "include_same_package": config.import_graph.include_same_package,
                "max_wildcard_hops": config.import_graph.max_wildcard_hops,
            },
            "signature": {
                "min_score": config.signature.min_score,
                "name_weight": config.signature.name_weight,
                "arity_weight": config.signature.arity_weight,
                "proximity_weight": config.signature.proximity_weight,
                "ambiguous_name_ceiling": config.signature.ambiguous_name_ceiling,
            },
            "lsp": {
                "enabled": config.resolution.lsp.enabled,
                "result_ttl_s": config.resolution.lsp.result_ttl_s,
                "idle_timeout_s": config.resolution.lsp.idle_timeout_s,
                "timeout_ms": config.resolution.lsp.timeout_ms,
                "override_threshold": config.resolution.lsp.override_threshold,
                "servers": Value::Object(servers),
            },
        },
        "embedding": {
            "model": &config.embedding.model,
            "dimension": config.embedding.dimension,
            "max_body_tokens": config.embedding.max_body_tokens,
            "batch_size": config.embedding.batch_size,
        },
        "database_policy": {
            "store_source_blobs": config.database.store_source_blobs,
        },
        "mutation": {
            "enabled": config.mutation.enabled,
            "default_dry_run": config.mutation.default_dry_run,
            "max_files_per_plan": config.mutation.max_files_per_plan,
            "max_edits_per_plan": config.mutation.max_edits_per_plan,
            "max_body_tokens": config.mutation.max_body_tokens,
            "backup_retention_hours": config.mutation.backup_retention_hours,
            "post_verify": config.mutation.post_verify,
            "max_repair_attempts": config.mutation.max_repair_attempts,
            "require_clean_git": config.mutation.require_clean_git,
            "allow": &config.mutation.allow,
            "deny": &config.mutation.deny,
        },
        "retention": {
            "archive_after_days": config.retention.archive_after_days,
        },
        "query": {
            "max_depth": config.query.max_depth,
            "default_top_k": config.query.default_top_k,
            "cache_ttl_seconds": config.query.cache_ttl_seconds,
            "cache_max_size": config.query.cache_max_size,
            "use_rust_graph_for_traversal": config.query.use_rust_graph_for_traversal,
        },
        "watch": {
            "debounce_ms": config.watch.debounce_ms,
            "max_file_size_bytes": config.watch.max_file_size_bytes,
        },
        "analysis": {
            "use_cfg_metrics": config.analysis.use_cfg_metrics,
        },
    })
}

/// `[retention] archive_after_days` → canonical UTC cutoff stamp.
///
/// # Errors
/// Returns Python's `InvalidRequest` messages for a missing or negative
/// value.
pub fn retention_cutoff(days: Option<i64>) -> Result<String, String> {
    let Some(days) = days else {
        return Err(
            "InvalidRequest: no archive cutoff: pass cutoff=... or set [retention] archive_after_days in .coderadar.toml"
                .to_string(),
        );
    };
    if days < 0 {
        return Err(format!(
            "InvalidRequest: [retention] archive_after_days is negative: {days}"
        ));
    }
    Ok(canonical_stamp_days_ago(days))
}

fn canonical_stamp_days_ago(days: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        - days.saturating_mul(86_400);
    let day = seconds.div_euclid(86_400);
    let within = seconds.rem_euclid(86_400);
    let (year, month, date) = civil_from_days(day);
    format!(
        "{year:04}-{month:02}-{date:02}T{:02}:{:02}:{:02}.000000Z",
        within / 3600,
        (within % 3600) / 60,
        within % 60
    )
}

/// Howard Hinnant's civil-from-days conversion, UTC only.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, date)
}

/// Active `[watch]` settings as `(debounce_ms, max_file_size_bytes)`.
pub fn watch_settings() -> (u64, u64) {
    let config = active_config();
    (config.watch.debounce_ms, config.watch.max_file_size_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_roots_mutation_and_watch_from_json() {
        let report = apply_config(&json!({
            "project": {"roots": ["src/", "lib/"]},
            "mutation": {"enabled": false, "deny": ["vendor/"]},
            "watch": {"debounce_ms": 250},
        }))
        .expect("valid config");
        assert_eq!(report.config.project.roots, vec!["src/", "lib/"]);
        assert!(!report.config.mutation.enabled);
        assert_eq!(report.config.mutation.deny, vec!["vendor/"]);
        assert_eq!(report.config.watch.debounce_ms, 250);
        assert!(report.ignored.is_empty());
    }

    #[test]
    fn unknown_keys_are_reported_by_set_config() {
        let report = apply_config(&json!({"llm": {"provider": "openai"}})).expect("valid config");
        assert_eq!(report.ignored, vec!["llm.provider"]);
    }

    #[test]
    fn schema_filter_drops_unknown_keys() {
        let filtered = filter_by_schema(
            &serde_json::from_value(json!({
                "embedding": {"model": "BAAI/bge-small-en-v1.5", "truncated_dimension": 128},
                "project": {"roots": ["src/"]},
            }))
            .expect("object"),
            ROOT_SCHEMA,
        );
        assert_eq!(
            filtered
                .get("embedding")
                .and_then(|v| v.get("truncated_dimension")),
            None
        );
        assert_eq!(filtered["embedding"]["model"], "BAAI/bge-small-en-v1.5");
    }

    #[test]
    fn wrong_types_name_the_path() {
        let error = apply_config(&json!({"mutation": {"max_edits_per_plan": "lots"}}))
            .expect_err("wrong type");
        assert!(error.contains("mutation.max_edits_per_plan"), "{error}");
    }

    #[test]
    fn broken_toml_reports_error_and_keeps_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".coderadar.toml"), "project = [").expect("write");
        let value = activate_root(dir.path()).expect("activation");
        assert_eq!(value["config_present"], true);
        assert!(value["config_error"].as_str().is_some());
        assert_eq!(active_config().embedding.model, "BAAI/bge-small-en-v1.5");
    }
}
