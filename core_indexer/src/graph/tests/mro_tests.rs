// C3 MRO tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

    #[test]
    fn test_c3_mro_single_inheritance() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class A:\n    def foo(self): pass\nclass B(A):\n    def bar(self): self.foo()\n",
            "mod.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        if let Some(b) = projection.classes.values().find(|c| c.name == "B") {
            assert!(b.mro.len() >= 2, "B should have at least 2 MRO entries, got {}", b.mro.len());
            assert!(matches!(&b.mro[0], MroNode::Class(_)));
        }
    }
    #[test]
    fn test_c3_mro_multiple_inheritance() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class X:\n    def x(self): pass\nclass Y:\n    def y(self): pass\nclass Z(X, Y):\n    def z(self): pass\n",
            "diamond.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        // Z's MRO should be: Z → X → Y → object
        if let Some(z) = projection.classes.values().find(|c| c.name == "Z") {
            assert!(z.mro.len() >= 3,
                    "Z should have at least 3 MRO entries, got {}", z.mro.len());
        }
    }
    #[test]
    fn test_mro_method_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Base:\n    def helper(self): pass\nclass Child(Base):\n    def run(self): self.helper()\n",
            "inherited.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_all_calls(&mut projection);
        // Child.run() calls self.helper() — should resolve to Base.helper via MRO
        if let Some(run) = projection.functions.values().find(|f| f.name == "run") {
            let callees = projection.callees_by_caller.get(&run.id);
            assert!(callees.is_some(), "run should have resolved callees");
            if let Some(callee_ids) = callees {
                let callee_names: Vec<_> = callee_ids.iter()
                    .filter_map(|id| projection.functions.get(id))
                    .map(|f| f.name.clone())
                    .collect();
                assert!(callee_names.contains(&"helper".to_string()),
                        "run should call helper via MRO, got: {:?}", callee_names);
            }
        }
    }
// ── Phase D back-fill: subclasses / importers / overrides ─────────────
    #[test]
    fn test_c3_diamond() {
        // Diamond inheritance:
        //   A
        //  / \
        // B   C
        //  \ /
        //   D
        // C3 MRO for D: D → B → C → A
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class A: pass\nclass B(A): pass\nclass C(A): pass\nclass D(B, C): pass\n",
            "diamond.py");
        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        if let Some(d) = projection.classes.values().find(|c| c.name == "D") {
            assert_eq!(d.mro.len(), 4, "D should have MRO [D, B, C, A], got {:?} entries", d.mro.len());
            // Verify order: D is first
            if let MroNode::Class(ref id) = d.mro[0] {
                assert!(id.contains("D"), "First MRO entry should be D");
            }
        }
    }

    // ── Ruby Indexing Tests ─────────────────────────────────────
