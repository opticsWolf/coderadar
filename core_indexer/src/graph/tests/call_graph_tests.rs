// CallGraph traversal tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

#[test]
fn test_call_graph_find_callers() {
    let mut g = CallGraph::new();
    make_call_edge(&mut g, "a", "b");
    let callers = g.find_callers("b", 5);
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].0.entity_id, "a");
}
#[test]
fn test_call_graph_chain() {
    let mut g = CallGraph::new();
    make_call_edge(&mut g, "a", "b");
    make_call_edge(&mut g, "b", "c");
    let chain = g.find_call_chain("a", "c", 5);
    assert!(chain.is_some());
    let chain = chain.unwrap();
    assert_eq!(chain.len(), 3);
    assert_eq!(chain[0].entity_id, "a");
    assert_eq!(chain[2].entity_id, "c");
}
#[test]
fn test_call_graph_cycle_safe() {
    let mut g = CallGraph::new();
    make_call_edge(&mut g, "a", "b");
    make_call_edge(&mut g, "b", "a");
    let callers = g.find_callers("a", 10);
    assert_eq!(callers.len(), 1);
}
#[test]
fn test_codegraph_snapshot() {
    let graph = CodeGraph::new(GraphConfig::default());
    let snap = graph.snapshot();
    assert!(snap.modules.is_empty());
    assert!(snap.functions.is_empty());
}
#[test]
fn test_codegraph_callers_of_empty() {
    let graph = CodeGraph::new(GraphConfig::default());
    assert!(graph.callers_of("nonexistent").is_empty());
}

#[test]
fn new_expression_calls_are_extracted_per_language() {
    // R2-3: `new Store()` produced zero targets in every language -- no
    // .scm captured new/object-creation expressions. Each leg indexes a
    // minimal constructor call and asserts the resolved edge (same-file
    // and cross-file names fall back to `external::`, which is the honest
    // answer until constructor-target resolution exists).
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "store.ts",
            "export class Store {}\nexport function makeStore() { return new Store(); }\n",
            "makeStore",
            "external::Store",
        ),
        (
            "make.js",
            "class Store {}\nfunction makeStore() { return new Store(); }\n",
            "makeStore",
            "external::Store",
        ),
        (
            "A.java",
            "class A { Object m() { return new Store(); } }\n",
            "m",
            "external::Store",
        ),
        (
            "A.cs",
            "class A { object M() { return new Store(); } }\n",
            "M",
            "external::Store",
        ),
        (
            "m.cpp",
            "class Store {};\nStore* m() { return new Store(); }\n",
            "m",
            "external::Store",
        ),
    ];
    for (file, src, caller_name, want) in cases {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, src, file);
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);
        // (C++ function names carry a trailing `()` -- pre-existing naming
        // quirk, out of scope here.)
        let caller = projection
            .functions
            .values()
            .find(|f| f.name.trim_end_matches("()") == *caller_name)
            .map(|f| f.id.clone())
            .unwrap_or_else(|| panic!("{file}: caller `{caller_name}` should be indexed"));
        let callees = projection
            .callees_by_caller
            .get(&caller)
            .cloned()
            .unwrap_or_default();
        assert!(
            callees.iter().any(|c| c == want),
            "{file}: `new Store()` must extract, got {callees:?}"
        );
    }
}

#[test]
fn reexport_chain_resolves_to_defining_module() {
    // Issue 9: `from app import combine` (re-exported via app/__init__
    // from .helpers) resolved to external::combine on every leg. FromImport
    // chains are followed transitively now.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "from .helpers import combine\n", "app/__init__.py");
    index_source(
        &graph,
        "def combine(items):\n    return items\n",
        "app/helpers.py",
    );
    index_source(
        &graph,
        "from app import combine\ndef run(items):\n    return combine(items)\n",
        "main.py",
    );
    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);
    graph.resolve_all_calls(&mut projection);
    let run = fn_id_of(&projection, "run");
    let callees = projection
        .callees_by_caller
        .get(&run)
        .cloned()
        .unwrap_or_default();
    assert!(
        callees
            .iter()
            .any(|c| c.contains("helpers") && c.ends_with("::combine")),
        "run -> combine must resolve through the re-export, got {callees:?}"
    );
    assert!(
        !callees.iter().any(|c| c == "external::combine"),
        "no external fallback should remain, got {callees:?}"
    );
}

#[test]
fn reexport_cycle_terminates() {
    // Issue 9 guard: A re-exports x from B, B re-exports x from A, no
    // definition anywhere -- resolution must terminate (with no answer),
    // not recurse forever.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "from mod_b import x\n", "mod_a.py");
    index_source(&graph, "from mod_a import x\n", "mod_b.py");
    index_source(
        &graph,
        "from mod_a import x\ndef run():\n    return x()\n",
        "main.py",
    );
    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);
    graph.resolve_all_calls(&mut projection);
    let run = fn_id_of(&projection, "run");
    let callees = projection
        .callees_by_caller
        .get(&run)
        .cloned()
        .unwrap_or_default();
    assert!(
        !callees
            .iter()
            .any(|c| c.contains("mod_a") || c.contains("mod_b")),
        "cyclic re-export with no definition resolves nowhere, got {callees:?}"
    );
}
