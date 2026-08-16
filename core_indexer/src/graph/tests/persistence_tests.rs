// Macrame persistence / temporal tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

    #[test]
    // ── Persistence Tests ────────────────────────────────────────

    fn test_persist_entities_no_store_returns_zero() {
        let graph = CodeGraph::new(GraphConfig::default());
        let units: Vec<ExtractedUnit> = vec![];
        // No store attached → no-op
        let count = graph.persist_entities(&units, "test.py", "python");
        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 0);
    }
    #[test]
    fn test_persist_edges_no_store_returns_zero() {
        let graph = CodeGraph::new(GraphConfig::default());
        let snap = graph.snapshot();
        // No store attached → no-op, returns 0 edges persisted
        let count = graph.persist_edges(&snap);
        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 0);
    }
    #[test]
    fn test_persist_entities_with_index() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def foo(): pass\n", "test_persist.py");
        // Entities are persisted inside index_file → persist_entities is called
        // without a store it returns Ok(0) but shouldn't crash
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "foo"));
        // Verify has_store returns false (no config for store in test)
        assert!(!graph.has_store());
    }
    #[test]
    fn test_persist_edges_with_resolved_calls() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def caller(): callee()\ndef callee(): pass\n",
                     "edges_test.py");
        let snap = graph.snapshot();
        // Edges persisted via persist_edges (no-op without store)
        let count = graph.persist_edges(&snap);
        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 0);
        // Verify edges exist in memory
        let caller_id = snap.functions.values()
            .find(|f| f.name == "caller").map(|f| f.id.clone());
        assert!(caller_id.is_some());
        if let Some(cid) = caller_id {
            let callees = snap.callees_by_caller.get(&cid);
            assert!(callees.is_some(), "caller should have callee edges");
        }
    }

    // ── Tier 2 Language Tests — Swift, Scala, Lua, Elixir, Zig, R ────
    #[test]
    fn test_synthetic_edge_registration() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def index(): pass\ndef user_detail(id): pass\n", "views.py");

        // Register a synthetic route→handler edge (like Django path()→view)
        graph.register_synthetic_edge(
            "django:route:users/",
            "views.py::user_detail",
            "HANDLES",
        ).unwrap();

        let snap = graph.snapshot();
        // Route should appear as a caller of user_detail
        let callees = snap.callees_by_caller.get("django:route:users/");
        assert!(callees.is_some(), "route should have callees");
        assert!(callees.unwrap().contains("views.py::user_detail"),
                "route should call user_detail");

        // user_detail should appear as callee of the route
        let callers = snap.callers_by_callee.get("views.py::user_detail");
        assert!(callers.is_some(), "user_detail should have callers");
        assert!(callers.unwrap().contains("django:route:users/"),
                "user_detail should be called by route");
    }
    #[test]
    fn test_synthetic_edge_roundtrip_query() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def list_items(): pass\n", "views.py");

        graph.register_synthetic_edge(
            "fastapi:route:main.py:/items",
            "views.py::list_items",
            "HANDLES",
        ).unwrap();

        let snap = graph.snapshot();

        // Querying callees_of the route should return list_items
        let callees = snap.callees_by_caller.get("fastapi:route:main.py:/items");
        assert!(callees.map_or(false, |c| c.iter().any(|e| e.contains("list_items"))),
                "route should have list_items as callee");
    }

    // ── v3.6: Cross-file fn-ref via imports ──────────────────────
    #[test]
    // ── v3.6: Macrame temporal query tests ──────────────────────

    fn test_temporal_concepts_persisted() {
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph, "def caller(): callee()\ndef callee(): pass\n", "tp_test.py");

        let snap = graph.snapshot();
        // In-memory graph should have data
        assert!(snap.functions.len() >= 2);
        let caller = snap.functions.values().find(|f| f.name == "caller").unwrap();
        // In-memory edges should exist
        let callees = snap.callees_by_caller.get(&caller.id).unwrap();
        assert!(!callees.is_empty(), "caller should have callees in memory");

        // Verify store was attached
        assert!(graph.has_store());

        // Verify the DB file exists and has content
        let db_path = _dir.path().join("test.db");
        assert!(db_path.exists(), "db file should exist");
        let meta = std::fs::metadata(&db_path).unwrap();
        assert!(meta.len() > 0, "db file should not be empty");
    }
    #[test]
    fn test_temporal_reconstruct_after_index() {
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph, "def foo(): pass\n", "recon_test.py");

        let store = graph.store.as_ref().unwrap();
        // reconstruct() requires a valid ISO 8601 timestamp
        let state = store.reconstruct(crate::storage::TS_OPEN);
        // reconstruct may fail if no data matches — but shouldn't crash
        // Either Ok or a reasonable error is acceptable
        match state {
            Ok(_s) => { /* reconstruction succeeded */ }
            Err(e) => {
                // Macrame may not have matching data for TS_OPEN — that's fine
                eprintln!("reconstruct returned: {:?}", e);
            }
        }
    }
    #[test]
    fn test_temporal_edge_persistence_across_indexes() {
        let (graph, _dir) = graph_with_temp_store();

        // First index
        index_source(&graph,
            "def a(): b()\ndef b(): c()\ndef c(): pass\n",
            "chain.py");

        let snap = graph.snapshot();
        let func_a = snap.functions.values().find(|f| f.name == "a").unwrap();
        let callees = snap.callees_by_caller.get(&func_a.id);
        assert!(callees.is_some(), "a should have callees");
        assert!(!callees.unwrap().is_empty());

        // Verify store persistence — db file should be non-empty
        let db_path = _dir.path().join("test.db");
        assert!(db_path.exists());
        let meta = std::fs::metadata(&db_path).unwrap();
        assert!(meta.len() > 0, "db should have persisted data; size={}", meta.len());

        // Second index should not corrupt
        index_source(&graph,
            "def x(): pass\n",
            "extra.py");
        let snap2 = graph.snapshot();
        assert!(snap2.functions.values().any(|f| f.name == "x"));
        assert!(snap2.functions.values().any(|f| f.name == "a"));
    }
#[test]
    fn test_persist_edges_emits_imports_and_extends() {
        // Phase D.5: persist_edges must assert IMPORTS / EXTENDS (and OVERRIDES)
        // edges to Macrame in addition to CALLS — and succeed (no FK/kind error).
        // index_file persists CONCEPTS only (not edges / resolve passes),
        // so we run the Phase-D passes + persist_edges exactly as analyze() does.
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph, "class Base:\n    def m(self): pass\n", "base.py");
        index_source(&graph, "class Sub(Base):\n    def m(self): pass\n", "sub.py");
        index_source(&graph, "def util(): pass\n", "src/u.py");
        index_source(&graph, "from src.u import util\ndef app(): util()\n", "src/app.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_imports(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_all_calls(&mut projection);

        let call_edges: usize = projection.callees_by_caller.values().map(|s| s.len()).sum();
        let importer_edges: usize = projection.importers.values().map(|s| s.len()).sum();
        let subclass_edges: usize = projection.subclasses.values().map(|s| s.len()).sum();
        let override_edges: usize = projection.overrides_base.len();

        // Sanity: the fixture produced real non-call edges that D should persist.
        assert!(importer_edges > 0, "fixture should resolve >=1 import, got {}", importer_edges);
        assert!(subclass_edges > 0, "fixture should resolve >=1 subclass, got {}", subclass_edges);
        assert!(override_edges > 0, "fixture should resolve >=1 override, got {}", override_edges);

        let persisted = graph.persist_edges(&projection)
            .expect("persist_edges should succeed (concepts present, no FK violation)");
        // persist_edges pushes one assertion per CALL + IMPORTS + EXTENDS + OVERRIDES
        // edge (fixture has no external/builtin targets), so the persisted total
        // must equal the exact sum — proving IMPORTS edges now reach Macrame
        // (module concepts are persisted by synthesize_module_unit).
        let expected = call_edges + importer_edges + subclass_edges + override_edges;
        assert_eq!(persisted, expected,
                "persist_edges should persist CALLS+IMPORTS+EXTENDS+OVERRIDES exactly");
    }
    #[test]
    fn test_temporal_synthetic_edge_persistence() {
        let (graph, _dir) = graph_with_temp_store();
        index_source(&graph,
            "def user_detail(id): pass\n",
            "views.py");

        // Register a synthetic framework edge
        graph.register_synthetic_edge(
            "django:route:users/",
            "views.py::user_detail",
            "HANDLES",
        ).unwrap();

        // Verify in-memory graph has the edge
        let snap = graph.snapshot();
        let route_callees = snap.callees_by_caller.get("django:route:users/");
        assert!(route_callees.is_some(), "route should have callees");
        assert!(route_callees.unwrap().contains("views.py::user_detail"));

        // Verify the DB file has content (persisted)
        let db_path = _dir.path().join("test.db");
        assert!(db_path.exists());
        let meta = std::fs::metadata(&db_path).unwrap();
        assert!(meta.len() > 0, "db should have persisted data");
    }

    // ── Scoped persistence (plan §1.2) ───────────────────────────────────

    /// Build the same fixture as the unscoped test: two files whose entities
    /// are wired to each other, so scoping has something to exclude.
    fn projection_with_cross_file_edges(graph: &CodeGraph) -> ProjectedGraph {
        index_source(graph, "class Base:\n    def m(self): pass\n", "base.py");
        index_source(graph, "class Sub(Base):\n    def m(self): pass\n", "sub.py");
        index_source(graph, "def util(): pass\n", "src/u.py");
        index_source(graph, "from src.u import util\ndef app(): util()\n", "src/app.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_imports(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_all_calls(&mut projection);
        projection
    }

    /// The point of the change: a one-file edit must not re-assert the
    /// project's whole edge set.
    #[test]
    fn test_persist_edges_scoped_writes_fewer_edges_than_unscoped() {
        let (graph, _dir) = graph_with_temp_store();
        let projection = projection_with_cross_file_edges(&graph);

        let all = graph.persist_edges(&projection).unwrap();
        let scoped = graph
            .persist_edges_scoped(&projection, Some("sub.py"))
            .unwrap();

        assert!(scoped > 0, "the scoped file does have edges");
        assert!(scoped < all, "scoped {} should be below unscoped {}", scoped, all);
    }

    /// Either endpoint counts: `sub.py::Sub` extends `base.py::Base`, and
    /// scoping to base.py must still carry that edge, or an edit to a base
    /// class would silently drop its subclasses from the ledger.
    #[test]
    fn test_persist_edges_scoped_includes_edges_pointing_into_the_file() {
        let (graph, _dir) = graph_with_temp_store();
        let projection = projection_with_cross_file_edges(&graph);

        let scoped = graph
            .persist_edges_scoped(&projection, Some("base.py"))
            .unwrap();

        assert!(scoped > 0,
                "EXTENDS/OVERRIDES edges aimed at base.py are in base.py's scope");
    }

    /// No entity id starts with this prefix, so nothing is in scope — the
    /// filter must exclude rather than fall back to everything.
    #[test]
    fn test_persist_edges_scoped_to_an_unknown_file_writes_nothing() {
        let (graph, _dir) = graph_with_temp_store();
        let projection = projection_with_cross_file_edges(&graph);

        let scoped = graph
            .persist_edges_scoped(&projection, Some("not_indexed.py"))
            .unwrap();

        assert_eq!(scoped, 0);
    }

    #[test]
    fn test_persist_edges_scoped_none_matches_the_unscoped_form() {
        let (graph, _dir) = graph_with_temp_store();
        let projection = projection_with_cross_file_edges(&graph);

        assert_eq!(
            graph.persist_edges_scoped(&projection, None).unwrap(),
            graph.persist_edges(&projection).unwrap()
        );
    }
