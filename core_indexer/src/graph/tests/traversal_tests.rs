// Traversal / BFS tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

#[test]
fn test_count_unresolved_targets() {
    // 2.3: count unresolved outgoing calls + imports (downstream only).
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "def foo():\n    undefined_func()\n", "mod.py");
    index_source(
        &graph,
        "import nonexistent_module\ndef bar(): pass\n",
        "mod2.py",
    );
    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);
    graph.resolve_all_calls(&mut projection);

    let foo = projection
        .functions
        .values()
        .find(|f| f.name == "foo")
        .unwrap();
    let calls_kind = vec!["calls".to_string()];
    assert_eq!(
        CodeGraph::count_unresolved_targets(&projection, &foo.id, &calls_kind, true),
        1,
        "foo should have 1 unresolved call"
    );
    assert_eq!(
        CodeGraph::count_unresolved_targets(&projection, &foo.id, &calls_kind, false),
        0,
        "upstream should report 0 unresolved"
    );

    let mod2 = projection
        .modules
        .values()
        .find(|m| m.path.to_string_lossy().contains("mod2.py"))
        .unwrap();
    let imports_kind = vec!["imports".to_string()];
    assert_eq!(
        CodeGraph::count_unresolved_targets(&projection, &mod2.id, &imports_kind, true),
        1,
        "mod2 should have 1 unresolved import"
    );
}
#[test]
fn test_traverse_calls_downstream_depth() {
    // a → b → c
    let proj = snapshot_from(&[("def a(): b()\ndef b(): c()\ndef c(): pass\n", "chain.py")]);
    let a = fn_id_of(&proj, "a");
    let reached = CodeGraph::traverse_bfs(&proj, &a, 1, &["calls".to_string()], false, true);
    // depth 0 = a, depth 1 = b. c is at depth 2, beyond max_depth=1.
    let ids: Vec<&str> = reached.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(ids.contains(&a.as_str()), "start a included at depth 0");
    assert!(
        reached
            .iter()
            .any(|(id, _, _)| proj.functions.get(id).is_some_and(|f| f.name == "b")),
        "b reached at depth 1, got {:?}",
        reached
    );
    assert!(
        !reached
            .iter()
            .any(|(id, _, _)| proj.functions.get(id).is_some_and(|f| f.name == "c")),
        "c should NOT be reached at max_depth=1"
    );
    // depth tags
    assert_eq!(reached.iter().find(|(id, _, _)| id == &a).unwrap().1, 0);
    assert_eq!(
        reached
            .iter()
            .find(|(_, _, ek)| ek == "calls")
            .map(|(_, d, _)| *d),
        Some(1)
    );
}
#[test]
fn test_traverse_calls_upstream() {
    // a → b → c ; upstream from c yields c, b, a
    let proj = snapshot_from(&[("def a(): b()\ndef b(): c()\ndef c(): pass\n", "chain.py")]);
    let c = fn_id_of(&proj, "c");
    let reached = CodeGraph::traverse_bfs(&proj, &c, 5, &["calls".to_string()], true, false);
    let names: Vec<String> = reached
        .iter()
        .filter_map(|(id, _, _)| proj.functions.get(id).map(|f| f.name.clone()))
        .collect();
    assert!(
        names.contains(&"c".to_string())
            && names.contains(&"b".to_string())
            && names.contains(&"a".to_string()),
        "upstream from c should reach b and a, got {:?}",
        names
    );
}
#[test]
fn test_traverse_cycle_terminates() {
    // a ↔ b (mutual call). BFS must terminate, each node once.
    let proj = snapshot_from(&[("def a(): b()\ndef b(): a()\n", "cycle.py")]);
    let a = fn_id_of(&proj, "a");
    let reached = CodeGraph::traverse_bfs(&proj, &a, 10, &["calls".to_string()], true, true);
    // start a + b = 2 distinct (cycle doesn't duplicate).
    assert_eq!(
        reached.len(),
        2,
        "cycle should yield exactly 2 nodes, got {:?}",
        reached
    );
}
#[test]
fn test_traverse_diamond_one_entry_per_node() {
    // a → b, a → c, b → d, c → d. d reached once.
    let proj = snapshot_from(&[(
        "def a(): b(); c()\ndef b(): d()\ndef c(): d()\ndef d(): pass\n",
        "diamond.py",
    )]);
    let a = fn_id_of(&proj, "a");
    let reached = CodeGraph::traverse_bfs(&proj, &a, 5, &["calls".to_string()], false, true);
    let d_count = reached
        .iter()
        .filter(|(id, _, _)| proj.functions.get(id).is_some_and(|f| f.name == "d"))
        .count();
    assert_eq!(
        d_count, 1,
        "d should appear exactly once in a diamond, got {}",
        d_count
    );
    assert_eq!(
        reached.len(),
        4,
        "a,b,c,d each once = 4, got {}",
        reached.len()
    );
}
#[test]
fn test_traverse_max_depth_zero_returns_only_start() {
    let proj = snapshot_from(&[("def a(): b()\ndef b(): pass\n", "md0.py")]);
    let a = fn_id_of(&proj, "a");
    let reached = CodeGraph::traverse_bfs(&proj, &a, 0, &["calls".to_string()], false, true);
    assert_eq!(reached.len(), 1, "max_depth=0 yields only the start node");
    assert_eq!(reached[0].0, a);
    assert_eq!(reached[0].1, 0);
}
#[test]
fn test_traverse_empty_edge_kinds_returns_only_start() {
    let proj = snapshot_from(&[("def a(): b()\ndef b(): pass\n", "empty.py")]);
    let a = fn_id_of(&proj, "a");
    let reached = CodeGraph::traverse_bfs(&proj, &a, 5, &[], true, true);
    assert_eq!(
        reached.len(),
        1,
        "empty edge_kinds yields only the start node"
    );
}
#[test]
fn test_traverse_imports_upstream_nonempty() {
    // b imports c (module-level). importers[c_mod] = {b_mod}.
    let proj = snapshot_from(&[
        ("def utility(): pass\n", "src/c.py"),
        (
            "from src.c import utility\ndef helper(): utility()\n",
            "src/b.py",
        ),
    ]);
    let c_mod = proj
        .modules
        .iter()
        .find(|(_, m)| m.path.to_string_lossy().contains("c.py"))
        .map(|(id, _)| id.clone())
        .unwrap();
    let reached = CodeGraph::traverse_bfs(&proj, &c_mod, 3, &["imports".to_string()], true, false);
    let who: Vec<&str> = reached.iter().map(|(id, _, _)| id.as_str()).collect();
    // start c_mod at depth 0; b_mod at depth 1.
    assert!(
        who.iter().any(|id| proj
            .modules
            .get(*id)
            .is_some_and(|m| m.path.to_string_lossy().contains("b.py"))),
        "imports upstream from c should reach b, got {:?}",
        who
    );
}
#[test]
fn test_traverse_extends_downstream_via_resolved_bases() {
    // B(A) same module → resolved_bases[B] = [A], so extends downstream from B reaches A.
    let proj = snapshot_from(&[(
        "class A:\n    def m(self): pass\nclass B(A):\n    def m(self): pass\n",
        "hier.py",
    )]);
    let b_id = proj
        .classes
        .iter()
        .find(|(_, c)| c.name == "B")
        .map(|(id, _)| id.clone())
        .unwrap();
    let reached = CodeGraph::traverse_bfs(&proj, &b_id, 3, &["extends".to_string()], false, true);
    let names: Vec<String> = reached
        .iter()
        .filter_map(|(id, _, _)| proj.classes.get(id).map(|c| c.name.clone()))
        .collect();
    assert!(
        names.contains(&"A".to_string()),
        "extends downstream from B should reach A, got {:?}",
        names
    );
}
#[test]
fn test_traverse_overrides_upstream_from_base() {
    // Base.helper overridden by Child.helper → overridden_by[base] = {child}.
    let proj = snapshot_from(&[(
        "class Base:\n    def helper(self): pass\nclass Child(Base):\n    def helper(self): pass\n",
        "ovr.py",
    )]);
    let base_f = proj
        .functions
        .iter()
        .find(|(_, f)| {
            f.name == "helper"
                && f.parent_class
                    .as_deref()
                    .is_some_and(|pc| proj.classes.get(pc).is_some_and(|c| c.name == "Base"))
        })
        .map(|(id, _)| id.clone())
        .unwrap();
    let reached =
        CodeGraph::traverse_bfs(&proj, &base_f, 3, &["overrides".to_string()], true, false);
    let names: Vec<String> = reached
        .iter()
        .filter_map(|(id, _, _)| proj.functions.get(id).map(|f| f.name.clone()))
        .collect();
    assert!(
        names.contains(&"helper".to_string()) && reached.len() >= 2,
        "overrides upstream from Base.helper should reach Child.helper, got {:?}",
        reached
    );
}
#[test]
fn test_traverse_inherits_alias_for_extends() {
    // The pyfunction normalizes "inherits"→"extends"; traverse_bfs itself
    // only knows "extends", confirming the alias mapping in lib.rs. Here
    // we just assert "extends" works (alias coverage is in the Python layer).
    let proj = snapshot_from(&[("class A:\n    pass\nclass B(A):\n    pass\n", "alias.py")]);
    let b_id = proj
        .classes
        .iter()
        .find(|(_, c)| c.name == "B")
        .map(|(id, _)| id.clone())
        .unwrap();
    let reached = CodeGraph::traverse_bfs(&proj, &b_id, 3, &["extends".to_string()], false, true);
    assert!(reached
        .iter()
        .any(|(id, _, _)| proj.classes.get(id).is_some_and(|c| c.name == "A")));
}

#[test]
fn test_traverse_implements_both_directions() {
    // §2.6: `implements` walks resolved_interfaces downstream and the
    // implementers reverse index upstream; `extends` no longer leaks
    // interface implementations.
    let proj = snapshot_from(&[(
        "interface Shape { area(): number; }\n\
         class Circle implements Shape { area(): number { return 0; } }\n\
         class Sub extends Circle { }\n",
        "shapes.ts",
    )]);
    let cls = |name: &str| {
        proj.classes
            .values()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} should be indexed"))
            .id
            .clone()
    };
    let (shape, circle, sub) = (cls("Shape"), cls("Circle"), cls("Sub"));
    let kinds = vec!["implements".to_string()];

    let down = CodeGraph::traverse_bfs(&proj, &circle, 5, &kinds, false, true);
    let down_ids: Vec<&str> = down.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        down_ids.contains(&shape.as_str()),
        "Circle implements Shape: {down_ids:?}"
    );
    assert!(
        !down_ids.contains(&sub.as_str()),
        "Sub is not an interface: {down_ids:?}"
    );

    let up = CodeGraph::traverse_bfs(&proj, &shape, 5, &kinds, true, false);
    let up_ids: Vec<&str> = up.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        up_ids.contains(&circle.as_str()),
        "Shape implemented by Circle: {up_ids:?}"
    );
    assert!(
        !up_ids.contains(&sub.as_str()),
        "Sub extends, not implements: {up_ids:?}"
    );

    // The extends arm is now interface-free.
    let ext = CodeGraph::traverse_bfs(&proj, &sub, 5, &["extends".to_string()], false, true);
    let ext_ids: Vec<&str> = ext.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        ext_ids.contains(&circle.as_str()),
        "Sub extends Circle: {ext_ids:?}"
    );
    assert!(
        !ext_ids.contains(&shape.as_str()),
        "Shape is not a base class: {ext_ids:?}"
    );
}

#[test]
fn test_decorators_resolve_unique_tier_hits() {
    // §2.6 slice C: decorator text reduces to a name and resolves through
    // the base tiers over classes and functions. Unique hits resolve;
    // builtins, ambiguous and unresolvable names leave no edge.
    let proj = snapshot_from(&[
        (
            "def validate(fn):\n    return fn\n\
             def route(path):\n    def wrap(fn):\n        return fn\n    return wrap\n",
            "helpers.py",
        ),
        (
            "from helpers import validate\n\
             import helpers\n\n\
             @validate\n\
             def alpha():\n    pass\n\n\
             @helpers.route(\"/x\")\n\
             def beta():\n    pass\n\n\
             @property\n\
             def gamma():\n    pass\n\n\
             @validate\n\
             @route(\"/y\")\n\
             def delta():\n    pass\n",
            "app.py",
        ),
        (
            "def entity(cls):\n    return cls\n\n\
             @entity\n\
             class Model:\n    pass\n",
            "models.py",
        ),
        ("def tag(fn):\n    return fn\n", "dup1.py"),
        ("def tag(fn):\n    return fn\n", "dup2.py"),
        ("@tag\ndef lonely():\n    pass\n", "use.py"),
    ]);
    let fn_named = |name: &str| {
        proj.functions
            .values()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} should be indexed"))
    };
    // Import-aware tier: @validate → helpers.py::validate.
    assert_eq!(
        fn_named("alpha").resolved_decorators,
        vec!["helpers.py::validate".to_string()]
    );
    // Dotted + applied: leaf `route` → global-unique helpers.py::route.
    assert_eq!(
        fn_named("beta").resolved_decorators,
        vec!["helpers.py::route".to_string()]
    );
    // Builtin with no indexed target: no edge.
    assert!(fn_named("gamma").resolved_decorators.is_empty());
    // Stacked decorators resolve in order; the applied `@route("/y")`
    // reduces to the leaf and lands on the global-unique target.
    assert_eq!(
        fn_named("delta").resolved_decorators,
        vec![
            "helpers.py::validate".to_string(),
            "helpers.py::route".to_string()
        ]
    );
    // Class decorators resolve too.
    let model = proj
        .classes
        .values()
        .find(|c| c.name == "Model")
        .expect("Model should be indexed");
    assert_eq!(
        model.resolved_decorators,
        vec!["models.py::entity".to_string()]
    );
    // Ambiguous (dup1 + dup2 both define tag, no import): no edge.
    assert!(fn_named("lonely").resolved_decorators.is_empty());
    // Reverse index mirrors the forward edges.
    assert!(proj.decorated_by["helpers.py::validate"].contains("app.py::alpha"));
    assert!(proj.decorated_by["models.py::entity"].contains("models.py::Model"));
    assert!(!proj.decorated_by.contains_key("dup1.py::tag"));
}

#[test]
fn test_traverse_decorates_both_directions() {
    // §2.6 slice C: `decorates` walks resolved_decorators downstream and
    // the decorated_by reverse index upstream, for functions and classes.
    let proj = snapshot_from(&[
        ("def validate(fn):\n    return fn\n", "helpers.py"),
        (
            "from helpers import validate\n\n\
             @validate\n\
             def alpha():\n    pass\n",
            "app.py",
        ),
        (
            "def entity(cls):\n    return cls\n\n\
             @entity\n\
             class Model:\n    pass\n",
            "models.py",
        ),
    ]);
    let kinds = vec!["decorates".to_string()];

    let down = CodeGraph::traverse_bfs(&proj, "app.py::alpha", 5, &kinds, false, true);
    let down_ids: Vec<&str> = down.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        down_ids.contains(&"helpers.py::validate"),
        "alpha decorated by validate: {down_ids:?}"
    );

    let up = CodeGraph::traverse_bfs(&proj, "helpers.py::validate", 5, &kinds, true, false);
    let up_ids: Vec<&str> = up.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        up_ids.contains(&"app.py::alpha"),
        "validate decorates alpha: {up_ids:?}"
    );

    // Class side, both directions.
    let cdown = CodeGraph::traverse_bfs(&proj, "models.py::Model", 5, &kinds, false, true);
    assert!(cdown.iter().any(|(id, _, _)| id == "models.py::entity"));
    let cup = CodeGraph::traverse_bfs(&proj, "models.py::entity", 5, &kinds, true, false);
    assert!(cup.iter().any(|(id, _, _)| id == "models.py::Model"));
}

#[test]
fn test_traverse_instantiates_both_directions() {
    // §2.6 slice D: `instantiates` walks Constructor targets downstream
    // and the instantiated_by reverse index upstream. A constructor
    // invocation both calls (CALLS, to `__init__`/class) and instantiates
    // (INSTANTIATES, to the class) — overlap, not partition.
    let proj = snapshot_from(&[
        (
            "class Engine:\n    def __init__(self):\n        pass\n",
            "eng.py",
        ),
        (
            "from eng import Engine\n\ndef build():\n    return Engine()\n\ndef helper():\n    return 1\n",
            "app.py",
        ),
    ]);
    let kinds = vec!["instantiates".to_string()];

    let down = CodeGraph::traverse_bfs(&proj, "app.py::build", 5, &kinds, false, true);
    let down_ids: Vec<&str> = down.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        down_ids.contains(&"eng.py::Engine"),
        "build instantiates Engine: {down_ids:?}"
    );
    assert!(
        down.iter()
            .all(|(_, _, k)| k.is_empty() || k == "instantiates"),
        "arm reaches only via instantiates: {down:?}"
    );

    let up = CodeGraph::traverse_bfs(&proj, "eng.py::Engine", 5, &kinds, true, false);
    let up_ids: Vec<&str> = up.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        up_ids.contains(&"app.py::build"),
        "Engine instantiated by build: {up_ids:?}"
    );
    assert!(
        !up_ids.contains(&"app.py::helper"),
        "helper never constructs: {up_ids:?}"
    );

    // The calls arm still sees the constructor invocation (overlap).
    let calls = CodeGraph::traverse_bfs(
        &proj,
        "app.py::build",
        5,
        &["calls".to_string()],
        false,
        true,
    );
    let call_ids: Vec<&str> = calls.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(
        call_ids.iter().any(|id| id.starts_with("eng.py::Engine")),
        "CALLS still covers the constructor: {call_ids:?}"
    );
}
