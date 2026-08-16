// Embedding store / similarity tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

    #[test]
    // ── Embedding Pipeline Tests ────────────────────────────────

    fn test_function_embedding_field() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");
        let mut projection = (*graph.snapshot()).clone();
        if let Some(add_fn) = projection.functions.get("math.py::add") {
            let mut updated = (**add_fn).clone();
            updated.embedding = EmbeddingVec { vec: vec![0.1, 0.2, 0.3], hash: String::new() };
            projection.functions.insert("math.py::add".to_string(), std::sync::Arc::new(updated));
        }
        graph.commit_projection(projection);
        let snap = graph.snapshot();
        let add = snap.functions.get("math.py::add").unwrap();
        assert_eq!(add.embedding.vec.len(), 3);
    }
    #[test]
    fn test_cosine_similarity() {
        use crate::cosine_similarity;
        let sim = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]);
        assert!((sim - 0.0).abs() < 0.001, "Orthogonal=0, got {}", sim);
        let sim = cosine_similarity(&[1.0, 2.0], &[1.0, 2.0]);
        assert!((sim - 1.0).abs() < 0.001, "Identical=1, got {}", sim);
    }
    // ── Bulk writes (plan §2.1) ──────────────────────────────────
    //
    // Each of these APIs clones the whole ProjectedGraph. Called once per
    // entity/module/edge, that is O(N²) in Arc clones — the dominant cost of
    // `coderadar init --with-embeddings`. The bulk forms clone once.

    #[test]
    fn test_set_embeddings_bulk_applies_every_entry() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def add(a, b): return a + b\ndef sub(a, b): return a - b\nclass Calc: pass\n",
            "math.py");

        let (applied, missing) = graph.set_embeddings_bulk(vec![
            ("math.py::add".into(), vec![0.1, 0.2], "h1".into()),
            ("math.py::sub".into(), vec![0.3, 0.4], "h2".into()),
            ("math.py::Calc".into(), vec![0.5, 0.6], "h3".into()),
        ]);

        assert_eq!(applied, 3);
        assert!(missing.is_empty());
        let snap = graph.snapshot();
        assert_eq!(snap.functions.get("math.py::add").unwrap().embedding.vec, vec![0.1, 0.2]);
        assert_eq!(snap.functions.get("math.py::sub").unwrap().embedding.hash, "h2");
        assert_eq!(snap.classes.get("math.py::Calc").unwrap().embedding.vec, vec![0.5, 0.6]);
    }

    #[test]
    fn test_set_embeddings_bulk_reports_unknown_ids_without_dropping_the_rest() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");

        let (applied, missing) = graph.set_embeddings_bulk(vec![
            ("math.py::ghost".into(), vec![9.9], "h".into()),
            ("math.py::add".into(), vec![0.1], "h".into()),
        ]);

        assert_eq!(applied, 1);
        assert_eq!(missing, vec!["math.py::ghost".to_string()]);
        let snap = graph.snapshot();
        assert_eq!(snap.functions.get("math.py::add").unwrap().embedding.vec, vec![0.1]);
    }

    /// The single-entity API is the one-element case of the bulk one now, so
    /// its not-found error has to survive the delegation.
    #[test]
    fn test_set_embedding_still_errors_on_an_unknown_id() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");
        let err = graph.set_embedding("math.py::ghost", &[0.1], "h").unwrap_err();
        assert!(err.contains("math.py::ghost"), "{}", err);
    }

    #[test]
    fn test_set_module_star_exports_bulk_applies_every_entry() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def a(): pass\n", "one.py");
        index_source(&graph, "def b(): pass\n", "two.py");

        let applied = graph.set_module_star_exports_bulk(vec![
            ("one.py::module".into(), vec!["a".to_string()]),
            ("two.py::module".into(), vec!["b".to_string()]),
            ("missing.py::module".into(), vec!["c".to_string()]),
        ]);

        assert_eq!(applied, 2, "unknown modules are skipped, not counted");
        let snap = graph.snapshot();
        assert_eq!(snap.modules.get("one.py::module").unwrap().star_exports,
                   Some(vec!["a".to_string()]));
        assert_eq!(snap.modules.get("two.py::module").unwrap().star_exports,
                   Some(vec!["b".to_string()]));
    }

    #[test]
    fn test_register_synthetic_edges_bulk_wires_both_directions() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def handler(): pass\ndef other(): pass\n", "views.py");

        let registered = graph.register_synthetic_edges_bulk(vec![
            ("route::/a".into(), "views.py::handler".into(), "ROUTE".into()),
            ("route::/b".into(), "views.py::other".into(), "ROUTE".into()),
        ]).unwrap();

        assert_eq!(registered, 2);
        let snap = graph.snapshot();
        assert!(snap.callees_by_caller.get("route::/a").unwrap()
                    .contains("views.py::handler"));
        assert!(snap.callers_by_callee.get("views.py::other").unwrap()
                    .contains("route::/b"));
    }

    #[test]
    fn test_bulk_writes_are_no_ops_when_empty() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");
        let before = graph.snapshot();

        assert_eq!(graph.set_embeddings_bulk(vec![]), (0, vec![]));
        assert_eq!(graph.set_module_star_exports_bulk(vec![]), 0);
        assert_eq!(graph.register_synthetic_edges_bulk(vec![]).unwrap(), 0);

        assert!(std::sync::Arc::ptr_eq(&before, &graph.snapshot()),
                "an empty batch must not swap the projection");
    }

    #[test]
    fn test_set_embedding_stores_vector() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\ndef sub(a, b): return a - b\n", "math.py");

        // Set embedding on add
        let vec = vec![0.1, 0.2, 0.3, 0.4];
        graph.set_embedding("math.py::add", &vec, "abc123").expect("set_embedding should succeed");

        let snap = graph.snapshot();
        let add = snap.functions.get("math.py::add").unwrap();
        assert_eq!(add.embedding.vec, vec);

        // sub should still have empty embedding
        let sub = snap.functions.get("math.py::sub").unwrap();
        assert!(sub.embedding.vec.is_empty());
    }
    #[test]
    fn test_set_embedding_entity_not_found() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");

        let result = graph.set_embedding("math.py::no_such_function", &[0.1, 0.2], "abc123");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Entity not found"));
    }
    #[test]
    fn test_set_embedding_overwrites_existing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def add(a, b): return a + b\n", "math.py");

        // First embedding
        graph.set_embedding("math.py::add", &[0.1], "abc123").unwrap();
        // Overwrite with different vector
        graph.set_embedding("math.py::add", &[0.9, 0.8, 0.7], "abc123").unwrap();

        let snap = graph.snapshot();
        let add = snap.functions.get("math.py::add").unwrap();
        assert_eq!(add.embedding.vec, vec![0.9, 0.8, 0.7]);
    }
    #[test]
    fn test_search_similar_after_set_embedding() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def auth_login(): pass\ndef render_html(): pass\ndef calc_tax(): pass\n",
            "mod.py");

        // Embed functions with contrasting vectors
        graph.set_embedding("mod.py::auth_login", &[1.0, 0.0, 0.0], "h1").unwrap();
        graph.set_embedding("mod.py::render_html", &[0.0, 1.0, 0.0], "h2").unwrap();
        graph.set_embedding("mod.py::calc_tax", &[0.0, 0.0, 1.0], "h3").unwrap();

        // Verify embeddings stored correctly
        let snap = graph.snapshot();
        assert_eq!(snap.functions.get("mod.py::auth_login").unwrap().embedding.vec, vec![1.0, 0.0, 0.0]);
        assert_eq!(snap.functions.get("mod.py::render_html").unwrap().embedding.vec, vec![0.0, 1.0, 0.0]);
        assert_eq!(snap.functions.get("mod.py::calc_tax").unwrap().embedding.vec, vec![0.0, 0.0, 1.0]);

        // Cosine similarity: vector to itself = 1.0, orthogonal = 0.0
        let sim = crate::cosine_similarity(&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0]);
        assert!((sim - 0.0).abs() < 0.001, "Orthogonal vectors should have similarity 0");
    }
    #[test]
    fn test_set_embedding_empty_vector() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def empty_fn(): pass\n", "mod.py");

        graph.set_embedding("mod.py::empty_fn", &[], "e1").unwrap();
        let snap = graph.snapshot();
        let f = snap.functions.get("mod.py::empty_fn").unwrap();
        assert!(f.embedding.vec.is_empty(), "Empty embedding should be stored as empty");
    }
