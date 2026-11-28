//! Unit 2: annotations, comments and their histories, straight onto the
//! `core_indexer::annotations` API (no Python bridge).

use serde_json::{Value, json};

use core_indexer::annotations as ann;

fn err(e: ann::AnnError) -> String {
    match e {
        ann::AnnError::Lint(fault) => format!("InvalidRequest: {fault}"),
        ann::AnnError::InvalidAbout(msg)
        | ann::AnnError::InvalidId(msg)
        | ann::AnnError::Import(msg) => format!("InvalidRequest: {msg}"),
        ann::AnnError::NotFound(msg) => format!("NotFound: {msg}"),
        ann::AnnError::WrongKind { id, kind } => {
            format!("NotFound: row {id} is a {kind}, not the requested kind")
        }
        ann::AnnError::AlreadyRetired(msg) => format!("NotFound: {msg}"),
        ann::AnnError::Store(msg) => format!("EngineError: {msg}"),
    }
}

fn require_inputs(about: &str, body: &str, noun: &str) -> Result<(), String> {
    if about.trim().is_empty() {
        return Err(format!(
            "InvalidRequest: Please provide a target concept id to {noun}."
        ));
    }
    if body.trim().is_empty() {
        return Err("InvalidRequest: The annotation body must be markdown text.".to_string());
    }
    Ok(())
}

fn json_of<T: serde::Serialize>(value: T, op: &str) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| format!("EngineError: {op} returned bad JSON: {e}"))
}

/// Mint an enforced-list annotation on a concept.
pub fn annotate(about: &str, body: &str) -> Result<Value, String> {
    require_inputs(about, body, "annotate")?;
    let store = crate::state::store()?;
    ann::create(&store, ann::ANNOTATION_KIND, about, body)
        .map_err(err)
        .and_then(|c| json_of(c, "annotate"))
}

/// Mint a freeform comment on a concept.
pub fn comment(about: &str, body: &str) -> Result<Value, String> {
    require_inputs(about, body, "comment on")?;
    let store = crate::state::store()?;
    ann::create(&store, ann::COMMENT_KIND, about, body)
        .map_err(err)
        .and_then(|c| json_of(c, "comment"))
}

/// Supersede one annotation/comment row.
pub fn revise_annotation(id: &str, body: &str) -> Result<Value, String> {
    let store = crate::state::store()?;
    ann::revise(&store, id, body)
        .map_err(err)
        .and_then(|r| json_of(r, "revise_annotation"))
}

/// Soft-retire one row, keeping its audit trail.
pub fn retire_annotation(id: &str) -> Result<Value, String> {
    let store = crate::state::store()?;
    let retired = ann::retire(&store, id).map_err(err)?;
    Ok(json!({"id": id, "retired": retired}))
}

fn versions(kind: &str, target: &str) -> Result<Value, String> {
    let store = crate::state::store()?;
    let rows = ann::versions_for_chain(&store, kind, target).map_err(err)?;
    Ok(json!({"target": target, "versions": json_of(rows, "versions")?}))
}

/// Every annotation version on a concept, newest first.
pub fn annotations_on(target: &str) -> Result<Value, String> {
    versions(ann::ANNOTATION_KIND, target)
}

/// Every comment version on a concept, newest first.
pub fn comments_on(target: &str) -> Result<Value, String> {
    versions(ann::COMMENT_KIND, target)
}

fn history(id: &str) -> Result<Value, String> {
    let store = crate::state::store()?;
    let rows = ann::history(&store, id).map_err(err)?;
    Ok(json!({"id": id, "versions": json_of(rows, "history")?}))
}

/// Every version of one annotation row.
pub fn annotation_history(id: &str) -> Result<Value, String> {
    history(id)
}

/// Every version of one comment row.
pub fn comment_history(id: &str) -> Result<Value, String> {
    history(id)
}

/// Every unchecked task item, optionally scoped to one id/prefix.
pub fn open_items(scope: Option<&str>) -> Result<Value, String> {
    let store = crate::state::store()?;
    let rows = ann::open_items(&store, scope).map_err(err)?;
    let count = rows.len();
    let items = json_of(rows, "open_items")?;
    Ok(json!({"scope": scope, "count": count, "items": items}))
}

/// The store's own rows as one JSON document.
pub fn export_annotations() -> Result<Value, String> {
    let store = crate::state::store()?;
    let file = ann::export_all(&store).map_err(err)?;
    json_of(file, "export_annotations")
}

/// Replay an export document into the store.
pub fn import_annotations(payload: &str, on_conflict: &str) -> Result<Value, String> {
    let store = crate::state::store()?;
    let report = ann::import_all(&store, payload, on_conflict).map_err(err)?;
    json_of(report, "import_annotations")
}
