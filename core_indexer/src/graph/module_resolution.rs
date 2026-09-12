use std::collections::HashMap;

use crate::types::*;

/// Normalize a file path string: convert backslashes to forward slashes,
/// strip leading ./ or .\ for consistent keying.
pub(crate) fn normalize_path_str(p: &str) -> String {
    let s = p.trim_start_matches("./").trim_start_matches(".\\");
    s.replace('\\', "/")
}

/// Lexically clean a path without touching the filesystem (no `canonicalize`:
/// `update_file` mints ids for files that may not exist on disk yet when
/// content is provided inline). Resolves `.` and `..` components.
fn clean_lexical(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// The ONE canonical file form for entity ids (F14 / items 12a, 16):
/// project-root-relative with the walk's dot prefix (`.\rel` on
/// Windows, `./rel` elsewhere) in native separators.
///
/// `analyze` used to mint whatever spelling the walk root produced
/// (`sub\x.py` for `root="sub"`, absolute ids for absolute roots) while
/// `update_file` minted `normalize_path_str` form (`x/y.py`) — three forms
/// in one store, invisible at query time (reader-side `canonical()`), fatal
/// to store keys, cross-form call edges and the slop scan's module lookup.
/// Both write paths now mint through here, so the form is root-independent:
/// `analyze(".")`, `analyze(abs_root)` and `update_file("x/y.py")` all
/// yield the same ids. Paths outside the indexed root keep absolute form
/// (no worse than today; they never matched anything anyway).
pub(crate) fn canonical_file_form(path: &str) -> String {
    let root = crate::indexed_root();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let p = std::path::Path::new(path);
    let abs = if p.is_absolute() {
        clean_lexical(p)
    } else {
        let via_cwd = clean_lexical(&cwd.join(p));
        if via_cwd.starts_with(&root) {
            via_cwd
        } else {
            clean_lexical(&root.join(p))
        }
    };
    let rel = match abs.strip_prefix(&root) {
        Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
        // Outside the root (or the root itself): absolute form, as before.
        _ => return normalize_path_str(path),
    };
    let sep = std::path::MAIN_SEPARATOR;
    let joined = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(&sep.to_string());
    format!(".{sep}{joined}")
}

/// Whether a concept-id file head is already canonical: relative with the/// dot prefix (`./` or `.\`). Everything else — forward-slash update-form
/// (`tests/x.py`), bare (`x.py`), absolute — is a pre-fix leftover and a
/// retraction candidate on the next analyze.
pub(crate) fn is_canonical_file_head(head: &str) -> bool {
    head.starts_with("./") || head.starts_with(".\\")
}

/// Extensions we recognize; also handles /__init__.* patterns for
/// Python-style packages (__init__.py), Elixir (__init__.ex), etc.
/// Shared by the dotted-name scanner and the `module_path_index` builder so
/// the two stay in exact lockstep (a module with an exotic extension must be
/// resolvable by one of them iff it is by the other).
pub(crate) const KNOWN_MODULE_EXTENSIONS: &[&str] = &[
    "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "rs", "java", "c", "h", "cpp", "cc",
    "cxx", "hpp", "rb", "php", "cs", "kt", "kts", "swift", "scala", "sc", "lua", "ex", "exs",
    "zig", "zon", "r",
];

/// Rebuild [`ProjectedGraph::module_path_index`] from the current module set.
///
/// O(modules × path_depth). Call after any operation that changes the module
/// set — full `analyze` and cold load (`projection_from_state`) — so
/// [`find_module_by_dotted_name`] takes the O(1)-per-suffix fast path instead
/// of scanning every module (on the 605-file benchmark repo the scan form
/// cost 8.3s inside `resolve_imports` alone).
pub(crate) fn rebuild_module_path_index(projection: &mut ProjectedGraph) {
    let mut index: HashMap<String, EntityId> = HashMap::new();
    for module in projection.modules.values() {
        let path = normalize_path_str(&module.path.to_string_lossy());
        // Extension-less path; the scanner only matches KNOWN_MODULE_EXTENSIONS,
        // so exotic-extension modules must not enter the index.
        let no_ext = match path.rsplit_once('.') {
            Some((stem, ext)) if KNOWN_MODULE_EXTENSIONS.contains(&ext) => stem,
            _ => continue,
        };
        // Segment-boundary suffixes of the extension-less path, both keeping
        // and (for a trailing __init__ file) dropping the __init__ segment —
        // exactly the strings the scanner's ends_with tests accept.
        let mut stems = vec![no_ext.to_string()];
        if let Some(stripped) = no_ext.strip_suffix("/__init__") {
            stems.push(stripped.to_string());
        }
        for stem in &stems {
            let mut tail = String::new();
            for seg in stem.rsplit('/') {
                if !tail.is_empty() {
                    tail.insert(0, '/');
                }
                tail.insert_str(0, seg);
                index
                    .entry(tail.clone())
                    .or_insert_with(|| module.id.clone());
            }
        }
    }
    projection.module_path_index = index;
}

/// Find a module by its dotted name (e.g., "coderadar.config" → config.py).
/// v0.5: Language-agnostic — matches any known extension (py, ex, zig, scala, lua, ...).
/// Converts the dotted name to path segments and matches against suffixes of
/// all module file paths.
pub(crate) fn find_module_by_dotted_name(
    projection: &ProjectedGraph,
    dotted_name: &str,
    _current_module: &str,
) -> Option<String> {
    // 2.2: normalize common TS path aliases before suffix matching.
    // `@/...` and `~/...` conventionally map to `src/...` (Vite/Next/tsconfig).
    let normalized;
    let dotted_name: &str = if dotted_name.starts_with("@/") {
        normalized = format!("src/{}", &dotted_name[2..]);
        &normalized
    } else if dotted_name.starts_with("~/") {
        normalized = format!("src/{}", &dotted_name[2..]);
        &normalized
    } else {
        dotted_name
    };

    let segments: Vec<&str> = dotted_name.split('.').collect();

    // Fast path (v0.8 P1): the suffix index, when built, is COMPLETE — a key
    // exists for every (module, suffix) pair the scan below could return, so
    // a miss means "no module matches this name" and the scan is skipped
    // entirely. That is what makes the common case cheap: relative imports
    // ("../models/user") and package imports ("zod") can never match a
    // project module, and each of them used to cost a full scan of every
    // module (8.3s of the 605-file benchmark's resolve_imports).
    //
    // A graph with modules but an empty index is legacy or hand-built (unit
    // tests) — it keeps the full-scan behaviour below, unchanged.
    if projection.modules.is_empty() || !projection.module_path_index.is_empty() {
        for start in 0..segments.len() {
            let tail = segments[start..].join("/");
            if let Some(id) = projection.module_path_index.get(&tail) {
                return Some(id.clone());
            }
        }
        return None;
    }

    // Legacy slow path: full scan over every module.
    //
    // Build candidate path suffixes by matching the last N segments
    for n in (1..=segments.len()).rev() {
        let suffix_parts = &segments[segments.len() - n..];
        let suffix_slash = suffix_parts.join("/");

        for (_, module) in &projection.modules {
            let path_str = module.path.to_string_lossy().to_string();
            let path_normalized = path_str.replace('\\', "/");
            // Check each known extension
            for ext in KNOWN_MODULE_EXTENSIONS {
                let suffix = format!("{}.{}", suffix_slash, ext);
                let init_suffix = format!("{}/__init__.{}", suffix_slash, ext);
                if path_normalized.ends_with(&suffix) || path_normalized.ends_with(&init_suffix) {
                    return Some(module.id.clone());
                }
            }
        }
    }

    // Fallback: strip any extension and match segments in reverse order
    let last_segment = segments.last().unwrap_or(&"");
    for (_, module) in &projection.modules {
        if module.name == *last_segment {
            let path_str = module.path.to_string_lossy().to_string();
            let path_normalized = path_str.replace('\\', "/");
            // Strip extension and __init__
            let stripped = path_normalized
                .rsplitn(2, "/__init__.")
                .last()
                .unwrap_or(&path_normalized);
            let without_ext = stripped.rsplitn(2, '.').last().unwrap_or(stripped);
            let file_segments: Vec<&str> = without_ext.split('/').collect();
            if file_segments.len() >= segments.len() {
                let file_suffix = &file_segments[file_segments.len() - segments.len()..];
                if file_suffix == segments.as_slice() {
                    return Some(module.id.clone());
                }
            }
        }
    }

    None
}

/// Find a symbol (function or class) with a given name within a specific module.
///
/// Direct definitions win; otherwise (Issue 9) the module's own `from`
/// imports are followed transitively: `from app import combine` lands in
/// `app/__init__`, which defines nothing but re-exports
/// `from .helpers import combine` -- the answer is `helpers::combine`, not
/// `external::combine`. Later imports shadow earlier ones (source order),
/// and a `visited` set of module ids stops A-re-exports-B-re-exports-A
/// cycles (a cycle with no definition resolves to nothing, correctly).
/// Star re-exports (`from x import *`, `Wildcard` resolutions) are followed
/// the same way when the name is exposed.
pub(crate) fn find_symbol_in_module(
    projection: &ProjectedGraph,
    module_id: &str,
    symbol_name: &str,
) -> Option<String> {
    let mut visited = std::collections::BTreeSet::new();
    find_symbol_in_module_guarded(projection, module_id, symbol_name, &mut visited)
}

fn find_symbol_in_module_guarded(
    projection: &ProjectedGraph,
    module_id: &str,
    symbol_name: &str,
    visited: &mut std::collections::BTreeSet<EntityId>,
) -> Option<String> {
    if !visited.insert(module_id.to_string()) {
        return None;
    }
    let module = projection.modules.get(module_id)?;
    // 1. Direct definitions (unchanged precedence).
    for func_id in &module.functions {
        if let Some(func) = projection.functions.get(func_id) {
            if func.name == symbol_name {
                return Some(func.id.clone());
            }
        }
    }
    for class_id in &module.classes {
        if let Some(class) = projection.classes.get(class_id) {
            if class.name == symbol_name {
                return Some(class.id.clone());
            }
        }
    }
    // 2. Re-export chain: `from X import <symbol> [as <alias>]`.
    // Later imports shadow earlier ones, so walk in reverse source order.
    for import_id in module.imports.iter().rev() {
        let import = match projection.imports.get(import_id) {
            Some(i) => i,
            None => continue,
        };
        // Which name does this import look up, and does it bind our symbol?
        // StarImport binds every name (checked against Wildcard exposure).
        enum Binding<'a> {
            Named(&'a str),
            Star,
        }
        let binding: Binding = match &import.kind {
            ImportKind::FromImport { names, .. } | ImportKind::RelativeImport { names, .. } => {
                match names.iter().find(|(n, a)| {
                    a.as_deref() == Some(symbol_name) || (a.is_none() && n == symbol_name)
                }) {
                    Some((original, _)) => Binding::Named(original.as_str()),
                    None => continue,
                }
            }
            ImportKind::StarImport { .. } => Binding::Star,
            _ => continue,
        };
        match &import.resolution {
            // Already resolved to a concrete callable: direct hit.
            ImportResolution::Symbol(SymbolId::Function(id))
            | ImportResolution::Symbol(SymbolId::Class(id)) => {
                return Some(id.clone());
            }
            // Bound to a MODULE (`from . import sibling`): a bare call
            // would be a TypeError at runtime, and an earlier shadowed
            // definition must NOT be resurrected -- stop, don't continue.
            ImportResolution::Symbol(SymbolId::Module(_)) => {
                return None;
            }
            ImportResolution::Module(source_mod) => {
                let want = match binding {
                    Binding::Named(original) => original,
                    // `from x import *` where x resolved only as a module:
                    // the name must be defined there (checked by recursion).
                    Binding::Star => symbol_name,
                };
                if let Some(hit) =
                    find_symbol_in_module_guarded(projection, source_mod, want, visited)
                {
                    return Some(hit);
                }
            }
            ImportResolution::Wildcard { module, exposed } => {
                let ok = match binding {
                    Binding::Named(_) => true,
                    Binding::Star => exposed.iter().any(|e| e == symbol_name),
                };
                if ok {
                    if let Some(hit) =
                        find_symbol_in_module_guarded(projection, module, symbol_name, visited)
                    {
                        return Some(hit);
                    }
                }
            }
            // Unresolved / External / Dynamic / Symbol(Import): fall back
            // to the dotted source name, then try the next import.
            _ => {
                let src_dotted: Option<&str> = match &import.kind {
                    ImportKind::FromImport { module, .. } | ImportKind::StarImport { module } => {
                        Some(module.as_str())
                    }
                    ImportKind::RelativeImport { module, .. } => module.as_deref(),
                    _ => None,
                };
                if let Some(dotted) = src_dotted {
                    if let Some(source_mod) =
                        find_module_by_dotted_name(projection, dotted, module_id)
                    {
                        let want = match binding {
                            Binding::Named(original) => original,
                            Binding::Star => symbol_name,
                        };
                        if let Some(hit) =
                            find_symbol_in_module_guarded(projection, &source_mod, want, visited)
                        {
                            return Some(hit);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod canonical_form_tests {
    use super::*;

    /// canonical_file_form reads the INDEXED_ROOT global, which tests must
    /// not touch (parallel execution). These tests therefore only assert
    /// the SHAPE INVARIANT — dot prefix, native separators, idempotence —
    /// plus the head predicate, which is pure. Exact-root tests live in
    /// projection_tests (agreement between write paths).
    #[test]
    fn canonical_form_has_dot_prefix_and_native_separators() {
        let c = canonical_file_form("some/dir/x.py");
        let sep = std::path::MAIN_SEPARATOR;
        assert!(c.starts_with(&format!(".{sep}")), "dot-prefixed, got {c:?}");
        assert!(
            !c.contains(if sep == '/' { '\\' } else { '/' }),
            "native separators, got {c:?}"
        );
        // Idempotent: canonicalizing twice is a fixed point.
        assert_eq!(canonical_file_form(&c), c);
        // Forward-slash update-form input converges to the same id.
        // (Backslash is a separator only on Windows — on POSIX it is a
        // valid filename char, so the equivalent assertion is cfg-gated.)
        #[cfg(windows)]
        assert_eq!(canonical_file_form("some\\dir\\x.py"), c);
        #[cfg(not(windows))]
        assert_eq!(canonical_file_form("some/dir/x.py"), c);
    }

    #[test]
    fn canonical_head_predicate_sorts_all_three_historical_forms() {
        assert!(is_canonical_file_head(r".\tests\x.py"));
        assert!(is_canonical_file_head("./tests/x.py"));
        assert!(!is_canonical_file_head("tests/x.py")); // update_file form (F14)
        assert!(!is_canonical_file_head("tests\\x.py")); // rooted-walk form
        assert!(!is_canonical_file_head("x.py")); // bare form
        assert!(!is_canonical_file_head(r"D:\proj\x.py")); // absolute form
    }
}

/// Resolve a canonical id-form path for disk IO (F14 follow-up): absolute
/// passes through; relative resolves against the indexed root (the file
/// itself decides via `exists()`, else its parent dir — tmp/backup targets
/// do not exist yet), then CWD. Report keys stay in id form — only fs ops
/// use the resolved path. Centralized here so analysis passes (clones,
/// smells, scaffold, dead-code) share it with the mutation engine instead
/// of each assuming CWD == indexed root.
pub(crate) fn disk_path_for(path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let root = crate::indexed_root();
    let cand = root.join(p);
    if cand.exists() {
        return cand;
    }
    if let Some(parent) = cand.parent() {
        if !parent.as_os_str().is_empty() && parent.exists() {
            return cand;
        }
    }
    std::env::current_dir().unwrap_or(root).join(p)
}

/// Read a project file by canonical id-form path: empty string when
/// missing (callers treat unreadable as skip, never as crash).
pub(crate) fn read_project_file(path: &str) -> String {
    std::fs::read_to_string(disk_path_for(path)).unwrap_or_default()
}
