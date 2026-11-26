//! Real-data visualizers for the CLI. No fabricated diagrams: an empty
//! graph or an unknown entity is an explicit `NothingToVisualize` error.

use std::collections::{BTreeMap, BTreeSet};

use core_indexer::types::ProjectedGraph;

fn safe_id(name: &str) -> String {
    format!(
        "n_{}",
        name.chars()
            .map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            })
            .collect::<String>()
    )
}

fn short_name(qualified: &str) -> String {
    let Some((head, tail)) = qualified.rsplit_once("::") else {
        return qualified
            .rsplit('.')
            .next()
            .unwrap_or(qualified)
            .to_string();
    };
    if tail == "module" {
        head.replace('\\', "/")
            .rsplit('/')
            .next()
            .unwrap_or(head)
            .rsplit('.')
            .next()
            .unwrap_or(head)
            .to_string()
    } else {
        tail.to_string()
    }
}

fn truncated(qualified: &str) -> String {
    let name = qualified
        .rsplit_once("::")
        .map_or(qualified, |(_, tail)| tail);
    if name.chars().count() > 40 {
        format!("{}...", name.chars().take(37).collect::<String>())
    } else {
        name.to_string()
    }
}

fn class_edges(snap: &ProjectedGraph) -> (Vec<(String, String)>, BTreeMap<String, String>) {
    let mut by_name = BTreeMap::new();
    for class in snap.classes.values() {
        by_name.insert(class.name.clone(), class.id.clone());
    }
    let mut edges = Vec::new();
    for class in snap.classes.values() {
        for base in &class.bases {
            if let Some(base_id) = by_name.get(&base.name)
                && base_id != &class.id
            {
                edges.push((base_id.clone(), class.id.clone()));
            }
        }
    }
    (edges, by_name)
}

fn module_edges(snap: &ProjectedGraph) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for (src, targets) in &snap.imports_by_importer {
        for target in targets {
            edges.push((src.clone(), target.clone()));
        }
    }
    edges.sort();
    edges
}

fn resolve_entity(snap: &ProjectedGraph, name: &str) -> Option<String> {
    if name.contains("::") && snap.functions.contains_key(name) {
        return Some(name.to_string());
    }
    let hits = core_indexer::api::search(snap, name, 1, None).unwrap_or_default();
    hits.first()
        .and_then(|row| row.get("id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
}

fn call_edges(snap: &ProjectedGraph, args: &[String]) -> Vec<(String, String)> {
    let Some(name) = args.first() else {
        return Vec::new();
    };
    let Some(root) = resolve_entity(snap, name) else {
        return Vec::new();
    };
    let direction = args.get(1).map(String::as_str).unwrap_or("out");
    let depth = args
        .get(2)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);
    let mut seen = BTreeSet::new();
    let mut edges = Vec::new();
    let mut frontier = vec![(root, depth)];
    while let Some((id, remaining)) = frontier.pop() {
        if remaining == 0 || !seen.insert(id.clone()) {
            continue;
        }
        if (direction == "out" || direction == "both")
            && let Some(targets) = snap.callees_by_caller.get(&id)
        {
            for target in targets {
                edges.push((id.clone(), target.clone()));
                frontier.push((target.clone(), remaining - 1));
            }
        }
        if (direction == "in" || direction == "both")
            && let Some(sources) = snap.callers_by_callee.get(&id)
        {
            for source in sources {
                edges.push((source.clone(), id.clone()));
                frontier.push((source.clone(), remaining - 1));
            }
        }
    }
    edges.sort();
    edges
}

/// Render one visualization with real graph data.
pub fn render(
    viz_type: &str,
    format: &str,
    args: &[String],
    snap: &ProjectedGraph,
) -> Result<String, String> {
    let dot = matches!(format, "graphviz" | "dot");
    match viz_type {
        "hierarchy" => {
            let (edges, names) = class_edges(snap);
            if snap.classes.is_empty() {
                return Err(
                    "No classes in the index. Run `coderadar init` or `coderadar reindex` in the project first.".to_string(),
                );
            }
            let mut lines = if dot {
                vec![
                    "digraph Hierarchy {".to_string(),
                    "    rankdir=BT;".to_string(),
                    "    node [shape=record, fontname=\"Helvetica\", style=rounded];".to_string(),
                    "    edge [arrowhead=empty];".to_string(),
                ]
            } else {
                vec!["classDiagram".to_string()]
            };
            let mut ids: BTreeSet<String> = BTreeSet::new();
            for class in snap.classes.values() {
                ids.insert(class.id.clone());
            }
            for id in &ids {
                let name = names
                    .iter()
                    .find(|(_, class_id)| *class_id == id)
                    .map(|(name, _)| name.as_str())
                    .unwrap_or_else(|| id.as_str());
                if dot {
                    lines.push(format!("    {} [label=\"{{{}}}\"];", safe_id(id), name));
                } else {
                    lines.push(format!("    class {} {{", safe_id(id)));
                    lines.push(format!("        +{}", truncated(name)));
                    lines.push("    }".to_string());
                }
            }
            for (base, child) in edges {
                if dot {
                    lines.push(format!("    {} -> {};", safe_id(&base), safe_id(&child)));
                } else {
                    lines.push(format!("    {} <|-- {}", safe_id(&base), safe_id(&child)));
                }
            }
            if dot {
                lines.push("}".to_string());
            }
            Ok(lines.join("\n"))
        }
        "dependencies" => {
            let edges = module_edges(snap);
            if edges.is_empty() {
                return Err(
                    "No module dependencies in the index. Run `coderadar init` or `coderadar reindex` in the project first.".to_string(),
                );
            }
            let mut nodes = BTreeSet::new();
            for (src, dst) in &edges {
                nodes.insert(src.clone());
                nodes.insert(dst.clone());
            }
            let mut lines = vec![if dot {
                "digraph Dependencies {".to_string()
            } else {
                "flowchart LR".to_string()
            }];
            for id in &nodes {
                let label = snap
                    .modules
                    .get(id)
                    .map(|module| module.name.clone())
                    .unwrap_or_else(|| short_name(id));
                if dot {
                    lines.push(format!("    {} [label=\"{}\"];", safe_id(id), label));
                } else {
                    lines.push(format!("    {}[\"{}\"]", safe_id(id), truncated(&label)));
                }
            }
            for (src, dst) in edges {
                if dot {
                    lines.push(format!("    {} -> {};", safe_id(&src), safe_id(&dst)));
                } else {
                    lines.push(format!("    {} --> {}", safe_id(&src), safe_id(&dst)));
                }
            }
            if dot {
                lines.push("}".to_string());
            }
            Ok(lines.join("\n"))
        }
        "call-graph" => {
            let edges = call_edges(snap, args);
            if edges.is_empty() {
                return Err(
                    "No call edges found for that entity. Either the index is empty (run `coderadar init` or `coderadar reindex` in the project first) or nothing indexed calls it and it calls nothing indexed.".to_string(),
                );
            }
            let mut lines = vec![if dot {
                "digraph CallGraph {".to_string()
            } else {
                "flowchart TD".to_string()
            }];
            for (src, dst) in edges {
                if dot {
                    lines.push(format!("    {} -> {};", safe_id(&src), safe_id(&dst)));
                } else {
                    lines.push(format!("    {} --> {}", safe_id(&src), safe_id(&dst)));
                }
            }
            if dot {
                lines.push("}".to_string());
            }
            Ok(lines.join("\n"))
        }
        _ => Err(format!("Unknown visualization type: {viz_type}")),
    }
}
