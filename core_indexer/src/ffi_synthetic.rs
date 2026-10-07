// Split out of lib.rs (DR-28, P4 hygiene). Move-only: no behavior change.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::ffi_convert::{class_to_dict, function_to_dict};
use crate::{with_graph, GLOBAL_GRAPH};

// ── v3.6: Synthetic Edge Registration ────────────────────────────────────

/// Register a synthetic edge from framework resolvers (Django/Flask/FastAPI).
///
/// Framework resolvers produce edges like route→handler that aren't
/// tree-sitter-extracted. This function merges them into the live graph
/// so agents can trace them via callers_of / callees_of / explore.
#[pyfunction]
pub(crate) fn register_synthetic_edge(
    source_id: &str,
    target_id: &str,
    kind: &str,
) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    graph
        .register_synthetic_edge(source_id, target_id, kind)
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    Ok(dict.into())
}

/// Register framework routes (§1.3, DR-10): nodes + route→handler edges
/// with scoped diff-retire, in one projection commit.
///
/// `nodes` are `(id, pattern, file_path, handler_id, methods, framework)`;
/// `edges` are `(source_id, target_id, kind)`; `scope` is `"full"` or a
/// root-relative file path. Returns `{routes_upserted, routes_retired,
/// pairs_retired}`.
#[pyfunction]
pub(crate) fn register_synthetic_routes(
    nodes: Vec<(String, String, String, String, Vec<String>, String)>,
    edges: Vec<(String, String, String)>,
    scope: &str,
) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    let (upserted, retired, pairs) = graph
        .register_synthetic_routes(nodes, edges, scope)
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    dict.set_item("routes_upserted", upserted)?;
    dict.set_item("routes_retired", retired)?;
    dict.set_item("pairs_retired", pairs)?;
    Ok(dict.into())
}

/// Register many synthetic edges in one pass.
///
/// `edges` is a list of `(source_id, target_id, kind)`. The framework
/// resolvers emit one edge per route/handler pair; the single-edge call clones
/// the whole projection each time.
#[pyfunction]
pub(crate) fn register_synthetic_edges_bulk(
    edges: Vec<(String, String, String)>,
) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    let registered = graph
        .register_synthetic_edges_bulk(edges)
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    dict.set_item("registered", registered)?;
    Ok(dict.into())
}

/// Store an embedding vector on a function entity in the projected graph.
///
/// Called from Python's compute_embeddings() pipeline. The embedding is
/// written directly into the in-memory Function.embedding field, making it
/// immediately available for search_similar() queries.
/// content_hash: xxHash64 hex of the entity body — used for incremental dedup.
#[pyfunction]
pub(crate) fn set_embedding(
    entity_id: &str,
    embedding: Vec<f64>,
    content_hash: &str,
) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    graph
        .set_embedding(entity_id, &embedding, content_hash)
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    Ok(dict.into())
}

/// Store many embeddings in one pass.
///
/// `entries` is a list of `(entity_id, embedding, content_hash)`. Each
/// `set_embedding` call clones the entire projection, so embedding N entities
/// one at a time is O(N²); this clones once. Returns `applied` and the ids
/// that matched no entity.
#[pyfunction]
pub(crate) fn set_embeddings_bulk(entries: Vec<(String, Vec<f64>, String)>) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    let (applied, missing) = graph
        .set_embeddings_bulk(entries)
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    dict.set_item("applied", applied)?;
    dict.set_item("missing", missing)?;
    Ok(dict.into())
}

/// Clear every stored embedding vector; returns `{"cleared"}`.
///
/// Maintenance primitive for the DR-11 model-switch story
/// (`compute_embeddings(recompute=True)` calls this before regenerating).
#[pyfunction]
pub(crate) fn clear_all_embeddings(py: Python<'_>) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    let cleared = graph.clear_all_embeddings();
    let dict = PyDict::new(py);
    dict.set_item("cleared", cleared)?;
    Ok(dict.into())
}

/// Resolve a module's children (classes, functions) to full entity dicts.
///
/// Module dicts carry EntityId lists for `classes`, `functions`, etc.
/// This function resolves those IDs to the full entity representation.
#[pyfunction]
pub(crate) fn module_children(py: Python<'_>, module_id: &str) -> PyResult<PyObject> {
    with_graph(|_graph, snap| {
        let module = snap.modules.get(module_id).ok_or_else(|| {
            pyo3::exceptions::PyKeyError::new_err(format!("Module not found: {}", module_id))
        })?;

        let dict = PyDict::new(py);
        dict.set_item("module_id", module_id)?;

        // Resolve classes
        let classes: Vec<PyObject> = module
            .classes
            .iter()
            .filter_map(|cid| {
                snap.classes
                    .get(cid)
                    .and_then(|c| class_to_dict(py, c).ok())
            })
            .collect();
        dict.set_item("classes", classes)?;

        // Resolve functions
        let functions: Vec<PyObject> = module
            .functions
            .iter()
            .filter_map(|fid| {
                snap.functions
                    .get(fid)
                    .and_then(|f| function_to_dict(py, f).ok())
            })
            .collect();
        dict.set_item("functions", functions)?;

        // Resolve imports
        let imports: Vec<PyObject> = module
            .imports
            .iter()
            .filter_map(|iid| {
                snap.imports.get(iid).map(|i| {
                    let d = PyDict::new(py);
                    let _ = d.set_item("id", &i.id);
                    let _ = d.set_item("raw", &i.raw);
                    let _ = d.set_item("kind", format!("{:?}", i.kind));
                    let _ = d.set_item("line", i.line);
                    d.into()
                })
            })
            .collect();
        dict.set_item("imports", imports)?;

        // Resolve constants
        let constants: Vec<PyObject> = module
            .constants
            .iter()
            .filter_map(|cid| {
                snap.constants.get(cid).map(|c| {
                    let d = PyDict::new(py);
                    let _ = d.set_item("id", &c.id);
                    let _ = d.set_item("name", &c.name);
                    d.into()
                })
            })
            .collect();
        dict.set_item("constants", constants)?;

        Ok(dict.into())
    })
}

/// Drop every stored embedding for the entities of one file (called before
/// re-embedding it). Returns `{"ok": True}`; raises `RuntimeError` when no
/// graph is loaded.
#[pyfunction]
pub(crate) fn clear_embeddings_for_file(py: Python<'_>, file_path: &str) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard
        .as_mut()
        .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("No graph loaded"))?;
    graph.clear_embeddings_for_file(file_path);
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    Ok(dict.into())
}

/// v0.5: Set a module's `__all__` star-export names list.
/// Called from Python after static `__all__` analysis (exports.py).
/// Enables resolution of `from X import *` wildcard imports.
#[pyfunction]
pub(crate) fn set_module_star_exports(module_id: &str, names: Vec<String>) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    graph.set_module_star_exports(module_id, names);
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    Ok(dict.into())
}

/// Set `__all__` for many modules in one pass.
///
/// `entries` is a list of `(module_id, names)`. The per-module call clones the
/// whole projection each time; analyze() has one module with `__all__` per
/// file, so that is a clone per file.
#[pyfunction]
pub(crate) fn set_module_star_exports_bulk(
    entries: Vec<(String, Vec<String>)>,
) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("No graph loaded — run coderadar analyze first")
    })?;
    let applied = graph.set_module_star_exports_bulk(entries);
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    dict.set_item("applied", applied)?;
    Ok(dict.into())
}
