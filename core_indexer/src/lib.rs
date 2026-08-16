// CodeRadar v3.6 — Rust Core Library
// PyO3 bindings for the Python layer (MCP server, CLI, resolvers).

pub mod buffers;
pub mod extract;
pub mod fs;
pub mod graph;
pub mod mutation;
pub mod query;
pub mod resolve;
pub mod smells;
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
    Class, Constant, EmbeddingVec, Function, Import, Module, ProjectedGraph, TypeAlias,
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
    m.add_function(wrap_pyfunction!(traverse, m)?)?;
    m.add_function(wrap_pyfunction!(traverse_unresolved, m)?)?;
    m.add_function(wrap_pyfunction!(lookup_entity, m)?)?;
    m.add_function(wrap_pyfunction!(search_entities, m)?)?;
    m.add_function(wrap_pyfunction!(graph_stats, m)?)?;
    m.add_function(wrap_pyfunction!(index_edge_stats, m)?)?;
    m.add_function(wrap_pyfunction!(get_smells, m)?)?;
    m.add_function(wrap_pyfunction!(export_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(load_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(search_similar, m)?)?;
    m.add_function(wrap_pyfunction!(register_synthetic_edge, m)?)?;
    m.add_function(wrap_pyfunction!(set_embedding, m)?)?;
    m.add_function(wrap_pyfunction!(clear_embeddings_for_file, m)?)?;
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

/// Root the current graph was indexed from.
///
/// The mutation policy confines writes to it, so a plan cannot reach outside
/// the project it was planned against. Set by `analyze`.
static INDEXED_ROOT: std::sync::LazyLock<RwLock<Option<std::path::PathBuf>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

/// Build a MutationEngine confined to the indexed root.
fn mutation_engine() -> mutation::MutationEngine {
    let engine = mutation::MutationEngine::new(crate::graph::MutationConfig::default());
    match INDEXED_ROOT.read().clone() {
        Some(root) => engine.with_project_root(root),
        None => engine,
    }
}

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

/// Like `with_graph`, but the read lock is released BEFORE `f` runs — `f`
/// receives an owned `Arc<ProjectedGraph>` snapshot. For long-running reads
/// (the smell engine) that must not block a writer (`reindex`/`update_file`).
fn with_graph_snapshot<F, R>(f: F) -> PyResult<R>
where
    F: FnOnce(Arc<ProjectedGraph>) -> PyResult<R>,
{
    let snap = {
        let guard = GLOBAL_GRAPH.read();
        match guard.as_ref() {
            Some(g) => g.snapshot(),
            None => {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "No graph loaded — run coderadar init first",
                ));
            }
        }
    };
    f(snap)
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
    dict.set_item("has_embedding", !m.embedding.vec.is_empty())?;
    dict.set_item("embedding_hash", m.embedding.hash.clone())?;
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
    dict.set_item("has_embedding", !c.embedding.vec.is_empty())?;
    dict.set_item("embedding_hash", c.embedding.hash.clone())?;
    dict.set_item("embedding_hash", c.embedding.hash.clone())?;
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
    if !f.embedding.vec.is_empty() {
        dict.set_item("has_embedding", true)?;
        dict.set_item("embedding_hash", f.embedding.hash.clone())?;
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
    dict.set_item("has_embedding", !i.embedding.vec.is_empty())?;
    dict.set_item("embedding_hash", i.embedding.hash.clone())?;
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
    dict.set_item("has_embedding", !c.embedding.vec.is_empty())?;
    dict.set_item("embedding_hash", c.embedding.hash.clone())?;
    dict.set_item("embedding_hash", c.embedding.hash.clone())?;
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
    dict.set_item("has_embedding", !ta.embedding.vec.is_empty())?;
    dict.set_item("embedding_hash", ta.embedding.hash.clone())?;
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
                        let mut language = Language::from_extension(ext);
                        // Fallback: filename-based detection (Dockerfile, CMake)
                        if language == Language::OtherTen {
                            language = Language::from_filename(
                                &path.to_string_lossy());
                        }
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
                    } else {
                        // Files without extension (Dockerfile, CMakeLists.txt)
                        let language = Language::from_filename(
                            &path.to_string_lossy());
                        if language != Language::OtherTen
                            && CodeGraph::ts_language(&language).is_some()
                        {
                            if let Ok(source) = fs::read_to_string(path) {
                                tasks.push(FileTask {
                                    path: path.to_string_lossy().to_string(),
                                    source,
                                    language,
                                });
                            }
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
        } else {
        // Sort by source size descending, then round-robin across threads
        // so large files (e.g. 300KB+ TypeScript) don't all land on one thread.
        tasks.sort_by(|a, b| b.source.len().cmp(&a.source.len()));
        let import_graph_ref = &graph.import_graph;

        type ChunkResult = Vec<(
            ProjectedGraph,                    // local fragment
            Vec<macrame::ConceptUpsert>,        // concepts
        )>;

        let mut all_results: Vec<ChunkResult> = Vec::new();

        // Bucket assignment: thread i gets tasks[i], tasks[i+N], tasks[i+2N], ...
        let mut buckets: Vec<Vec<&FileTask>> = (0..num_threads).map(|_| Vec::new()).collect();
        for (i, task) in tasks.iter().enumerate() {
            buckets[i % num_threads].push(task);
        }

        std::thread::scope(|s| {
            let mut handles = Vec::new();
            for bucket in buckets {
                if bucket.is_empty() { continue; }
                handles.push(s.spawn(move || {
                    let mut results = Vec::new();
                    for task in &bucket {
                        match CodeGraph::extract_only(
                            &task.source, &task.path, &task.language)
                        {
                            Ok((units, concepts)) => {
                                let module_id = format!("{}::module", task.path);
                                ImportGraph::build_import_edges(
                                    import_graph_ref, &units, &task.path,
                                    task.language, &module_id);
                                let fragment = CodeGraph::build_fragment(
                                    &units, &task.path, &task.language);
                                results.push((fragment, concepts));
                            }
                            Err(_) => {}
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

        // Phase 3: Merge all local fragments into the main projection.
        // Fragment keys are unique per file, so HashMap::extend is safe.
        // This replaces the old sequential projection-clone + insert_extracted
        // pattern, which was O(n²) in files (cloning the growing projection n times).
        {
            let mut proj = (*graph.snapshot()).clone();
            for chunk_results in all_results {
                for (fragment, concepts) in chunk_results {
                    let entity_count = fragment.functions.len()
                        + fragment.classes.len()
                        + fragment.imports.len()
                        + fragment.constants.len()
                        + fragment.type_aliases.len();
                    proj.modules.extend(fragment.modules);
                    proj.classes.extend(fragment.classes);
                    proj.functions.extend(fragment.functions);
                    proj.imports.extend(fragment.imports);
                    proj.constants.extend(fragment.constants);
                    proj.type_aliases.extend(fragment.type_aliases);
                    for (k, v) in fragment.file_to_modules {
                        proj.file_to_modules.entry(k).or_default().extend(v);
                    }
                    proj.module_by_dotted_name.extend(fragment.module_by_dotted_name);
                    for (k, v) in fragment.importers { proj.importers.entry(k).or_default().extend(v); }
                    for (k, v) in fragment.callers_by_callee { proj.callers_by_callee.entry(k).or_default().extend(v); }
                    for (k, v) in fragment.callees_by_caller { proj.callees_by_caller.entry(k).or_default().extend(v); }
                    for (k, v) in fragment.subclasses { proj.subclasses.entry(k).or_default().extend(v); }
                    for (k, v) in fragment.overridden_by { proj.overridden_by.entry(k).or_default().extend(v); }
                    total_entities += entity_count;
                    files_indexed += 1;
                    all_concepts.extend(concepts);
                }
            }
            graph.commit_projection(proj);
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
        graph.resolve_imports(&mut projection);
        graph.populate_class_methods(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_all_calls(&mut projection);
        // Persist resolved edges to Macrame store
        let _ = graph.persist_edges(&projection);
        graph.commit_projection(projection);
    }

    let mut guard = GLOBAL_GRAPH.write();
    *guard = Some(graph);
    *INDEXED_ROOT.write() = std::fs::canonicalize(root)
        .ok()
        .or_else(|| Some(std::path::PathBuf::from(root)));

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
        let outcome = graph
            .update_file(file_path, content, force)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
        let quality = match outcome.parse_quality {
            crate::types::ParseQuality::Clean => "clean",
            crate::types::ParseQuality::Partial => "partial",
            crate::types::ParseQuality::Tainted => "tainted",
            crate::types::ParseQuality::Deferred => "deferred",
        };
        let dict = PyDict::new(py);
        // "Fully applied" means the file parsed cleanly and the graph took
        // every entity in it; a recovered parse is a partial update.
        dict.set_item("fully_applied", outcome.parse_errors == 0)?;
        dict.set_item("entities_added", outcome.entities_added)?;
        dict.set_item("entities_removed", outcome.entities_removed)?;
        dict.set_item("affected_files", outcome.affected_files)?;
        dict.set_item("parse_quality", quality)?;
        dict.set_item("parse_errors", outcome.parse_errors)?;
        dict.set_item("elapsed_ms", outcome.elapsed_ms)?;
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
        let engine = mutation_engine();
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
        let engine = mutation_engine();
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
        let engine = mutation_engine();
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
        let engine = mutation_engine();
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
        #[serde(default)]
        expected_hash: String,
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
            expected_hash: e.expected_hash.clone(),
        }).collect(),
        affected_files: req.affected_files,
        diff_preview: String::new(),
        unverified_sites: Vec::new(),
        warnings: Vec::new(),
    };

    with_graph(|_graph, _snap| {
        let mut engine = mutation_engine();
        let result = engine.apply(&plan);
        let dict = PyDict::new(py);
        dict.set_item("applied", matches!(result.status, mutation::MutationStatus::Applied))?;
        dict.set_item("status", format!("{:?}", result.status))?;
        dict.set_item("files_written", result.files_written)?;
        dict.set_item("errors", result.syntax_errors.iter().map(|e| format!("{}:{} — {}", e.file, e.line, e.message)).collect::<Vec<_>>())?;
        dict.set_item("backup_path", result.backup_path.clone().unwrap_or_default())?;
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
    // Serialize edits as list of {file, span_start, span_end, replacement, expected_hash}
    let edits: Vec<PyObject> = plan.edits.iter().map(|e| {
        let ed = PyDict::new(py);
        let _ = ed.set_item("file", &e.file);
        let _ = ed.set_item("span_start", e.span.start);
        let _ = ed.set_item("span_end", e.span.end);
        let _ = ed.set_item("replacement", &e.replacement);
        let _ = ed.set_item("expected_hash", &e.expected_hash);
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

/// Generalized edge-kind traversal over the in-memory `ProjectedGraph`.
///
/// Thin PyO3 wrapper around `CodeGraph::traverse_bfs` (graph.rs): validates
/// direction + the `as_of` honesty guard, acquires the snapshot via
/// `with_graph`, releases the GIL for the BFS, and re-acquires it only to
/// materialize result dicts. The BFS itself is pure Rust and unit-tested in
/// graph.rs (no GLOBAL_GRAPH needed there).
///
/// Edge kinds (case-insensitive; `inherits` is an alias for `extends`):
///   - `calls`    → callers_by_callee / callees_by_caller
///   - `imports`  → importers (upstream) / imports_by_importer (downstream)
///   - `extends`  → subclasses (upstream) / Class.resolved_bases (downstream)
///   - `overrides`→ overridden_by (upstream) / overrides_base (downstream)
///
/// Direction accepts both vocabularies: `in`/`upstream`, `out`/`downstream`,
/// `both`. Upstream = dependents (who calls/imports/extends me).
///
/// The start entity is included at depth 0; each reached neighbor is tagged
/// with its BFS depth and the edge kind that first reached it. Entities not
/// present in the graph (e.g. `external::` targets) are naturally filtered —
/// `entity_ref_to_dict` returns `None` for them.
#[pyfunction]
#[pyo3(signature = (start_id, max_depth, edge_kinds, direction, as_of=None))]
fn traverse(
    py: Python<'_>,
    start_id: &str,
    max_depth: usize,
    edge_kinds: Vec<String>,
    direction: &str,
    as_of: Option<String>,
) -> PyResult<Vec<PyObject>> {
    // ── Direction normalization ────────────────────────────────────
    let (up, down) = match direction.trim().to_ascii_lowercase().as_str() {
        "in" | "upstream" => (true, false),
        "out" | "downstream" => (false, true),
        "both" => (true, true),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Unknown direction `{other}` (expected upstream/in, downstream/out, both)"
            )));
        }
    };

    // ── Normalize + dedupe edge kinds (`inherits` → `extends`) ──────
    let kinds: Vec<String> = {
        let mut v: Vec<String> = edge_kinds
            .iter()
            .map(|k| {
                let k = k.trim().to_ascii_lowercase();
                if k == "inherits" { "extends".to_string() } else { k }
            })
            .collect();
        v.sort();
        v.dedup();
        v
    };

    // ── Temporal traversal: route `as_of` to Macrame (downstream only) ──
    if let Some(ts) = as_of {
        if up {
            return Err(pyo3::exceptions::PyNotImplementedError::new_err(
                "as_of traversal supports downstream ('out') only — Macrame's \
                 TraversalBuilder follows outgoing edges",
            ));
        }
        let edge_types: Vec<String> = kinds
            .iter()
            .filter_map(|k| match k.as_str() {
                "calls" => Some("CALLS".to_string()),
                "imports" => Some("IMPORTS".to_string()),
                "extends" => Some("EXTENDS".to_string()),
                "overrides" => Some("OVERRIDES".to_string()),
                _ => None,
            })
            .collect();
        let start_owned = start_id.to_string();
        let ts_owned = ts.to_string();
        // Snapshot graph + store under the read lock, then release the lock
        // BEFORE the DB traversal — a slow `load_subgraph_with` must not
        // block a writer (`reindex`/`update_file`). Mirrors the 2.6 fix.
        let (snap, store) = {
            let guard = GLOBAL_GRAPH.read();
            match guard.as_ref() {
                Some(g) => (g.snapshot(), g.store.clone()),
                None => {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "No graph loaded — run coderadar init first",
                    ));
                }
            }
        };
        let store = match store {
            Some(s) => s,
            None => {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "No persistent store — temporal traversal needs a .coderadar store",
                ));
            }
        };
        let sub = py
            .allow_threads({
                let s = start_owned.clone();
                let e = edge_types.clone();
                let t = ts_owned.clone();
                move || store.traverse_at(&s, max_depth, &e, &t)
            })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("as_of traversal failed: {e:?}")))?;
        let reached = subgraph_bfs(&sub, &start_owned, max_depth);
        let mut results = Vec::with_capacity(reached.len());
        for (id, depth, ek) in reached {
            if let Some(d) = entity_ref_to_dict(py, &id, &snap) {
                if let Ok(dd) = d.downcast_bound::<pyo3::types::PyDict>(py) {
                    let _ = dd.set_item("depth", depth);
                    let _ = dd.set_item("edge_type", &ek);
                }
                results.push(d);
            }
        }
        return Ok(results);
    }

    // ── Current-state in-memory BFS ─────────────────────────────────
    with_graph(|_graph, snap| {
        if entity_ref_to_dict(py, start_id, snap).is_none() {
            return Ok(Vec::new());
        }
        let snap_owned: std::sync::Arc<ProjectedGraph> = snap.clone();
        let snap_for_bfs: std::sync::Arc<ProjectedGraph> = snap_owned.clone();
        let start = start_id.to_string();
        let kinds_for_bfs = kinds.clone();
        let reached: Vec<(String, usize, String)> = py.allow_threads(move || {
            crate::graph::CodeGraph::traverse_bfs(&snap_for_bfs, &start, max_depth, &kinds_for_bfs, up, down)
        });
        let mut results = Vec::with_capacity(reached.len());
        for (id, depth, ek) in reached {
            if let Some(d) = entity_ref_to_dict(py, &id, &snap_owned) {
                if let Ok(dd) = d.downcast_bound::<pyo3::types::PyDict>(py) {
                    let _ = dd.set_item("depth", depth);
                    let _ = dd.set_item("edge_type", &ek);
                }
                results.push(d);
            }
        }
        Ok(results)
    })
}

/// BFS over a Macrame `Subgraph` to recover (node, depth, edge_type) tuples.
/// `Subgraph` stores topology + edge types but not BFS depth, so depth is
/// recomputed here (the `as_of` path uses this instead of the in-memory BFS).
fn subgraph_bfs(
    sub: &macrame::graph::Subgraph,
    start: &str,
    max_depth: usize,
) -> Vec<(String, usize, String)> {
    use std::collections::{HashSet, VecDeque};
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut out: Vec<(String, usize, String)> = Vec::new();
    if !sub.contains_node(start) {
        return out;
    }
    visited.insert(start.to_string());
    queue.push_back((start.to_string(), 0usize));
    out.push((start.to_string(), 0usize, String::new()));
    while let Some((cur, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for edge in sub.out_edges(&cur) {
            let target = edge.node(sub).to_string();
            let etype = edge.edge_type(sub).to_string();
            if visited.insert(target.clone()) {
                out.push((target.clone(), depth + 1, etype));
                queue.push_back((target, depth + 1));
            }
        }
    }
    out
}

/// Count unresolved outgoing targets across the traversal from `start_id`.
/// Mirrors `traverse` (same BFS, direction/kinds normalization) but returns
/// the number of targets the walk could NOT follow — surfaces silent
/// truncation (plan 2.3) without changing the `traverse` contract.
#[pyfunction]
#[pyo3(signature = (start_id, max_depth, edge_kinds, direction))]
fn traverse_unresolved(
    py: Python<'_>,
    start_id: &str,
    max_depth: usize,
    edge_kinds: Vec<String>,
    direction: &str,
) -> PyResult<usize> {
    let (up, down) = match direction.trim().to_ascii_lowercase().as_str() {
        "in" | "upstream" => (true, false),
        "out" | "downstream" => (false, true),
        "both" => (true, true),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Unknown direction `{other}` (expected upstream/in, downstream/out, both)"
            )));
        }
    };

    let kinds: Vec<String> = {
        let mut v: Vec<String> = edge_kinds
            .iter()
            .map(|k| {
                let k = k.trim().to_ascii_lowercase();
                if k == "inherits" { "extends".to_string() } else { k }
            })
            .collect();
        v.sort();
        v.dedup();
        v
    };

    with_graph(|_graph, snap| {
        if entity_ref_to_dict(py, start_id, snap).is_none() {
            return Ok(0);
        }
        let snap_owned: std::sync::Arc<ProjectedGraph> = snap.clone();
        let start = start_id.to_string();
        let snap_for_bfs = snap_owned.clone();
        let kinds_for_bfs = kinds.clone();
        let reached: Vec<(String, usize, String)> = py.allow_threads(move || {
            crate::graph::CodeGraph::traverse_bfs(&snap_for_bfs, &start, max_depth, &kinds_for_bfs, up, down)
        });
        let total: usize = reached
            .iter()
            .map(|(id, _, _)| {
                crate::graph::CodeGraph::count_unresolved_targets(&snap_owned, id, &kinds, down)
            })
            .sum();
        Ok(total)
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

/// Read-only diagnostic: counts of every reverse/forward edge index.
/// Observability for the Phase-D back-fill (subclasses / importers /
/// overridden_by / overrides_base) and the call indexes. Cheap — one snap.
#[pyfunction]
fn index_edge_stats(py: Python<'_>) -> PyResult<PyObject> {
    with_graph(|_graph, snap| {
        let dict = PyDict::new(py);
        let count = |m: &std::collections::HashMap<String, std::collections::BTreeSet<String>>|
            m.values().map(|s| s.len()).sum::<usize>();
        dict.set_item("callers_by_callee", count(&snap.callers_by_callee))?;
        dict.set_item("callees_by_caller", count(&snap.callees_by_caller))?;
        dict.set_item("importers", count(&snap.importers))?;
        dict.set_item("subclasses", count(&snap.subclasses))?;
        dict.set_item("overridden_by", count(&snap.overridden_by))?;
        dict.set_item("overrides_base", snap.overrides_base.len())?;
        // How many Import entities actually resolved (resolution != Unresolved).
        let resolved_imports = snap.imports.values()
            .filter(|i| !matches!(i.resolution, crate::types::ImportResolution::Unresolved)).count();
        dict.set_item("resolved_imports", resolved_imports)?;
        // Keys-with-entries — useful even when edge count is 0 (shows the
        // index is non-empty but targets may have 0 inbound).
        dict.set_item("importer_keys", snap.importers.len())?;
        dict.set_item("subclass_keys", snap.subclasses.len())?;
        dict.set_item("overridden_by_keys", snap.overridden_by.len())?;
        // Ambiguous base-resolution findings (2.1b): count + truncated details.
        dict.set_item("ambiguous_bases", snap.ambiguous_bases.len())?;
        let details: Vec<PyObject> = snap
            .ambiguous_bases
            .iter()
            .take(20)
            .map(|a| {
                let d = PyDict::new(py);
                d.set_item("class", &a.class_name)?;
                d.set_item("base", &a.base_name)?;
                d.set_item("candidates", a.candidates.clone())?;
                Ok(d.into())
            })
            .collect::<PyResult<_>>()?;
        dict.set_item("ambiguous_base_details", details)?;
        Ok(dict.into())
    })
}

/// Detect code smells (architectural issues) across the resolved graph.
///
/// Runs the native `SmellEngine` (see `smells/`) over the current snapshot
/// with the GIL released, then materializes findings as dicts. Findings can
/// be filtered by `entity_id` and/or `rule_id`; each carries its rule id,
/// entity id + name, severity, message, and the metric signals that triggered
/// it.
#[pyfunction]
#[pyo3(signature = (entity_id=None, rule_id=None))]
fn get_smells(
    py: Python<'_>,
    entity_id: Option<String>,
    rule_id: Option<String>,
) -> PyResult<Vec<PyObject>> {
    with_graph_snapshot(|snap| {
        let engine = crate::smells::engine::SmellEngine::new();
        let snap_owned: Arc<ProjectedGraph> = snap.clone();
        let findings = py.allow_threads(move || engine.run(&snap_owned));

        let mut results = Vec::with_capacity(findings.len());
        for f in &findings {
            if let Some(ref eid) = entity_id {
                if &f.entity_id != eid {
                    continue;
                }
            }
            if let Some(ref rid) = rule_id {
                if &f.rule_id != rid {
                    continue;
                }
            }

            let dict = PyDict::new(py);
            dict.set_item("rule_id", &f.rule_id)?;
            dict.set_item("entity_id", &f.entity_id)?;
            dict.set_item("severity", f.severity.as_str())?;
            dict.set_item("message", &f.message)?;
            if let Some(name) = entity_name_of(&f.entity_id, &snap) {
                dict.set_item("entity_name", name)?;
            }

            let signals = PyDict::new(py);
            for (k, v) in &f.signals {
                signals.set_item(k, *v)?;
            }
            dict.set_item("signals", signals)?;

            results.push(dict.into());
        }
        Ok(results)
    })
}

/// Look up a human-readable name for an entity id.
fn entity_name_of(entity_id: &str, snap: &ProjectedGraph) -> Option<String> {
    if let Some(f) = snap.functions.get(entity_id) {
        Some(f.name.clone())
    } else if let Some(c) = snap.classes.get(entity_id) {
        Some(c.name.clone())
    } else if let Some(m) = snap.modules.get(entity_id) {
        Some(m.name.clone())
    } else {
        None
    }
}

/// Vector similarity search against entity embeddings.
/// Uses cosine similarity against stored embedding vectors.
/// Scans ALL entity types (functions, classes, modules, imports, constants, type aliases).
#[pyfunction]
fn search_similar(
    py: Python<'_>, query_vec: Vec<f64>, top_k: usize,
) -> PyResult<Vec<PyObject>> {
    with_graph(|_graph, snap| {
        let mut scored: Vec<(f64, String)> = Vec::new();

        // Scan all entity maps for non-empty embeddings
        let mut collect = |scored: &mut Vec<(f64, String)>, id: &str, emb: &EmbeddingVec| {
            if !emb.vec.is_empty() {
                let sim = cosine_similarity(&query_vec, &emb.vec);
                scored.push((sim, id.to_string()));
            }
        };

        for (id, f) in &snap.functions { collect(&mut scored, id, &f.embedding); }
        for (id, c) in &snap.classes { collect(&mut scored, id, &c.embedding); }
        for (id, m) in &snap.modules { collect(&mut scored, id, &m.embedding); }
        for (id, i) in &snap.imports { collect(&mut scored, id, &i.embedding); }
        for (id, c) in &snap.constants { collect(&mut scored, id, &c.embedding); }
        for (id, ta) in &snap.type_aliases { collect(&mut scored, id, &ta.embedding); }

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        let results: Vec<PyObject> = scored
            .into_iter()
            .filter_map(|(sim, id)| {
                let dict: Option<PyObject> =
                    snap.functions.get(&id).and_then(|f| function_to_dict(py, f).ok())
                    .or_else(|| snap.classes.get(&id).and_then(|c| class_to_dict(py, c).ok()))
                    .or_else(|| snap.modules.get(&id).and_then(|m| module_to_dict(py, m).ok()))
                    .or_else(|| snap.imports.get(&id).and_then(|i| import_to_dict(py, i).ok()))
                    .or_else(|| snap.constants.get(&id).and_then(|c| constant_to_dict(py, c).ok()))
                    .or_else(|| snap.type_aliases.get(&id).and_then(|ta| type_alias_to_dict(py, ta).ok()));
                dict.map(|mut d| {
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
//
// Honesty pass: these were silent `Ok(())` stubs — `export_snapshot` is
// wired through cli.py (`coderadar export`) and would silently produce no
// file; `load_snapshot` is not yet called from Python but the `load(db_path)`
// entry exists. Both now raise loudly so callers see the truth rather than a
// silent no-op. In-memory persistence (cold-start without re-analyze) is Phase
// 3B work — see docs/traversal-matrix.md §3.

#[pyfunction]
fn export_snapshot(_path: &str) -> PyResult<()> {
    Err(pyo3::exceptions::PyNotImplementedError::new_err(
        "export_snapshot(path) is not implemented. The in-memory ProjectedGraph \
         is rebuilt by analyze() on each run; serialising it to disk is Phase 3B \
         work (see docs/traversal-matrix.md §3)."
    ))
}

#[pyfunction]
fn load_snapshot(_path: &str) -> PyResult<()> {
    Err(pyo3::exceptions::PyNotImplementedError::new_err(
        "load_snapshot(path) is not implemented. Load a project by calling \
         analyze(root); the Macrame-ledger-backed cold-start is Phase 3B work \
         (see docs/traversal-matrix.md §3)."
    ))
}

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

/// Store an embedding vector on a function entity in the projected graph.
///
/// Called from Python's compute_embeddings() pipeline. The embedding is
/// written directly into the in-memory Function.embedding field, making it
/// immediately available for search_similar() queries.
/// content_hash: xxHash64 hex of the entity body — used for incremental dedup.
#[pyfunction]
fn set_embedding(
    entity_id: &str, embedding: Vec<f64>, content_hash: &str,
) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut()
        .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
            "No graph loaded — run coderadar analyze first"
        ))?;
    graph.set_embedding(entity_id, &embedding, content_hash)
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

/// Set a module's `__all__` star-export names list.
#[pyfunction]
fn clear_embeddings_for_file(file_path: &str) -> PyResult<PyObject> {
    let mut guard = GLOBAL_GRAPH.write();
    let graph = guard.as_mut()
        .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
            "No graph loaded"
        ))?;
    graph.clear_embeddings_for_file(file_path);
    let py = unsafe { Python::assume_gil_acquired() };
    let dict = PyDict::new(py);
    dict.set_item("ok", true)?;
    Ok(dict.into())
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
