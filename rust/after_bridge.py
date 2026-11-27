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
        cargo += '\n[[bin]]\nname = "coderadar-rs"\npath = "src/main.rs"\n'
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
        if "from .codegraph import" not in text:
            text += (
                "\n# Unit 25: thin CodeGraph facade over the Rust-native ops.\n"
                "from .codegraph import (\n"
                "    BatchContext, CodeGraph, GitWatcher, UpdateReport, Watcher, analyze, load,\n"
                "    watch, watch_git,\n"
                ")\n\n"
                '__all__ = sorted(set(__all__) | {\n'
                '    "BatchContext", "CodeGraph", "GitWatcher", "UpdateReport", "Watcher",\n'
                '    "analyze", "load", "watch", "watch_git",\n'
                '})\n'
            )
            facade.write_text(text, encoding="utf-8")

    print("patched rust/coderadar for pyo3 0.24 + CodeRadar errors")


if __name__ == "__main__":
    main()
