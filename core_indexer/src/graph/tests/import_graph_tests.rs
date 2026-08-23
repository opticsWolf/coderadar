// ImportGraph resolution tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;
use std::path::PathBuf;

    #[test]
    fn test_import_graph_add_and_find() {
        let mut g = ImportGraph::new();
        g.add_file("src/main.py", None, Language::Python);
        let imports = g.transitive_imports("src/main.py", 3);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].path, PathBuf::from("src/main.py"));
    }
    #[test]
    fn test_import_graph_remove_file() {
        let mut g = ImportGraph::new();
        g.add_file("a.py", None, Language::Python);
        g.add_file("b.py", None, Language::Python);
        g.remove_file("a.py");
        let imports = g.transitive_imports("b.py", 1);
        assert_eq!(imports.len(), 1);
    }
    #[test]
    fn test_import_graph_transitive() {
        let mut g = ImportGraph::new();
        g.add_file("a.py", None, Language::Python);
        g.add_file("b.py", None, Language::Python);
        g.add_file("c.py", None, Language::Python);
        g.add_import_edge("a.py", "b.py");
        g.add_import_edge("b.py", "c.py");
        let depth2 = g.transitive_imports("a.py", 2);
        assert!(depth2.len() >= 2);
    }
    #[test]
    fn test_multi_hop_import_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());

        index_source(&graph, "def utility(): pass\n", "src/c.py");
        index_source(&graph, "from src.c import utility\ndef helper(): utility()\n", "src/b.py");
        index_source(&graph, "from src.b import helper\ndef app(): helper()\n", "src/a.py");

        let ig = graph.import_graph.read();

        eprintln!("From A (3): {:?}", ig.transitive_imports("src/a.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect::<Vec<_>>());
        eprintln!("From B (3): {:?}", ig.transitive_imports("src/b.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect::<Vec<_>>());
        eprintln!("From C (1): {:?}", ig.transitive_imports("src/c.py", 1).iter().map(|n| n.path.to_string_lossy().to_string()).collect::<Vec<_>>());

        let from_b: Vec<_> = ig.transitive_imports("src/b.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect();
        assert!(from_b.iter().any(|p| p.contains("c.py")), "B→C: {:?}", from_b);

        let from_a: Vec<_> = ig.transitive_imports("src/a.py", 3).iter().map(|n| n.path.to_string_lossy().to_string()).collect();
        assert!(from_a.iter().any(|p| p.contains("c.py")), "A→B→C: {:?}", from_a);
    }
    #[test]
    fn test_star_exports_wildcard_import() {
        let graph = CodeGraph::new(GraphConfig::default());

        index_source(&graph, "__all__ = ['public_api', 'internal_helper']\n\ndef public_api(): pass\ndef private_impl(): pass\ndef internal_helper(): pass\n", "src/lib.py");
        graph.set_module_star_exports("src/lib.py::module",
            vec!["public_api".to_string(), "internal_helper".to_string()]);

        index_source(&graph, "from src.lib import *\ndef consumer(): public_api()\n", "src/consumer.py");

        // v0.5: Manually resolve calls — resolve_all_calls is normally called
        // from update_file, not index_file. In production, calls are resolved
        // after all files are indexed (batch mode).
        {
            let mut projection = (*graph.snapshot()).clone();
            graph.compute_all_mro(&mut projection);
            graph.resolve_all_calls(&mut projection);
            graph.commit_projection(projection);
        }

        let snap = graph.snapshot();

        // Debug: check import graph edges
        let ig = graph.import_graph.read();
        let trans = ig.transitive_imports("src/consumer.py", 3);
        assert!(trans.iter().any(|n| n.path.to_string_lossy().to_string().contains("lib.py")),
            "consumer should transitively reach lib.py");

        for (_fid, func) in &snap.functions {
            if func.name == "consumer" {
                let resolved: Vec<_> = func.resolved_calls.iter()
                    .filter_map(|rc| match rc {
                        crate::types::ResolvedCall::Function(f) => Some(f.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(resolved.len(), 1,
                    "Expected 1 resolved, got {:?}", resolved);
            }
        }
    }
    #[test]
    fn test_extension_agnostic_module_resolution() {
        // v0.5: find_module_by_dotted_name handles all extensions.
        // Java: com.foo.bar.Baz → com/foo/bar/Baz.java
        // Scala: com.foo.bar.Qux → com/foo/bar/Qux.scala
        // Zig: foo.bar → foo/bar.zig (dotted name uses '.' as separator)
        let graph = CodeGraph::new(GraphConfig::default());

        // Java package resolution
        index_source(&graph, "package com.foo.bar;\npublic class Baz { public void run() {} }\n", "com/foo/bar/Baz.java");
        index_source(&graph, "import com.foo.bar.Baz;\nclass User { void go() { new Baz().run(); } }\n", "User.java");

        // Scala resolution
        index_source(&graph, "package com.foo.bar\nclass Qux { def doit(): Unit = () }\n", "com/foo/bar/Qux.scala");

        // Elixir resolution
        index_source(&graph, "defmodule Mix.Tasks.Hello do\nend\n", "lib/mix/tasks/hello.ex");

        // The key test: find_module_by_dotted_name should find any extension.
        let snap = graph.snapshot();
        let found = super::find_module_by_dotted_name(&snap, "com.foo.bar.Baz", "");
        assert!(found.is_some(), "Should find Baz.java via com.foo.bar.Baz");

        let found_scala = super::find_module_by_dotted_name(&snap, "com.foo.bar.Qux", "");
        assert!(found_scala.is_some(), "Should find Qux.scala via com.foo.bar.Qux");

        // Note: Elixir uses PascalCase module names but lowercase filenames
        // (e.g., Mix.Tasks.Hello → lib/mix/tasks/hello.ex).
        // Case-insensitive matching is a future enhancement.
    }
    #[test]
    fn test_import_graph_nonexistent() {
        let g = ImportGraph::new();
        assert!(g.transitive_imports("nope.py", 3).is_empty());
    }
    #[test]
    fn test_alias_aware_module_resolution() {
        // `@/models/user` should resolve to `src/models/user` (2.2 alias).
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class User {}\n", "src/models/user.ts");
        let projection = (*graph.snapshot()).clone();
        let target = crate::graph::find_module_by_dotted_name(&projection, "@/models/user", "");
        assert!(target.is_some(), "@/models/user should resolve via alias");
        let m = projection.modules.get(&target.unwrap()).unwrap();
        assert!(m.path.to_string_lossy().ends_with("src/models/user.ts"),
            "alias should resolve to src/models/user.ts, got {}", m.path.to_string_lossy());
    }
