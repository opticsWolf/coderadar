use crate::types::*;

/// Normalize a file path string: convert backslashes to forward slashes,
/// strip leading ./ or .\ for consistent keying.
pub(super) fn normalize_path_str(p: &str) -> String {
    let s = p.trim_start_matches("./").trim_start_matches(".\\");
    s.replace('\\', "/")
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

    /// Extensions we recognize; also handles /__init__.* patterns for
    /// Python-style packages (__init__.py), Elixir (__init__.ex), etc.
    const KNOWN_EXTENSIONS: &[&str] = &[
        "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs",
        "go", "rs", "java", "c", "h", "cpp", "cc", "cxx", "hpp",
        "rb", "php", "cs", "kt", "kts", "swift", "scala", "sc",
        "lua", "ex", "exs", "zig", "zon", "r",
    ];

    let segments: Vec<&str> = dotted_name.split('.').collect();

    // Build candidate path suffixes by matching the last N segments
    for n in (1..=segments.len()).rev() {
        let suffix_parts = &segments[segments.len() - n..];
        let suffix_slash = suffix_parts.join("/");

        for (_, module) in &projection.modules {
            let path_str = module.path.to_string_lossy().to_string();
            let path_normalized = path_str.replace('\\', "/");
            // Check each known extension
            for ext in KNOWN_EXTENSIONS {
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
