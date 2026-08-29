use std::collections::HashMap;

use crate::types::*;

/// Normalize a file path string: convert backslashes to forward slashes,
/// strip leading ./ or .\ for consistent keying.
pub(super) fn normalize_path_str(p: &str) -> String {
    let s = p.trim_start_matches("./").trim_start_matches(".\\");
    s.replace('\\', "/")
}

/// Extensions we recognize; also handles /__init__.* patterns for
/// Python-style packages (__init__.py), Elixir (__init__.ex), etc.
/// Shared by the dotted-name scanner and the `module_path_index` builder so
/// the two stay in exact lockstep (a module with an exotic extension must be
/// resolvable by one of them iff it is by the other).
pub(crate) const KNOWN_MODULE_EXTENSIONS: &[&str] = &[
    "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs",
    "go", "rs", "java", "c", "h", "cpp", "cc", "cxx", "hpp",
    "rb", "php", "cs", "kt", "kts", "swift", "scala", "sc",
    "lua", "ex", "exs", "zig", "zon", "r",
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
                index.entry(tail.clone()).or_insert_with(|| module.id.clone());
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
pub(super) fn find_symbol_in_module(
    projection: &ProjectedGraph,
    module_id: &str,
    symbol_name: &str,
) -> Option<String> {
    if let Some(module) = projection.modules.get(module_id) {
        // Search functions
        for func_id in &module.functions {
            if let Some(func) = projection.functions.get(func_id) {
                if func.name == symbol_name {
                    return Some(func.id.clone());
                }
            }
        }
        // Search classes
        for class_id in &module.classes {
            if let Some(class) = projection.classes.get(class_id) {
                if class.name == symbol_name {
                    return Some(class.id.clone());
                }
            }
        }
    }
    None
}
