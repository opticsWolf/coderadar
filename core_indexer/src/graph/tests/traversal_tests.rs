// Traversal / BFS tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

    #[test]
    fn test_count_unresolved_targets() {
        // 2.3: count unresolved outgoing calls + imports (downstream only).
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def foo():\n    undefined_func()\n", "mod.py");
        index_source(&graph, "import nonexistent_module\ndef bar(): pass\n", "mod2.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.resolve_all_calls(&mut projection);

        let foo = projection.functions.values().find(|f| f.name == "foo").unwrap();
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

        let mod2 = projection.modules.values()
            .find(|m| m.path.to_string_lossy().contains("mod2.py")).unwrap();
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
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): c()\ndef c(): pass\n", "chain.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 1, &["calls".to_string()], false, true);
        // depth 0 = a, depth 1 = b. c is at depth 2, beyond max_depth=1.
        let ids: Vec<&str> = reached.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&a.as_str()), "start a included at depth 0");
        assert!(reached.iter().any(|(id, _, _)| proj.functions.get(id).map_or(false, |f| f.name == "b")),
                "b reached at depth 1, got {:?}", reached);
        assert!(!reached.iter().any(|(id, _, _)| proj.functions.get(id).map_or(false, |f| f.name == "c")),
                "c should NOT be reached at max_depth=1");
        // depth tags
        assert_eq!(reached.iter().find(|(id, _, _)| id == &a).unwrap().1, 0);
        assert_eq!(reached.iter().find(|(_, _, ek)| ek == "calls").map(|(_, d, _)| *d), Some(1));
    }
    #[test]
    fn test_traverse_calls_upstream() {
        // a → b → c ; upstream from c yields c, b, a
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): c()\ndef c(): pass\n", "chain.py"),
        ]);
        let c = fn_id_of(&proj, "c");
        let reached = CodeGraph::traverse_bfs(&proj, &c, 5, &["calls".to_string()], true, false);
        let names: Vec<String> = reached.iter().filter_map(|(id, _, _)|
            proj.functions.get(id).map(|f| f.name.clone())).collect();
        assert!(names.contains(&"c".to_string()) && names.contains(&"b".to_string()) && names.contains(&"a".to_string()),
                "upstream from c should reach b and a, got {:?}", names);
    }
    #[test]
    fn test_traverse_cycle_terminates() {
        // a ↔ b (mutual call). BFS must terminate, each node once.
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): a()\n", "cycle.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 10, &["calls".to_string()], true, true);
        // start a + b = 2 distinct (cycle doesn't duplicate).
        assert_eq!(reached.len(), 2, "cycle should yield exactly 2 nodes, got {:?}", reached);
    }
    #[test]
    fn test_traverse_diamond_one_entry_per_node() {
        // a → b, a → c, b → d, c → d. d reached once.
        let proj = snapshot_from(&[
            ("def a(): b(); c()\ndef b(): d()\ndef c(): d()\ndef d(): pass\n", "diamond.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 5, &["calls".to_string()], false, true);
        let d_count = reached.iter().filter(|(id, _, _)| proj.functions.get(id).map_or(false, |f| f.name == "d")).count();
        assert_eq!(d_count, 1, "d should appear exactly once in a diamond, got {}", d_count);
        assert_eq!(reached.len(), 4, "a,b,c,d each once = 4, got {}", reached.len());
    }
    #[test]
    fn test_traverse_max_depth_zero_returns_only_start() {
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): pass\n", "md0.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 0, &["calls".to_string()], false, true);
        assert_eq!(reached.len(), 1, "max_depth=0 yields only the start node");
        assert_eq!(reached[0].0, a);
        assert_eq!(reached[0].1, 0);
    }
    #[test]
    fn test_traverse_empty_edge_kinds_returns_only_start() {
        let proj = snapshot_from(&[
            ("def a(): b()\ndef b(): pass\n", "empty.py"),
        ]);
        let a = fn_id_of(&proj, "a");
        let reached = CodeGraph::traverse_bfs(&proj, &a, 5, &[], true, true);
        assert_eq!(reached.len(), 1, "empty edge_kinds yields only the start node");
    }
    #[test]
    fn test_traverse_imports_upstream_nonempty() {
        // b imports c (module-level). importers[c_mod] = {b_mod}.
        let proj = snapshot_from(&[
            ("def utility(): pass\n", "src/c.py"),
            ("from src.c import utility\ndef helper(): utility()\n", "src/b.py"),
        ]);
        let c_mod = proj.modules.iter()
            .find(|(_, m)| m.path.to_string_lossy().contains("c.py"))
            .map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &c_mod, 3, &["imports".to_string()], true, false);
        let who: Vec<&str> = reached.iter().map(|(id, _, _)| id.as_str()).collect();
        // start c_mod at depth 0; b_mod at depth 1.
        assert!(who.iter().any(|id| proj.modules.get(*id).map_or(false, |m| m.path.to_string_lossy().contains("b.py"))),
                "imports upstream from c should reach b, got {:?}", who);
    }
    #[test]
    fn test_traverse_extends_downstream_via_resolved_bases() {
        // B(A) same module → resolved_bases[B] = [A], so extends downstream from B reaches A.
        let proj = snapshot_from(&[
            ("class A:\n    def m(self): pass\nclass B(A):\n    def m(self): pass\n", "hier.py"),
        ]);
        let b_id = proj.classes.iter().find(|(_, c)| c.name == "B").map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &b_id, 3, &["extends".to_string()], false, true);
        let names: Vec<String> = reached.iter().filter_map(|(id, _, _)|
            proj.classes.get(id).map(|c| c.name.clone())).collect();
        assert!(names.contains(&"A".to_string()), "extends downstream from B should reach A, got {:?}", names);
    }
    #[test]
    fn test_traverse_overrides_upstream_from_base() {
        // Base.helper overridden by Child.helper → overridden_by[base] = {child}.
        let proj = snapshot_from(&[
            ("class Base:\n    def helper(self): pass\nclass Child(Base):\n    def helper(self): pass\n", "ovr.py"),
        ]);
        let base_f = proj.functions.iter()
            .find(|(_, f)| f.name == "helper" && f.parent_class.as_deref().map_or(false, |pc|
                proj.classes.get(pc).map_or(false, |c| c.name == "Base")))
            .map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &base_f, 3, &["overrides".to_string()], true, false);
        let names: Vec<String> = reached.iter().filter_map(|(id, _, _)|
            proj.functions.get(id).map(|f| f.name.clone())).collect();
        assert!(names.contains(&"helper".to_string()) && reached.len() >= 2,
                "overrides upstream from Base.helper should reach Child.helper, got {:?}", reached);
    }
    #[test]
    fn test_traverse_inherits_alias_for_extends() {
        // The pyfunction normalizes "inherits"→"extends"; traverse_bfs itself
        // only knows "extends", confirming the alias mapping in lib.rs. Here
        // we just assert "extends" works (alias coverage is in the Python layer).
        let proj = snapshot_from(&[
            ("class A:\n    pass\nclass B(A):\n    pass\n", "alias.py"),
        ]);
        let b_id = proj.classes.iter().find(|(_, c)| c.name == "B").map(|(id, _)| id.clone()).unwrap();
        let reached = CodeGraph::traverse_bfs(&proj, &b_id, 3, &["extends".to_string()], false, true);
        assert!(reached.iter().any(|(id, _, _)| proj.classes.get(id).map_or(false, |c| c.name == "A")));
    }
