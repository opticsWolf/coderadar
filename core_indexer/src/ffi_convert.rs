// Split out of lib.rs (DR-28, P4 hygiene). Move-only: no behavior change.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::types::{Class, Constant, Function, Import, Module, ProjectedGraph, Route, TypeAlias};

// ── Entity → Python dict helpers ──────────────────────────────────────────

pub(crate) fn module_to_dict(py: Python<'_>, m: &Module) -> PyResult<PyObject> {
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

pub(crate) fn class_to_dict(py: Python<'_>, c: &Class) -> PyResult<PyObject> {
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
    Ok(dict.into())
}

pub(crate) fn function_to_dict(py: Python<'_>, f: &Function) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &f.id)?;
    dict.set_item("name", &f.name)?;
    dict.set_item("references", &f.resolved_refs)?;
    dict.set_item(
        "kind",
        match f.kind {
            crate::types::FunctionKind::Free => "function",
            crate::types::FunctionKind::Method
            | crate::types::FunctionKind::AbstractMethod
            | crate::types::FunctionKind::DataclassSynthesized { .. } => "method",
            crate::types::FunctionKind::StaticMethod | crate::types::FunctionKind::ClassMethod => {
                "function"
            }
            crate::types::FunctionKind::Property
            | crate::types::FunctionKind::PropertySetter
            | crate::types::FunctionKind::PropertyDeleter
            | crate::types::FunctionKind::CachedProperty => "method",
        },
    )?;
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
    // Set unconditionally, like every other *_to_dict: this branch left the
    // key absent for un-embedded functions, so Python code reading
    // entity["has_embedding"] raised KeyError for exactly the entities it
    // was asking about.
    dict.set_item("has_embedding", !f.embedding.vec.is_empty())?;
    dict.set_item("embedding_hash", f.embedding.hash.clone())?;
    // Build signature string from parameters
    let params: Vec<String> = f
        .parameters
        .iter()
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
    let keyword = declaration_keyword(&f.id);
    let sig = if keyword.is_empty() {
        format!("{}({})", f.name, params.join(", "))
    } else {
        format!("{} {}({})", keyword, f.name, params.join(", "))
    };
    if let Some(ref ret) = f.return_type {
        dict.set_item("signature", format!("{} -> {}", sig, ret))?;
    } else {
        dict.set_item("signature", sig)?;
    }
    Ok(dict.into())
}

/// The keyword a reader of this language expects in front of a signature.
///
/// Every signature was rendered `def name(...)` regardless of language, so
/// a PHP method came back as `def hello()` — wrong for the eight of nine
/// Tier-1 languages that are not Python, and misleading to an agent about
/// to call `update_signature` with it.
pub(crate) fn declaration_keyword(entity_id: &str) -> &'static str {
    let path = entity_id.split("::").next().unwrap_or("");
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "py" | "pyi" => "def",
        "rs" => "fn",
        "go" => "func",
        "php" => "function",
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => "function",
        "rb" => "def",
        "kt" | "kts" | "swift" => "fun",
        "lua" => "function",
        "ex" | "exs" => "def",
        // C, C++, Java, C# and friends write the return type instead of a
        // keyword; an empty prefix is trimmed off below.
        _ => "",
    }
}

pub(crate) fn import_to_dict(py: Python<'_>, i: &Import) -> PyResult<PyObject> {
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

pub(crate) fn constant_to_dict(py: Python<'_>, c: &Constant) -> PyResult<PyObject> {
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
    Ok(dict.into())
}

pub(crate) fn type_alias_to_dict(py: Python<'_>, ta: &TypeAlias) -> PyResult<PyObject> {
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

/// §1.3: a framework route to a dict. `name` is the URL pattern (what
/// `resolve_route` substring-matches); `handler` is the edge target so
/// resolve can attach it without a second lookup.
pub(crate) fn route_to_dict(py: Python<'_>, r: &Route) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("id", &r.id)?;
    dict.set_item("name", &r.pattern)?;
    dict.set_item("kind", "route")?;
    dict.set_item("file_path", &r.file_path)?;
    dict.set_item("handler", &r.handler_id)?;
    dict.set_item("methods", &r.methods)?;
    dict.set_item("framework", &r.framework)?;
    Ok(dict.into())
}

// ── Entity References to Dict ──────────────────────────────────────────────

/// Convert a thin entity reference (just ID + name + kind) to a dict.
/// Used for callers_of / callees_of which return lists of EntityIds.
/// Does the projection know this entity id, under any kind?
///
/// Cheaper than the `entity_ref_to_dict(...).is_none()` this replaced, which
/// built a full PyDict — parameters, spans, decorators — only to drop it.
pub(crate) fn entity_exists(snap: &ProjectedGraph, entity_id: &str) -> bool {
    snap.functions.contains_key(entity_id)
        || snap.classes.contains_key(entity_id)
        || snap.modules.contains_key(entity_id)
        || snap.imports.contains_key(entity_id)
        || snap.constants.contains_key(entity_id)
        || snap.type_aliases.contains_key(entity_id)
        || snap.routes.contains_key(entity_id)
}

pub(crate) fn entity_ref_to_dict(
    py: Python<'_>,
    entity_id: &str,
    snap: &ProjectedGraph,
) -> Option<PyObject> {
    // Try each entity type and also resolve file_path from parent module
    if let Some(f) = snap.functions.get(entity_id) {
        let dict = function_to_dict(py, f).ok()?;
        // Resolve file_path from parent module
        if let Ok(d) = dict.downcast_bound::<PyDict>(py) {
            if let Some(m) = snap.modules.get(&f.parent_module) {
                let _ = d.set_item("file_path", m.path.to_string_lossy().to_string());
            }
        }
        Some(dict)
    } else if let Some(c) = snap.classes.get(entity_id) {
        let dict = class_to_dict(py, c).ok()?;
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
    } else if let Some(r) = snap.routes.get(entity_id) {
        route_to_dict(py, r).ok()
    } else {
        None
    }
}

/// Classify a call-graph id with no concept row (pure half of the R2-1
/// fallback; unit-tested below without needing a Python interpreter).
///
/// `external::{name}` covers builtins, third-party imports, and Issue-9
/// re-export-chain targets alike -- all "outside the indexed project".
/// Anything else without a concept row is a symbolic heuristic target
/// (`Date::now`-style) the resolver invented, reported as `unresolved`.
pub(crate) fn unresolved_ref_kind(entity_id: &str) -> &'static str {
    if let Some(name) = entity_id.strip_prefix("external::") {
        if crate::resolve::orchestrator::is_python_builtin(name) {
            "builtin"
        } else {
            "external"
        }
    } else {
        "unresolved"
    }
}

/// Minimal dict for a call-graph id with no concept row.
///
/// `callees_of` / `callers_of` / `traverse` used to drop these silently
/// (`entity_ref_to_dict` -> `None` -> skipped), so `run -> combine`
/// (resolved to `external::combine` via the re-export chain) presented as
/// "run calls nothing". The edge exists -- only the presentation dropped
/// it. R2-1 materializes the honest answer instead: id, derived name, and
/// the external/builtin/unresolved kind. Every downstream consumer
/// (CLI, MCP, visualizers) reads these keys via `.get()` with defaults,
/// so the sparse shape is safe.
pub(crate) fn unresolved_ref_to_dict(py: Python<'_>, entity_id: &str) -> PyObject {
    let dict = PyDict::new(py);
    let name = entity_id.rsplit("::").next().unwrap_or(entity_id);
    let _ = dict.set_item("id", entity_id);
    let _ = dict.set_item("name", name);
    let _ = dict.set_item("kind", unresolved_ref_kind(entity_id));
    dict.into()
}
