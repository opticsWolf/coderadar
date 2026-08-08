// CodeRadar — Rust Core Library (spec v3.3)
// PyO3 bindings for the Python layer.

pub mod extract;
pub mod fs;
pub mod graph;
pub mod mutation;
pub mod query;
pub mod resolve;
pub mod types;
pub mod update;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use pyo3::prelude::*;

use crate::graph::{CodeGraph, GraphConfig};
use crate::query::exec::{execute_query, QueryIterator};
use crate::query::grammar::parse_query;
use crate::update::patch::UpdateReport;

// ── Python Module ──────────────────────────────────────────────────────────

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(analyze, m)?)?;
    m.add_function(wrap_pyfunction!(query_graph, m)?)?;
    m.add_function(wrap_pyfunction!(update_file, m)?)?;
    m.add_function(wrap_pyfunction!(plan_body_replacement, m)?)?;
    m.add_function(wrap_pyfunction!(plan_signature_update, m)?)?;
    m.add_function(wrap_pyfunction!(plan_rename, m)?)?;
    m.add_function(wrap_pyfunction!(plan_create_entity, m)?)?;
    m.add_function(wrap_pyfunction!(apply_mutation, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_symbol, m)?)?;
    m.add_function(wrap_pyfunction!(callers_of, m)?)?;
    m.add_function(wrap_pyfunction!(export_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(load_snapshot, m)?)?;
    m.add_class::<PyCodeGraph>()?;
    m.add_class::<QueryIterator>()?;
    Ok(())
}

// ── Internal state ─────────────────────────────────────────────────────────

static GLOBAL_GRAPH: std::sync::LazyLock<RwLock<Option<CodeGraph>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

fn with_graph<F, R>(f: F) -> PyResult<R>
where
    F: FnOnce(&CodeGraph) -> PyResult<R>,
{
    let guard = GLOBAL_GRAPH.read();
    match guard.as_ref() {
        Some(g) => f(g),
        None => Err(pyo3::exceptions::PyRuntimeError::new_err(
            "No graph loaded",
        )),
    }
}

// ── PyO3-exposed Graph wrapper ─────────────────────────────────────────────

#[pyclass(name = "CodeGraph")]
pub struct PyCodeGraph {
    inner: Arc<RwLock<CodeGraph>>,
}

#[pymethods]
impl PyCodeGraph {
    #[new]
    fn new() -> Self {
        let config = GraphConfig::default();
        Self {
            inner: Arc::new(RwLock::new(CodeGraph::new(config))),
        }
    }

    fn query(&self, query_str: &str) -> PyResult<QueryIterator> {
        let guard = self.inner.read();
        let snapshot = guard.snapshot();
        let parsed = parse_query(query_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e))?;
        let rows = execute_query(&snapshot, &parsed);
        Ok(QueryIterator::new(rows))
    }
}

// ── analyze() ──────────────────────────────────────────────────────────────

#[pyfunction]
fn analyze(root: &str) -> PyResult<()> {
    let config = GraphConfig::default();
    let graph = CodeGraph::new(config);
    let mut guard = GLOBAL_GRAPH.write();
    *guard = Some(graph);
    Ok(())
}

// ── query_graph() ──────────────────────────────────────────────────────────

#[pyfunction]
fn query_graph(py: Python<'_>, query_str: &str) -> PyResult<PyObject> {
    with_graph(|graph| {
        let snapshot = graph.snapshot();
        let parsed = parse_query(query_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e))?;
        let rows = execute_query(&snapshot, &parsed);
        let result = rows.into_iter().map(|r| r.into_pyobject(py)).collect::<Vec<_>>();
        Ok(result.into_py(py))
    })
}

// ── update_file() ──────────────────────────────────────────────────────────

#[pyfunction]
fn update_file(_file_path: &str, _content: Option<&str>, _force: Option<bool>) -> PyResult<()> {
    Ok(())
}

// ── Mutation planning ──────────────────────────────────────────────────────

#[pyfunction]
fn plan_body_replacement(
    _entity_id: &str, _new_body: &str,
    _expected_hash: Option<String>, _dry_run: Option<bool>,
) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn plan_signature_update(
    _entity_id: &str, _new_signature: &str,
    _call_site_values: Option<HashMap<String, String>>,
    _inject_defaults: Option<bool>, _dry_run: Option<bool>,
) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn plan_rename(
    _entity_id: &str, _new_name: &str,
    _include_strings: Option<bool>, _dry_run: Option<bool>,
) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn plan_create_entity(
    _target_file: &str, _anchor: &str, _code: &str, _dry_run: Option<bool>,
) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn apply_mutation(_plan_json: &str) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn resolve_symbol(_qualified_name: &str) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn callers_of(_qualified_name: &str) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn export_snapshot(_path: &str) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn load_snapshot(_path: &str) -> PyResult<()> { Ok(()) }
