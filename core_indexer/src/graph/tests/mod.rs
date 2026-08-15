    use super::*;
    // Explicit imports for items no longer glob-visible from this module's new
    // location (they were in scope via the old single-file graph.rs header).
    use petgraph::graph::NodeIndex;

    mod query_compile_tests;
    mod import_graph_tests;
    mod call_graph_tests;
    mod mro_tests;
    mod traversal_tests;
    mod inheritance_tests;
    mod embedding_tests;
    mod persistence_tests;
    mod projection_tests;
    mod indexing_tests;

    fn make_call_node(g: &mut CallGraph, id: &str) -> NodeIndex {
        if let Some(existing) = g.path_to_node.get(id) {
            return *existing;
        }
        let idx = g.graph.add_node(CallNode {
            entity_id: id.into(),
            qualified_name: format!("mod.{}", id),
        });
        g.path_to_node.insert(id.into(), idx);
        idx
    }

    fn make_call_edge(g: &mut CallGraph, from: &str, to: &str) {
        let a = make_call_node(g, from);
        let b = make_call_node(g, to);
        g.graph.add_edge(a, b, CallEdge {
            confidence: 0.95,
            resolution_method: ResolutionMethod::StackGraph,
            call_site_span: ByteSpan { start: 0, end: 1 },
            args_span: None,
        });
    }

    /// Helper: index a source string with language auto-detection from extension.
    fn index_source(graph: &CodeGraph, source: &str, file_path: &str) {
        let lang = Language::from_extension(
            std::path::Path::new(file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("py")
        );
        graph.index_file(source, file_path, &lang).unwrap();
    }

// ── Phase 1: traverse_bfs (generalized Rust traversal) ──────────────

    /// Build a fresh projection with the Phase-D passes run, from sources.
    fn snapshot_from(sources: &[(&str, &str)]) -> ProjectedGraph {
        let graph = CodeGraph::new(GraphConfig::default());
        for (src, path) in sources {
            let lang = Language::from_extension(
                std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("py"),
            );
            graph.index_file(src, path, &lang).unwrap();
        }
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_all_calls(&mut projection);
        projection
    }

    fn fn_id_of(proj: &ProjectedGraph, name: &str) -> String {
        proj.functions.iter().find(|(_, f)| f.name == name).map(|(id, _)| id.clone())
            .unwrap_or_else(|| panic!("function `{}` should be indexed", name))
    }


    /// Helper: create a CodeGraph with a real Macrame store in a temp dir.
    fn graph_with_temp_store() -> (CodeGraph, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let store = crate::storage::CodeGraphStore::open(&db_path).expect("open store");
        let graph = CodeGraph::new(GraphConfig::default()).with_store(store);
        (graph, dir)
    }
