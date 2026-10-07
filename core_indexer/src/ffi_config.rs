// Split out of lib.rs (DR-28, P4 hygiene). Move-only: no behavior change.

use std::collections::HashSet;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::graph;
use crate::{active_config, ACTIVE_CONFIG};

// -- Configuration (plan section 3) -----------------------------------------

/// Fetch a sub-table, or None when absent. A non-table value is an error.
pub(crate) fn cfg_section<'py>(
    parent: &Bound<'py, PyDict>,
    key: &str,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    match parent.get_item(key)? {
        None => Ok(None),
        Some(v) => v.downcast_into::<PyDict>().map(Some).map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(format!(
                "config section '{}' must be a table",
                key
            ))
        }),
    }
}

/// Fetch and convert one key, naming the dotted path when the type is wrong.
pub(crate) fn cfg_value<'py, T: FromPyObject<'py>>(
    section: &Bound<'py, PyDict>,
    key: &str,
    path: &str,
) -> PyResult<Option<T>> {
    match section.get_item(key)? {
        None => Ok(None),
        Some(v) => v.extract::<T>().map(Some).map_err(|e| {
            pyo3::exceptions::PyTypeError::new_err(format!("config '{}': {}", path, e))
        }),
    }
}

/// Every leaf path in a nested config dict, dotted.
pub(crate) fn cfg_leaf_paths(d: &Bound<'_, PyDict>, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in d.iter() {
        let key = k.extract::<String>().unwrap_or_default();
        let path = if prefix.is_empty() {
            key
        } else {
            format!("{}.{}", prefix, key)
        };
        match v.downcast_into::<PyDict>() {
            Ok(sub) => cfg_leaf_paths(&sub, &path, out),
            Err(_) => out.push(path),
        }
    }
}

/// Push a `.coderadar.toml`-shaped dict into the process configuration.
///
/// The Python layer owns loading and schema validation (pydantic gives better
/// errors than anything worth writing here); this maps the result onto
/// `GraphConfig` for every consumer in the process.
///
/// Returns `{"applied": {...}, "ignored": [...]}`. `applied` is what landed on
/// `GraphConfig`; `ignored` names keys the caller sent that map to nothing, so
/// a config full of aspirational knobs reports itself instead of appearing to
/// work. Of the applied keys, the ones a consumer actually reads today are the
/// `mutation.*` policy gate and `resolution.import_graph.*`; the rest are
/// carried on `GraphConfig` but not yet read by any live path.
#[pyfunction]
pub(crate) fn set_config(py: Python<'_>, cfg: &Bound<'_, PyDict>) -> PyResult<PyObject> {
    let mut c = graph::GraphConfig::default();
    let applied = PyDict::new(py);
    let mut consumed: HashSet<String> = HashSet::new();

    macro_rules! take {
        ($section:expr, $key:literal, $path:literal, $target:expr, $ty:ty) => {
            if let Some(v) = cfg_value::<$ty>(&$section, $key, $path)? {
                applied.set_item($path, v.clone())?;
                $target = v;
            }
            consumed.insert($path.to_string());
        };
    }

    if let Some(proj) = cfg_section(cfg, "project")? {
        take!(proj, "roots", "project.roots", c.project.roots, Vec<String>);
        take!(
            proj,
            "exclude",
            "project.exclude",
            c.project.exclude,
            Vec<String>
        );
    }

    if let Some(db) = cfg_section(cfg, "database")? {
        take!(db, "path", "database.path", c.database.path, String);
        take!(
            db,
            "store_source_blobs",
            "database.store_source_blobs",
            c.database.store_source_blobs,
            bool
        );
        // Merge, not replace: user patterns UNION the built-in secret
        // defaults (see SECRET_BLOB_EXCLUDE_DEFAULTS) so adding one pattern
        // can never silently drop the secret belt. Full off is the
        // kill-switch above, not an empty list.
        if let Some(extra) = cfg_value::<Vec<String>>(&db, "blob_exclude", "database.blob_exclude")?
        {
            let mut merged = c.database.blob_exclude.clone();
            for pat in extra {
                if !merged.contains(&pat) {
                    merged.push(pat);
                }
            }
            merged.sort();
            applied.set_item("database.blob_exclude", merged.clone())?;
            c.database.blob_exclude = merged;
            consumed.insert("database.blob_exclude".to_string());
        }
    }

    if let Some(res) = cfg_section(cfg, "resolution")? {
        take!(
            res,
            "min_confidence",
            "resolution.min_confidence",
            c.resolution.min_confidence,
            f32
        );

        if let Some(ig) = cfg_section(&res, "import_graph")? {
            take!(
                ig,
                "max_import_depth",
                "resolution.import_graph.max_import_depth",
                c.import_graph.max_import_depth,
                usize
            );
            take!(
                ig,
                "include_same_package",
                "resolution.import_graph.include_same_package",
                c.import_graph.include_same_package,
                bool
            );
            take!(
                ig,
                "max_wildcard_hops",
                "resolution.import_graph.max_wildcard_hops",
                c.import_graph.max_wildcard_hops,
                u8
            );
        }
        if let Some(sig) = cfg_section(&res, "signature")? {
            take!(
                sig,
                "min_score",
                "resolution.signature.min_score",
                c.signature.min_score,
                f32
            );
            take!(
                sig,
                "name_weight",
                "resolution.signature.name_weight",
                c.signature.name_weight,
                f32
            );
            take!(
                sig,
                "arity_weight",
                "resolution.signature.arity_weight",
                c.signature.arity_weight,
                f32
            );
            take!(
                sig,
                "proximity_weight",
                "resolution.signature.proximity_weight",
                c.signature.proximity_weight,
                f32
            );
            take!(
                sig,
                "ambiguous_name_ceiling",
                "resolution.signature.ambiguous_name_ceiling",
                c.signature.ambiguous_name_ceiling,
                usize
            );
        }
    }

    if let Some(an) = cfg_section(cfg, "analysis")? {
        take!(
            an,
            "use_cfg_metrics",
            "analysis.use_cfg_metrics",
            c.analysis.use_cfg_metrics,
            bool
        );
    }

    if let Some(m) = cfg_section(cfg, "mutation")? {
        take!(m, "enabled", "mutation.enabled", c.mutation.enabled, bool);
        take!(
            m,
            "default_dry_run",
            "mutation.default_dry_run",
            c.mutation.default_dry_run,
            bool
        );
        take!(
            m,
            "max_files_per_plan",
            "mutation.max_files_per_plan",
            c.mutation.max_files_per_plan,
            usize
        );
        take!(
            m,
            "max_edits_per_plan",
            "mutation.max_edits_per_plan",
            c.mutation.max_edits_per_plan,
            usize
        );
        take!(
            m,
            "max_body_tokens",
            "mutation.max_body_tokens",
            c.mutation.max_body_tokens,
            usize
        );
        take!(
            m,
            "backup_retention_hours",
            "mutation.backup_retention_hours",
            c.mutation.backup_retention_hours,
            u64
        );
        take!(
            m,
            "post_verify",
            "mutation.post_verify",
            c.mutation.post_verify,
            bool
        );
        take!(
            m,
            "max_repair_attempts",
            "mutation.max_repair_attempts",
            c.mutation.max_repair_attempts,
            u32
        );
        take!(
            m,
            "require_clean_git",
            "mutation.require_clean_git",
            c.mutation.require_clean_git,
            bool
        );
        take!(m, "allow", "mutation.allow", c.mutation.allow, Vec<String>);
        take!(m, "deny", "mutation.deny", c.mutation.deny, Vec<String>);
    }

    if let Some(q) = cfg_section(cfg, "query")? {
        take!(q, "max_depth", "query.max_depth", c.query.max_depth, usize);
        take!(
            q,
            "default_top_k",
            "query.default_top_k",
            c.query.default_top_k,
            usize
        );
        take!(
            q,
            "cache_ttl_seconds",
            "query.cache_ttl_seconds",
            c.query.cache_ttl_seconds,
            u64
        );
        take!(
            q,
            "cache_max_size",
            "query.cache_max_size",
            c.query.cache_max_size,
            usize
        );
        take!(
            q,
            "use_rust_graph_for_traversal",
            "query.use_rust_graph_for_traversal",
            c.query.use_rust_graph_for_traversal,
            bool
        );
    }

    let mut leaves = Vec::new();
    cfg_leaf_paths(cfg, "", &mut leaves);
    let mut ignored: Vec<String> = leaves
        .into_iter()
        .filter(|p| !consumed.contains(p))
        .collect();
    ignored.sort();

    *ACTIVE_CONFIG.write() = Arc::new(c);

    let out = PyDict::new(py);
    out.set_item("applied", applied)?;
    out.set_item("ignored", ignored)?;
    Ok(out.into())
}

/// The configuration currently in force, as a dict.
#[pyfunction]
pub(crate) fn get_config(py: Python<'_>) -> PyResult<PyObject> {
    let c = active_config();
    let out = PyDict::new(py);

    let project = PyDict::new(py);
    project.set_item("roots", c.project.roots.clone())?;
    project.set_item("exclude", c.project.exclude.clone())?;
    out.set_item("project", project)?;

    let database = PyDict::new(py);
    database.set_item("path", c.database.path.clone())?;
    database.set_item("store_source_blobs", c.database.store_source_blobs)?;
    database.set_item("blob_exclude", c.database.blob_exclude.clone())?;
    out.set_item("database", database)?;

    let resolution = PyDict::new(py);
    resolution.set_item("min_confidence", c.resolution.min_confidence)?;
    let import_graph = PyDict::new(py);
    import_graph.set_item("max_import_depth", c.import_graph.max_import_depth)?;
    import_graph.set_item("include_same_package", c.import_graph.include_same_package)?;
    import_graph.set_item("max_wildcard_hops", c.import_graph.max_wildcard_hops)?;
    resolution.set_item("import_graph", import_graph)?;
    out.set_item("resolution", resolution)?;

    let mutation = PyDict::new(py);
    mutation.set_item("enabled", c.mutation.enabled)?;
    mutation.set_item("default_dry_run", c.mutation.default_dry_run)?;
    mutation.set_item("max_files_per_plan", c.mutation.max_files_per_plan)?;
    mutation.set_item("max_edits_per_plan", c.mutation.max_edits_per_plan)?;
    mutation.set_item("require_clean_git", c.mutation.require_clean_git)?;
    mutation.set_item("allow", c.mutation.allow.clone())?;
    mutation.set_item("deny", c.mutation.deny.clone())?;
    out.set_item("mutation", mutation)?;

    let analysis = PyDict::new(py);
    analysis.set_item("use_cfg_metrics", c.analysis.use_cfg_metrics)?;
    out.set_item("analysis", analysis)?;

    Ok(out.into())
}
