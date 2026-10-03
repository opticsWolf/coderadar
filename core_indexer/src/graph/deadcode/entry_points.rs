// CodeRadar Stage 1.1 — entry-point detection over the resolved graph.
//
// Fossil reference: `src/dead_code/entry_points.rs` re-collects functions from
// source, which is where most of its 2,170 LOC and its cyclomatic hotspots
// come from. Ours starts from the already-resolved `ProjectedGraph`: one
// linear scan plus data tables. Heuristics stay in TABLES, not nested
// `matches!` trees — that is precisely how fossil's `collect_defs` grew to
// cyclomatic 28+ and became a brain-method generator.

use std::collections::HashSet;
use std::path::Path;

use crate::graph::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};
use crate::types::{EntityId, FunctionKind, MroNode, ProjectedGraph};

/// Decorators that mark a function as invoked by a framework/runtime, so an
/// absent in-repo caller does NOT mean dead. One flat table; extend via this
/// table (or config later), not code branches.
///
/// Patterns are matched with `contains` against the raw decorator text so
/// `@app.route`, `@router.get("/x")` and bare `@get` all hit.
pub const ENTRY_DECORATORS: &[&str] = &[
    // Web frameworks (Flask/FastAPI/Starlette/Sanic/...)
    "app.route",
    "router.route",
    "api.route",
    ".get(",
    ".post(",
    ".put(",
    ".delete(",
    ".patch(",
    ".head(",
    ".options(",
    "@app.",
    "@router.",
    "@api.",
    "route",
    "websocket",
    // CLIs (Click/Typer/argparse)
    "click.command",
    "click.group",
    "typer.command",
    "app.command",
    "cli.command",
    // Click subcommand decorators on a local group object (`@main.command()`,
    // `@cli.group()`): the group variable is rarely named app/cli/click (F7).
    "main.command",
    "main.group",
    ".command(",
    ".group(",
    // Spring / Java-ish annotations
    "RequestMapping",
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "EventListener",
    "Scheduled",
    "Async",
    // Message/event handlers
    "EventHandler",
    "Subscribe",
    "Listener",
    "consumer",
    "handler",
];

/// Decorators that mark a function as a TEST entry point: reachable, but only
/// from test code, which classifies differently (`DeadKind::TestOnly`).
pub const TEST_DECORATORS: &[&str] = &["pytest.fixture", "fixture", "given", "parametrize"];

/// Cross-language bridge attributes (F7): invoked from outside the indexed
/// call graph — Python via PyO3 — so no in-repo caller does NOT mean dead.
/// `#[pyfunction]` annotates the function itself; `#[pymethods]` annotates
/// the impl block (its methods are covered by the Rust-`pub` rule below);
/// `#[pymodule]` annotates the module init fn, called by the Python import
/// system and never by name in-repo. (`#[pyclass]` is deliberately absent:
/// classes are not dead-code findings, and their methods fall under the
/// `pub` rule like `#[pymethods]` members do.)
pub const BRIDGE_DECORATORS: &[&str] = &["pyfunction", "pymethods", "pymodule"];

/// Conventional program-entry names per language family (free functions).
const MAIN_NAMES: &[&str] = &["main", "__main__", "_start"];

/// Does the file look like test code? Only the file name and its immediate
/// parent directory are considered — walking ALL ancestors would classify
/// anything nested under a repo-level `tests/` tree as test code regardless
/// of which subdirectory was actually handed to `analyze`.
pub fn is_test_path(path: &Path) -> bool {
    let parent_is_tests = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| {
            let c = n.to_string_lossy().to_lowercase();
            c == "tests" || c == "test"
        })
        .unwrap_or(false);
    if parent_is_tests {
        return true;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.starts_with("test_") || name.ends_with("_test") || name.ends_with("_test.py")
}

fn decorator_matches(decorators: &[String], table: &[&str]) -> bool {
    decorators.iter().any(|d| {
        let d = d.trim_start_matches('@');
        table.iter().any(|pat| d.contains(pat))
    })
}

fn is_public(name: &str) -> bool {
    !name.starts_with('_')
}

/// Entities considered live roots without requiring an inbound call edge,
/// split by whether they make things live *in production* or *only in tests*.
pub struct EntryPoints {
    pub production: HashSet<EntityId>,
    pub test_only: HashSet<EntityId>,
}

/// Detect entry points. Cheapest-first ladder:
/// 1. conventional mains (free functions),
/// 2. decorator-driven framework entries (production vs test tables),
/// 3. dunder protocol methods (invoked by the runtime),
/// 3c. overrides of external bases — virtual dispatch from code outside the
///     indexed root (Qt `eventFilter`, Django `View.get`, `TestCase.setUp`),
/// 3d. interface declarations — `typing.Protocol` members and
///     `@abstractmethod` declarations are contract, not deletable code,
/// 4. public top-level API of modules nobody imports (library surface).
pub fn detect_entry_points(graph: &ProjectedGraph, root: Option<&Path>) -> EntryPoints {
    let mut production = HashSet::new();
    // Functions some other function passes around as a value (callbacks).
    let referenced: HashSet<&EntityId> = graph
        .functions
        .values()
        .flat_map(|f| f.resolved_refs.iter())
        .collect();
    let mut test_only = HashSet::new();
    // Module source cache for the Rust-`pub` rule (step 5): each .rs
    // module reads once per detection run.
    let mut rust_lines: std::collections::HashMap<EntityId, Vec<String>> =
        std::collections::HashMap::new();

    // Module-level context computed once.
    let test_modules: HashSet<&EntityId> = graph
        .modules
        .iter()
        .filter(|(_, m)| is_test_path(&m.path))
        .map(|(id, _)| id)
        .collect();
    // A module nobody imports exports public API that external code may call.
    let imported: HashSet<&EntityId> = graph
        .importers
        .iter()
        .filter(|(_, users)| !users.is_empty())
        .map(|(mid, _)| mid)
        .collect();
    // Method names defined per in-repo class (plan §2.3): the dispatch-target
    // check asks whether any class below this one in the MRO defines the same
    // name, i.e. whether the override targets something we can see.
    let mut methods_by_class: std::collections::HashMap<&EntityId, HashSet<&str>> =
        std::collections::HashMap::new();
    for f in graph.functions.values() {
        if let Some(cid) = f.parent_class.as_ref() {
            methods_by_class
                .entry(cid)
                .or_default()
                .insert(f.name.as_str());
        }
    }

    // Framework packs (plan §2.4) — table-driven, opt-in by detection. A
    // project that never imports pytest gets no pytest semantics.
    let mut packs = Packs::default();
    for imp in graph.imports.values() {
        if !packs.pytest && imp.raw.contains("pytest") {
            packs.pytest = true;
        }
        if !packs.qt && (imp.raw.contains("PySide") || imp.raw.contains("PyQt")) {
            packs.qt = true;
        }
    }
    if !packs.pytest {
        // A project uses pytest when a conftest.py exists even if no visible
        // import does (plugin autoload).
        packs.pytest = graph
            .modules
            .values()
            .any(|m| m.path.file_name() == Some(std::ffi::OsStr::new("conftest.py")));
    }
    // Extended test-path classification per module (ancestors up to root).
    let mut module_under_test: std::collections::HashMap<&EntityId, bool> =
        std::collections::HashMap::new();

    for (id, f) in &graph.functions {
        let in_tests = test_modules.contains(&f.parent_module);
        // Extended test-path classification for the packs (plan §2.4):
        // ancestors up to the analyzed root, not the immediate parent only.
        let under_test = *module_under_test
            .entry(&f.parent_module)
            .or_insert_with(|| {
                graph
                    .modules
                    .get(&f.parent_module)
                    .map(|m| is_test_path_in_root(&m.path, root))
                    .unwrap_or(in_tests)
            });

        // 1. Conventional mains — free functions only.
        if f.parent_class.is_none()
            && matches!(f.kind, FunctionKind::Free)
            && MAIN_NAMES.contains(&f.name.as_str())
        {
            if in_tests {
                test_only.insert(id.clone());
            } else {
                production.insert(id.clone());
            }
            continue;
        }

        // 2. Framework decorators — production table wins, then test table.
        if decorator_matches(&f.decorators, ENTRY_DECORATORS) {
            production.insert(id.clone());
            continue;
        }
        // 2a. Cross-language bridge (F7): PyO3 entry points are called
        // from Python, invisible to the Rust call graph.
        if decorator_matches(&f.decorators, BRIDGE_DECORATORS) {
            production.insert(id.clone());
            continue;
        }
        if decorator_matches(&f.decorators, TEST_DECORATORS) {
            test_only.insert(id.clone());
            continue;
        }

        // 3. Dunder protocol methods are invoked by the runtime.
        if f.name.starts_with("__") && f.name.ends_with("__") && f.name.len() > 4 {
            if in_tests {
                test_only.insert(id.clone());
            } else {
                production.insert(id.clone());
            }
            continue;
        }

        // 3c/3d apply to methods only; the class carries the evidence.
        if let Some(cls) = f
            .parent_class
            .as_ref()
            .and_then(|cid| graph.classes.get(cid))
        {
            // 3c. Overrides of external bases (plan §2.3): the class's MRO
            //     contains a base outside the indexed root, so the runtime
            //     dispatches public methods from code we cannot see. Root a
            //     public method — unless an in-repo class below in the MRO
            //     defines the same name; then that visible override is the
            //     dispatch target, and this method needs real callers (or
            //     RTA) to be live. Private names are never dispatch targets
            //     (external frameworks invoke documented public API), so a
            //     dead private helper of a framework subclass is still
            //     reported — rooting it would erase real findings.
            let external_bases: Vec<&str> = cls
                .mro
                .iter()
                .filter_map(|n| match n {
                    MroNode::External { name } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            let public_name = !f.name.starts_with('_');
            if !external_bases.is_empty() && public_name {
                let visible_override = cls.mro.iter().skip(1).any(|n| match n {
                    MroNode::Class(cid) => methods_by_class
                        .get(cid)
                        .is_some_and(|names| names.contains(f.name.as_str())),
                    MroNode::External { .. } => false,
                });
                if !visible_override {
                    if in_tests {
                        test_only.insert(id.clone());
                    } else {
                        production.insert(id.clone());
                    }
                    continue;
                }
            }

            // 3d. Interface declarations (plan §2.3): `typing.Protocol`
            //     members define an interface consumers implement elsewhere;
            //     `@abstractmethod` (plain or `@abc.abstractmethod`) declares
            //     a contract its subclasses fulfill. Neither is deletable
            //     code.
            let is_protocol_member = external_bases.contains(&"Protocol");
            let is_abstract_decl = matches!(f.kind, FunctionKind::AbstractMethod)
                || f.decorators.iter().any(|d| d.contains("abstractmethod"));
            if is_protocol_member || is_abstract_decl {
                if in_tests {
                    test_only.insert(id.clone());
                } else {
                    production.insert(id.clone());
                }
                continue;
            }
        }

        // 3e. Framework packs (plan §2.4, table-driven, opt-in by detection):
        //     a project that never imports pytest gets no pytest semantics.
        if packs.pytest {
            // `test_*` anywhere under a test path — including nested helper
            // trees the immediate-parent rule missed — is discovered by name.
            // conftest hooks (`pytest_configure`, …) are called by the runner.
            let is_conftest = graph
                .modules
                .get(&f.parent_module)
                .is_some_and(|m| m.path.file_name() == Some(std::ffi::OsStr::new("conftest.py")));
            if (under_test && f.name.starts_with("test_"))
                || (is_conftest && f.name.starts_with("pytest_"))
            {
                test_only.insert(id.clone());
                continue;
            }
        }
        // Qt pack: `.connect(...)` targets are already live through the
        // §1.7 reference edges in reachability; `@Slot` needs its own root.
        if packs.qt && f.decorators.iter().any(|d| d.contains("Slot")) {
            if under_test {
                test_only.insert(id.clone());
            } else {
                production.insert(id.clone());
            }
            continue;
        }

        // 3b. Functions inside test modules with no inbound callers are test
        //     roots: the framework discovers them by name, so "nobody calls
        //     this" is normal for them rather than evidence of death. The
        //     extended classification covers nested test trees too.
        if in_tests || under_test {
            let has_callers = graph
                .callers_by_callee
                .get(id)
                .is_some_and(|c| !c.is_empty());
            if !has_callers && !referenced.contains(id) {
                test_only.insert(id.clone());
                continue;
            }
        }

        // 4. Public API of never-imported, non-test modules: the outside world
        //    is allowed to call it even though nothing in-repo does. A module
        //    under a test path (extended, §2.4) is not production surface.
        if !under_test
            && !imported.contains(&f.parent_module)
            && f.parent_class.is_none()
            && is_public(&f.name)
        {
            production.insert(id.clone());
            continue;
        }

        // 5. Rust `pub` surface (F7): `pub fn` is callable from outside the
        // crate — including the PyO3 bridge — so absence of in-repo callers
        // proves nothing. `pub(...)` (crate/super/self) stays crate-internal
        // and falls through. Source-backed: visibility never entered the
        // graph schema, so the def line is checked directly (cached per
        // module). Until export analysis lands, `pub` suppresses the
        // finding rather than merely capping it.
        if is_rust_pub_export(graph, &mut rust_lines, &f.parent_module, f.line) {
            production.insert(id.clone());
        }
    }

    // Entry-point pack (plan §2.4): pyproject.toml [project.scripts] and
    // [project.entry-points.*] name callables the packaging ecosystem
    // invokes — invisible to the call graph by construction.
    if let Some(root) = root {
        if let Ok(text) = std::fs::read_to_string(root.join("pyproject.toml")) {
            for (dotted, attr) in parse_project_entry_points(&text) {
                let Some(mid) = find_module_by_dotted_name(graph, &dotted, "") else {
                    continue;
                };
                if let Some(fid) = find_symbol_in_module(graph, &mid, &attr) {
                    production.insert(fid);
                }
            }
        }
    }

    // `__all__` pack (plan §2.4): a listed name is the module's public API
    // even when the module IS imported — step 4 only protects never-imported
    // modules. (star_exports only exists when export analysis ran in-process;
    // cold starts degrade to the step-4 behaviour.)
    for (mid, m) in &graph.modules {
        let Some(names) = m.star_exports.as_ref() else {
            continue;
        };
        let under_test = *module_under_test
            .entry(mid)
            .or_insert_with(|| is_test_path_in_root(&m.path, root));
        for name in names {
            if let Some(fid) = find_symbol_in_module(graph, mid, name) {
                if under_test {
                    test_only.insert(fid);
                } else {
                    production.insert(fid);
                }
            }
        }
    }

    EntryPoints {
        production,
        test_only,
    }
}

/// Which framework packs a project opted into, by detection.
#[derive(Default)]
struct Packs {
    pytest: bool,
    qt: bool,
}

/// Is `path` inside a test tree, walking ancestors up to — never past — the
/// analyzed root (plan §2.4 note)? The immediate-parent rule of
/// [`is_test_path`] missed helper modules nested in test trees
/// (`tests/visual/corner_harness.py`); walking past the root would instead
/// classify unrelated sibling trees as tests. Module paths are treated as
/// root-relative (the walker's own form); `.`/`..` components are skipped,
/// and a `..` anywhere falls back to the strict immediate-parent rule.
fn is_test_path_in_root(path: &Path, root: Option<&Path>) -> bool {
    if is_test_path(path) {
        return true;
    }
    let Some(root) = root else {
        return false;
    };
    let rel = if path.is_absolute() {
        match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => return false, // outside the analyzed root — no opinion
        }
    } else {
        path
    };
    let comps: Vec<std::path::Component<'_>> = rel
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
    if comps
        .iter()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        // Would escape above the root; the strict rule is all we know.
        return is_test_path(path);
    }
    // Every directory component except the file name itself.
    for c in comps.iter().take(comps.len().saturating_sub(1)) {
        let name = c.as_os_str().to_string_lossy().to_lowercase();
        if name == "tests" || name == "test" {
            return true;
        }
    }
    false
}

/// Minimal `[project.scripts]` / `[project.entry-points.*]` extraction from
/// pyproject.toml text — `name = "module:attr"` pairs. No TOML dependency:
/// sections and string values are regular enough for this one shape, and a
/// malformed file simply roots nothing.
fn parse_project_entry_points(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l == "[project.scripts]" || l.starts_with("[project.entry-points");
            continue;
        }
        if !in_section || l.is_empty() || l.starts_with('#') {
            continue;
        }
        let Some((_, target)) = l.split_once('=') else {
            continue;
        };
        let target = target.trim().trim_matches('"').trim_matches('\'');
        if let Some((module, attr)) = target.rsplit_once(':') {
            if !module.is_empty() && !attr.is_empty() {
                out.push((module.to_string(), attr.to_string()));
            }
        }
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether a Rust def line exports unrestrictedly: a `pub` word NOT
/// followed by `(` (which would make it `pub(crate)`/`pub(super)`/…).
fn rust_line_is_pub_export(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i..i + 3] == *b"pub"
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && (i + 3 >= bytes.len() || !is_ident_byte(bytes[i + 3]))
        {
            let mut j = i + 3;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if bytes.get(j) != Some(&b'(') {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Source-backed unrestricted-`pub` check for one Rust function (F7).
/// `cache` maps module id → file lines so each module reads once per run.
fn is_rust_pub_export(
    graph: &ProjectedGraph,
    cache: &mut std::collections::HashMap<EntityId, Vec<String>>,
    parent_module: &EntityId,
    def_line: usize,
) -> bool {
    let m = match graph.modules.get(parent_module) {
        Some(m) => m,
        None => return false,
    };
    if !matches!(m.language, crate::types::Language::Rust) {
        return false;
    }
    if !cache.contains_key(parent_module) {
        // F14 follow-up: canonical module paths resolve via the indexed root.
        let text = crate::graph::module_resolution::read_project_file(&m.path.to_string_lossy());
        cache.insert(
            parent_module.clone(),
            text.lines().map(|l| l.to_string()).collect(),
        );
    }
    let Some(lines) = cache.get(parent_module) else {
        return false; // just inserted above; unreachable in practice
    };
    def_line
        .checked_sub(1)
        .and_then(|idx| lines.get(idx))
        .is_some_and(|line| rust_line_is_pub_export(line))
}
