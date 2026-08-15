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
