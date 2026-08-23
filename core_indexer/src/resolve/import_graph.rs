// CodeRadar v3.6 — Resolution: Import Graph + Scope Layer 2 (§6.3)
// Walk the import graph BFS to collect modules exporting a reference name.

use crate::graph::ImportGraph;
use crate::types::{EntityId, ExportSource};

/// A candidate match found via import-graph traversal.
#[derive(Clone, Debug)]
pub struct ImportMatch {
    pub module_path: String,
    pub module_id: Option<EntityId>,
    pub export_name: String,
    pub source: ExportSource,
}

/// Resolve a name via the import graph (Layer 2).
///
/// Walks the import graph BFS up to `max_import_depth`, collecting modules
/// that export the given name. Results are cached per (file_path, depth)
/// since transitive imports are deterministic and BFS is O(V+E).
///
/// Technique: per-file transitive-import cache adopted from CodeGraph's
/// `reachableModules` cache in resolution/index.ts. MIT license.
/// https://github.com/opticsWolf/codegraph
pub fn resolve_in_imports(
    graph: &ImportGraph,
    file_path: &str,
    name: &str,
    max_import_depth: usize,
    _include_same_package: bool,
    transitives_cache: &mut std::collections::HashMap<String, Vec<crate::graph::ImportNode>>,
) -> Option<(Vec<ImportMatch>, f32)> {
    let cache_key = format!("{}|{}", file_path, max_import_depth);
    let candidates = transitives_cache
        .entry(cache_key)
        .or_insert_with(|| graph.transitive_imports(file_path, max_import_depth));

    let mut matches: Vec<ImportMatch> = Vec::new();

    for import_node in candidates.iter() {
        let path_key = import_node.path.to_string_lossy().to_string();
        if let Some(exports) = graph.get_exports(&path_key) {
            for export in exports.iter() {
                if export.name == name {
                    matches.push(ImportMatch {
                        module_path: import_node.path.to_string_lossy().to_string(),
                        module_id: import_node.module_id.clone(),
                        export_name: export.name.clone(),
                        source: export.source.clone(),
                    });
                }
            }
        }
    }

    if matches.is_empty() {
        return None;
    }

    let confidence = if matches.len() == 1 {
        0.89
    } else {
        0.80
    };

    Some((matches, confidence))
}

/// Rank candidates by proximity: same package > same directory > deeper import.
pub fn rank_candidates(matches: &mut [ImportMatch], _query_file: &str) {
    matches.sort_by(|a, b| {
        // Prefer candidates in the same top-level package
        let a_pkg = a.module_path.split('/').next().unwrap_or("");
        let b_pkg = b.module_path.split('/').next().unwrap_or("");
        let pkg_cmp = a_pkg.cmp(b_pkg);

        // Then compare depth in directory hierarchy
        let a_depth = a.module_path.split('/').count();
        let b_depth = b.module_path.split('/').count();
        let depth_cmp = a_depth.cmp(&b_depth);

        pkg_cmp.then(depth_cmp)
    });
}
