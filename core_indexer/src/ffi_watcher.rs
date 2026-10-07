// Split out of lib.rs (DR-28, P4 hygiene). Move-only: no behavior change.

use pyo3::prelude::*;

use crate::active_config;

// ── File Watcher Bindings ──────────────────────────────────────────────

static GLOBAL_WATCHER: std::sync::LazyLock<
    std::sync::Mutex<Option<crate::fs::watcher::FileWatcher>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Start the file watcher on the given paths. Must have run analyze() first.
///
/// `debounce_ms` was accepted by `coderadar watch --debounce` and by
/// `CodeGraph.watch(...)`, stored, and never read: this binding took only
/// `paths`, so every watcher ran at the 100 ms default no matter what the
/// user asked for. Same for `max_file_size_bytes`, which the config declared
/// and nothing enforced (plan §1.4).
#[pyfunction]
#[pyo3(signature = (paths, debounce_ms=None, max_file_size_bytes=None))]
pub(crate) fn start_watcher(
    paths: Vec<String>,
    debounce_ms: Option<u64>,
    max_file_size_bytes: Option<u64>,
) -> PyResult<()> {
    use crate::fs::watcher::{FileWatcher, WatcherConfig};
    let defaults = WatcherConfig::default();
    // Item 7: the watcher filters with the same baseline + the user's own
    // `[project] exclude`, so an excluded folder never triggers updates.
    let mut exclude_patterns = defaults.exclude_patterns.clone();
    exclude_patterns.extend(active_config().project.exclude.iter().cloned());
    let config = WatcherConfig {
        watch_paths: paths,
        exclude_patterns,
        debounce_ms: debounce_ms.unwrap_or(defaults.debounce_ms),
        max_file_size_bytes: max_file_size_bytes.unwrap_or(defaults.max_file_size_bytes),
    };
    let watcher = FileWatcher::start(config)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    *guard = Some(watcher);
    Ok(())
}

/// Get the next batch of file changes (blocks until events arrive).
#[pyfunction]
pub(crate) fn next_watcher_batch() -> PyResult<Option<Vec<(String, String)>>> {
    // Take the watcher out of the global, call next_batch, put it back
    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    if guard.is_none() {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Watcher not started",
        ));
    }
    let watcher = guard.take().unwrap();
    drop(guard);

    let batch = watcher.next_batch();

    // Put the watcher back
    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    *guard = Some(watcher);

    Ok(batch.map(|b| {
        b.changes
            .into_iter()
            .map(|c| (c.path, format!("{:?}", c.kind)))
            .collect()
    }))
}

/// Stop the file watcher.
#[pyfunction]
pub(crate) fn stop_watcher() -> PyResult<()> {
    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    *guard = None;
    Ok(())
}

/// Get the next batch with a timeout (ms). Returns None if timeout expires.
#[pyfunction]
pub(crate) fn next_watcher_batch_timeout(
    timeout_ms: u64,
) -> PyResult<Option<Vec<(String, String)>>> {
    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    if guard.is_none() {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Watcher not started",
        ));
    }
    let watcher = guard.take().unwrap();
    drop(guard);

    let batch = watcher.next_batch_timeout(timeout_ms);

    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    *guard = Some(watcher);

    Ok(batch.map(|b| {
        b.changes
            .into_iter()
            .map(|c| (c.path, format!("{:?}", c.kind)))
            .collect()
    }))
}
