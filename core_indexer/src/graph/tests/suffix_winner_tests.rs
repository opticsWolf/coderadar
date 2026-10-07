// DR-14 (§0.4): suffix-collision winners must be a total order, not
// HashMap-iteration luck. Two `helpers.py` in different fixture trees share
// every suffix key; `config` collides across languages (config.py vs
// config.rs). Winners — same language first, smallest id second — must agree
// on every path (fast index, legacy scan, fallback) so repeated indexes on
// an unchanged tree assert the same edges and retire nothing.

use super::*;

const R2PROJ_HELPERS: &str = "tests/cr_edit_tests/r2proj/app/helpers.py";
const FIXTURE_HELPERS: &str = "tests/fixtures/cold_start/app/helpers.py";

fn module_id(path: &str) -> String {
    format!("{path}::module")
}

fn collision_projection() -> ProjectedGraph {
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "def helper(): pass\n", R2PROJ_HELPERS);
    index_source(&graph, "def helper(): pass\n", FIXTURE_HELPERS);
    index_source(&graph, "x = 1\n", "pkg/config.py");
    index_source(&graph, "pub fn f() {}\n", "src/config.rs");
    index_source(&graph, "import config\n", "pkg/user.py");
    (*graph.snapshot()).clone()
}

#[test]
fn suffix_collision_fast_path_ranks_language_then_id() {
    let mut proj = collision_projection();
    crate::graph::rebuild_module_path_index(&mut proj);
    // Same-language collision: both helpers are Python, smallest id wins for
    // every importer — stable, and right for r2proj (its own sibling).
    for importer in [R2PROJ_HELPERS, FIXTURE_HELPERS] {
        let got = crate::graph::find_module_by_dotted_name(&proj, "helpers", &module_id(importer))
            .expect("helpers must resolve");
        assert_eq!(
            got,
            module_id(R2PROJ_HELPERS),
            "same-language tie must pick the smallest id"
        );
    }
    // Cross-language collision: the importer's language decides.
    let got = crate::graph::find_module_by_dotted_name(&proj, "config", "pkg/user.py::module")
        .expect("config must resolve for a Python importer");
    assert_eq!(got, module_id("pkg/config.py"), "Python importer → .py");
    let got = crate::graph::find_module_by_dotted_name(&proj, "config", "src/config.rs::module")
        .expect("config must resolve for a Rust importer");
    assert_eq!(got, module_id("src/config.rs"), "Rust importer → .rs");
    // Unknown importer: deterministic smallest id, no luck involved.
    let got = crate::graph::find_module_by_dotted_name(&proj, "config", "")
        .expect("config must resolve without an importer");
    assert_eq!(
        got,
        [module_id("pkg/config.py"), module_id("src/config.rs")]
            .into_iter()
            .min()
            .unwrap()
    );
}

#[test]
fn suffix_collision_scan_path_agrees_with_fast_path() {
    // Empty index forces the legacy full-scan + fallback paths; winners must
    // agree with the fast path on every query.
    let mut proj = collision_projection();
    proj.module_path_index.clear();
    for (query, importer, expect) in [
        (
            "helpers",
            module_id(R2PROJ_HELPERS),
            module_id(R2PROJ_HELPERS),
        ),
        (
            "app.helpers",
            module_id(FIXTURE_HELPERS),
            module_id(R2PROJ_HELPERS),
        ),
        (
            "config",
            "pkg/user.py::module".to_string(),
            module_id("pkg/config.py"),
        ),
        (
            "config",
            "src/config.rs::module".to_string(),
            module_id("src/config.rs"),
        ),
    ] {
        let got = crate::graph::find_module_by_dotted_name(&proj, query, &importer)
            .unwrap_or_else(|| panic!("{query:?} must resolve"));
        assert_eq!(got, expect, "scan path disagrees on {query:?}");
    }
}

#[test]
fn double_persist_round_trip_retires_nothing() {
    // Full persist → retract → re-resolve → persist → retract cycle on the
    // collision fixture: the second retraction must close zero rows and the
    // open edge set must be identical (sorted comparison).
    let (graph, _dir) = graph_with_temp_store();
    index_source(
        &graph,
        "from .helpers import helper\n\nhelper()\n",
        "pkg/a.py",
    );
    index_source(&graph, "def helper(): pass\n", "pkg/helpers.py");
    index_source(&graph, "def helper(): pass\n", "other/helpers.py");

    let resolve_persist = |graph: &CodeGraph| -> Vec<(String, String, String)> {
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.resolve_all_calls(&mut projection);
        graph.commit_projection(projection.clone());
        graph.persist_edges(&projection).expect("persist_edges");
        let retired = graph
            .store
            .as_ref()
            .unwrap()
            .retire_stale_edges(&crate::stale_edge_keep_set(&projection))
            .expect("retire_stale_edges");
        assert_eq!(retired, 0, "retraction on a just-persisted projection");
        let mut open: Vec<(String, String, String)> = graph
            .store
            .as_ref()
            .unwrap()
            .open_edge_triples(&crate::storage::now_iso8601())
            .expect("open triples");
        open.sort();
        open
    };

    let first = resolve_persist(&graph);
    assert!(
        !first.is_empty(),
        "fixture must produce at least one edge, got {first:?}"
    );
    let second = resolve_persist(&graph);
    assert_eq!(first, second, "re-resolve must reproduce the same edge set");
}
