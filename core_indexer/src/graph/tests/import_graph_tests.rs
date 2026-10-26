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
    index_source(
        &graph,
        "from src.c import utility\ndef helper(): utility()\n",
        "src/b.py",
    );
    index_source(
        &graph,
        "from src.b import helper\ndef app(): helper()\n",
        "src/a.py",
    );

    let ig = graph.import_graph.read();

    eprintln!(
        "From A (3): {:?}",
        ig.transitive_imports("src/a.py", 3)
            .iter()
            .map(|n| n.path.to_string_lossy().to_string())
            .collect::<Vec<_>>()
    );
    eprintln!(
        "From B (3): {:?}",
        ig.transitive_imports("src/b.py", 3)
            .iter()
            .map(|n| n.path.to_string_lossy().to_string())
            .collect::<Vec<_>>()
    );
    eprintln!(
        "From C (1): {:?}",
        ig.transitive_imports("src/c.py", 1)
            .iter()
            .map(|n| n.path.to_string_lossy().to_string())
            .collect::<Vec<_>>()
    );

    let from_b: Vec<_> = ig
        .transitive_imports("src/b.py", 3)
        .iter()
        .map(|n| n.path.to_string_lossy().to_string())
        .collect();
    assert!(
        from_b.iter().any(|p| p.contains("c.py")),
        "B→C: {:?}",
        from_b
    );

    let from_a: Vec<_> = ig
        .transitive_imports("src/a.py", 3)
        .iter()
        .map(|n| n.path.to_string_lossy().to_string())
        .collect();
    assert!(
        from_a.iter().any(|p| p.contains("c.py")),
        "A→B→C: {:?}",
        from_a
    );
}
#[test]
fn test_star_exports_wildcard_import() {
    let graph = CodeGraph::new(GraphConfig::default());

    index_source(&graph, "__all__ = ['public_api', 'internal_helper']\n\ndef public_api(): pass\ndef private_impl(): pass\ndef internal_helper(): pass\n", "src/lib.py");
    graph.set_module_star_exports(
        "src/lib.py::module",
        vec!["public_api".to_string(), "internal_helper".to_string()],
    );

    index_source(
        &graph,
        "from src.lib import *\ndef consumer(): public_api()\n",
        "src/consumer.py",
    );

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
    assert!(
        trans
            .iter()
            .any(|n| n.path.to_string_lossy().to_string().contains("lib.py")),
        "consumer should transitively reach lib.py"
    );

    for func in snap.functions.values() {
        if func.name == "consumer" {
            let resolved: Vec<_> = func
                .resolved_calls
                .iter()
                .filter_map(|rc| match rc {
                    crate::types::ResolvedCall::Function(f) => Some(f.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(resolved.len(), 1, "Expected 1 resolved, got {:?}", resolved);
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
    index_source(
        &graph,
        "package com.foo.bar;\npublic class Baz { public void run() {} }\n",
        "com/foo/bar/Baz.java",
    );
    index_source(
        &graph,
        "import com.foo.bar.Baz;\nclass User { void go() { new Baz().run(); } }\n",
        "User.java",
    );

    // Scala resolution
    index_source(
        &graph,
        "package com.foo.bar\nclass Qux { def doit(): Unit = () }\n",
        "com/foo/bar/Qux.scala",
    );

    // Elixir resolution
    index_source(
        &graph,
        "defmodule Mix.Tasks.Hello do\nend\n",
        "lib/mix/tasks/hello.ex",
    );

    // The key test: find_module_by_dotted_name should find any extension.
    let snap = graph.snapshot();
    let found = super::find_module_by_dotted_name(&snap, "com.foo.bar.Baz", "");
    assert!(found.is_some(), "Should find Baz.java via com.foo.bar.Baz");

    let found_scala = super::find_module_by_dotted_name(&snap, "com.foo.bar.Qux", "");
    assert!(
        found_scala.is_some(),
        "Should find Qux.scala via com.foo.bar.Qux"
    );

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
    assert!(
        m.path.to_string_lossy().ends_with("src/models/user.ts"),
        "alias should resolve to src/models/user.ts, got {}",
        m.path.to_string_lossy()
    );
}

fn projection_with_maps(
    files: &[(&str, &str)],
    maps: crate::graph::path_maps::PathMaps,
    build_index: bool,
) -> ProjectedGraph {
    let graph = CodeGraph::new(GraphConfig::default());
    for (src, path) in files {
        index_source(&graph, src, path);
    }
    let mut projection = (*graph.snapshot()).clone();
    projection.path_maps = maps;
    if build_index {
        crate::graph::rebuild_module_path_index(&mut projection);
    }
    projection
}

fn ts_alias_maps() -> crate::graph::path_maps::PathMaps {
    crate::graph::path_maps::PathMaps {
        ts_aliases: vec![crate::graph::path_maps::TsAlias {
            from: "@utils/".to_string(),
            exact: false,
            to: vec!["lib/utils/*".to_string()],
        }],
        ts_base_url: None,
        python_roots: vec![],
    }
}

#[test]
fn test_pathmap_alias_resolves_unresolvable() {
    // §2.5: `@utils/a` has no dots, so suffix matching never fires —
    // without maps it resolves to nothing (the gap). With the alias
    // it lands on the mapped target — on both the indexed fast path
    // and the legacy scan.
    let files: &[(&str, &str)] = &[
        ("export const a = 1;\n", "aaa/utils/a.ts"),
        ("export const a = 2;\n", "lib/utils/a.ts"),
        ("import { a } from '@utils/a';\n", "app.ts"),
    ];
    for build_index in [true, false] {
        let plain =
            projection_with_maps(files, crate::graph::path_maps::PathMaps::new(), build_index);
        let target = crate::graph::find_module_by_dotted_name(&plain, "@utils/a", "app.ts::module");
        assert!(
            target.is_none(),
            "no maps: @-prefixed specifier is unresolvable (index={build_index})"
        );
        let mapped = projection_with_maps(files, ts_alias_maps(), build_index);
        let target =
            crate::graph::find_module_by_dotted_name(&mapped, "@utils/a", "app.ts::module");
        assert_eq!(
            target.as_deref(),
            Some("lib/utils/a.ts::module"),
            "alias maps to lib (index={build_index})"
        );
    }
}

#[test]
fn test_pathmap_alias_target_order_and_fallback() {
    // Targets are tried in tsconfig order (missing first target falls
    // through); an unmapped specifier keeps the old suffix behaviour.
    let files: &[(&str, &str)] = &[("export const a = 2;\n", "lib/utils/a.ts")];
    let mut maps = ts_alias_maps();
    maps.ts_aliases[0].to = vec!["missing/*".to_string(), "lib/utils/*".to_string()];
    let projection = projection_with_maps(files, maps, true);
    let target =
        crate::graph::find_module_by_dotted_name(&projection, "@utils/a", "app.ts::module");
    assert_eq!(target.as_deref(), Some("lib/utils/a.ts::module"));
    // Unmapped specifier: suffix fallback still works.
    let target =
        crate::graph::find_module_by_dotted_name(&projection, "lib.utils.a", "app.ts::module");
    assert_eq!(target.as_deref(), Some("lib/utils/a.ts::module"));
}

#[test]
fn test_pathmap_base_url_anchors_ts() {
    // §2.5: baseUrl=src anchors `u/b` under src/ for TS importers;
    // without it the smallest id (`lib/...`) wins the suffix match.
    let files: &[(&str, &str)] = &[
        ("export const b = 0;\n", "lib/u/b.ts"),
        ("export const b = 1;\n", "src/u/b.ts"),
        ("import { b } from 'u/b';\n", "app.ts"),
    ];
    let plain = projection_with_maps(files, crate::graph::path_maps::PathMaps::new(), true);
    assert_eq!(
        crate::graph::find_module_by_dotted_name(&plain, "u/b", "app.ts::module").as_deref(),
        Some("lib/u/b.ts::module")
    );
    let maps = crate::graph::path_maps::PathMaps {
        ts_aliases: vec![],
        ts_base_url: Some("src".to_string()),
        python_roots: vec![],
    };
    let anchored = projection_with_maps(files, maps, true);
    assert_eq!(
        crate::graph::find_module_by_dotted_name(&anchored, "u/b", "app.ts::module").as_deref(),
        Some("src/u/b.ts::module")
    );
}

#[test]
fn test_pathmap_python_src_root_wins() {
    // §2.5: with src-layout declared, `mylib.x` anchors exactly under
    // src/ instead of losing to the id-ordered suffix match.
    let files: &[(&str, &str)] = &[
        ("X = 1\n", "aaa/mylib/x.py"),
        ("X = 2\n", "src/mylib/x.py"),
        ("import mylib.x\n", "app.py"),
    ];
    let plain = projection_with_maps(files, crate::graph::path_maps::PathMaps::new(), true);
    assert_eq!(
        crate::graph::find_module_by_dotted_name(&plain, "mylib.x", "app.py::module").as_deref(),
        Some("aaa/mylib/x.py::module")
    );
    let maps = crate::graph::path_maps::PathMaps {
        ts_aliases: vec![],
        ts_base_url: None,
        python_roots: vec!["src".to_string()],
    };
    let rooted = projection_with_maps(files, maps, true);
    assert_eq!(
        crate::graph::find_module_by_dotted_name(&rooted, "mylib.x", "app.py::module").as_deref(),
        Some("src/mylib/x.py::module")
    );
}

#[test]
fn test_pathmap_alias_end_to_end_import_edge() {
    // §2.5: the mapped specifier becomes a real import edge —
    // extraction → alias rewrite → edge, not just unit resolution.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "export const a = 1;\n", "aaa/utils/a.ts");
    index_source(&graph, "export const a = 2;\n", "lib/utils/a.ts");
    index_source(
        &graph,
        "import { a } from '@utils/a';\nconsole.log(a);\n",
        "app.ts",
    );
    let mut projection = (*graph.snapshot()).clone();
    projection.path_maps = ts_alias_maps();
    crate::graph::rebuild_module_path_index(&mut projection);
    graph.resolve_imports(&mut projection);
    let targets = projection.imports_by_importer.get("app.ts::module");
    let got: Option<Vec<&str>> =
        targets.map(|t| t.iter().map(|s| s.as_str()).collect());
    assert_eq!(
        got,
        Some(vec!["lib/utils/a.ts::module"]),
        "app.ts must import the aliased target"
    );
}
