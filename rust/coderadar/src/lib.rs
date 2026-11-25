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
mod render_remaining;
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

fn blob_get_inner(digest: String) -> Result<Option<serde_json::Value>, String> {
    let out = port::blob_get(digest)?;
    Ok(out)
}

/// `blob_get`.
#[pyfunction]
#[pyo3(name = "blob_get", signature = (digest))]
fn blob_get(py: Python<'_>, digest: String) -> PyResult<Py<PyAny>> {
    blob_get_inner(digest)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn archive_inner(cutoff: Option<String>) -> Result<serde_json::Value, String> {
    let out = port::archive(cutoff)?;
    Ok(out)
}

/// `archive`.
#[pyfunction]
#[pyo3(name = "archive", signature = (cutoff=None))]
fn archive(py: Python<'_>, cutoff: Option<String>) -> PyResult<Py<PyAny>> {
    archive_inner(cutoff)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn read_source_inner(entity: Bound<'_, PyAny>) -> Result<Option<String>, String> {
    let entity: serde_json::Value = crate::pyconv::FromPy::from_py(&entity)?;
    let out = port::read_source(entity)?;
    Ok(out)
}

/// `read_source`.
#[pyfunction]
#[pyo3(name = "read_source", signature = (entity))]
fn read_source(entity: Bound<'_, PyAny>) -> PyResult<Option<String>> {
    read_source_inner(entity).map_err(error)
}

fn stale_files_inner(file_paths: Vec<String>) -> Result<Vec<serde_json::Value>, String> {
    let out = port::stale_files(file_paths)?;
    Ok(out)
}

/// `stale_files`.
#[pyfunction]
#[pyo3(name = "stale_files", signature = (file_paths))]
fn stale_files(py: Python<'_>, file_paths: Vec<String>) -> PyResult<Py<PyAny>> {
    stale_files_inner(file_paths)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn dead_code_inner(
    min_confidence: f64,
    include_test_reachable: bool,
    max_findings: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let out = port::dead_code(min_confidence, include_test_reachable, max_findings)?;
    Ok(out)
}

/// `dead_code`.
#[pyfunction]
#[pyo3(name = "dead_code", signature = (min_confidence=0.6, include_test_reachable=false, max_findings=100))]
fn dead_code(
    py: Python<'_>,
    min_confidence: f64,
    include_test_reachable: bool,
    max_findings: i64,
) -> PyResult<Py<PyAny>> {
    dead_code_inner(min_confidence, include_test_reachable, max_findings)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn get_smells_inner(
    entity_id: Option<String>,
    rule_id: Option<String>,
    strictness: String,
) -> Result<Vec<serde_json::Value>, String> {
    let out = port::get_smells(entity_id, rule_id, strictness)?;
    Ok(out)
}

/// `get_smells`.
#[pyfunction]
#[pyo3(name = "get_smells", signature = (entity_id=None, rule_id=None, strictness=String::from("normal")))]
fn get_smells(
    py: Python<'_>,
    entity_id: Option<String>,
    rule_id: Option<String>,
    strictness: String,
) -> PyResult<Py<PyAny>> {
    get_smells_inner(entity_id, rule_id, strictness)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn find_clones_inner(
    min_lines: i64,
    min_similarity: f64,
    max_groups: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let out = port::find_clones(min_lines, min_similarity, max_groups)?;
    Ok(out)
}

/// `find_clones`.
#[pyfunction]
#[pyo3(name = "find_clones", signature = (min_lines=10, min_similarity=0.8, max_groups=100))]
fn find_clones(
    py: Python<'_>,
    min_lines: i64,
    min_similarity: f64,
    max_groups: i64,
) -> PyResult<Py<PyAny>> {
    find_clones_inner(min_lines, min_similarity, max_groups)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn find_scaffolding_inner(
    include_secrets: bool,
    max_findings: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let out = port::find_scaffolding(include_secrets, max_findings)?;
    Ok(out)
}

/// `find_scaffolding`.
#[pyfunction]
#[pyo3(name = "find_scaffolding", signature = (include_secrets=false, max_findings=100))]
fn find_scaffolding(
    py: Python<'_>,
    include_secrets: bool,
    max_findings: i64,
) -> PyResult<Py<PyAny>> {
    find_scaffolding_inner(include_secrets, max_findings)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn replace_body_inner(
    entity_id: String,
    new_body: String,
    expected_hash: Option<String>,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::replace_body(entity_id, new_body, expected_hash, dry_run, graph)?;
    Ok(out)
}

/// `replace_body`.
#[pyfunction]
#[pyo3(name = "replace_body", signature = (entity_id, new_body, expected_hash=None, dry_run=true, *, graph=None))]
fn replace_body(
    py: Python<'_>,
    entity_id: String,
    new_body: String,
    expected_hash: Option<String>,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    replace_body_inner(entity_id, new_body, expected_hash, dry_run, graph)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn update_signature_inner(
    entity_id: String,
    new_signature: String,
    inject_defaults: bool,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::update_signature(entity_id, new_signature, inject_defaults, dry_run, graph)?;
    Ok(out)
}

/// `update_signature`.
#[pyfunction]
#[pyo3(name = "update_signature", signature = (entity_id, new_signature, inject_defaults=false, dry_run=true, *, graph=None))]
fn update_signature(
    py: Python<'_>,
    entity_id: String,
    new_signature: String,
    inject_defaults: bool,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    update_signature_inner(entity_id, new_signature, inject_defaults, dry_run, graph)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn rename_inner(
    entity_id: String,
    new_name: String,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::rename(entity_id, new_name, dry_run, graph)?;
    Ok(out)
}

/// `rename`.
#[pyfunction]
#[pyo3(name = "rename", signature = (entity_id, new_name, dry_run=true, *, graph=None))]
fn rename(
    py: Python<'_>,
    entity_id: String,
    new_name: String,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    rename_inner(entity_id, new_name, dry_run, graph)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn render_entity_code_inner(
    language: String,
    kind: String,
    name: String,
    body: String,
    decorators: Bound<'_, PyAny>,
    signature: String,
) -> Result<String, String> {
    let decorators: Option<Vec<String>> = crate::pyconv::FromPy::from_py(&decorators)?;
    let out = port::render_entity_code(language, kind, name, body, decorators, signature)?;
    Ok(out)
}

/// `render_entity_code`.
#[pyfunction]
#[pyo3(name = "render_entity_code", signature = (language, kind, name, body, decorators, signature=String::from("")))]
fn render_entity_code(
    py: Python<'_>,
    language: String,
    kind: String,
    name: String,
    body: String,
    decorators: Option<Bound<'_, PyAny>>,
    signature: String,
) -> PyResult<String> {
    let decorators = decorators.unwrap_or_else(|| py.None().into_bound(py));
    render_entity_code_inner(language, kind, name, body, decorators, signature).map_err(error)
}

fn create_entity_inner(
    file_path: String,
    language: String,
    kind: String,
    name: String,
    body: String,
    decorators: Bound<'_, PyAny>,
    anchor: String,
    signature: Option<String>,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let decorators: Option<Vec<String>> = crate::pyconv::FromPy::from_py(&decorators)?;
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::create_entity(
        file_path, language, kind, name, body, decorators, anchor, signature, dry_run, graph,
    )?;
    Ok(out)
}

/// `create_entity`.
#[pyfunction]
#[pyo3(name = "create_entity", signature = (file_path, language, kind, name, body, decorators=None, anchor=String::from("end"), signature=None, dry_run=true, *, graph=None))]
fn create_entity(
    py: Python<'_>,
    file_path: String,
    language: String,
    kind: String,
    name: String,
    body: String,
    decorators: Option<Bound<'_, PyAny>>,
    anchor: String,
    signature: Option<String>,
    dry_run: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let decorators = decorators.unwrap_or_else(|| py.None().into_bound(py));
    create_entity_inner(
        file_path, language, kind, name, body, decorators, anchor, signature, dry_run, graph,
    )
    .map_err(error)
    .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn update_file_inner(
    file_path: String,
    content: Option<String>,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::update_file(file_path, content, graph)?;
    Ok(out)
}

/// `update_file`.
#[pyfunction]
#[pyo3(name = "update_file", signature = (file_path, content=None, *, graph=None))]
fn update_file(
    py: Python<'_>,
    file_path: String,
    content: Option<String>,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    update_file_inner(file_path, content, graph)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn branches_inner(graph: Option<Bound<'_, PyAny>>) -> Result<Vec<serde_json::Value>, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::branches(graph)?;
    Ok(out)
}

/// `branches`.
#[pyfunction]
#[pyo3(name = "branches", signature = (*, graph=None))]
fn branches(py: Python<'_>, graph: Option<Bound<'_, PyAny>>) -> PyResult<Py<PyAny>> {
    branches_inner(graph)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn as_of_inner(
    timestamp: String,
    query: String,
    symbols: Bound<'_, PyAny>,
    branch: String,
    strict: bool,
    at: Option<String>,
    valid: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let symbols: Option<Vec<String>> = crate::pyconv::FromPy::from_py(&symbols)?;
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::as_of(timestamp, query, symbols, branch, strict, at, valid, graph)?;
    Ok(out)
}

/// `as_of`.
#[pyfunction]
#[pyo3(name = "as_of", signature = (timestamp, query=String::from(""), symbols=None, branch=String::from("main"), *, strict=false, at=None, valid=false, graph=None))]
fn as_of(
    py: Python<'_>,
    timestamp: String,
    query: Option<String>,
    symbols: Option<Bound<'_, PyAny>>,
    branch: Option<String>,
    strict: Option<bool>,
    at: Option<String>,
    valid: Option<bool>,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let symbols = symbols.unwrap_or_else(|| py.None().into_bound(py));
    as_of_inner(
        timestamp,
        query.unwrap_or_default(),
        symbols,
        branch.unwrap_or_else(|| "main".to_string()),
        strict.unwrap_or(false),
        at,
        valid.unwrap_or(false),
        graph,
    )
    .map_err(error)
    .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn verify_history_inner(
    from_ts: Option<String>,
    to_ts: Option<String>,
    generations: i64,
    all: bool,
    branch: String,
    offenders: i64,
    max_generations: Option<i64>,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::verify_history(
        from_ts,
        to_ts,
        generations,
        all,
        branch,
        offenders,
        max_generations,
        graph,
    )?;
    Ok(out)
}

/// `verify_history`.
#[pyfunction]
#[pyo3(name = "verify_history", signature = (from_ts=None, to_ts=None, generations=25, all=false, branch=String::from("main"), offenders=10, max_generations=None, *, graph=None))]
fn verify_history(
    py: Python<'_>,
    from_ts: Option<String>,
    to_ts: Option<String>,
    generations: Option<i64>,
    all: Option<bool>,
    branch: Option<String>,
    offenders: Option<i64>,
    max_generations: Option<i64>,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    verify_history_inner(
        from_ts,
        to_ts,
        generations.unwrap_or(25),
        all.unwrap_or(false),
        branch.unwrap_or_else(|| "main".to_string()),
        offenders.unwrap_or(10),
        max_generations,
        graph,
    )
    .map_err(error)
    .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn history_of_inner(
    file: String,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::history_of(file, graph)?;
    Ok(out)
}

/// `history_of`.
#[pyfunction]
#[pyo3(name = "history_of", signature = (file, *, graph=None))]
fn history_of(
    py: Python<'_>,
    file: String,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    history_of_inner(file, graph)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn changes_between_inner(
    t1: String,
    t2: String,
    scope: Option<String>,
    branch: String,
    valid: bool,
    graph: Option<Bound<'_, PyAny>>,
) -> Result<serde_json::Value, String> {
    let graph: Option<serde_json::Value> = match &graph {
        None => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::changes_between(t1, t2, scope, branch, valid, graph)?;
    Ok(out)
}

/// `changes_between`.
#[pyfunction]
#[pyo3(name = "changes_between", signature = (t1, t2, scope=None, branch=String::from("main"), *, valid=false, graph=None))]
fn changes_between(
    py: Python<'_>,
    t1: String,
    t2: String,
    scope: Option<String>,
    branch: Option<String>,
    valid: Option<bool>,
    graph: Option<Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    changes_between_inner(
        t1,
        t2,
        scope,
        branch.unwrap_or_else(|| "main".to_string()),
        valid.unwrap_or(false),
        graph,
    )
    .map_err(error)
    .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn extract_all_exports_inner(source: String) -> Result<Option<Vec<String>>, String> {
    let out = port::extract_all_exports(source)?;
    Ok(out)
}

/// `extract_all_exports`.
#[pyfunction]
#[pyo3(name = "extract_all_exports", signature = (source))]
fn extract_all_exports(py: Python<'_>, source: String) -> PyResult<Py<PyAny>> {
    extract_all_exports_inner(source)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn canonical_route_pattern_inner(path: String) -> Result<serde_json::Value, String> {
    let out = port::canonical_route_pattern(path)?;
    Ok(out)
}

/// `canonical_route_pattern`.
#[pyfunction]
#[pyo3(name = "canonical_route_pattern", signature = (path))]
fn canonical_route_pattern(py: Python<'_>, path: String) -> PyResult<Py<PyAny>> {
    canonical_route_pattern_inner(path)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn route_patterns_match_inner(query: String, candidate: String) -> Result<bool, String> {
    let out = port::route_patterns_match(query, candidate)?;
    Ok(out)
}

/// `route_patterns_match`.
#[pyfunction]
#[pyo3(name = "route_patterns_match", signature = (query, candidate))]
fn route_patterns_match(query: String, candidate: String) -> PyResult<bool> {
    route_patterns_match_inner(query, candidate).map_err(error)
}

fn prefer_in_dir_inner(
    candidates: Bound<'_, PyAny>,
    name: String,
    dir_hint: String,
    confidence: f64,
) -> Result<Option<serde_json::Value>, String> {
    let candidates: Vec<serde_json::Value> = crate::pyconv::FromPy::from_py(&candidates)?;
    let out = port::prefer_in_dir(candidates, name, dir_hint, confidence)?;
    Ok(out)
}

/// `prefer_in_dir`.
#[pyfunction]
#[pyo3(name = "prefer_in_dir", signature = (candidates, name, dir_hint, *, confidence=0.85))]
fn prefer_in_dir(
    py: Python<'_>,
    candidates: Bound<'_, PyAny>,
    name: String,
    dir_hint: String,
    confidence: f64,
) -> PyResult<Py<PyAny>> {
    prefer_in_dir_inner(candidates, name, dir_hint, confidence)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn not_found_inner(
    entity_id: String,
    candidates: Bound<'_, PyAny>,
    hint: String,
) -> Result<String, String> {
    let candidates: Vec<serde_json::Value> = if candidates.is_none() {
        Vec::new()
    } else {
        crate::pyconv::FromPy::from_py(&candidates)?
    };
    let out = port::render_not_found(entity_id, candidates, hint)?;
    Ok(out)
}

/// `not_found`.
#[pyfunction]
#[pyo3(name = "not_found", signature = (entity_id, candidates, hint=String::from("coderadar_search")))]
fn not_found(entity_id: String, candidates: Bound<'_, PyAny>, hint: String) -> PyResult<String> {
    not_found_inner(entity_id, candidates, hint).map_err(error)
}

fn search_miss_inner(
    query: String,
    kind: Option<String>,
    search_similar: String,
    explore: String,
) -> Result<String, String> {
    let out = port::search_miss(query, kind, search_similar, explore)?;
    Ok(out)
}

/// `search_miss`.
#[pyfunction]
#[pyo3(name = "search_miss", signature = (query, kind, *, search_similar=String::from("`coderadar_search_similar`"), explore=String::from("`coderadar_explore`")))]
fn search_miss(
    query: String,
    kind: Option<String>,
    search_similar: String,
    explore: String,
) -> PyResult<String> {
    search_miss_inner(query, kind, search_similar, explore).map_err(error)
}

fn diagnose_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::render_diagnose(result)?;
    Ok(out)
}

/// `diagnose`.
#[pyfunction]
#[pyo3(name = "diagnose", signature = (result))]
fn diagnose(result: Bound<'_, PyAny>) -> PyResult<String> {
    diagnose_inner(result).map_err(error)
}

fn store_repair_inner(db_path: String, result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::store_repair(db_path, result)?;
    Ok(out)
}

/// `store_repair`.
#[pyfunction]
#[pyo3(name = "store_repair", signature = (db_path, result))]
fn store_repair(db_path: String, result: Bound<'_, PyAny>) -> PyResult<String> {
    store_repair_inner(db_path, result).map_err(error)
}

fn stale_banner_inner(
    stale_files: Bound<'_, PyAny>,
    referenced_paths: Vec<String>,
) -> Result<String, String> {
    let stale_files: serde_json::Value = crate::pyconv::FromPy::from_py(&stale_files)?;
    let out = port::stale_banner(stale_files, referenced_paths)?;
    Ok(out)
}

/// `stale_banner`.
#[pyfunction]
#[pyo3(name = "stale_banner", signature = (stale_files, referenced_paths))]
fn stale_banner(stale_files: Bound<'_, PyAny>, referenced_paths: Vec<String>) -> PyResult<String> {
    stale_banner_inner(stale_files, referenced_paths).map_err(error)
}

fn explore_usage_inner(example: Option<String>) -> Result<String, String> {
    let out = port::explore_usage(example)?;
    Ok(out)
}

/// `explore_usage`.
#[pyfunction]
#[pyo3(name = "explore_usage", signature = (*, example=None))]
fn explore_usage(example: Option<String>) -> PyResult<String> {
    explore_usage_inner(example).map_err(error)
}

fn explore_miss_inner(
    names: Vec<String>,
    search: String,
    search_similar: String,
) -> Result<String, String> {
    let out = port::explore_miss(names, search, search_similar)?;
    Ok(out)
}

/// `explore_miss`.
#[pyfunction]
#[pyo3(name = "explore_miss", signature = (names, *, search=String::from("`coderadar_search`"), search_similar=String::from("`coderadar_search_similar`")))]
fn explore_miss(names: Vec<String>, search: String, search_similar: String) -> PyResult<String> {
    explore_miss_inner(names, search, search_similar).map_err(error)
}

fn apply_output_budget_inner(
    lines: Vec<String>,
    max_chars: Option<i64>,
    max_per_file: Option<i64>,
) -> Result<String, String> {
    let out = port::apply_output_budget(lines, max_chars, max_per_file)?;
    Ok(out)
}

/// `apply_output_budget`.
#[pyfunction]
#[pyo3(name = "apply_output_budget", signature = (lines, max_chars=None, max_per_file=None))]
fn apply_output_budget(
    lines: Vec<String>,
    max_chars: Option<i64>,
    max_per_file: Option<i64>,
) -> PyResult<String> {
    apply_output_budget_inner(lines, max_chars, max_per_file).map_err(error)
}

fn extract_path_from_header_inner(header: String) -> Result<String, String> {
    let out = port::extract_path_from_header(header)?;
    Ok(out)
}

/// `extract_path_from_header`.
#[pyfunction]
#[pyo3(name = "extract_path_from_header", signature = (header))]
fn extract_path_from_header(header: String) -> PyResult<String> {
    extract_path_from_header_inner(header).map_err(error)
}

fn trim_to_char_budget_inner(
    body_lines: Vec<String>,
    max_chars: i64,
) -> Result<Vec<String>, String> {
    let out = port::trim_to_char_budget(body_lines, max_chars)?;
    Ok(out)
}

/// `trim_to_char_budget`.
#[pyfunction]
#[pyo3(name = "trim_to_char_budget", signature = (body_lines, max_chars))]
fn trim_to_char_budget(body_lines: Vec<String>, max_chars: i64) -> PyResult<Vec<String>> {
    trim_to_char_budget_inner(body_lines, max_chars).map_err(error)
}

fn query_usage_inner() -> Result<String, String> {
    let out = port::query_usage()?;
    Ok(out)
}

/// `query_usage`.
#[pyfunction]
#[pyo3(name = "query_usage", signature = ())]
fn query_usage() -> PyResult<String> {
    query_usage_inner().map_err(error)
}

fn search_similar_inner(query: String, results: Bound<'_, PyAny>) -> Result<String, String> {
    let results: Vec<serde_json::Value> = crate::pyconv::FromPy::from_py(&results)?;
    let out = port::search_similar(query, results)?;
    Ok(out)
}

/// `search_similar`.
#[pyfunction]
#[pyo3(name = "search_similar", signature = (query, results))]
fn search_similar(query: String, results: Bound<'_, PyAny>) -> PyResult<String> {
    search_similar_inner(query, results).map_err(error)
}

fn search_hybrid_inner(query: String, results: Bound<'_, PyAny>) -> Result<String, String> {
    let results: serde_json::Value = crate::pyconv::FromPy::from_py(&results)?;
    let out = port::search_hybrid(query, results)?;
    Ok(out)
}

/// `search_hybrid`.
#[pyfunction]
#[pyo3(name = "search_hybrid", signature = (query, results))]
fn search_hybrid(query: String, results: Bound<'_, PyAny>) -> PyResult<String> {
    search_hybrid_inner(query, results).map_err(error)
}

fn fastembed_missing_inner(compute: String) -> Result<String, String> {
    let out = port::fastembed_missing(compute)?;
    Ok(out)
}

/// `fastembed_missing`.
#[pyfunction]
#[pyo3(name = "fastembed_missing", signature = (compute=String::from("`coderadar_compute_embeddings`")))]
fn fastembed_missing(compute: String) -> PyResult<String> {
    fastembed_missing_inner(compute).map_err(error)
}

fn no_embeddings_inner(
    compute: Option<Bound<'_, PyAny>>,
    reindex: String,
) -> Result<String, String> {
    let compute = match compute {
        None if reindex == "`coderadar_reindex with_embeddings=True`" => {
            "`coderadar_compute_embeddings`".to_string()
        }
        None => "None".to_string(),
        Some(value) => value
            .str()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
    };
    let out = port::no_embeddings(compute, reindex)?;
    Ok(out)
}

/// `no_embeddings`.
#[pyfunction]
#[pyo3(name = "no_embeddings", signature = (compute=None, reindex=String::from("`coderadar_reindex with_embeddings=True`")))]
fn no_embeddings(compute: Option<Bound<'_, PyAny>>, reindex: String) -> PyResult<String> {
    no_embeddings_inner(compute, reindex).map_err(error)
}

fn compute_embeddings_inner(
    metrics: Bound<'_, PyAny>,
    search_similar: String,
) -> Result<String, String> {
    let metrics: serde_json::Value = crate::pyconv::FromPy::from_py(&metrics)?;
    let out = port::compute_embeddings(metrics, search_similar)?;
    Ok(out)
}

/// `compute_embeddings`.
#[pyfunction]
#[pyo3(name = "compute_embeddings", signature = (metrics, *, search_similar=String::from("`coderadar_search_similar`")))]
fn compute_embeddings(metrics: Bound<'_, PyAny>, search_similar: String) -> PyResult<String> {
    compute_embeddings_inner(metrics, search_similar).map_err(error)
}

fn ingest_status_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::render_ingest_status(result)?;
    Ok(out)
}

/// `ingest_status`.
#[pyfunction]
#[pyo3(name = "ingest_status", signature = (result))]
fn ingest_status(result: Bound<'_, PyAny>) -> PyResult<String> {
    ingest_status_inner(result).map_err(error)
}

fn export_annotations_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::export_annotations(result)?;
    Ok(out)
}

/// `export_annotations`.
#[pyfunction]
#[pyo3(name = "export_annotations", signature = (result))]
fn export_annotations(result: Bound<'_, PyAny>) -> PyResult<String> {
    export_annotations_inner(result).map_err(error)
}

fn import_annotations_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::import_annotations(result)?;
    Ok(out)
}

/// `import_annotations`.
#[pyfunction]
#[pyo3(name = "import_annotations", signature = (result))]
fn import_annotations(result: Bound<'_, PyAny>) -> PyResult<String> {
    import_annotations_inner(result).map_err(error)
}

fn rebuild_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::rebuild(result)?;
    Ok(out)
}

/// `rebuild`.
#[pyfunction]
#[pyo3(name = "rebuild", signature = (result))]
fn rebuild(result: Bound<'_, PyAny>) -> PyResult<String> {
    rebuild_inner(result).map_err(error)
}

fn open_items_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::open_items(result)?;
    Ok(out)
}

/// `open_items`.
#[pyfunction]
#[pyo3(name = "open_items", signature = (result))]
fn open_items(result: Bound<'_, PyAny>) -> PyResult<String> {
    open_items_inner(result).map_err(error)
}

fn annotation_versions_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::annotation_versions(result)?;
    Ok(out)
}

/// `annotation_versions`.
#[pyfunction]
#[pyo3(name = "annotation_versions", signature = (result))]
fn annotation_versions(result: Bound<'_, PyAny>) -> PyResult<String> {
    annotation_versions_inner(result).map_err(error)
}

fn annotation_stored_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::annotation_stored(result)?;
    Ok(out)
}

/// `annotation_stored`.
#[pyfunction]
#[pyo3(name = "annotation_stored", signature = (result))]
fn annotation_stored(result: Bound<'_, PyAny>) -> PyResult<String> {
    annotation_stored_inner(result).map_err(error)
}

fn ingest_history_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    port::render_ingest_history(result)
}

/// `ingest_history`.
#[pyfunction]
#[pyo3(name = "ingest_history", signature = (result))]
fn ingest_history(result: Bound<'_, PyAny>) -> PyResult<String> {
    ingest_history_inner(result).map_err(error)
}

fn migrate_to_commit_time_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::migrate_to_commit_time(result)?;
    Ok(out)
}

/// `migrate_to_commit_time`.
#[pyfunction]
#[pyo3(name = "migrate_to_commit_time", signature = (result))]
fn migrate_to_commit_time(result: Bound<'_, PyAny>) -> PyResult<String> {
    migrate_to_commit_time_inner(result).map_err(error)
}

fn edit_inner(outcome: Bound<'_, PyAny>, apply_hint: String) -> Result<String, String> {
    let outcome: serde_json::Value = crate::pyconv::FromPy::from_py(&outcome)?;
    let out = port::edit(outcome, apply_hint)?;
    Ok(out)
}

/// `edit`.
#[pyfunction]
#[pyo3(name = "edit", signature = (outcome, apply_hint=String::from("call again with `dry_run=False`")))]
fn edit(outcome: Bound<'_, PyAny>, apply_hint: String) -> PyResult<String> {
    edit_inner(outcome, apply_hint).map_err(error)
}

fn mutation_error_inner(raw: String, update_file: String) -> Result<String, String> {
    let out = port::mutation_error(raw, update_file)?;
    Ok(out)
}

/// `mutation_error`.
#[pyfunction]
#[pyo3(name = "mutation_error", signature = (raw, *, update_file=String::from("`coderadar_update_file`")))]
fn mutation_error(raw: String, update_file: String) -> PyResult<String> {
    mutation_error_inner(raw, update_file).map_err(error)
}

fn mutation_plan_inner(plan: Bound<'_, PyAny>) -> Result<String, String> {
    let plan: serde_json::Value = crate::pyconv::FromPy::from_py(&plan)?;
    let out = port::mutation_plan(plan)?;
    Ok(out)
}

/// `mutation_plan`.
#[pyfunction]
#[pyo3(name = "mutation_plan", signature = (plan))]
fn mutation_plan(plan: Bound<'_, PyAny>) -> PyResult<String> {
    mutation_plan_inner(plan).map_err(error)
}

fn mutation_applied_inner(
    result: Bound<'_, PyAny>,
    unverified_sites: Option<Bound<'_, PyAny>>,
) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let unverified_sites: Option<serde_json::Value> = match &unverified_sites {
        None => None,
        Some(v) if !v.is_truthy().unwrap_or(false) => None,
        Some(v) => Some(crate::pyconv::FromPy::from_py(v)?),
    };
    let out = port::mutation_applied(result, unverified_sites)?;
    Ok(out)
}

/// `mutation_applied`.
#[pyfunction]
#[pyo3(name = "mutation_applied", signature = (result, unverified_sites=None))]
fn mutation_applied(
    result: Bound<'_, PyAny>,
    unverified_sites: Option<Bound<'_, PyAny>>,
) -> PyResult<String> {
    mutation_applied_inner(result, unverified_sites).map_err(error)
}

fn reindex_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::reindex(result)?;
    Ok(out)
}

/// `reindex`.
#[pyfunction]
#[pyo3(name = "reindex", signature = (result))]
fn reindex(result: Bound<'_, PyAny>) -> PyResult<String> {
    reindex_inner(result).map_err(error)
}

fn status_inner(result: Bound<'_, PyAny>) -> Result<String, String> {
    let result: serde_json::Value = crate::pyconv::FromPy::from_py(&result)?;
    let out = port::status(result)?;
    Ok(out)
}

/// `status`.
#[pyfunction]
#[pyo3(name = "status", signature = (result))]
fn status(result: Bound<'_, PyAny>) -> PyResult<String> {
    status_inner(result).map_err(error)
}

fn run_framework_extraction_inner(root: Bound<'_, PyAny>) -> Result<serde_json::Value, String> {
    let root: serde_json::Value = crate::pyconv::FromPy::from_py(&root)?;
    let out = port::run_framework_extraction(root)?;
    Ok(out)
}

/// `run_framework_extraction`.
#[pyfunction]
#[pyo3(name = "run_framework_extraction", signature = (root))]
fn run_framework_extraction(py: Python<'_>, root: Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    run_framework_extraction_inner(root)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
}

fn refresh_framework_file_inner(
    root: Bound<'_, PyAny>,
    rel_posix: String,
    content: Option<String>,
) -> Result<serde_json::Value, String> {
    let root: serde_json::Value = crate::pyconv::FromPy::from_py(&root)?;
    let out = port::refresh_framework_file(root, rel_posix, content)?;
    Ok(out)
}

/// `refresh_framework_file`.
#[pyfunction]
#[pyo3(name = "refresh_framework_file", signature = (root, rel_posix, content=None))]
fn refresh_framework_file(
    py: Python<'_>,
    root: Bound<'_, PyAny>,
    rel_posix: String,
    content: Option<String>,
) -> PyResult<Py<PyAny>> {
    refresh_framework_file_inner(root, rel_posix, content)
        .map_err(error)
        .and_then(|v| crate::pyconv::ToPy::to_py(&v, py))
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
    m.add_function(wrap_pyfunction!(blob_get, m)?)?;
    m.add_function(wrap_pyfunction!(archive, m)?)?;
    m.add_function(wrap_pyfunction!(read_source, m)?)?;
    m.add_function(wrap_pyfunction!(stale_files, m)?)?;
    m.add_function(wrap_pyfunction!(dead_code, m)?)?;
    m.add_function(wrap_pyfunction!(get_smells, m)?)?;
    m.add_function(wrap_pyfunction!(find_clones, m)?)?;
    m.add_function(wrap_pyfunction!(find_scaffolding, m)?)?;
    m.add_function(wrap_pyfunction!(replace_body, m)?)?;
    m.add_function(wrap_pyfunction!(update_signature, m)?)?;
    m.add_function(wrap_pyfunction!(rename, m)?)?;
    m.add_function(wrap_pyfunction!(render_entity_code, m)?)?;
    m.add_function(wrap_pyfunction!(create_entity, m)?)?;
    m.add_function(wrap_pyfunction!(update_file, m)?)?;
    m.add_function(wrap_pyfunction!(branches, m)?)?;
    m.add_function(wrap_pyfunction!(as_of, m)?)?;
    m.add_function(wrap_pyfunction!(verify_history, m)?)?;
    m.add_function(wrap_pyfunction!(history_of, m)?)?;
    m.add_function(wrap_pyfunction!(changes_between, m)?)?;
    m.add_function(wrap_pyfunction!(extract_all_exports, m)?)?;
    m.add_function(wrap_pyfunction!(canonical_route_pattern, m)?)?;
    m.add_function(wrap_pyfunction!(route_patterns_match, m)?)?;
    m.add_function(wrap_pyfunction!(prefer_in_dir, m)?)?;
    m.add_function(wrap_pyfunction!(not_found, m)?)?;
    m.add_function(wrap_pyfunction!(search_miss, m)?)?;
    m.add_function(wrap_pyfunction!(diagnose, m)?)?;
    m.add_function(wrap_pyfunction!(store_repair, m)?)?;
    m.add_function(wrap_pyfunction!(stale_banner, m)?)?;
    m.add_function(wrap_pyfunction!(explore_usage, m)?)?;
    m.add_function(wrap_pyfunction!(explore_miss, m)?)?;
    m.add_function(wrap_pyfunction!(apply_output_budget, m)?)?;
    m.add_function(wrap_pyfunction!(extract_path_from_header, m)?)?;
    m.add_function(wrap_pyfunction!(trim_to_char_budget, m)?)?;
    m.add_function(wrap_pyfunction!(query_usage, m)?)?;
    m.add_function(wrap_pyfunction!(search_similar, m)?)?;
    m.add_function(wrap_pyfunction!(search_hybrid, m)?)?;
    m.add_function(wrap_pyfunction!(fastembed_missing, m)?)?;
    m.add_function(wrap_pyfunction!(no_embeddings, m)?)?;
    m.add_function(wrap_pyfunction!(compute_embeddings, m)?)?;
    m.add_function(wrap_pyfunction!(ingest_status, m)?)?;
    m.add_function(wrap_pyfunction!(export_annotations, m)?)?;
    m.add_function(wrap_pyfunction!(import_annotations, m)?)?;
    m.add_function(wrap_pyfunction!(rebuild, m)?)?;
    m.add_function(wrap_pyfunction!(open_items, m)?)?;
    m.add_function(wrap_pyfunction!(annotation_versions, m)?)?;
    m.add_function(wrap_pyfunction!(annotation_stored, m)?)?;
    m.add_function(wrap_pyfunction!(migrate_to_commit_time, m)?)?;
    m.add_function(wrap_pyfunction!(ingest_history, m)?)?;
    m.add_function(wrap_pyfunction!(edit, m)?)?;
    m.add_function(wrap_pyfunction!(mutation_error, m)?)?;
    m.add_function(wrap_pyfunction!(mutation_plan, m)?)?;
    m.add_function(wrap_pyfunction!(mutation_applied, m)?)?;
    m.add_function(wrap_pyfunction!(reindex, m)?)?;
    m.add_function(wrap_pyfunction!(status, m)?)?;
    m.add_function(wrap_pyfunction!(run_framework_extraction, m)?)?;
    m.add_function(wrap_pyfunction!(refresh_framework_file, m)?)?;
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
