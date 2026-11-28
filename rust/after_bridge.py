"""Re-apply the hand-written compatibility patches after `rust-anything bridge`.

The bridge regenerates Cargo.toml, src/lib.rs, src/pyconv.rs and the Python
facade. It does not know that this workspace pins pyo3 0.24 (core_indexer's
version), that optional arguments need an explicit `None` object on 0.24, or
that CodeRadar's Python callers catch named OpError subclasses. Run this
after every re-bridge of rust/coderadar. Unit 6 adds 14 more `ops.py` exports;
re-bridging must keep all 34 exports listed in `rust/units.json`, and the
optional `decorators` argument patch must be re-applied by hand. Unit 7 adds
five temporal exports; `rust/units.json` lists all 39.
"""

from pathlib import Path

CRATE = Path(__file__).parent / "coderadar"


def patch_rel(path: str, old: str, new: str, count: int = 1) -> None:
    p = CRATE / path
    text = p.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"patch target missing in {path}: {old[:70]!r}")
    p.write_text(text.replace(old, new, count), encoding="utf-8")


def main() -> None:
    patch_rel(
        "Cargo.toml",
        '[dependencies]\npyo3 = { version = "0.30", features = ["extension-module", "abi3-py39"] }',
        '[dependencies]\ncore_indexer = { path = "../../core_indexer" }\npyo3 = { version = "0.24", features = ["extension-module", "abi3-py39"] }',
    )
    patch_rel(
        "Cargo.toml",
        'regex = "1"',
        'regex = "1"\nfancy-regex = "0.14"\npathdiff = "0.2"\ntoml = "0.8"\nserde_json = { version = "1", features = ["preserve_order", "float_roundtrip", "unbounded_depth"] }',
    )
    # The generated Cargo.toml has serde_json in the dependency block already;
    # remove the duplicate line above if the generator adds it back.
    cargo = (CRATE / "Cargo.toml").read_text(encoding="utf-8")
    lines = cargo.splitlines()
    seen = False
    kept = []
    for line in lines:
        if line.startswith("serde_json ="):
            if seen:
                continue
            seen = True
        kept.append(line)
    (CRATE / "Cargo.toml").write_text("\n".join(kept) + "\n", encoding="utf-8")

    cargo = (CRATE / "Cargo.toml").read_text(encoding="utf-8")
    if "[[bin]]" not in cargo:
        cargo += '\n[[bin]]\nname = "coderadar"\npath = "src/main.rs"\n'
        (CRATE / "Cargo.toml").write_text(cargo, encoding="utf-8")

    patch_rel("src/pyconv.rs", ".cast::<PyDict>()", ".downcast::<PyDict>()")
    patch_rel("src/pyconv.rs", ".cast::<PyTuple>()", ".downcast::<PyTuple>()")

    lib = CRATE / "src/lib.rs"
    text = lib.read_text(encoding="utf-8")
    text = text.replace(
        "pub mod port;",
        'pub mod port;\npub mod state;\npub mod annotations_ops;\npub mod render;\npub mod cli;\npub mod mcp;',
        1,
    )
    exceptions = '''use pyo3::prelude::*;

// CodeRadar's error taxonomy is part of the public contract: callers catch
// named subclasses, so the bridge maps `Err("NoIndex: ...")` to them.
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
'''
    text = text.replace("use pyo3::prelude::*;\n", exceptions, 1)
    text = text.replace(
        '        "ValueError" => e::PyValueError::new_err(text),\n        _ => e::PyValueError::new_err(message),',
        '''        "ValueError" => e::PyValueError::new_err(text),
        "NoIndex" => NoIndex::new_err(text),
        "InvalidRequest" => InvalidRequest::new_err(text),
        "NotFound" => NotFound::new_err(text),
        "EngineError" => EngineError::new_err(text),
        "MissingDependency" => MissingDependency::new_err(text),
        "NoEmbeddings" => NoEmbeddings::new_err(text),
        "MutationFailed" => MutationFailed::new_err(text),
        "ContentUnavailable" => ContentUnavailable::new_err(text),
        "TemporalUnsupported" => TemporalUnsupported::new_err(text),
        _ => e::PyValueError::new_err(message),''',
        1,
    )
    # pyo3 0.24 needs an explicit None object for `Option<Bound<PyAny>>`
    # parameters whose signature default is None.
    for fn, param in [
        ("parse_names", "symbols"),
        ("explore", "symbols"),
        ("traverse", "edge_kinds"),
        ("as_of", "symbols"),
    ]:
        text = text.replace(
            f"fn {fn}({param}: Bound<'_, PyAny>",
            f"fn {fn}(py: Python<'_>, {param}: Option<Bound<'_, PyAny>>",
        )
    text = text.replace(
        "fn parse_names(query: String, symbols: Option<Bound<'_, PyAny>>) -> PyResult<Vec<String>> {\n    parse_names_inner(query, symbols).map_err(error)",
        "fn parse_names(py: Python<'_>, query: String, symbols: Option<Bound<'_, PyAny>>) -> PyResult<Vec<String>> {\n    let symbols = symbols.unwrap_or_else(|| py.None().into_bound(py));\n    parse_names_inner(query, symbols).map_err(error)",
    )
    text = text.replace(
        """fn explore(
    py: Python<'_>,
    query: String,
    symbols: Bound<'_, PyAny>,""",
        """fn explore(
    py: Python<'_>,
    query: String,
    symbols: Option<Bound<'_, PyAny>>,""",
    )
    text = text.replace(
        "    explore_inner(query, symbols, direction, max_files)",
        "    let symbols = symbols.unwrap_or_else(|| py.None().into_bound(py));\n    explore_inner(query, symbols, direction, max_files)",
        1,
    )
    text = text.replace(
        """fn traverse(
    py: Python<'_>,
    entity_id: String,
    direction: String,
    edge_kinds: Bound<'_, PyAny>,""",
        """fn traverse(
    py: Python<'_>,
    entity_id: String,
    direction: String,
    edge_kinds: Option<Bound<'_, PyAny>>,""",
    )
    text = text.replace(
        "    traverse_inner(entity_id, direction, edge_kinds, max_depth)",
        "    let edge_kinds = edge_kinds.unwrap_or_else(|| py.None().into_bound(py));\n    traverse_inner(entity_id, direction, edge_kinds, max_depth)",
        1,
    )
    # Register the exception classes on the native module.
    marker = "    Ok(())\n}\n"
    idx = text.rfind(marker)
    if idx == -1:
        raise SystemExit("pymodule tail not found")
    regs = "".join(
        f'    m.add("{name}", m.py().get_type::<{name}>())?;\n'
        for name in [
            "OpError",
            "NoExtension",
            "NoIndex",
            "InvalidRequest",
            "TemporalUnsupported",
            "NotFound",
            "EngineError",
            "MissingDependency",
            "NoEmbeddings",
            "MutationFailed",
            "ContentUnavailable",
        ]
    )
    text = text[:idx] + regs + text[idx:]
    facade_ops = """#[pyfunction]
#[pyo3(name = "framework_claims_reference", signature = (name, framework))]
fn framework_claims_reference(name: String, framework: String) -> PyResult<bool> {
    crate::port::framework_claims_reference(name, framework).map_err(error)
}

#[pyfunction]
#[pyo3(name = "framework_detect", signature = (root, framework))]
fn framework_detect(root: String, framework: String) -> PyResult<bool> {
    crate::port::framework_detect(root, framework).map_err(error)
}

#[pyfunction]
#[pyo3(name = "framework_extract", signature = (file_path, source, framework))]
fn framework_extract(
    py: Python<'_>,
    file_path: String,
    source: String,
    framework: String,
) -> PyResult<Py<PyAny>> {
    value_result(py, crate::port::framework_extract(file_path, source, framework))
}

#[pyfunction]
#[pyo3(name = "framework_resolve_one", signature = (name, framework, candidates))]
fn framework_resolve_one(
    py: Python<'_>,
    name: String,
    framework: String,
    candidates: Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let rows: Vec<serde_json::Value> = crate::pyconv::FromPy::from_py(&candidates).map_err(error)?;
    value_result(py, crate::port::framework_resolve_one(name, framework, rows))
}

#[pyfunction]
#[pyo3(name = "framework_resolve_reference", signature = (name, candidates, limit=5))]
fn framework_resolve_reference(
    py: Python<'_>,
    name: String,
    candidates: Bound<'_, PyAny>,
    limit: i64,
) -> PyResult<Py<PyAny>> {
    let rows: Vec<serde_json::Value> = crate::pyconv::FromPy::from_py(&candidates).map_err(error)?;
    value_result(py, crate::port::framework_resolve_reference(name, rows, limit))
}

#[pyfunction]
#[pyo3(name = "framework_resolve_route", signature = (path, routes, callees, limit=5))]
fn framework_resolve_route(
    py: Python<'_>,
    path: String,
    routes: Bound<'_, PyAny>,
    callees: Bound<'_, PyAny>,
    limit: i64,
) -> PyResult<Py<PyAny>> {
    let routes: Vec<serde_json::Value> = crate::pyconv::FromPy::from_py(&routes).map_err(error)?;
    let callees: Vec<serde_json::Value> = crate::pyconv::FromPy::from_py(&callees).map_err(error)?;
    value_result(py, crate::port::framework_resolve_route(path, routes, callees, limit))
}

#[pyfunction]
#[pyo3(name = "embed_texts_op", signature = (texts, model=None))]
fn embed_texts_op(
    py: Python<'_>,
    texts: Vec<String>,
    model: Option<String>,
) -> PyResult<Py<PyAny>> {
    value_result(py, crate::port::embed_texts_op(texts, model))
}

#[pyfunction]
#[pyo3(name = "config_op", signature = ())]
fn config_op(py: Python<'_>) -> PyResult<Py<PyAny>> {
    value_result(py, Ok(crate::port::config_op()))
}

#[pyfunction]
#[pyo3(name = "batch_update_op", signature = (files, contents))]
fn batch_update_op(
    py: Python<'_>,
    files: Vec<String>,
    contents: Vec<Option<String>>,
) -> PyResult<Py<PyAny>> {
    value_result(py, crate::port::batch_update_op(files, contents))
}

#[pyfunction]
#[pyo3(name = "watch_changes_op", signature = (paths, previous, max_bytes=None))]
fn watch_changes_op(
    py: Python<'_>,
    paths: Vec<String>,
    previous: Bound<'_, PyAny>,
    max_bytes: Option<i64>,
) -> PyResult<Py<PyAny>> {
    let previous: serde_json::Value = crate::pyconv::FromPy::from_py(&previous).map_err(error)?;
    value_result(py, crate::port::watch_changes_op(paths, previous, max_bytes))
}
"""
    if "fn batch_update_op(" not in text:
        marker = "#[pymodule]\n"
        idx = text.index(marker)
        text = text[:idx] + facade_ops + "\n" + text[idx:]
    facade_regs = (
        "    m.add_function(wrap_pyfunction!(framework_claims_reference, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(framework_detect, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(framework_extract, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(framework_resolve_one, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(framework_resolve_reference, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(framework_resolve_route, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(embed_texts_op, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(config_op, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(batch_update_op, m)?)?;\n"
        "    m.add_function(wrap_pyfunction!(watch_changes_op, m)?)?;\n"
    )
    if "wrap_pyfunction!(batch_update_op" not in text:
        idx = text.rfind("    Ok(())\n}\n")
        if idx == -1:
            raise SystemExit("pymodule tail not found for facade ops")
        text = text[:idx] + facade_regs + text[idx:]
    lib.write_text(text, encoding="utf-8")

    # Unit 25: keep the thin CodeGraph facade attached to the generated
    # package. The shim itself lives in a separate stable Python file.
    facade = CRATE / "python" / "coderadar" / "__init__.py"
    if facade.exists():
        text = facade.read_text(encoding="utf-8")
        if "framework_claims_reference = _native.framework_claims_reference" not in text:
            marker = "extract_all_exports = _native.extract_all_exports\n"
            text = text.replace(
                marker,
                marker
                + "framework_claims_reference = _native.framework_claims_reference\n"
                + "framework_detect = _native.framework_detect\n"
                + "framework_extract = _native.framework_extract\n"
                + "framework_resolve_one = _native.framework_resolve_one\n",
                1,
            )
        if "from .codegraph import" not in text:
            text += (
                "\n# Unit 25: thin CodeGraph facade over the Rust-native ops.\n"
                "from .codegraph import (\n"
                "    BatchContext, CodeGraph, GitWatcher, UpdateReport, Watcher, analyze, load,\n"
                "    resolve_reference, resolve_route, watch, watch_git,\n"
                ")\n\n"
                '__all__ = sorted(set(__all__) | {\n'
                '    "BatchContext", "CodeGraph", "GitWatcher", "UpdateReport", "Watcher",\n'
                '    "analyze", "load", "resolve_reference", "resolve_route", "watch", "watch_git",\n'
                '})\n'
            )
            facade.write_text(text, encoding="utf-8")
        if "coldstart_build_graph = _native.coldstart_build_graph" not in text:
            text += (
                "\n# Unit S1: engine-backed helpers used by the module shims.\n"
                "from ._native import (  # noqa: F401 - re-export the Rust error taxonomy\n"
                "    ContentUnavailable, EngineError, InvalidRequest, MissingDependency,\n"
                "    MutationFailed, NoEmbeddings, NoExtension, NoIndex, NotFound, OpError,\n"
                "    TemporalUnsupported,\n"
                ")\n\n"
                '__all__ = sorted(set(__all__) | {\n'
                '    "ContentUnavailable", "EngineError", "InvalidRequest",\n'
                '    "MissingDependency", "MutationFailed", "NoEmbeddings", "NoExtension",\n'
                '    "NoIndex", "NotFound", "OpError", "TemporalUnsupported",\n'
                '})\n'
            )
            for name in (
                "doc_description", "doc_names", "doc_text", "skill_description",
                "skill_names", "skill_text", "coldstart_build_graph", "is_excluded",
                "project_files", "find_marker", "record_project", "last_project_for",
                "project_state_file", "config_json", "embedding_settings", "visualize",
                "embed_preprocess_version", "embed_body", "embed_cache_key",
                "stored_key_model", "content_hash", "embedding_batch", "plan_query",
                "graphrag_build_context", "graphrag_execute",
                "roots_uri_to_path", "roots_resolve_project_root",
                "roots_resolve_from_client_roots", "roots_resolve_selector",
                "roots_adopt_project_root", "roots_describe",
                "startup_configure", "startup_start", "startup_restart",
                "startup_status", "startup_wait", "startup_root",
                "startup_elapsed", "startup_progress_message",
                "lifecycle_parent_alive", "lifecycle_parent_pid",
                "lazy_should_ask", "lazy_trade_up", "stats",
            ):
                text += f"{name} = _native.{name}\n"
            facade.write_text(text, encoding="utf-8")
        if "def _coerce(" not in text:
            text += (
                "\n# Public result types (data shapes) and the boundary translation.\n"
                "from dataclasses import fields as _dc_fields\n"
                "from dataclasses import is_dataclass as _is_dataclass\n"
                "from .models import (\n"
                "    CodeRadarError, MutationEdit, MutationError, MutationFailure,\n"
                "    MutationFileError, MutationFileResult, MutationPlan, MutationResult,\n"
                "    MutationSyncSummary, ParseError, PolicyViolation, ResolutionError,\n"
                "    StaleHandle, SymbolChange, UpdateReport,\n"
                ")\n\n"
                "def _coerce(value):\n"
                '    """Turn the public result dataclasses into the plain data the engine takes."""\n'
                "    if _is_dataclass(value) and not isinstance(value, type):\n"
                "        return {\n"
                "            field.name: _coerce(getattr(value, field.name))\n"
                "            for field in _dc_fields(value)\n"
                "        }\n"
                "    if isinstance(value, list):\n"
                "        return [_coerce(item) for item in value]\n"
                "    if isinstance(value, tuple):\n"
                "        return tuple(_coerce(item) for item in value)\n"
                "    if isinstance(value, dict):\n"
                "        return {key: _coerce(item) for key, item in value.items()}\n"
                "    return value\n\n"
                "_mutation_plan = _native.mutation_plan\n"
                "_mutation_applied = _native.mutation_applied\n"
                "_edit = _native.edit\n\n"
                "def mutation_plan(plan):\n"
                '    """Render a mutation plan as text (dry-run preview)."""\n'
                "    return _mutation_plan(_coerce(plan))\n\n"
                "def mutation_applied(result, unverified_sites=None):\n"
                '    """Render an applied mutation result as text."""\n'
                "    sites = None if unverified_sites is None else _coerce(unverified_sites)\n"
                "    return _mutation_applied(_coerce(result), sites)\n\n"
                "def edit(outcome, apply_hint=\"call again with `dry_run=False`\"):\n"
                '    """Render one mutation outcome."""\n'
                "    return _edit(_coerce(outcome), apply_hint)\n\n"
                "__all__ = sorted(set(__all__) | {\n"
                '    "CodeRadarError", "MutationEdit", "MutationError", "MutationFailure",\n'
                '    "MutationFileError", "MutationFileResult", "MutationPlan",\n'
                '    "MutationResult", "MutationSyncSummary", "ParseError",\n'
                '    "PolicyViolation", "ResolutionError", "StaleHandle", "SymbolChange",\n'
                '    "UpdateReport",\n'
                "})\n"
            )
            facade.write_text(text, encoding="utf-8")

        if "sync_git = _native.sync_git" not in text:
            text += (
                "\n# Unit S1: ops names the engine already provides.\n"
                "annotate = _native.annotate\n"
                "comment = _native.comment\n"
                "revise_annotation = _native.revise_annotation\n"
                "retire_annotation = _native.retire_annotation\n"
                "annotations_on = _native.annotations_on\n"
                "comments_on = _native.comments_on\n"
                "annotation_history = _native.annotation_history\n"
                "comment_history = _native.comment_history\n"
                "open_items = _native.open_items\n"
                "sync_git = _native.sync_git\n"
                "reingest_merged_commits = _native.reingest_merged_commits\n"
                "backfill_commit_index = _native.backfill_commit_index\n"
                "backfill_commit_messages = _native.backfill_commit_messages\n"
                "activate_config = _native.activate_config\n"
                "lsp_status = _native.lsp_status\n"
                "lsp_definition = _native.lsp_definition\n"
                "lsp_override_edges = _native.lsp_override_edges\n"
                "__all__ = sorted(set(__all__) | {\n"
                '    "annotate", "annotation_history", "annotations_on",\n'
                '    "backfill_commit_index", "backfill_commit_messages", "comment",\n'
                '    "comment_history", "comments_on", "open_items",\n'
                '    "reingest_merged_commits", "retire_annotation",\n'
                '    "revise_annotation", "sync_git",\n'
                "})\n"
            )
            facade.write_text(text, encoding="utf-8")
        if "from .resolvers import" not in text:
            text += (
                "\n# Unit 46: Rust-backed framework resolver classes.\n"
                "from .resolvers import (\n"
                "    ALL_RESOLVERS, AspNetResolver, DjangoResolver, ExpressResolver,\n"
                "    FastAPIResolver, FlaskResolver, FrameworkExtraction, FrameworkResolver,\n"
                "    GoResolver, LaravelResolver, NestJSResolver, RailsResolver,\n"
                "    ReactRouterResolver, RustActixResolver, RustFrameworkResolver,\n"
                "    SpringBootResolver, SyntheticEdge, SyntheticNode, VueRouterResolver,\n"
                ")\n\n"
                "__all__ = sorted(set(__all__) | {\n"
                "    \"ALL_RESOLVERS\", \"AspNetResolver\", \"DjangoResolver\", \"ExpressResolver\",\n"
                "    \"FastAPIResolver\", \"FlaskResolver\", \"FrameworkExtraction\",\n"
                "    \"FrameworkResolver\", \"GoResolver\", \"LaravelResolver\",\n"
                "    \"NestJSResolver\", \"RailsResolver\", \"ReactRouterResolver\",\n"
                "    \"RustActixResolver\", \"RustFrameworkResolver\",\n"
                "    \"SpringBootResolver\", \"SyntheticEdge\",\n"
                "    \"SyntheticNode\", \"VueRouterResolver\",\n"
                "})\n"
            )
            facade.write_text(text, encoding="utf-8")

    print("patched rust/coderadar for pyo3 0.24 + CodeRadar errors")


if __name__ == "__main__":
    main()
