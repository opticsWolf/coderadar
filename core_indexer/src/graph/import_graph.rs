use std::collections::BTreeSet;
use std::path::PathBuf;

use dashmap::DashMap;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};

use crate::types::*;

// ── Import Graph (§3.4a) — EntityId-based ───────────────────────────────────

#[derive(Clone, Debug)]
pub struct ImportNode {
    pub path: PathBuf,
    pub module_id: Option<EntityId>,
    pub language: Language,
}

pub struct ImportGraph {
    graph: StableDiGraph<ImportNode, ()>,
    path_to_node: DashMap<String, NodeIndex>,
    node_to_path: DashMap<NodeIndex, String>,
    exports: DashMap<String, Vec<Export>>,
}

impl ImportGraph {
    pub fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            path_to_node: DashMap::new(),
            node_to_path: DashMap::new(),
            exports: DashMap::new(),
        }
    }

    pub fn remove_file(&mut self, file_path: &str) {
        if let Some(node) = self.path_to_node.remove(file_path) {
            let (_, old) = node;
            self.node_to_path.remove(&old);
            self.graph.remove_node(old);
        }
    }

    pub fn transitive_imports(&self, file_path: &str, max_depth: usize) -> Vec<ImportNode> {
        let path_str = file_path.to_string();
        let start = match self.path_to_node.get(&path_str) {
            Some(n) => *n,
            None => return vec![],
        };

        let mut visited = BTreeSet::new();
        let mut queue = vec![(start, 0usize)];
        let mut result = Vec::new();

        while let Some((node, depth)) = queue.pop() {
            if depth > max_depth || !visited.insert(node.index()) {
                continue;
            }
            if let Some(import_node) = self.graph.node_weight(node) {
                result.push(import_node.clone());
            }
            for neighbor in self.graph.neighbors_directed(node, petgraph::Outgoing) {
                queue.push((neighbor, depth + 1));
            }
        }
        result
    }

    pub fn add_file(
        &mut self,
        file_path: &str,
        module_id: Option<EntityId>,
        language: Language,
    ) -> NodeIndex {
        // v0.5: Return existing node if already registered (prevents
        // duplicate nodes with missing outgoing edges in multi-hop chains).
        if let Some(existing) = self.path_to_node.get(file_path) {
            return *existing;
        }
        let node = ImportNode {
            path: PathBuf::from(file_path),
            module_id,
            language,
        };
        let idx = self.graph.add_node(node);
        self.path_to_node.insert(file_path.to_string(), idx);
        self.node_to_path.insert(idx, file_path.to_string());
        idx
    }

    /// Add all import edges for a file's extracted imports. Thread-safe:
    /// takes the import_graph lock internally. Called from parallel workers.
    pub fn build_import_edges(
        import_graph: &parking_lot::RwLock<Self>,
        units: &[ExtractedUnit],
        file_path: &str,
        language: Language,
        module_id: &str,
    ) {
        use crate::types::ImportKind;

        let mut ig = import_graph.write();
        ig.add_file(file_path, Some(module_id.to_string()), language);

        for unit in units {
            if let ExtractedUnit::Import(i) = unit {
                let src_mod: Option<String> = match &i.kind {
                    ImportKind::FromImport { module, .. }
                    | ImportKind::ModuleImport { module, .. }
                    | ImportKind::StarImport { module, .. } => Some(module.clone()),
                    ImportKind::RelativeImport { module, .. } => module.clone(),
                    _ => None,
                };
                if let Some(src_mod) = src_mod {
                    // Use a simplified path: assume same-directory modules.
                    // The full find_module_by_dotted_name requires the projection
                    // which isn't available in parallel. The import graph edges
                    // will be refined during the sequential insert phase.
                    let target_path = format!("{}/{}.py",
                        std::path::Path::new(file_path).parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        src_mod.replace('.', "/"));
                    ig.add_file(&target_path, None, language);
                    ig.add_import_edge(file_path, &target_path);
                }
            }
        }
    }

    pub fn add_import_edge(&mut self, importer: &str, imported: &str) {
        let from = self.path_to_node.get(importer);
        let to = self.path_to_node.get(imported);
        if let (Some(f), Some(t)) = (from, to) {
            self.graph.add_edge(*f, *t, ());
        }
    }

    pub fn get_exports(
        &self,
        path: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, Vec<Export>>> {
        self.exports.get(path)
    }
}
