// What changed in one file between two projections — the data behind
// `UpdateReport.changed_symbols` / `new_unresolved_references` /
// `newly_resolved_references`. Pure functions over two snapshots, so the
// ingest path stays untouched and the diff is testable on its own.

use std::collections::BTreeSet;

use super::module_resolution::normalize_path_str;
use super::CodeGraph;
use crate::types::*;

/// Everything the update report needs, computed from the projections taken
/// before and after the file was re-ingested.
#[derive(Default)]
pub struct FileDiff {
    pub symbols: Vec<SymbolChange>,
    pub new_unresolved: Vec<(EntityId, String)>,
    pub newly_resolved: Vec<(EntityId, String)>,
}

/// An entity belongs to `file` when its id is `<file>::<name>`. The `::` is
/// part of the match: `a.py` must not claim the entities of `a.pyi`.
fn in_file(id: &str, file_prefix: &str) -> bool {
    normalize_path_str(id).starts_with(file_prefix)
}

fn file_prefix(file_path: &str) -> String {
    format!("{}::", normalize_path_str(file_path))
}

pub fn diff_file(old: &ProjectedGraph, new: &ProjectedGraph, file_path: &str) -> FileDiff {
    let prefix = file_prefix(file_path);
    let mut out = FileDiff::default();

    // Functions: keyed by normalized id so OS separators cannot fake a change.
    let funcs =
        |g: &ProjectedGraph| -> std::collections::BTreeMap<String, (EntityId, usize, u64, u64)> {
            g.functions
                .iter()
                .filter(|(id, _)| in_file(id, &prefix))
                .map(|(id, f)| {
                    (
                        normalize_path_str(id),
                        (id.clone(), f.line, f.signature_hash, f.body_hash),
                    )
                })
                .collect()
        };
    let (old_f, new_f) = (funcs(old), funcs(new));
    for (key, (id, line, sig, body)) in &new_f {
        let name = new
            .functions
            .get(id)
            .map(|f| f.name.clone())
            .unwrap_or_default();
        match old_f.get(key) {
            None => out.symbols.push(SymbolChange {
                kind: "function",
                operation: "added",
                id: id.clone(),
                name,
                line: *line,
            }),
            Some((_, _, old_sig, _)) if old_sig != sig => out.symbols.push(SymbolChange {
                kind: "function",
                operation: "signature_changed",
                id: id.clone(),
                name,
                line: *line,
            }),
            Some((_, _, _, old_body)) if old_body != body => out.symbols.push(SymbolChange {
                kind: "function",
                operation: "body_changed",
                id: id.clone(),
                name,
                line: *line,
            }),
            Some(_) => {}
        }
    }
    for (key, (id, line, _, _)) in &old_f {
        if !new_f.contains_key(key) {
            let name = old
                .functions
                .get(id)
                .map(|f| f.name.clone())
                .unwrap_or_default();
            out.symbols.push(SymbolChange {
                kind: "function",
                operation: "removed",
                id: id.clone(),
                name,
                line: *line,
            });
        }
    }

    // Classes carry one content hash (no separate signature).
    let classes =
        |g: &ProjectedGraph| -> std::collections::BTreeMap<String, (EntityId, usize, u64)> {
            g.classes
                .iter()
                .filter(|(id, _)| in_file(id, &prefix))
                .map(|(id, c)| (normalize_path_str(id), (id.clone(), c.line, c.content_hash)))
                .collect()
        };
    let (old_c, new_c) = (classes(old), classes(new));
    for (key, (id, line, hash)) in &new_c {
        let name = new
            .classes
            .get(id)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let operation = match old_c.get(key) {
            None => "added",
            Some((_, _, old_hash)) if old_hash != hash => "body_changed",
            Some(_) => continue,
        };
        out.symbols.push(SymbolChange {
            kind: "class",
            operation,
            id: id.clone(),
            name,
            line: *line,
        });
    }
    for (key, (id, line, _)) in &old_c {
        if !new_c.contains_key(key) {
            let name = old
                .classes
                .get(id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            out.symbols.push(SymbolChange {
                kind: "class",
                operation: "removed",
                id: id.clone(),
                name,
                line: *line,
            });
        }
    }

    // Unresolved targets, per function of this file.
    let targets = |g: &ProjectedGraph| -> BTreeSet<(String, String)> {
        g.functions
            .keys()
            .filter(|id| in_file(id, &prefix))
            .flat_map(|id| {
                CodeGraph::list_unresolved_targets(g, id)
                    .into_iter()
                    .map(|t| (id.clone(), t))
            })
            .collect()
    };
    let (old_t, new_t) = (targets(old), targets(new));
    out.new_unresolved = new_t.difference(&old_t).cloned().collect();
    out.newly_resolved = old_t.difference(&new_t).cloned().collect();

    out.symbols
        .sort_by(|a, b| (a.line, &a.id).cmp(&(b.line, &b.id)));
    out
}
