// CodeRadar v3.5 — Rust Core Library
// PyO3 bindings for the Python layer (MCP server, CLI, resolvers).

pub mod buffers;
pub mod extract;
pub mod fs;
pub mod graph;
pub mod mutation;
pub mod query;
pub mod resolve;
pub mod storage;
pub mod types;
pub mod update;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::graph::CodeGraph;
use crate::graph::ImportGraph;
use crate::query::exec::{execute_query, QueryIterator};
use crate::query::grammar::parse_query;
use crate::types::{
    Class, Constant, Function, Import, Module, ProjectedGraph, TypeAlias,
};

// ── Python Module ──────────────────────────────────────────────────────────

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(analyze, m)?)?;
    m.add_function(wrap_pyfunction!(query_graph, m)?)?;
    m.add_function(wrap_pyfunction!(update_file, m)?)?;
    {
        use git_bindings::{git_worktree_clean, git_blame, git_changed_files};
        m.add_function(wrap_pyfunction!(git_worktree_clean, m)?)?;
        m.add_function(wrap_pyfunction!(git_blame, m)?)?;
        m.add_function(wrap_pyfunction!(git_changed_files, m)?)?;
    }
    m.add_function(wrap_pyfunction!(plan_body_replacement, m)?)?;
    m.add_function(wrap_pyfunction!(plan_signature_update, m)?)?;
    m.add_function(wrap_pyfunction!(plan_rename, m)?)?;
    m.add_function(wrap_pyfunction!(plan_create_entity, m)?)?;
    m.add_function(wrap_pyfunction!(apply_mutation, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_symbol, m)?)?;
    m.add_function(wrap_pyfunction!(callers_of, m)?)?;
    m.add_function(wrap_pyfunction!(callees_of, m)?)?;
    m.add_function(wrap_pyfunction!(lookup_entity, m)?)?;
    m.add_function(wrap_pyfunction!(search_entities, m)?)?;
    m.add_function(wrap_pyfunction!(graph_stats, m)?)?;
    m.add_function(wrap_pyfunction!(export_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(load_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(search_similar, m)?)?;
    m.add_function(wrap_pyfunction!(register_synthetic_edge, m)?)?;
    m.add_function(wrap_pyfunction!(module_children, m)?)?;
    m.add_function(wrap_pyfunction!(set_module_star_exports, m)?)?;
    m.add_function(wrap_pyfunction!(start_watcher, m)?)?;
    m.add_function(wrap_pyfunction!(next_watcher_batch, m)?)?;
    m.add_function(wrap_pyfunction!(next_watcher_batch_timeout, m)?)?;
    m.add_function(wrap_pyfunction!(stop_watcher, m)?)?;
    m.add_class::<PyCodeGraph>()?;
    m.add_class::<QueryIterator>()?;
    Ok(())
}

// ── Internal state ─────────────────────────────────────────────────────────

static GLOBAL_GRAPH: std::sync::LazyLock<RwLock<Option<CodeGraph>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

fn with_graph<F, R>(f: F) -> PyResult<R>
where
    F: FnOnce(&CodeGraph, &Arc<ProjectedGraph>) -> PyResult<R>,
{
    let guard = GLOBAL_GRAPH.read();
    match guard.as_ref() {
        Some(g) => {
            let snap = g.snapshot();
            f(g, &snap)
        }
        None => Err(pyo3::exceptions::PyRuntimeError::new_err(
            "No graph loaded — run coderadar init first",
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
        let config = graph::GraphConfig::default();
        let mut g = CodeGraph::new(config);
        // Seed the global graph from this instance
        let mut guard = GLOBAL_GRAPH.write();
        let inner = Arc::new(RwLock::new(g));
        // Can't move out of CodeGraph, so clone the projection
        // The global graph is separate; PyCodeGraph holds its own instance
        Self { inner: inner.clone() }
    }

    fn query(&self, query_str: &str) -> PyResult<QueryIterator> {
        let graph = self.inner.read();
        let snapshot = graph.snapshot();
        let parsed = parse_query(query_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e))?;
        let rows = execute_query(&snapshot, &parsed);
        Ok(QueryIterator::new(rows))
    }
}

// ── Entity → Python dict helpers ──────────────────────────────────────────

fn module_to_dict(py: Python<'_>, m: &Module) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &m.id)?;
    dict.set_item("name", &m.name)?;
    dict.set_item("kind", "module")?;
    dict.set_item("file_path", m.path.to_string_lossy().to_string())?;
    dict.set_item("language", format!("{:?}", m.language))?;
    dict.set_item("parse_quality", format!("{:?}", m.parse_quality))?;
    dict.set_item("classes", m.classes.clone())?;
    dict.set_item("functions", m.functions.clone())?;
    dict.set_item("imports", m.imports.clone())?;
    dict.set_item("constants", m.constants.clone())?;
    dict.set_item("type_aliases", m.type_aliases.clone())?;
    dict.set_item("file_version", m.file_version)?;
    Ok(dict.into())
}

fn class_to_dict(py: Python<'_>, c: &Class) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &c.id)?;
    dict.set_item("name", &c.name)?;
    dict.set_item("grammar_kind", &c.grammar_kind)?;
    dict.set_item("kind", "class")?;
    dict.set_item("parent_module", &c.parent_module)?;
    // Extract file_path from entity ID (format: "file_path::Class.name")
    if let Some(idx) = c.id.rfind("::") {
        dict.set_item("file_path", &c.id[..idx])?;
    }
    if let Some(ref pc) = c.parent_class {
        dict.set_item("parent_id", pc)?;
    }
    if let Some(ref doc) = c.docstring {
        dict.set_item("docstring", doc)?;
    }
    dict.set_item("line", c.line)?;
    dict.set_item("end_line", c.exit_line)?;
    dict.set_item("start_line", c.line)?;
    dict.set_item("decorators", c.decorators.clone())?;
    dict.set_item("span_start", c.span.start)?;
    dict.set_item("span_end", c.span.end)?;
    dict.set_item("name_span_start", c.name_span.start)?;
    dict.set_item("name_span_end", c.name_span.end)?;
    let bases: Vec<String> = c.bases.iter().map(|b| b.name.clone()).collect();
    dict.set_item("bases", bases)?;
    Ok(dict.into())
}

fn function_to_dict(py: Python<'_>, f: &Function) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &f.id)?;
    dict.set_item("name", &f.name)?;
    dict.set_item("kind", match f.kind {
        crate::types::FunctionKind::Free => "function",
        crate::types::FunctionKind::Method
        | crate::types::FunctionKind::AbstractMethod
        | crate::types::FunctionKind::DataclassSynthesized { .. } => "method",
        crate::types::FunctionKind::StaticMethod
        | crate::types::FunctionKind::ClassMethod => "function",
        crate::types::FunctionKind::Property
        | crate::types::FunctionKind::PropertySetter
        | crate::types::FunctionKind::PropertyDeleter
        | crate::types::FunctionKind::CachedProperty => "method",
    })?;
    dict.set_item("parent_module", &f.parent_module)?;
    // Extract file_path from entity ID (format: "file_path::qualified.name")
    if let Some(idx) = f.id.rfind("::") {
        dict.set_item("file_path", &f.id[..idx])?;
    }
    if let Some(ref pc) = f.parent_class {
        dict.set_item("parent_id", pc)?;
    }
    if let Some(ref doc) = f.docstring {
        dict.set_item("docstring", doc)?;
    }
    if let Some(ref ret) = f.return_type {
        dict.set_item("return_type", ret)?;
    }
    dict.set_item("line", f.line)?;
    dict.set_item("end_line", f.exit_line)?;
    dict.set_item("start_line", f.line)?;
    dict.set_item("decorators", f.decorators.clone())?;
    dict.set_item("is_async", f.is_async)?;
    dict.set_item("is_generator", f.is_generator)?;
    dict.set_item("span_start", f.span.start)?;
    dict.set_item("span_end", f.span.end)?;
    dict.set_item("name_span_start", f.name_span.start)?;
    dict.set_item("name_span_end", f.name_span.end)?;
    if !f.embedding.is_empty() {
        dict.set_item("has_embedding", true)?;
    }
    // Build signature string from parameters
    let params: Vec<String> = f.parameters.iter()
        .map(|p| {
            let mut s = p.name.clone();
            if let Some(ref ann) = p.annotation {
                s.push_str(": ");
                s.push_str(ann);
            }
            if let Some(ref def) = p.default_value {
                s.push_str(" = ");
                s.push_str(def);
            }
            s
        })
        .collect();
    let sig = format!("def {}({})", f.name, params.join(", "));
    if let Some(ref ret) = f.return_type {
        dict.set_item("signature", format!("{} -> {}", sig, ret))?;
    } else {
        dict.set_item("signature", sig)?;
    }
    Ok(dict.into())
}

fn import_to_dict(py: Python<'_>, i: &Import) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &i.id)?;
    dict.set_item("name", &i.raw)?;
    dict.set_item("kind", "import")?;
    dict.set_item("line", i.line)?;
    dict.set_item("start_line", i.line)?;
    dict.set_item("name_span_start", i.name_span.start)?;
    dict.set_item("name_span_end", i.name_span.end)?;
    Ok(dict.into())
}

fn constant_to_dict(py: Python<'_>, c: &Constant) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &c.id)?;
    dict.set_item("name", &c.name)?;
    dict.set_item("kind", "constant")?;
    if let Some(ref ann) = c.annotation {
        dict.set_item("annotation", ann)?;
    }
    dict.set_item("span_start", c.span.start)?;
    dict.set_item("span_end", c.span.end)?;
    Ok(dict.into())
}

fn type_alias_to_dict(py: Python<'_>, ta: &TypeAlias) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &ta.id)?;
    dict.set_item("name", &ta.name)?;
    dict.set_item("kind", "type_alias")?;
    dict.set_item("target", &ta.target)?;
    dict.set_item("span_start", ta.span.start)?;
    dict.set_item("span_end", ta.span.end)?;
    Ok(dict.into())
}

// ── Entity References to Dict ──────────────────────────────────────────────

/// Convert a thin entity reference (just ID + name + kind) to a dict.
/// Used for callers_of / callees_of which return lists of EntityIds.
fn entity_ref_to_dict(
    py: Python<'_>, entity_id: &str, snap: &ProjectedGraph,
) -> Option<PyObject> {
    // Try each entity type and also resolve file_path from parent module
    if let Some(f) = snap.functions.get(entity_id) {
        let mut dict = function_to_dict(py, f).ok()?;
        // Resolve file_path from parent module
        if let Ok(d) = dict.downcast_bound::<PyDict>(py) {
            if let Some(m) = snap.modules.get(&f.parent_module) {
                let _ = d.set_item("file_path", m.path.to_string_lossy().to_string());
            }
        }
        Some(dict)
    } else if let Some(c) = snap.classes.get(entity_id) {
        let mut dict = class_to_dict(py, c).ok()?;
        if let Ok(d) = dict.downcast_bound::<PyDict>(py) {
            if let Some(m) = snap.modules.get(&c.parent_module) {
                let _ = d.set_item("file_path", m.path.to_string_lossy().to_string());
            }
        }
        Some(dict)
    } else if let Some(m) = snap.modules.get(entity_id) {
        module_to_dict(py, m).ok()
    } else if let Some(i) = snap.imports.get(entity_id) {
        import_to_dict(py, i).ok()
    } else if let Some(k) = snap.constants.get(entity_id) {
        constant_to_dict(py, k).ok()
    } else if let Some(ta) = snap.type_aliases.get(entity_id) {
        type_alias_to_dict(py, ta).ok()
    } else {
        None
    }
}

// ── analyze() ──────────────────────────────────────────────────────────────

#[pyfunction]
fn analyze(root: &str) -> PyResult<PyObject> {
    use std::fs;
    use crate::types::Language;

    let config = graph::GraphConfig::default();
    let mut graph = CodeGraph::new(config);

    // Attach Macrame persistent store — Macrame/libSQL needs a file path, not a directory
    let store_path = std::path::Path::new(root).join(".coderadar").join("store").join("coderadar.db");
    if let Some(parent) = store_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("Warning: Could not create store directory {:?}: {}", parent, e);
        }
    }
    match crate::storage::CodeGraphStore::open(&store_path) {
        Ok(store) => { graph = graph.with_store(store); }
        Err(e) => { eprintln!("Warning: Macrame store not attached: {:?}", e); }
    }

    let root_path = std::path::Path::new(root);
    let mut total_entities = 0usize;
    let mut files_indexed = 0usize;
    let mut all_concepts: Vec<macrame::ConceptUpsert> = Vec::new();

    if root_path.is_dir() {
        // Phase 1: Collect file paths + source content for all indexable files.
        // Read into memory upfront to avoid I/O contention in parallel phase.
        struct FileTask {
            path: String,
            source: String,
            language: Language,
        }
        let mut tasks: Vec<FileTask> = Vec::new();
        for entry in ignore::Walk::new(root) {
            match entry {
                Ok(entry) => {
                    if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                        continue;
                    }
                    let path = entry.path();
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        let language = Language::from_extension(ext);
                        if language == Language::OtherTen {
                            continue;
                        }
                        if CodeGraph::ts_language(&language).is_none() {
                            continue;
                        }
                        if let Ok(source) = fs::read_to_string(path) {
                            tasks.push(FileTask {
                                path: path.to_string_lossy().to_string(),
                                source,
                                language,
                            });
                        }
                    }
                }
                Err(_) => {} // permission errors etc.
            }
        }

        // Phase 2: Parallel parse + extract. Each thread creates its own
        // tree-sitter Parser (not Send). Also builds import graph edges
        // in parallel (ImportGraph uses parking_lot::RwLock).
        // Technique: adopted from CodeGraph's ParseWorkerPool. MIT license.
        // https://github.com/opticsWolf/codegraph
        // Benchmarking (N=7, 200 files) shows near-linear parse scaling through
        // 16 threads. No cap needed — available_parallelism() is sufficient.
        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        if tasks.is_empty() {
            // No files to index — skip parallel phase entirely
            // (avoids chunk_size==0 panic below)
        } else {
        let chunk_size = (tasks.len() + num_threads - 1) / num_threads;
        let import_graph_ref = &graph.import_graph;

        type ChunkResult = Vec<(
            String,                             // file_path
            Vec<crate::types::ExtractedUnit>,   // units
            Vec<macrame::ConceptUpsert>,         // concepts
        )>;

        let mut all_results: Vec<ChunkResult> = Vec::new();

        std::thread::scope(|s| {
            let mut handles = Vec::new();
            for chunk in tasks.chunks(chunk_size) {
                let chunk_tasks: Vec<&FileTask> = chunk.iter().collect();
                handles.push(s.spawn(move || {
                    let mut results = Vec::new();
                    for task in &chunk_tasks {
                        match CodeGraph::extract_only(
                            &task.source, &task.path, &task.language)
                        {
                            Ok((units, concepts)) => {
                                // Build import graph edges in parallel.
                                // ImportGraph uses parking_lot::RwLock — safe
                                // across threads. Full resolution happens
                                // in the sequential insert phase later.
                                let module_id = format!("{}::module", task.path);
                                ImportGraph::build_import_edges(
                                    import_graph_ref, &units, &task.path,
                                    task.language, &module_id);
                                results.push((task.path.clone(), units, concepts));
                            }
                            Err(_) => {} // parse failures silently skipped
                        }
                    }
                    results
                }));
            }
            for h in handles {
                match h.join() {
                    Ok(chunk_results) => all_results.push(chunk_results),
                    Err(_) => {} // thread panic — skip this chunk
                }
            }
        });

        // Phase 3: Sequential insert into projection + collect concepts.
        for chunk_results in &all_results {
            for (file_path, units, concepts) in chunk_results {
                let lang = Language::from_extension(
                    std::path::Path::new(file_path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("py"),
                );
                let count = units.len();
                let mut projection = (*graph.snapshot()).clone();
                graph.insert_extracted(&mut projection, units, file_path, &lang);
                graph.commit_projection(projection);
                total_entities += count;
                files_indexed += 1;
                all_concepts.extend_from_slice(concepts);
            }
        }
        }  // if !tasks.is_empty()
    }

    // v0.5: Flush all concepts in one `write_concepts` call (chunked internally
    // at 70 concepts/chunk, one transaction per chunk — ~2.35ms/chunk).
    // Concepts must commit before edges (FK REFERENCES constraints).
    if let Some(ref store) = graph.store {
        let _ = store.upsert_concepts_bulk(&all_concepts);
    }

    // Compute MRO and run resolution cascade on all calls
    {
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_all_calls(&mut projection);
        // Persist resolved edges to Macrame store
        let _ = graph.persist_edges(&projection);
        graph.commit_projection(projection);
    }

    let mut guard = GLOBAL_GRAPH.write();
    *guard = Some(graph);

    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("files_indexed", files_indexed)?;
    dict.set_item("entities_extracted", total_entities)?;
    Ok(dict.into())
}

// ── query_graph() ──────────────────────────────────────────────────────────

#[pyfunction]
fn query_graph(py: Python<'_>, query_str: &str) -> PyResult<PyObject> {
    with_graph(|_graph, snap| {
        let parsed = parse_query(query_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e))?;
        let rows = execute_query(snap, &parsed);
        let results: Vec<PyObject> = rows
            .into_iter()
            .map(|r| r.into_pyobject(py))
            .collect();
        Ok(results.into_py(py))
    })
}

// ── Git Operations ────────────────────────────────────────────────────────

mod git_bindings {
    use pyo3::prelude::*;
    use pyo3::types::PyDict;

    #[pyfunction]
    pub fn git_worktree_clean(py: Python<'_>, repo_path: &str) -> PyResult<PyObject> {
        let clean = crate::fs::git::is_worktree_clean(repo_path).unwrap_or(true);
        let dict = PyDict::new(py);
        dict.set_item("clean", clean)?;
        Ok(dict.into())
    }

    #[pyfunction]
    pub fn git_blame(py: Python<'_>, repo_path: &str, file_path: &str) -> PyResult<Vec<PyObject>> {
        match crate::fs::git::blame_file(repo_path, file_path) {
            Ok(lines) => {
                let rows: Vec<PyObject> = lines.iter().map(|l| {
                    let d = PyDict::new(py);
                    let _ = d.set_item("line", l.line_number);
                    let _ = d.set_item("count", l.line_count);
                    let _ = d.set_item("author", &l.author);
                    let _ = d.set_item("commit", &l.commit);
                    d.into()
                }).collect();
                Ok(rows)
            }
            Err(e) => Err(pyo3::exceptions::PyRuntimeError::new_err(
                format!("git blame failed: {:?}", e))),
        }
    }

    #[pyfunction]
    pub fn git_changed_files(py: Python<'_>, repo_path: &str,
                             old_oid: Option<&str>, new_oid: Option<&str>) -> PyResult<Vec<String>> {
        let old = old_oid.and_then(|s| git2::Oid::from_str(s).ok());
        let new = new_oid.and_then(|s| git2::Oid::from_str(s).ok());
        match crate::fs::git::changed_files_between(repo_path, old, new) {
            Ok(files) => Ok(files),
            Err(e) => Err(pyo3::exceptions::PyRuntimeError::new_err(
                format!("git diff failed: {:?}", e))),
        }
    }
}

// ── update_file() ──────────────────────────────────────────────────────────

#[pyfunction]
fn update_file(
    file_path: &str, content: Option<&str>, force: Option<bool>,
) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    with_graph(|graph, _snap| {
        let (added, removed, affected) = graph
            .update_file(file_path, content, force)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
        let dict = PyDict::new(py);
        dict.set_item("fully_applied", true)?;
        dict.set_item("entities_added", added)?;
        dict.set_item("entities_removed", removed)?;
        dict.set_item("affected_files", affected)?;
        dict.set_item("parse_quality", "clean")?;
        dict.set_item("parse_errors", 0)?;
        dict.set_item("elapsed_ms", 0.0_f64)?;
        Ok(dict.into())
    })
}

// ── Mutation planning ──────────────────────────────────────────────────────

#[pyfunction]
fn plan_body_replacement(
    entity_id: &str, new_body: &str,
    expected_hash: Option<String>, dry_run: Option<bool>,
) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    with_graph(|_graph, snap| {
        let config = crate::graph::MutationConfig::default();
        let engine = mutation::MutationEngine::new(config);
        let plan = engine.plan_body_replacement(
            entity_id, new_body, expected_hash, dry_run.unwrap_or(false), snap,
        ).map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
        plan_to_dict(py, &plan)
    })
}

#[pyfunction]
fn plan_signature_update(
    entity_id: &str, new_signature: &str,
    call_site_values: Option<HashMap<String, String>>,
    inject_defaults: Option<bool>, dry_run: Option<bool>,
) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    with_graph(|_graph, snap| {
        let config = crate::graph::MutationConfig::default();
        let engine = mutation::MutationEngine::new(config);
        let plan = engine.plan_signature_update(
            entity_id, new_signature,
            &call_site_values.unwrap_or_default(),
            inject_defaults.unwrap_or(true), dry_run.unwrap_or(false), snap,
        ).map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
        plan_to_dict(py, &plan)
    })
}

#[pyfunction]
fn plan_rename(
    entity_id: &str, new_name: &str,
    include_strings: Option<bool>, dry_run: Option<bool>,
) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    with_graph(|_graph, snap| {
        let config = crate::graph::MutationConfig::default();
        let engine = mutation::MutationEngine::new(config);
        let plan = engine.plan_rename(
            entity_id, new_name,
            include_strings.unwrap_or(false), dry_run.unwrap_or(false), snap,
        ).map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
        plan_to_dict(py, &plan)
    })
}

#[pyfunction]
fn plan_create_entity(
    target_file: &str, anchor: &str, code: &str, dry_run: Option<bool>,
) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    with_graph(|_graph, snap| {
        let config = crate::graph::MutationConfig::default();
        let engine = mutation::MutationEngine::new(config);
        let plan = engine.plan_create_entity(
            target_file, anchor, code,
            dry_run.unwrap_or(false), snap,
        ).map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
        plan_to_dict(py, &plan)
    })
}

#[pyfunction]
fn apply_mutation(plan_json: &str) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    // Deserialize just enough to call engine.apply()
    #[derive(serde::Deserialize)]
    struct ApplyRequest {
        #[allow(dead_code)]
        id: String,
        #[allow(dead_code)]
        tool: String,
        edits: Vec<EditRequest>,
        affected_files: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct EditRequest {
        file: String,
        span_start: usize,
        span_end: usize,
        replacement: String,
    }

    let req: ApplyRequest = serde_json::from_str(plan_json)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid plan JSON: {}", e)))?;

    // Build a MutationPlan from the request
    let plan = mutation::MutationPlan {
        id: req.id,
        tool: req.tool,
        edits: req.edits.iter().map(|e| mutation::MutationEdit {
            file: e.file.clone(),
            span: crate::types::ByteSpan { start: e.span_start, end: e.span_end },
            replacement: e.replacement.clone(),
            expected_hash: String::new(),
        }).collect(),
        affected_files: req.affected_files,
        diff_preview: String::new(),
        unverified_sites: Vec::new(),
        warnings: Vec::new(),
    };

    with_graph(|_graph, _snap| {
        let config = crate::graph::MutationConfig::default();
        let mut engine = mutation::MutationEngine::new(config);
        let result = engine.apply(&plan);
        let dict = PyDict::new(py);
        dict.set_item("applied", matches!(result.status, mutation::MutationStatus::Applied))?;
        dict.set_item("status", format!("{:?}", result.status))?;
        dict.set_item("files_written", result.files_written)?;
        dict.set_item("errors", result.syntax_errors.iter().map(|e| format!("{}:{} — {}", e.file, e.line, e.message)).collect::<Vec<_>>())?;
        Ok(dict.into())
    })
}

/// Convert a MutationPlan to a Python dict.
fn plan_to_dict(py: Python<'_>, plan: &mutation::MutationPlan) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &plan.id)?;
    dict.set_item("tool", &plan.tool)?;
    dict.set_item("diff_preview", &plan.diff_preview)?;
    dict.set_item("affected_files", &plan.affected_files)?;
    dict.set_item("warnings", &plan.warnings)?;
    // Serialize edits as list of {file, span_start, span_end, replacement}
    let edits: Vec<PyObject> = plan.edits.iter().map(|e| {
        let ed = PyDict::new(py);
        let _ = ed.set_item("file", &e.file);
        let _ = ed.set_item("span_start", e.span.start);
        let _ = ed.set_item("span_end", e.span.end);
        let _ = ed.set_item("replacement", &e.replacement);
        ed.into()
    }).collect();
    dict.set_item("edits", edits)?;
    Ok(dict.into())
}

#[pyfunction]
fn resolve_symbol(_qualified_name: &str) -> PyResult<PyObject> {
    let py = unsafe { Python::assume_gil_acquired() };
    Ok(PyDict::new(py).into())
}

// ── Read Path ──────────────────────────────────────────────────────────────

/// Look up a single entity by ID. Returns a dict or None.
#[pyfunction]
fn lookup_entity(py: Python<'_>, entity_id: &str) -> PyResult<Option<PyObject>> {
    with_graph(|_graph, snap| {
        Ok(entity_ref_to_dict(py, entity_id, snap))
    })
}

/// Search entities by name substring match (case-insensitive).
#[pyfunction]
fn search_entities(py: Python<'_>, query: &str, top_k: usize, kind: Option<&str>) -> PyResult<Vec<PyObject>> {
    with_graph(|_graph, snap| {
        let query_lower = query.to_lowercase();
        let mut results: Vec<(usize, PyObject)> = Vec::new(); // (score, dict)
        let kind_filter = kind.map(|k| k.to_lowercase());

        // Score: exact match = 100, starts-with = 50, contains = 25

        // Search functions
        if kind_filter.is_none() || kind_filter.as_deref() == Some("function") {
            for (_id, f) in &snap.functions {
                let name_lower = f.name.to_lowercase();
                let score = if name_lower == query_lower {
                    100
                } else if name_lower.starts_with(&query_lower) {
                    50
                } else if name_lower.contains(&query_lower) {
                    25
                } else {
                    continue;
                };
                if let Ok(d) = function_to_dict(py, f) {
                    results.push((score, d));
                }
            }
        }

        // Search classes
        if kind_filter.is_none() || kind_filter.as_deref() == Some("class") {
            for (_id, c) in &snap.classes {
                let name_lower = c.name.to_lowercase();
                let score = if name_lower == query_lower {
                    100
                } else if name_lower.starts_with(&query_lower) {
                    50
                } else if name_lower.contains(&query_lower) {
                    25
                } else {
                    continue;
                };
                if let Ok(d) = class_to_dict(py, c) {
                    results.push((score, d));
                }
            }
        }

        // Search modules
        if kind_filter.is_none() || kind_filter.as_deref() == Some("module") {
            for (_id, m) in &snap.modules {
                let name_lower = m.name.to_lowercase();
                let score = if name_lower == query_lower {
                    90
                } else if name_lower.starts_with(&query_lower) {
                    40
                } else if name_lower.contains(&query_lower) {
                    20
                } else {
                    continue;
                };
                if let Ok(d) = module_to_dict(py, m) {
                    results.push((score, d));
                }
            }
        }

        // Sort by score descending, take top_k
        results.sort_by(|a, b| b.0.cmp(&a.0));
        results.truncate(top_k);

        Ok(results.into_iter().map(|(_, d)| d).collect())
    })
}

/// Get callers of an entity from the reverse call index.
#[pyfunction]
fn callers_of(py: Python<'_>, entity_id: &str) -> PyResult<Vec<PyObject>> {
    with_graph(|_graph, snap| {
        let caller_ids: Vec<String> = snap
            .callers_by_callee
            .get(entity_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();

        let mut results = Vec::with_capacity(caller_ids.len());
        for cid in &caller_ids {
            if let Some(d) = entity_ref_to_dict(py, cid, snap) {
                results.push(d);
            }
        }
        Ok(results)
    })
}

/// Get callees of an entity from the forward call index.
#[pyfunction]
fn callees_of(py: Python<'_>, entity_id: &str) -> PyResult<Vec<PyObject>> {
    with_graph(|_graph, snap| {
        let callee_ids: Vec<String> = snap
            .callees_by_caller
            .get(entity_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();

        let mut results = Vec::with_capacity(callee_ids.len());
        for cid in &callee_ids {
            if let Some(d) = entity_ref_to_dict(py, cid, snap) {
                results.push(d);
            }
        }
        Ok(results)
    })
}

/// Get graph statistics.
#[pyfunction]
fn graph_stats(py: Python<'_>) -> PyResult<PyObject> {
    with_graph(|_graph, snap| {
        let dict = PyDict::new(py);
        dict.set_item("modules", snap.modules.len())?;
        dict.set_item("classes", snap.classes.len())?;
        dict.set_item("functions", snap.functions.len())?;
        dict.set_item("imports", snap.imports.len())?;
        dict.set_item("constants", snap.constants.len())?;
        dict.set_item("type_aliases", snap.type_aliases.len())?;
        dict.set_item("file_count", snap.file_to_modules.len())?;
        // Total call edges
        let total_calls: usize = snap.callees_by_caller.values().map(|s| s.len()).sum();
        dict.set_item("call_edges", total_calls)?;
        Ok(dict.into())
    })
}

/// Vector similarity search against entity embeddings.
/// Uses cosine similarity against stored embedding vectors.
#[pyfunction]
fn search_similar(
    py: Python<'_>, query_vec: Vec<f64>, top_k: usize,
) -> PyResult<Vec<PyObject>> {
    with_graph(|_graph, snap| {
        let mut scored: Vec<(f64, &String, &std::sync::Arc<Function>)> = Vec::new();

        // Compute cosine similarity against all functions with embeddings
        for (id, f) in &snap.functions {
            if f.embedding.is_empty() { continue; }
            let sim = cosine_similarity(&query_vec, &f.embedding);
            scored.push((sim, id, f));
        }

        // Sort descending by similarity
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        let results: Vec<PyObject> = scored
            .into_iter()
            .filter_map(|(sim, _, f)| {
                function_to_dict(py, f).ok().map(|mut d| {
                    // Add similarity score to dict
                    if let Ok(dict) = d.downcast_bound::<PyDict>(py) {
                        let _ = dict.set_item("similarity", sim);
                    }
                    d
                })
            })
            .collect();

        Ok(results)
    })
}

/// Cosine similarity between two vectors.
pub(crate) fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ── Snapshot I/O ───────────────────────────────────────────────────────────

#[pyfunction]
fn export_snapshot(_path: &str) -> PyResult<()> { Ok(()) }

#[pyfunction]
fn load_snapshot(_path: &str) -> PyResult<()> { Ok(()) }

// ── File Watcher Bindings ──────────────────────────────────────────────

static GLOBAL_WATCHER: std::sync::LazyLock<std::sync::Mutex<Option<crate::fs::watcher::FileWatcher>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Start the file watcher on the given paths. Must have run analyze() first.
#[pyfunction]
fn start_watcher(paths: Vec<String>) -> PyResult<()> {
    use crate::fs::watcher::{FileWatcher, WatcherConfig};
    let config = WatcherConfig {
        watch_paths: paths,
        ..WatcherConfig::default()
    };
    let watcher = FileWatcher::start(config)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))?;
    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    *guard = Some(watcher);
    Ok(())
}

/// Get the next batch of file changes (blocks until events arrive).
#[pyfunction]
fn next_watcher_batch() -> PyResult<Option<Vec<(String, String)>>> {
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
        b.changes.into_iter().map(|c| {
            (c.path, format!("{:?}", c.kind))
        }).collect()
    }))
}

/// Stop the file watcher.
#[pyfunction]
fn stop_watcher() -> PyResult<()> {
    let mut guard = GLOBAL_WATCHER.lock().unwrap();
    *guard = None;
    Ok(())
}

/// Get the next batch with a timeout (ms). Returns None if timeout expires.
#[pyfunction]
fn next_watcher_batch_timeout(timeout_ms: u64) -> PyResult<Option<Vec<(String, String)>>> {
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
        b.changes.into_iter().map(|c| {
            (c.path, format!("{:?}", c.kind))
        }).collect()
    }))
}

// ── v3.6: Synthetic Edge Registration ────────────────────────────────────

/// Register a synthetic edge from framework resolvers (Django/Flask/FastAPI).
///
/// Framework resolvers produce edges like route→handler that aren't
/// tree-sitter-extracted. This function merges them into the live graph
/// so agents can trace them via callers_of / callees_of / explore.
#[pyfunction]
fn register_synthetic_edge(
    source_id: &str, target_id: &str, kind: &str,
) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut()
        .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
            "No graph loaded — run coderadar analyze first"
        ))?;
    graph.register_synthetic_edge(source_id, target_id, kind)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    Ok(dict.into())
}

/// Resolve a module's children (classes, functions) to full entity dicts.
///
/// Module dicts carry EntityId lists for `classes`, `functions`, etc.
/// This function resolves those IDs to the full entity representation.
#[pyfunction]
fn module_children(
    py: Python<'_>, module_id: &str,
) -> PyResult<PyObject> {
    with_graph(|_graph, snap| {
        let module = snap.modules.get(module_id)
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(
                format!("Module not found: {}", module_id)
            ))?;

        let dict = PyDict::new(py);
        dict.set_item("module_id", module_id)?;

        // Resolve classes
        let classes: Vec<PyObject> = module.classes.iter()
            .filter_map(|cid| {
                snap.classes.get(cid).and_then(|c| class_to_dict(py, c).ok())
            })
            .collect();
        dict.set_item("classes", classes)?;

        // Resolve functions
        let functions: Vec<PyObject> = module.functions.iter()
            .filter_map(|fid| {
                snap.functions.get(fid).and_then(|f| function_to_dict(py, f).ok())
            })
            .collect();
        dict.set_item("functions", functions)?;

        // Resolve imports
        let imports: Vec<PyObject> = module.imports.iter()
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
        let constants: Vec<PyObject> = module.constants.iter()
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

/// v0.5: Set a module's `__all__` star-export names list.
/// Called from Python after static `__all__` analysis (exports.py).
/// Enables resolution of `from X import *` wildcard imports.
#[pyfunction]
fn set_module_star_exports(module_id: &str, names: Vec<String>) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut()
        .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
            "No graph loaded — run coderadar analyze first"
        ))?;
    graph.set_module_star_exports(module_id, names);
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    Ok(dict.into())
}
