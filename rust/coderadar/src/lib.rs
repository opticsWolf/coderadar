//! Python bindings for `py_agent/src/coderadar/ops.py`: generated from the signatures in `port.rs` by rust-anything.
//! Regenerate with `rust-anything refresh` after changing them.

#![allow(clippy::too_many_arguments)]

use pyo3::prelude::*;

// The Python API's error taxonomy is part of the contract: callers catch
// `NoIndex`, `InvalidRequest`, etc. by class, not by message text.
pyo3::create_exception!(coderadar, OpError, pyo3::exceptions::PyException);
pyo3::create_exception!(coderadar, NoExtension, OpError);
pyo3::create_exception!(coderadar, NoIndex, OpError);
pyo3::create_exception!(coderadar, InvalidRequest, OpError);
pyo3::create_exception!(coderadar, TemporalUnsupported, InvalidRequest);
pyo3::create_exception!(coderadar, NotFound, OpError);
pyo3::create_exception!(coderadar, EngineError, OpError);
pyo3::create_exception!(coderadar, MissingDependency, OpError);
pyo3::create_exception!(coderadar, NoEmbeddings, OpError);
pyo3::create_exception!(coderadar, MutationFailed, OpError);
pyo3::create_exception!(coderadar, ContentUnavailable, OpError);

pub mod annotations_ops;
pub mod cli;
pub mod js;
pub mod mcp;
pub mod port;
pub mod pyconv;
pub mod pynum;
pub mod render;
pub mod state;

#[allow(unused_imports)]
use pynum::Num;

fn error(message: String) -> PyErr {
    use pyo3::exceptions as e;
    let (kind, text) = message.split_once(": ").unwrap_or(("", message.as_str()));
    let text = text.to_string();
    match kind {
        "TypeError" => e::PyTypeError::new_err(text),
        "KeyError" => e::PyKeyError::new_err(text),
        "IndexError" => e::PyIndexError::new_err(text),
        "OverflowError" => e::PyOverflowError::new_err(text),
        "ZeroDivisionError" => e::PyZeroDivisionError::new_err(text),
        "AttributeError" => e::PyAttributeError::new_err(text),
        "RuntimeError" => e::PyRuntimeError::new_err(text),
        "NotImplementedError" => e::PyNotImplementedError::new_err(text),
        "LookupError" => e::PyLookupError::new_err(text),
        "ArithmeticError" => e::PyArithmeticError::new_err(text),
        "UnicodeError" => e::PyUnicodeError::new_err(text),
        "ValueError" => e::PyValueError::new_err(text),
        "NoIndex" => NoIndex::new_err(text),
        "InvalidRequest" => InvalidRequest::new_err(text),
        "NotFound" => NotFound::new_err(text),
        "EngineError" => EngineError::new_err(text),
        "MissingDependency" => MissingDependency::new_err(text),
        "NoEmbeddings" => NoEmbeddings::new_err(text),
        "MutationFailed" => MutationFailed::new_err(text),
        "ContentUnavailable" => ContentUnavailable::new_err(text),
        "TemporalUnsupported" => TemporalUnsupported::new_err(text),
        _ => e::PyValueError::new_err(message),
    }
}

fn friendly_entity_id_inner(entity_id: String) -> Result<String, String> {
    let out = port::friendly_entity_id(entity_id)?;
    Ok(out)
}

/// `friendly_entity_id`.
#[pyfunction]
#[pyo3(name = "friendly_entity_id", signature = (entity_id))]
fn friendly_entity_id(entity_id: String) -> PyResult<String> {
    friendly_entity_id_inner(entity_id).map_err(error)
}

fn canonical_entity_id_inner(entity_id: String) -> Result<String, String> {
    let out = port::canonical_entity_id(entity_id)?;
    Ok(out)
}

/// `canonical_entity_id`.
#[pyfunction]
#[pyo3(name = "canonical_entity_id", signature = (entity_id))]
fn canonical_entity_id(entity_id: String) -> PyResult<String> {
    canonical_entity_id_inner(entity_id).map_err(error)
}

fn display_file_inner(d: Bound<'_, PyAny>) -> Result<String, String> {
    let d: serde_json::Value = crate::pyconv::FromPy::from_py(&d)?;
    let out = port::display_file(d)?;
    Ok(out)
}

/// `display_file`.
#[pyfunction]
#[pyo3(name = "display_file", signature = (d))]
fn display_file(d: Bound<'_, PyAny>) -> PyResult<String> {
    display_file_inner(d).map_err(error)
}

fn find_entity_inner(entity_id: String) -> Result<Option<serde_json::Value>, String> {
    let out = port::find_entity(entity_id)?;
    Ok(out)
}

/// `find_entity`.
#[pyfunction]
#[pyo3(name = "find_entity", signature = (entity_id))]
fn find_entity(py: Python<'_>, entity_id: String) -> PyResult<Py<PyAny>> {
    find_entity_inner(entity_id)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn callers_inner(entity_id: String) -> Result<Vec<serde_json::Value>, String> {
    let out = port::callers(entity_id)?;
    Ok(out)
}

/// `callers`.
#[pyfunction]
#[pyo3(name = "callers", signature = (entity_id))]
fn callers(py: Python<'_>, entity_id: String) -> PyResult<Py<PyAny>> {
    callers_inner(entity_id)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn callees_inner(entity_id: String) -> Result<Vec<serde_json::Value>, String> {
    let out = port::callees(entity_id)?;
    Ok(out)
}

/// `callees`.
#[pyfunction]
#[pyo3(name = "callees", signature = (entity_id))]
fn callees(py: Python<'_>, entity_id: String) -> PyResult<Py<PyAny>> {
    callees_inner(entity_id)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn suggest_entities_inner(entity_id: String, limit: i64) -> Result<Vec<serde_json::Value>, String> {
    let out = port::suggest_entities(entity_id, limit)?;
    Ok(out)
}

/// `suggest_entities`.
#[pyfunction]
#[pyo3(name = "suggest_entities", signature = (entity_id, limit=3))]
fn suggest_entities(py: Python<'_>, entity_id: String, limit: i64) -> PyResult<Py<PyAny>> {
    suggest_entities_inner(entity_id, limit)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn search_inner(
    query: String,
    kind: Option<String>,
    top_k: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let out = port::search(query, kind, top_k)?;
    Ok(out)
}

/// `search`.
#[pyfunction]
#[pyo3(name = "search", signature = (query, kind=None, top_k=10))]
fn search(py: Python<'_>, query: String, kind: Option<String>, top_k: i64) -> PyResult<Py<PyAny>> {
    search_inner(query, kind, top_k)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn search_symbols_inner(
    query: String,
    top_k: i64,
    raw: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let out = port::search_symbols(query, top_k, raw)?;
    Ok(out)
}

/// `search_symbols`.
#[pyfunction]
#[pyo3(name = "search_symbols", signature = (query, top_k=10, raw=false))]
fn search_symbols(py: Python<'_>, query: String, top_k: i64, raw: bool) -> PyResult<Py<PyAny>> {
    search_symbols_inner(query, top_k, raw)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn node_inner(entity_id: String, include_neighbors: bool) -> Result<serde_json::Value, String> {
    let out = port::node(entity_id, include_neighbors)?;
    Ok(out)
}

/// `node`.
#[pyfunction]
#[pyo3(name = "node", signature = (entity_id, include_neighbors=false))]
fn node(py: Python<'_>, entity_id: String, include_neighbors: bool) -> PyResult<Py<PyAny>> {
    node_inner(entity_id, include_neighbors)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn affected_inner(entity_id: String, max_depth: i64) -> Result<serde_json::Value, String> {
    let out = port::affected(entity_id, max_depth)?;
    Ok(out)
}

/// `affected`.
#[pyfunction]
#[pyo3(name = "affected", signature = (entity_id, max_depth=5))]
fn affected(py: Python<'_>, entity_id: String, max_depth: i64) -> PyResult<Py<PyAny>> {
    affected_inner(entity_id, max_depth)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn module_children_inner(module_id: String) -> Result<serde_json::Value, String> {
    let out = port::module_children(module_id)?;
    Ok(out)
}

/// `module_children`.
#[pyfunction]
#[pyo3(name = "module_children", signature = (module_id))]
fn module_children(py: Python<'_>, module_id: String) -> PyResult<Py<PyAny>> {
    module_children_inner(module_id)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn resolve_inner(name: String, limit: i64) -> Result<serde_json::Value, String> {
    let out = port::resolve(name, limit)?;
    Ok(out)
}

/// `resolve`.
#[pyfunction]
#[pyo3(name = "resolve", signature = (name, limit=5))]
fn resolve(py: Python<'_>, name: String, limit: i64) -> PyResult<Py<PyAny>> {
    resolve_inner(name, limit)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn normalize_query_spelling_inner(query: String) -> Result<String, String> {
    let out = port::normalize_query_spelling(query)?;
    Ok(out)
}

/// `normalize_query_spelling`.
#[pyfunction]
#[pyo3(name = "normalize_query_spelling", signature = (query))]
fn normalize_query_spelling(query: String) -> PyResult<String> {
    normalize_query_spelling_inner(query).map_err(error)
}

fn parse_names_inner(query: String, symbols: Bound<'_, PyAny>) -> Result<Vec<String>, String> {
    let symbols: Option<Vec<String>> = crate::pyconv::FromPy::from_py(&symbols)?;
    let out = port::parse_names(query, symbols)?;
    Ok(out)
}

/// `parse_names`.
#[pyfunction]
#[pyo3(name = "parse_names", signature = (query, symbols=None))]
fn parse_names(
    py: Python<'_>,
    query: String,
    symbols: Option<Bound<'_, PyAny>>,
) -> PyResult<Vec<String>> {
    let symbols = symbols.unwrap_or_else(|| py.None().into_bound(py));
    parse_names_inner(query, symbols).map_err(error)
}

fn resolve_names_inner(names: Vec<String>) -> Result<Vec<serde_json::Value>, String> {
    let out = port::resolve_names(names)?;
    Ok(out)
}

/// `resolve_names`.
#[pyfunction]
#[pyo3(name = "resolve_names", signature = (names))]
fn resolve_names(py: Python<'_>, names: Vec<String>) -> PyResult<Py<PyAny>> {
    resolve_names_inner(names)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn explore_inner(
    query: String,
    symbols: Bound<'_, PyAny>,
    direction: String,
    max_files: i64,
) -> Result<serde_json::Value, String> {
    let symbols: Option<Vec<String>> = crate::pyconv::FromPy::from_py(&symbols)?;
    let out = port::explore(query, symbols, direction, max_files)?;
    Ok(out)
}

/// `explore`.
#[pyfunction]
#[pyo3(name = "explore", signature = (query=String::from(""), symbols=None, direction=String::from("both"), max_files=8))]
fn explore(
    py: Python<'_>,
    query: String,
    symbols: Option<Bound<'_, PyAny>>,
    direction: String,
    max_files: i64,
) -> PyResult<Py<PyAny>> {
    let symbols = symbols.unwrap_or_else(|| py.None().into_bound(py));
    explore_inner(query, symbols, direction, max_files)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn traverse_inner(
    entity_id: String,
    direction: String,
    edge_kinds: Bound<'_, PyAny>,
    max_depth: i64,
) -> Result<serde_json::Value, String> {
    let edge_kinds: Option<Vec<String>> = crate::pyconv::FromPy::from_py(&edge_kinds)?;
    let out = port::traverse(entity_id, direction, edge_kinds, max_depth)?;
    Ok(out)
}

/// `traverse`.
#[pyfunction]
#[pyo3(name = "traverse", signature = (entity_id, direction=String::from("both"), edge_kinds=None, max_depth=3))]
fn traverse(
    py: Python<'_>,
    entity_id: String,
    direction: String,
    edge_kinds: Option<Bound<'_, PyAny>>,
    max_depth: i64,
) -> PyResult<Py<PyAny>> {
    let edge_kinds = edge_kinds.unwrap_or_else(|| py.None().into_bound(py));
    traverse_inner(entity_id, direction, edge_kinds, max_depth)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn query_inner(query: String) -> Result<Vec<serde_json::Value>, String> {
    let out = port::query(query)?;
    Ok(out)
}

/// `query`.
#[pyfunction]
#[pyo3(name = "query", signature = (query))]
fn query(py: Python<'_>, query: String) -> PyResult<Py<PyAny>> {
    query_inner(query)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn canonical_file_path_inner(file_path: String) -> Result<String, String> {
    let out = port::canonical_file_path(file_path)?;
    Ok(out)
}

/// `canonical_file_path`.
#[pyfunction]
#[pyo3(name = "canonical_file_path", signature = (file_path))]
fn canonical_file_path(file_path: String) -> PyResult<String> {
    canonical_file_path_inner(file_path).map_err(error)
}

#[pymodule]
#[pyo3(name = "_native")]
fn native_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(friendly_entity_id, m)?)?;
    m.add_function(wrap_pyfunction!(canonical_entity_id, m)?)?;
    m.add_function(wrap_pyfunction!(display_file, m)?)?;
    m.add_function(wrap_pyfunction!(find_entity, m)?)?;
    m.add_function(wrap_pyfunction!(callers, m)?)?;
    m.add_function(wrap_pyfunction!(callees, m)?)?;
    m.add_function(wrap_pyfunction!(suggest_entities, m)?)?;
    m.add_function(wrap_pyfunction!(search, m)?)?;
    m.add_function(wrap_pyfunction!(search_symbols, m)?)?;
    m.add_function(wrap_pyfunction!(node, m)?)?;
    m.add_function(wrap_pyfunction!(affected, m)?)?;
    m.add_function(wrap_pyfunction!(module_children, m)?)?;
    m.add_function(wrap_pyfunction!(resolve, m)?)?;
    m.add_function(wrap_pyfunction!(normalize_query_spelling, m)?)?;
    m.add_function(wrap_pyfunction!(parse_names, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_names, m)?)?;
    m.add_function(wrap_pyfunction!(explore, m)?)?;
    m.add_function(wrap_pyfunction!(traverse, m)?)?;
    m.add_function(wrap_pyfunction!(query, m)?)?;
    m.add_function(wrap_pyfunction!(canonical_file_path, m)?)?;
    m.add("OpError", m.py().get_type::<OpError>())?;
    m.add("NoExtension", m.py().get_type::<NoExtension>())?;
    m.add("NoIndex", m.py().get_type::<NoIndex>())?;
    m.add("InvalidRequest", m.py().get_type::<InvalidRequest>())?;
    m.add(
        "TemporalUnsupported",
        m.py().get_type::<TemporalUnsupported>(),
    )?;
    m.add("NotFound", m.py().get_type::<NotFound>())?;
    m.add("EngineError", m.py().get_type::<EngineError>())?;
    m.add("MissingDependency", m.py().get_type::<MissingDependency>())?;
    m.add("NoEmbeddings", m.py().get_type::<NoEmbeddings>())?;
    m.add("MutationFailed", m.py().get_type::<MutationFailed>())?;
    m.add(
        "ContentUnavailable",
        m.py().get_type::<ContentUnavailable>(),
    )?;
    Ok(())
}
