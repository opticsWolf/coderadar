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

use crate::types::{EntityId, FunctionKind, ProjectedGraph};

/// Decorators that mark a function as invoked by a framework/runtime, so an
/// absent in-repo caller does NOT mean dead. One flat table; extend via this
/// table (or config later), not code branches.
///
/// Patterns are matched with `contains` against the raw decorator text so
/// `@app.route`, `@router.get("/x")` and bare `@get` all hit.
pub const ENTRY_DECORATORS: &[&str] = &[
    // Web frameworks (Flask/FastAPI/Starlette/Sanic/...)
    "app.route", "router.route", "api.route",
    ".get(", ".post(", ".put(", ".delete(", ".patch(", ".head(", ".options(",
    "@app.", "@router.", "@api.",
    "route", "websocket",
    // CLIs (Click/Typer/argparse)
    "click.command", "click.group", "typer.command", "app.command", "cli.command",
    // Spring / Java-ish annotations
    "RequestMapping", "GetMapping", "PostMapping", "PutMapping", "DeleteMapping",
    "EventListener", "Scheduled", "Async",
    // Message/event handlers
    "EventHandler", "Subscribe", "Listener", "consumer", "handler",
];

/// Decorators that mark a function as a TEST entry point: reachable, but only
/// from test code, which classifies differently (`DeadKind::TestOnly`).
pub const TEST_DECORATORS: &[&str] = &["pytest.fixture", "fixture", "given", "parametrize"];

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
/// 4. public top-level API of modules nobody imports (library surface).
pub fn detect_entry_points(graph: &ProjectedGraph) -> EntryPoints {
    let mut production = HashSet::new();
    let mut test_only = HashSet::new();

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

    for (id, f) in &graph.functions {
        let in_tests = test_modules.contains(&f.parent_module);

        // 1. Conventional mains — free functions only.
        if f.parent_class.is_none()
            && matches!(f.kind, FunctionKind::Free)
            && MAIN_NAMES.contains(&f.name.as_str())
        {
            if in_tests { test_only.insert(id.clone()); } else { production.insert(id.clone()); }
            continue;
        }

        // 2. Framework decorators — production table wins, then test table.
        if decorator_matches(&f.decorators, ENTRY_DECORATORS) {
            production.insert(id.clone());
            continue;
        }
        if decorator_matches(&f.decorators, TEST_DECORATORS) {
            test_only.insert(id.clone());
            continue;
        }

        // 3. Dunder protocol methods are invoked by the runtime.
        if f.name.starts_with("__") && f.name.ends_with("__") && f.name.len() > 4 {
            if in_tests { test_only.insert(id.clone()); } else { production.insert(id.clone()); }
            continue;
        }

        // 3b. Functions inside test modules with no inbound callers are test
        //     roots: the framework discovers them by name, so "nobody calls
        //     this" is normal for them rather than evidence of death.
        if in_tests {
            let has_callers = graph
                .callers_by_callee
                .get(id)
                .is_some_and(|c| !c.is_empty());
            if !has_callers {
                test_only.insert(id.clone());
                continue;
            }
        }

        // 4. Public API of never-imported, non-test modules: the outside world
        //    is allowed to call it even though nothing in-repo does.
        if !in_tests
            && !imported.contains(&f.parent_module)
            && f.parent_class.is_none()
            && is_public(&f.name)
        {
            production.insert(id.clone());
        }
    }

    EntryPoints { production, test_only }
}
