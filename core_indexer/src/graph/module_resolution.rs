use std::collections::HashMap;

use crate::types::*;

/// Normalize a file path string: convert backslashes to forward slashes,
/// strip leading ./ or .\ for consistent keying.
pub(crate) fn normalize_path_str(p: &str) -> String {
    // Strip every leading `./` / `.\` layer (`./.\x.py` converges too);
    // the old two-call chain only stripped one spelling once.
    let mut s = p;
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest;
            continue;
        }
        if let Some(rest) = s.strip_prefix(".\\") {
            s = rest;
            continue;
        }
        break;
    }
    s.replace('\\', "/")
}

/// Whether a forward-slash spelling is absolute on any platform: POSIX
/// `/…`, UNC `//…`, or a drive prefix (`C:/…`, bare `C:`).
fn is_absolute_fwd(s: &str) -> bool {
    if s.starts_with('/') {
        return true;
    }
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Lexically clean a forward-slash path without touching the filesystem
/// (no `canonicalize`: `update_file` mints ids for files that may not
/// exist on disk yet when content is provided inline). Resolves `.` and
/// `..` components. Platform-independent: `std::path::components` treats
/// `\` as a separator on Windows but as a filename char on POSIX, so the
/// old `Path`-based cleaner never split `a\b` on Linux CI.
fn clean_lexical_fwd(s: &str) -> String {
    let (prefix, rest) = if let Some(rest) = s.strip_prefix("//") {
        ("//".to_string(), rest)
    } else if let Some(stripped) = s.strip_prefix('/') {
        ("/".to_string(), stripped)
    } else if s.len() >= 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':' {
        if s.len() >= 3 && s.as_bytes()[2] == b'/' {
            (s[..3].to_string(), &s[3..])
        } else if s.len() == 2 {
            (s[..2].to_string(), "")
        } else {
            // Drive-relative (`C:foo`): keep the drive as the prefix.
            (s[..2].to_string(), &s[2..])
        }
    } else {
        (String::new(), s)
    };
    let absolute = !prefix.is_empty();
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        } else if seg == ".." {
            if parts.pop().is_none() {
                // Beyond the root: absolute stays put, relative keeps `..`.
                if !absolute {
                    // `parts` cannot hold `..` via push of `seg` borrow?
                    // Re-push as a literal (rest outlives the call).
                    parts.push("..");
                }
            }
        } else {
            parts.push(seg);
        }
    }
    if parts.is_empty() {
        return if prefix.is_empty() {
            ".".to_string()
        } else {
            prefix
        };
    }
    if prefix.is_empty() {
        parts.join("/")
    } else if prefix == "/" || prefix == "//" {
        format!("{}{}", prefix, parts.join("/"))
    } else if prefix.ends_with('/') {
        // Drive root (`C:/`).
        format!("{}{}", prefix, parts.join("/"))
    } else {
        // Bare drive (`C:`) + relative tail.
        format!("{}/{}", prefix, parts.join("/"))
    }
}

/// Strip a forward-slash absolute path by a forward-slash root with a
/// separator boundary (`/foo` must not strip `/foobar/x`). Returns `None`
/// for the root itself (the old `!r.empty` guard: the root mints absolute
/// form, not an empty id).
fn strip_root_fwd(abs: &str, root: &str) -> Option<String> {
    if root == "/" {
        let rest = abs.trim_start_matches('/');
        return if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        };
    }
    let root_trim = root.trim_end_matches('/');
    if root_trim.is_empty() {
        return None;
    }
    if abs == root_trim {
        return None;
    }
    abs.strip_prefix(&format!("{root_trim}/"))
        .map(str::to_string)
}

/// The ONE canonical file form for entity ids (F14 / items 12a, 16 / plan
/// §5.1): project-root-relative, forward slashes, no `./` prefix —
/// `lace/dock_manager.py::DockManager.save_state` on every platform.
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
///
/// §5.1 dropped the dot prefix and the native separators: ids saved on one
/// OS (snapshots, baselines, MCP transcripts, CI artefacts) were spelled
/// with `\` on Windows and `/` on POSIX, so they never matched across
/// platforms. Legacy dot-prefixed ids are still accepted on input
/// (`canonical_lookup_id`) and are retired by
/// [`crate::retire_noncanonical_concepts`] on the next analyze.
pub(crate) fn canonical_file_form(path: &str) -> String {
    canonical_file_form_with_root(path, &crate::indexed_root())
}

/// Inner funnel with the root injectable (unit tests must not touch the
/// process-global `INDEXED_ROOT`; parallel tests share it).
fn canonical_file_form_with_root(path: &str, root: &std::path::Path) -> String {
    // Cross-platform (§5.1): `\` is a separator on every OS, not just
    // Windows. The old `Path`-based cleaner treated `a\b` as one filename
    // on Linux, so Windows spellings never converged there.
    let root_fwd_raw = crate::fs::strip_verbatim_prefix(root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    let root_fwd = clean_lexical_fwd(&root_fwd_raw);
    let path_fwd_raw = crate::fs::strip_verbatim_prefix(std::path::PathBuf::from(path))
        .to_string_lossy()
        .replace('\\', "/");
    let cwd_fwd_raw = std::env::current_dir()
        .map(|p| {
            crate::fs::strip_verbatim_prefix(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .unwrap_or_else(|_| ".".to_string());
    let cwd_fwd = clean_lexical_fwd(&cwd_fwd_raw);
    let abs_fwd = if is_absolute_fwd(&path_fwd_raw) {
        clean_lexical_fwd(&path_fwd_raw)
    } else {
        let join = |base: &str, rel: &str| -> String {
            let base_trim = base.trim_end_matches('/');
            if base_trim.is_empty() {
                clean_lexical_fwd(rel)
            } else {
                clean_lexical_fwd(&format!("{base_trim}/{rel}"))
            }
        };
        let via_cwd = join(&cwd_fwd, &path_fwd_raw);
        if strip_root_fwd(&via_cwd, &root_fwd).is_some() || via_cwd == root_fwd {
            via_cwd
        } else {
            join(&root_fwd, &path_fwd_raw)
        }
    };
    if let Some(rel) = strip_root_fwd(&abs_fwd, &root_fwd) {
        // Forward slashes on every platform (§5.1) and no `./` prefix:
        // the id is a portable key, not a path a shell will run.
        return rel;
    }
    // Aliased root: the walk spells the path as passed (8.3 short
    // names like C:\Users\RUNNER~1\… on CI, symlinks, verbatim
    // `\\?\` form, on-disk case) while INDEXED_ROOT is
    // filesystem-canonicalized — a lexical strip can never match
    // those. Resolve through the FS once (mismatch path only, never
    // the hot path) and retry before falling back to absolute form,
    // which `retire_noncanonical_concepts` would close as an orphan
    // (Windows CI's `test_as_of_temporal_traversal`: 3 concepts
    // retired, as_of reads []).
    let abs_path = std::path::PathBuf::from(&abs_fwd);
    match std::fs::canonicalize(&abs_path) {
        Ok(canon) => {
            let canon = crate::fs::strip_verbatim_prefix(canon);
            match canon.strip_prefix(root) {
                Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().replace('\\', "/"),
                // Outside the root (or the root itself): absolute form.
                _ => normalize_path_str(path),
            }
        }
        // Unresolvable (update_file mints ids for files that may not
        // exist yet): absolute form, as before.
        _ => normalize_path_str(path),
    }
}

/// Whether a concept-id file head is already canonical: relative, forward
/// slashes, no `./` prefix (`tests/x.py`). Everything else — the legacy
/// dot-prefixed forms (`./x.py`, `.\x.py`, `.\tests\x.py`), absolute
/// paths — is a pre-§5.1 leftover and a retraction candidate on the next
/// analyze, which is also the store-repair step: the fresh index writes the
/// canonical ids and this pass closes the old ones. (A POSIX file whose own
/// name contains `:` or `\` reads as non-canonical and is re-minted each
/// analyze — exotic enough to prefer the simple predicate.)
pub(crate) fn is_canonical_file_head(head: &str) -> bool {
    !head.is_empty()
        && !head.starts_with("./")
        && !head.starts_with(".\\")
        && !head.contains('\\')
        && !head.starts_with('/')
        && !head.contains(':')
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
    let mut index: HashMap<String, Vec<EntityId>> = HashMap::new();
    // DR-14: every module sharing a suffix key is recorded (sorted), and
    // the query ranks same-language first, smallest id second. A single
    // winner per key reintroduced HashMap-iteration luck (per-process
    // RandomState): it flapped resolution across CLI invocations — e.g.
    // `config` flip-flopping between config.py and config.rs — and fed
    // perpetual stale-edge retirements on unchanged trees. (Finer
    // proximity ranking — nearest-same-package wins — is a precision
    // follow-up, not this fix; see the v0.12 deviations log.)
    let mut modules: Vec<_> = projection.modules.values().collect();
    modules.sort_by(|a, b| a.id.cmp(&b.id));
    for module in modules {
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
                // Sorted-id insertion order keeps every vec ascending, so
                // query-time ranking is a stable pick-first with no sort.
                index
                    .entry(tail.clone())
                    .or_default()
                    .push(module.id.clone());
            }
        }
    }
    projection.module_path_index = index;
}

/// Resolve a Rust module path (`::` chains from `use` or call sites) to a
/// module entity (DR-13 §0.3). `src_mod` arrives dotted (`super.alpha` —
/// the extractor normalizes `::` to `.`). Roots: `crate`/`self` strip to a
/// crate-relative tail (the suffix match below is root-independent, so no
/// crate-root lookup is needed); leading `super`s walk up from the
/// importer's file directory (file-modules cover real code; inline
/// `mod x {}` inside another file resolves against the file's directory
/// instead — documented miss, not silent wrongness: the suffix still has
/// to match a real module file). Anything else is already crate-relative
/// (`mod beta;` makes `beta::run` callable with no `use`) or an extern
/// crate (matches nothing → external, correctly).
///
/// Sibling-or-self tails (`helper` in the importing file) are NOT modules
/// and never match — the caller falls back to sibling lookup for those.
///
/// Based on the Python L2–L3 import cascade (same file); the Rust half it
/// never got. Algorithm intentionally mirrors `find_module_by_dotted_name`
/// and delegates the actual suffix match to it.
pub(crate) fn find_rust_module(
    projection: &ProjectedGraph,
    src_mod: &str,
    importer_file: &std::path::Path,
    current_module: &str,
) -> Option<String> {
    let mut segs: Vec<&str> = src_mod.split('.').collect();
    while segs.first().is_some_and(|s| *s == "crate" || *s == "self") {
        segs.remove(0);
    }
    let mut supers = 0usize;
    while segs.first().is_some_and(|s| *s == "super") {
        segs.remove(0);
        supers += 1;
    }
    if segs.is_empty() {
        return None;
    }
    if supers > 0 {
        // Walk up from the importing file's directory. `parent()` of the
        // empty dir (a top-level file like `beta.rs`) is None, not "" —
        // and anything past the representable root is unknowable — so a
        // depleted walk falls back to the bare tail: the suffix match
        // below tries every tail anyway.
        let mut dir = importer_file.parent();
        for _ in 0..supers {
            match dir {
                Some(d) if !d.as_os_str().is_empty() => dir = d.parent(),
                _ => {
                    dir = None;
                    break;
                }
            }
        }
        let dotted = match dir {
            Some(d) => {
                // Absolute prefixes are harmless: the suffix match below
                // tries every tail, so only the trailing segments have
                // to be right.
                let ds = d.to_string_lossy().replace('\\', "/");
                if ds.is_empty() {
                    segs.join(".")
                } else {
                    format!("{}.{}", ds.replace('/', "."), segs.join("."))
                }
            }
            None => segs.join("."),
        };
        return find_module_by_dotted_name(projection, &dotted, current_module);
    }
    find_module_by_dotted_name(projection, &segs.join("."), current_module)
}

/// Rank one suffix key's candidates (DR-14): same language as the importing
/// module first — a Python import never means a Rust file and vice versa —
/// smallest module id second. Order-robust: true for index vecs (ascending)
/// and scan collections (HashMap order) alike, hence stable in every process.
fn pick_suffix_winner(
    projection: &ProjectedGraph,
    ids: &[EntityId],
    importer_lang: Option<Language>,
) -> Option<String> {
    let mut best: Option<&str> = None;
    let mut best_same_lang = false;
    for id in ids {
        let same = importer_lang
            .is_some_and(|l| projection.modules.get(id).is_some_and(|m| m.language == l));
        let better = match (same, best_same_lang) {
            (true, false) => true,
            (false, true) => false,
            _ => best.is_none_or(|b| id.as_str() < b),
        };
        if better {
            best = Some(id.as_str());
            best_same_lang = same;
        }
    }
    best.map(str::to_string)
}

/// Find a module by its dotted name (e.g., "coderadar.config" → config.py).
/// v0.5: Language-agnostic — matches any known extension (py, ex, zig, scala, lua, ...).
/// Converts the dotted name to path segments and matches against suffixes of
/// all module file paths.
pub(crate) fn find_module_by_dotted_name(
    projection: &ProjectedGraph,
    dotted_name: &str,
    current_module: &str,
) -> Option<String> {
    // 2.2: normalize common TS path aliases before suffix matching.
    // `@/...` and `~/...` conventionally map to `src/...` (Vite/Next/tsconfig).
    let normalized;
    // `@/` and `~/` are two spellings for the same `src/` root (one arm).
    let dotted_name: &str = if dotted_name.starts_with("@/") || dotted_name.starts_with("~/") {
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
    let importer_lang = projection.modules.get(current_module).map(|m| m.language);
    if projection.modules.is_empty() || !projection.module_path_index.is_empty() {
        for start in 0..segments.len() {
            let tail = segments[start..].join("/");
            if let Some(ids) = projection.module_path_index.get(&tail) {
                if let Some(winner) = pick_suffix_winner(projection, ids, importer_lang) {
                    return Some(winner);
                }
            }
        }
        return None;
    }

    // Legacy slow path: full scan over every module. Longest-suffix-first
    // priority is preserved (outer loop), but within one suffix length the
    // winner is the smallest module id — DR-14: first-HashMap-hit-wins
    // flapped across processes. Same for the fallback below.
    //
    // Build candidate path suffixes by matching the last N segments
    for n in (1..=segments.len()).rev() {
        let suffix_parts = &segments[segments.len() - n..];
        let suffix_slash = suffix_parts.join("/");

        let mut ids = Vec::new();
        for module in projection.modules.values() {
            let path_str = module.path.to_string_lossy().to_string();
            let path_normalized = path_str.replace('\\', "/");
            // Check each known extension
            for ext in KNOWN_MODULE_EXTENSIONS {
                let suffix = format!("{}.{}", suffix_slash, ext);
                let init_suffix = format!("{}/__init__.{}", suffix_slash, ext);
                if path_normalized.ends_with(&suffix) || path_normalized.ends_with(&init_suffix) {
                    ids.push(module.id.clone());
                    break;
                }
            }
        }
        if !ids.is_empty() {
            return pick_suffix_winner(projection, &ids, importer_lang);
        }
    }

    // Fallback: strip any extension and match segments in reverse order
    let last_segment = segments.last().unwrap_or(&"");
    let mut ids = Vec::new();
    for module in projection.modules.values() {
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
                    ids.push(module.id.clone());
                }
            }
        }
    }

    if ids.is_empty() {
        return None;
    }
    pick_suffix_winner(projection, &ids, importer_lang)
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
    // 1. Direct definitions. Methods and nested functions are not module
    // names: `module.functions` lists them, but `import x` cannot see them.
    for func_id in &module.functions {
        if let Some(func) = projection.functions.get(func_id) {
            let nested = func
                .id
                .rsplit_once('.')
                .is_some_and(|(head, _)| projection.functions.contains_key(head));
            if func.name == symbol_name && func.parent_class.is_none() && !nested {
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

#[cfg(test)]
mod canonical_form_tests {
    use super::*;

    /// canonical_file_form reads the INDEXED_ROOT global, which tests must
    /// not touch (parallel execution). These tests therefore only assert
    /// the SHAPE INVARIANT — relative, forward slashes, no `./` prefix,
    /// idempotence — plus the head predicate, which is pure. Exact-root
    /// tests live in projection_tests (agreement between write paths).
    #[test]
    fn canonical_form_is_relative_with_forward_slashes() {
        let c = canonical_file_form("some/dir/x.py");
        assert_eq!(c, "some/dir/x.py", "portable id form, got {c:?}");
        assert!(!c.contains('\\'), "forward slashes only, got {c:?}");
        assert!(!c.starts_with("./"), "no dot prefix, got {c:?}");
        // Idempotent: canonicalizing twice is a fixed point.
        assert_eq!(canonical_file_form(&c), c);
        // Every historical spelling converges to the same id.
        #[cfg(windows)]
        assert_eq!(canonical_file_form("some\\dir\\x.py"), c);
        assert_eq!(canonical_file_form("./some/dir/x.py"), c);
        assert_eq!(canonical_file_form(".\\some\\dir\\x.py"), c);
    }

    /// Aliased root (Windows): the walk may spell a path verbatim
    /// (`\\\\?\\C:\\…`) while INDEXED_ROOT is the stripped form — same
    /// file, lexical mismatch. Models CI's 8.3 TEMP (`RUNNER~1`), which
    /// retired all 3 concepts of `test_as_of_temporal_traversal`'s fixture
    /// as "non-canonical" and made as_of read [].
    #[cfg(windows)]
    #[test]
    fn verbatim_aliased_path_still_mints_relative_id() {
        let base = std::env::temp_dir().join(format!("cr_alias_{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.py"), "x = 1\n").unwrap();
        // What INDEXED_ROOT holds: canonicalized, verbatim stripped.
        let root = crate::fs::strip_verbatim_prefix(std::fs::canonicalize(&real).unwrap());
        // What the walk may yield for the same file: verbatim spelling.
        let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", real.join("a.py").display()));
        let got = canonical_file_form_with_root(&verbatim.to_string_lossy(), &root);
        assert_eq!(
            got, "a.py",
            "aliased path must mint the canonical relative id, got {got:?}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Aliased root (POSIX): same mismatch class via symlink — lexical
    /// strip of the unresolved spelling fails, the FS fallback resolves.
    #[cfg(unix)]
    #[test]
    fn symlinked_path_still_mints_relative_id() {
        let base = std::env::temp_dir().join(format!("cr_alias_{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.py"), "x = 1\n").unwrap();
        std::os::unix::fs::symlink(&real, base.join("alias")).unwrap();
        let root = std::fs::canonicalize(&real).unwrap();
        let via_alias = base.join("alias").join("a.py");
        let got = canonical_file_form_with_root(&via_alias.to_string_lossy(), &root);
        assert_eq!(
            got, "a.py",
            "aliased path must mint the canonical relative id, got {got:?}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn canonical_head_predicate_sorts_every_historical_form() {
        assert!(is_canonical_file_head("tests/x.py"));
        assert!(is_canonical_file_head("x.py")); // bare is canonical now
        assert!(!is_canonical_file_head(r".\tests\x.py")); // pre-§5.1 Windows
        assert!(!is_canonical_file_head("./tests/x.py")); // pre-§5.1 POSIX
        assert!(!is_canonical_file_head("tests\\x.py")); // rooted-walk form
        assert!(!is_canonical_file_head(r"D:\proj\x.py")); // absolute form
        assert!(!is_canonical_file_head("/home/proj/x.py")); // absolute (POSIX)
        assert!(!is_canonical_file_head("")); // empty
    }
}
