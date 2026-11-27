//! Process-wide graph handle for the Rust app (library, CLI and MCP).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use serde_json::Value;

use core_indexer::types::ProjectedGraph;

static PROJECTION: OnceLock<RwLock<Option<Arc<ProjectedGraph>>>> = OnceLock::new();
static STORE_PATH: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
static ROOT: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn projection_slot() -> &'static RwLock<Option<Arc<ProjectedGraph>>> {
    PROJECTION.get_or_init(|| RwLock::new(None))
}

fn store_slot() -> &'static RwLock<Option<PathBuf>> {
    STORE_PATH.get_or_init(|| RwLock::new(None))
}

fn root_slot() -> &'static RwLock<Option<PathBuf>> {
    ROOT.get_or_init(|| RwLock::new(None))
}

/// The store path CodeRadar writes by default under a project root.
pub fn default_store_path(root: &Path) -> PathBuf {
    root.join(".coderadar").join("store").join("coderadar.db")
}

/// Resolve `[database] path` from the activated config, relative to `root`.
///
/// Activation happens here as well as at CLI/MCP startup so library callers
/// that go straight to `state::load` still get the project's policy.
fn configured_store_path(root: &Path) -> PathBuf {
    let config = core_indexer::api::active_config_json();
    let configured = config
        .get("database")
        .and_then(|section| section.get("path"))
        .and_then(Value::as_str)
        .unwrap_or(".coderadar/store/coderadar.db");
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Load (or reload) the projection from `root`'s Macrame store.
///
/// # Errors
/// Returns the engine's message when the store is missing or unreadable.
pub fn load(root: &Path) -> Result<(), String> {
    let _activation = core_indexer::api::activate_config(&root.to_string_lossy());
    let db = configured_store_path(root);
    if !db.is_file() {
        return Err(format!(
            "no store at {} — run `coderadar init` first",
            db.display()
        ));
    }
    let projection =
        core_indexer::api::load_projection(&db.to_string_lossy(), Some(&root.to_string_lossy()))?;
    *root_slot().write().map_err(|e| e.to_string())? = Some(root.to_path_buf());
    *store_slot().write().map_err(|e| e.to_string())? = Some(db);
    *projection_slot().write().map_err(|e| e.to_string())? = Some(projection);
    Ok(())
}

/// Load a projection from an explicit Macrame ledger snapshot.
///
/// `root` keys the entity ids; the snapshot need not live at the project's
/// default store path. This backs `coderadar load-snapshot` and the facade's
/// `load()` helper.
pub fn load_from(snapshot: &Path, root: &Path) -> Result<(), String> {
    let _activation = core_indexer::api::activate_config(&root.to_string_lossy());
    if !snapshot.is_file() {
        return Err(format!(
            "no snapshot at {} — pass an existing store",
            snapshot.display()
        ));
    }
    let projection = core_indexer::api::load_projection(
        &snapshot.to_string_lossy(),
        Some(&root.to_string_lossy()),
    )?;
    *root_slot().write().map_err(|e| e.to_string())? = Some(root.to_path_buf());
    *store_slot().write().map_err(|e| e.to_string())? = Some(snapshot.to_path_buf());
    *projection_slot().write().map_err(|e| e.to_string())? = Some(projection);
    Ok(())
}

/// The loaded projection, if any.
pub fn projection() -> Option<Arc<ProjectedGraph>> {
    projection_slot()
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

/// The project root the loaded projection came from.
pub fn root() -> Option<PathBuf> {
    root_slot().read().ok().and_then(|guard| guard.clone())
}

/// The Macrame store path the loaded projection came from.
pub fn store_path() -> Option<PathBuf> {
    store_slot().read().ok().and_then(|guard| guard.clone())
}

/// Forget the loaded graph (used by `init` before a fresh load).
pub fn clear() {
    if let Ok(mut p) = projection_slot().write() {
        *p = None;
    }
}

/// Open the Macrame store backing the loaded projection.
///
/// # Errors
/// Returns the engine's message when the store path is unset or unreadable.
pub fn store() -> Result<core_indexer::storage::CodeGraphStore, String> {
    let path =
        store_path().ok_or_else(|| "no store loaded — run `coderadar init` first".to_string())?;
    core_indexer::storage::CodeGraphStore::open(path.to_string_lossy().as_ref())
        .map_err(|e| format!("{e:?}"))
}
