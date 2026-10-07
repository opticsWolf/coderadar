// CodeRadar Stage 1.1 — entry-point detection over the resolved graph.
//
// Fossil reference: `src/dead_code/entry_points.rs` re-collects functions from
// source, which is where most of its 2,170 LOC and its cyclomatic hotspots
// come from. Ours starts from the already-resolved `ProjectedGraph`: one
// linear scan plus data tables. Heuristics stay in TABLES, not nested
// `matches!` trees — that is precisely how fossil's `collect_defs` grew to
// cyclomatic 28+ and became a brain-method generator.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::graph::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};
use crate::types::{EntityId, FunctionKind, Language, MroNode, ProjectedGraph};

/// Decorators that mark a function as invoked by a framework/runtime, so an
/// absent in-repo caller does NOT mean dead. One flat table; extend via this
/// table (or config later), not code branches.
///
/// Patterns are matched with `contains` against the raw decorator text so
/// `@app.route`, `@router.get("/x")` and bare `@get` all hit.
///
/// On top of the table, `is_registration_decorator` treats *any* decorator
/// that is an attribute call (`@obj.tool(...)`, `@server.list_tools()`) as
/// registration: a receiver method taking the function as its argument is
/// how plugin/tool/route registries are written in every framework we have
/// seen, and no table can enumerate them all (dogfooding CodeRadar on its
/// own MCP server flagged all 24 `@mcp.tool(...)` handlers as unreachable).
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

/// `@obj.method(...)` / `@obj.method(...)` with arguments: a call on an
/// attribute receiver. `@lru_cache()` and `@property` are attribute
/// *references* or bare names, so they stay out; a decorator applied by an
/// object that we cannot see is registration by construction.
fn is_registration_decorator(decorators: &[String]) -> bool {
    decorators.iter().any(|d| {
        let d = d.trim_start_matches('@').trim();
        let Some(open) = d.find('(') else {
            return false;
        };
        let head = &d[..open];
        // Receiver + attribute: `mcp.tool`, `app.route`, `server.list_tools`.
        head.contains('.') && !head.contains(' ') && !head.contains('"')
    })
}

fn is_public(name: &str) -> bool {
    !name.starts_with('_')
}

const TRIPLE_DOUBLE: &str = "\"\"\"";
const TRIPLE_SINGLE: &str = "'''";

/// Names called from module-level code: `__version__ = _resolve_version()`,
/// `_TIMEOUT = _env_seconds("X")` (plain or inside a module-level `try:`).
/// Module bodies run at import, but no function frame owns them, so the call
/// graph never sees the edge and a one-shot initializer looked dead. Def and
/// class bodies are skipped by indentation; files are cached per module.
fn module_level_calls<'a>(
    path: &str,
    cache: &'a mut HashMap<PathBuf, HashSet<String>>,
) -> &'a HashSet<String> {
    cache.entry(PathBuf::from(path)).or_insert_with(|| {
        let mut names = HashSet::new();
        // F14 follow-up: canonical module paths resolve via the indexed root.
        let text = crate::graph::module_resolution::read_project_file(path);
        let mut body_indent: Option<usize> = None;
        // Multi-line strings (module docstrings) are prose, not statements:
        // "Dead: `_orphan` (no callers)" must not read as a call to `_orphan`.
        let mut in_doc = false;
        for line in text.lines() {
            let triples = line.matches(TRIPLE_DOUBLE).count() + line.matches(TRIPLE_SINGLE).count();
            let was_in_doc = in_doc;
            if triples % 2 == 1 {
                in_doc = !in_doc;
            }
            if was_in_doc || triples > 0 {
                continue;
            }
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(bound) = body_indent {
                if indent > bound {
                    continue; // inside a def/class body
                }
                body_indent = None;
            }
            if trimmed.starts_with("def ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with("class ")
            {
                body_indent = Some(indent);
                continue;
            }
            let Some(open) = trimmed.find('(') else {
                continue;
            };
            let head = trimmed[..open].trim_end();
            let name = head
                .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
        names
    })
}

/// Entities considered live roots without requiring an inbound call edge,
/// split by whether they make things live *in production* or *only in tests*.
pub struct EntryPoints {
    pub production: HashSet<EntityId>,
    pub test_only: HashSet<EntityId>,
    /// Findings whose liveness could go either way, with the reason. They
    /// stay visible at the weakest tier instead of claiming 0.9 (plan §7.2:
    /// a number nobody can trust is worse than no number): the receiver
    /// types we resolve do not reach every call, or the function escapes as
    /// a value.
    pub weak_surface: HashMap<EntityId, &'static str>,
}

/// Detect entry points. Cheapest-first ladder:
///
///   1. conventional mains (free functions),
///   2. decorator-driven framework entries (production vs test tables),
///   3. dunder protocol methods (invoked by the runtime), including
///      overrides of external bases (Qt `eventFilter`, Django
///      `View.get`, `TestCase.setUp`) and interface declarations
///      (`typing.Protocol` members, `@abstractmethod` — contract, not
///      deletable code),
///   4. public top-level API of modules nobody imports (library surface).
pub fn detect_entry_points(graph: &ProjectedGraph, root: Option<&Path>) -> EntryPoints {
    let mut production = HashSet::new();
    // Functions some other function passes around as a value (callbacks).
    let referenced: HashSet<&EntityId> = graph
        .functions
        .values()
        .flat_map(|f| f.resolved_refs.iter())
        .collect();
    let mut test_only = HashSet::new();
    let mut weak_surface: HashMap<EntityId, &'static str> = HashMap::new();
    // Names used as decorators anywhere in the repo: the definition is called
    // by the decoration machinery at import time, which the call graph does
    // not model. Dogfooding CodeRadar on itself flagged `requires_index`,
    // applied 24 times, as High unreachable.
    let decorator_names: HashSet<&str> = graph
        .functions
        .values()
        .flat_map(|f| f.decorators.iter())
        .map(|d| {
            let d = d.trim_start_matches('@');
            let head = d.split('(').next().unwrap_or(d);
            head.rsplit('.').next().unwrap_or(head)
        })
        .collect();
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
    // Package surface: modules a package re-exports through its `__init__.py`
    // when nothing outside imports that package either. `coderadar.lsp`
    // re-exports `coderadar.lsp.pool`, and `import coderadar.lsp` is a
    // documented way in — so the pool's public surface is API, not dead
    // code. Fixed point over the import graph: a façade nothing imports makes
    // everything it re-exports surface, transitively.
    let is_facade = |id: &EntityId| {
        graph
            .modules
            .get(id)
            .is_some_and(|m| m.path.file_name() == Some(std::ffi::OsStr::new("__init__.py")))
    };
    let mut package_surface: HashSet<&EntityId> = graph
        .modules
        .keys()
        .filter(|id| is_facade(id) && !imported.contains(id))
        .collect();
    loop {
        let mut grew = false;
        for (mid, importers) in &graph.importers {
            if package_surface.contains(mid) || is_facade(mid) {
                continue;
            }
            // Imported only from modules that are themselves package surface.
            if !importers.is_empty() && importers.iter().all(|imp| package_surface.contains(imp)) {
                grew |= package_surface.insert(mid);
            }
        }
        if !grew {
            break;
        }
    }
    // A module nothing imports is surface by definition (step 4's existing
    // rule), so fold that in and every later check reads one set.
    let unimported_or_surface =
        |id: &EntityId| !imported.contains(id) || package_surface.contains(id);

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

    // Attribute names read anywhere: a property is used by `x.name`, which
    // is no call.
    let attr_reads: HashSet<&str> = graph
        .modules
        .values()
        .flat_map(|m| m.attr_reads.iter().map(String::as_str))
        .collect();
    // `class -> name -> method`, for asking which definition an MRO picks.
    let mut method_ids: HashMap<&EntityId, HashMap<&str, &EntityId>> = HashMap::new();
    for (fid, f) in &graph.functions {
        if let Some(cid) = f.parent_class.as_ref() {
            method_ids
                .entry(cid)
                .or_default()
                .entry(f.name.as_str())
                .or_insert(fid);
        }
    }
    // Is `fid` (named `name`, on `class_id`) what some subclass with an
    // external base dispatches to? A mixin's `closeEvent` overrides the
    // framework's for every widget class that mixes it in first.
    let mixed_into_external = |class_id: &EntityId, name: &str, fid: &EntityId| {
        let mut seen: HashSet<&EntityId> = HashSet::new();
        let mut stack = vec![class_id];
        while let Some(c) = stack.pop() {
            for sub in graph.subclasses.get(c).into_iter().flatten() {
                if !seen.insert(sub) {
                    continue;
                }
                stack.push(sub);
                let Some(d) = graph.classes.get(sub) else {
                    continue;
                };
                if !d.mro.iter().any(|n| matches!(n, MroNode::External { .. })) {
                    continue;
                }
                let lookup =
                    |cid: &EntityId| method_ids.get(cid).and_then(|ms| ms.get(name).copied());
                let picked = lookup(sub).or_else(|| {
                    d.mro.iter().find_map(|n| match n {
                        MroNode::Class(cid) => lookup(cid),
                        _ => None,
                    })
                });
                if picked == Some(fid) {
                    return true;
                }
            }
        }
        false
    };

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
    // Module source cache for step 4b (one read per module, not per function).
    let mut module_level_cache: HashMap<PathBuf, HashSet<String>> = HashMap::new();

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
        // 2b. Unlisted registration decorators (`@mcp.tool(...)`): a
        //     receiver method taking the function is a registry.
        if is_registration_decorator(&f.decorators) {
            production.insert(id.clone());
            continue;
        }
        // 2c. A definition used as a decorator somewhere is invoked by the
        //     decoration machinery, not by a call site we can see.
        if decorator_names.contains(f.name.as_str()) {
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

            // 3c'. Mixins: the same dispatch, reached through a subclass that
            //      combines this class with an external base.
            if public_name && mixed_into_external(&cls.id, &f.name, id) {
                if in_tests {
                    test_only.insert(id.clone());
                } else {
                    production.insert(id.clone());
                }
                continue;
            }

            // 3c''. Properties are read, not called: `x.name` anywhere keeps
            //       `@property def name` (and its setter/deleter) alive.
            let is_property = f.decorators.iter().any(|d| {
                let d = d.trim_start_matches('@');
                d == "property"
                    || d.ends_with("cached_property")
                    || d.ends_with(".setter")
                    || d.ends_with(".getter")
                    || d.ends_with(".deleter")
            });
            if is_property && attr_reads.contains(f.name.as_str()) {
                if in_tests {
                    test_only.insert(id.clone());
                } else {
                    production.insert(id.clone());
                }
                continue;
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

            // 3f. Library surface of a package façade (plan §2.4/§7):
            //     `pkg/__init__.py` exists to be imported by name, so a
            //     public method of a public class defined there is called by
            //     consumers — the same argument step 4 makes for module-level
            //     functions of never-imported modules, which does not cover
            //     methods at all. Dogfooding CodeRadar on itself reported
            //     `CodeGraph.query`, `update_file`, … as High unreachable;
            //     every one of them is the documented API.
            let class_public = cls.name.chars().next().is_some_and(|c| c != '_');
            let module_is_facade = graph
                .modules
                .get(&f.parent_module)
                .is_some_and(|m| m.path.file_name() == Some(std::ffi::OsStr::new("__init__.py")));
            if class_public && public_name && module_is_facade && !under_test {
                production.insert(id.clone());
                continue;
            }

            // 3g. Weak surface: a public method of an instantiated public
            //     class. The receiver may be an untyped attribute
            //     (`self._executor.search_text(...)`) that resolution cannot
            //     follow, so "no callers" is not 0.9 evidence. The class
            //     itself proves instances exist, which is what separates this
            //     from the rta-dead case (never instantiated at all).
            if class_public && public_name && !under_test {
                weak_surface.insert(
                    id.clone(),
                    "public method of an instantiated class (receiver unresolved)",
                );
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
            && unimported_or_surface(&f.parent_module)
            && f.parent_class.is_none()
            && is_public(&f.name)
        {
            production.insert(id.clone());
            continue;
        }

        // 4c. Public methods of a public class in a module a package
        //     re-exports: `LSPPool.shutdown` is called by anyone who does
        //     `import coderadar.lsp`. Only the façade-reachable case — an
        //     unimported module's class is a different question, which is
        //     exactly what the rta-dead pass exists to answer.
        let public_method_of_public_class = f
            .parent_class
            .as_ref()
            .is_some_and(|cid| graph.classes.get(cid).is_some_and(|c| is_public(&c.name)));
        if !under_test
            && package_surface.contains(&f.parent_module)
            && public_method_of_public_class
            && is_public(&f.name)
        {
            production.insert(id.clone());
            continue;
        }

        // 4a. The same argument for a package façade that IS imported:
        //     `coderadar/__init__.py` exists to be imported by name, so its
        //     module-level public functions are the package's API
        //     (`coderadar.watch(...)`), not dead code.
        let module_is_facade = graph
            .modules
            .get(&f.parent_module)
            .is_some_and(|m| m.path.file_name() == Some(std::ffi::OsStr::new("__init__.py")));
        if !under_test && f.parent_class.is_none() && is_public(&f.name) && module_is_facade {
            production.insert(id.clone());
            continue;
        }

        // 4b. Called from module-level code (import-time initializers).
        //     Python modules carry their module-scope uses from the syntax
        //     tree (rooted after this loop); other languages keep the
        //     line-based scan.
        if f.parent_class.is_none()
            && graph.modules.get(&f.parent_module).is_some_and(|m| {
                !matches!(m.language, Language::Python)
                    && module_level_calls(&m.path.to_string_lossy(), &mut module_level_cache)
                        .contains(&f.name)
            })
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

    // Module-scope uses (Python): statements, class bodies and decorators run
    // at import, so whatever they call or hold is live — dispatch tables,
    // `qInstallMessageHandler(_handler)`, `AfterValidator(_check)`.
    for (mid, m) in &graph.modules {
        if m.resolved_uses.is_empty() {
            continue;
        }
        let under_test = *module_under_test
            .entry(mid)
            .or_insert_with(|| is_test_path_in_root(&m.path, root));
        for fid in &m.resolved_uses {
            if under_test {
                test_only.insert(fid.clone());
            } else {
                production.insert(fid.clone());
            }
        }
    }

    // Only classes the root itself constructs qualify: a class built solely
    // by external code is a different question (rta-dead).
    let constructed = crate::graph::rta_lite::instantiated_classes(graph);
    weak_surface.retain(|id, reason| {
        let Some(f) = graph.functions.get(id) else {
            return false;
        };
        match f.parent_class.as_ref() {
            // Value references and unresolved receiver calls need no class.
            None => true,
            Some(cid) => {
                *reason == "used as a value; may be called through that value"
                    || *reason == "named by an unresolved receiver call site"
                    || constructed.contains(cid)
            }
        }
    });

    // Values that escape: a function handed around (`handlers = [cb]`,
    // `self._check = _index_is_empty`) is called through that value, which
    // the call graph cannot follow. Reported at the weakest tier instead of
    // 0.9-dead.
    for id in referenced.iter() {
        if graph.functions.contains_key(id.as_str()) {
            weak_surface
                .entry((*id).clone())
                .or_insert("used as a value; may be called through that value");
        }
    }
    // An unresolved `x.foo()` / `self.foo()` call site names a method we
    // could not bind; any method called `foo` may be its target. Same
    // reasoning as the rta-dead pass, one step weaker: the receiver is
    // unknown, not the class.
    let unresolved_receiver_calls: HashSet<&str> = graph
        .functions
        .values()
        .flat_map(|f| f.resolved_calls.iter())
        .filter_map(|rc| match rc {
            crate::types::ResolvedCall::Unresolved { raw, .. } if !raw.path.is_empty() => {
                Some(raw.name.as_str())
            }
            _ => None,
        })
        .collect();
    for (id, f) in &graph.functions {
        if f.parent_class.is_some() && unresolved_receiver_calls.contains(f.name.as_str()) {
            weak_surface
                .entry(id.clone())
                .or_insert("named by an unresolved receiver call site");
        }
    }

    EntryPoints {
        production,
        test_only,
        weak_surface,
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
