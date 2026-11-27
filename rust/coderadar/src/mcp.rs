//! Unit 5: a stdio JSON-RPC server over the same library as the CLI.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn tool_result(id: Value, result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default()}],
                "structuredContent": value,
            },
        }),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32000, "message": message},
        }),
    }
}

fn tool_names() -> Value {
    json!([
        "coderadar_status",
        "coderadar_search",
        "coderadar_node",
        "coderadar_callers",
        "coderadar_callees",
        "coderadar_affected",
        "coderadar_explore",
        "coderadar_traverse",
        "coderadar_query",
        "coderadar_annotations_on",
        "coderadar_open_items",
        "coderadar_dead_code",
        "coderadar_get_smells",
        "coderadar_find_clones",
        "coderadar_find_scaffolding",
        "coderadar_blob_get",
        "coderadar_archive",
        "coderadar_stale_files",
        "coderadar_read_source",
        "coderadar_as_of",
        "coderadar_verify_history",
        "coderadar_history_of",
        "coderadar_changes_between",
        "coderadar_branches",
        "coderadar_ingest_status",
        "coderadar_ingest_history",
        "coderadar_reingest_merged_commits",
        "coderadar_backfill_commit_index",
        "coderadar_backfill_commit_messages",
        "coderadar_replace_body",
        "coderadar_update_signature",
        "coderadar_rename",
        "coderadar_create_entity",
        "coderadar_update_file",
        "coderadar_compute_embeddings",
        "coderadar_search_similar",
        "coderadar_search_hybrid",
        "coderadar_comments_on",
        "coderadar_annotation_history",
        "coderadar_comment_history",
        "coderadar_diagnose",
        "coderadar_module_children",
        "coderadar_resolve",
        "coderadar_reindex",
        "coderadar_set_project",
    ])
}

fn call_tool(name: &str, args: &Value) -> Result<Value, String> {
    let arg = |key: &str| -> String {
        args.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match name {
        "coderadar_status" => {
            let snap = crate::state::projection()
                .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
            Ok(json!({
                "loaded": true,
                "root": crate::state::root().map(|p| p.to_string_lossy().to_string()),
                "stats": core_indexer::api::stats(&snap),
            }))
        }
        "coderadar_search" => crate::port::search(
            arg("query"),
            args.get("kind").and_then(Value::as_str).map(str::to_string),
            args.get("top_k").and_then(Value::as_i64).unwrap_or(10),
        )
        .map(Value::Array),
        "coderadar_node" => crate::port::node(arg("entity_id"), false),
        "coderadar_callers" => Ok(Value::Array(crate::port::callers(arg("entity_id"))?)),
        "coderadar_callees" => Ok(Value::Array(crate::port::callees(arg("entity_id"))?)),
        "coderadar_affected" => crate::port::affected(
            arg("entity_id"),
            args.get("max_depth").and_then(Value::as_i64).unwrap_or(5),
        ),
        "coderadar_explore" => crate::port::explore(arg("query"), None, "both".into(), 8),
        "coderadar_traverse" => crate::port::traverse(
            arg("entity_id"),
            args.get("direction")
                .and_then(Value::as_str)
                .unwrap_or("both")
                .to_string(),
            None,
            args.get("max_depth").and_then(Value::as_i64).unwrap_or(3),
        ),
        "coderadar_query" => crate::port::query(arg("query")).map(Value::Array),
        "coderadar_annotations_on" => crate::annotations_ops::annotations_on(&arg("target")),
        "coderadar_open_items" => {
            crate::annotations_ops::open_items(args.get("scope").and_then(Value::as_str))
        }
        "coderadar_dead_code" => crate::port::dead_code(
            args.get("min_confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.6),
            args.get("include_test_reachable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("max_findings")
                .and_then(Value::as_i64)
                .unwrap_or(100),
        )
        .map(Value::Array),
        "coderadar_get_smells" => crate::port::get_smells(
            args.get("entity_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("rule_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("strictness")
                .and_then(Value::as_str)
                .unwrap_or("normal")
                .to_string(),
        )
        .map(Value::Array),
        "coderadar_find_clones" => crate::port::find_clones(
            args.get("min_lines").and_then(Value::as_i64).unwrap_or(10),
            args.get("min_similarity")
                .and_then(Value::as_f64)
                .unwrap_or(0.8),
            args.get("max_groups")
                .and_then(Value::as_i64)
                .unwrap_or(100),
        )
        .map(Value::Array),
        "coderadar_find_scaffolding" => crate::port::find_scaffolding(
            args.get("include_secrets")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("max_findings")
                .and_then(Value::as_i64)
                .unwrap_or(100),
        )
        .map(Value::Array),
        "coderadar_blob_get" => {
            crate::port::blob_get(arg("digest")).map(|v| v.unwrap_or(Value::Null))
        }
        "coderadar_archive" => crate::port::archive(
            args.get("cutoff")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        "coderadar_stale_files" => crate::port::stale_files(
            args.get("paths")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        )
        .map(Value::Array),
        "coderadar_read_source" => {
            let entity = crate::port::find_entity(arg("entity_id"))?
                .ok_or_else(|| "NotFound: entity does not exist".to_string())?;
            crate::port::read_source(entity)?
                .map(Value::String)
                .ok_or_else(|| "ContentUnavailable: no source for that entity".to_string())
        }
        "coderadar_as_of" => crate::port::as_of(
            arg("timestamp"),
            arg("query"),
            args.get("symbols").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            }),
            args.get("branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            args.get("strict").and_then(Value::as_bool).unwrap_or(false),
            args.get("at").and_then(Value::as_str).map(str::to_string),
            args.get("valid").and_then(Value::as_bool).unwrap_or(false),
            None,
        ),
        "coderadar_verify_history" => crate::port::verify_history(
            args.get("from_ts")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("to_ts")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("generations")
                .and_then(Value::as_i64)
                .unwrap_or(25),
            args.get("all").and_then(Value::as_bool).unwrap_or(false),
            args.get("branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            args.get("offenders").and_then(Value::as_i64).unwrap_or(10),
            args.get("max_generations").and_then(Value::as_i64),
            None,
        ),
        "coderadar_history_of" => crate::port::history_of(arg("file"), None),
        "coderadar_changes_between" => crate::port::changes_between(
            arg("t1"),
            arg("t2"),
            args.get("scope")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            args.get("valid").and_then(Value::as_bool).unwrap_or(false),
            None,
        ),
        "coderadar_branches" => crate::port::branches(None).map(Value::Array),
        "coderadar_ingest_status" => crate::port::ingest_status(
            args.get("store_path")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        "coderadar_ingest_history" => crate::port::ingest_history(
            arg("repo"),
            args.get("branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            args.get("from_sha")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("dry_run").and_then(Value::as_bool).unwrap_or(true),
            args.get("lineage")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("parent_ref")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            args.get("to_sha")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("store_path")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        "coderadar_reingest_merged_commits" => crate::port::reingest_merged_commits(
            arg("repo"),
            args.get("branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            args.get("dry_run")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("store_path")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        "coderadar_backfill_commit_index" => crate::port::backfill_commit_index(
            args.get("branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string(),
            args.get("store_path")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        "coderadar_backfill_commit_messages" => crate::port::backfill_commit_messages(
            arg("repo"),
            args.get("store_path")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        "coderadar_replace_body" => crate::port::replace_body(
            arg("entity_id"),
            arg("new_body"),
            args.get("expected_hash")
                .and_then(Value::as_str)
                .map(str::to_string),
            !args.get("apply").and_then(Value::as_bool).unwrap_or(false),
            None,
        ),
        "coderadar_update_signature" => crate::port::update_signature(
            arg("entity_id"),
            arg("new_signature"),
            args.get("inject_defaults")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            !args.get("apply").and_then(Value::as_bool).unwrap_or(false),
            None,
        ),
        "coderadar_rename" => crate::port::rename(
            arg("entity_id"),
            arg("new_name"),
            !args.get("apply").and_then(Value::as_bool).unwrap_or(false),
            None,
        ),
        "coderadar_create_entity" => crate::port::create_entity(
            arg("file_path"),
            arg("language"),
            arg("kind"),
            arg("name"),
            arg("body"),
            args.get("decorators").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            }),
            args.get("anchor")
                .and_then(Value::as_str)
                .unwrap_or("end")
                .to_string(),
            args.get("signature")
                .and_then(Value::as_str)
                .map(str::to_string),
            !args.get("apply").and_then(Value::as_bool).unwrap_or(false),
            None,
        ),
        "coderadar_update_file" => crate::port::update_file(
            arg("file_path"),
            args.get("content")
                .and_then(Value::as_str)
                .map(str::to_string),
            None,
        ),
        "coderadar_compute_embeddings" => crate::port::compute_embeddings_op(
            args.get("model_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            args.get("batch_size").and_then(Value::as_i64).unwrap_or(32),
            args.get("recompute")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "coderadar_search_similar" => crate::port::search_similar_op(
            args.get("query").cloned().unwrap_or(Value::Null),
            args.get("top_k").and_then(Value::as_i64).unwrap_or(10),
        ),
        "coderadar_search_hybrid" => crate::port::search_hybrid_op(
            args.get("query").cloned().unwrap_or(Value::Null),
            args.get("top_k").and_then(Value::as_i64).unwrap_or(10),
            args.get("query_text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        "coderadar_comments_on" => crate::annotations_ops::comments_on(&arg("target")),
        "coderadar_annotation_history" => crate::annotations_ops::annotation_history(&arg("id")),
        "coderadar_comment_history" => crate::annotations_ops::comment_history(&arg("id")),
        "coderadar_diagnose" => crate::port::diagnose(
            args.get("unresolved")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            args.get("low_confidence")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        ),
        "coderadar_module_children" => crate::port::module_children(arg("module_id")),
        "coderadar_resolve" => crate::port::resolve(
            arg("name"),
            args.get("limit").and_then(Value::as_i64).unwrap_or(5),
        ),
        "coderadar_reindex" => {
            let root = crate::state::root()
                .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
            crate::port::reindex_op(&root)
        }
        "coderadar_set_project" => {
            let path = std::path::PathBuf::from(arg("project_path"));
            crate::state::load(&path)?;
            Ok(json!({"project": path.to_string_lossy(), "loaded": true}))
        }
        other => Err(format!("unknown tool `{other}`")),
    }
}

fn resource_entries() -> Vec<Value> {
    let mut rows = Vec::new();
    for name in crate::assets::SKILL_NAMES {
        rows.push(json!({
            "uri": format!("skills://{name}"),
            "name": name,
            "description": crate::assets::skill_description(name).unwrap_or(""),
            "mimeType": "text/markdown",
        }));
    }
    for (name, description) in crate::assets::DOC_NAMES
        .iter()
        .zip(crate::assets::DOC_DESCRIPTIONS)
    {
        rows.push(json!({
            "uri": format!("docs://{name}"),
            "name": name,
            "description": description,
            "mimeType": "text/markdown",
        }));
    }
    rows
}

fn read_resource(uri: &str) -> Result<Value, String> {
    let (text, mime) = if let Some(name) = uri.strip_prefix("skills://") {
        (
            crate::assets::skill_text(name).ok_or_else(|| format!("unknown resource {uri:?}"))?,
            "text/markdown",
        )
    } else if let Some(name) = uri.strip_prefix("docs://") {
        (
            crate::assets::doc_text(name).ok_or_else(|| format!("unknown resource {uri:?}"))?,
            "text/markdown",
        )
    } else {
        return Err(format!("unknown resource {uri:?}"));
    };
    Ok(json!({
        "contents": [{"uri": uri, "mimeType": mime, "text": text}],
    }))
}

fn dispatch(message: &Value) -> Option<Value> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str)?;
    let response = match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "coderadar-rs", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"tools": {}, "resources": {}},
            },
        }),
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        "tools/list" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"tools": tool_names()},
        }),
        "resources/list" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"resources": resource_entries()},
        }),
        "resources/read" => {
            let params = message.get("params").unwrap_or(&Value::Null);
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match read_resource(uri) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(error) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32002, "message": error},
                }),
            }
        }
        "tools/call" => {
            let params = message.get("params").unwrap_or(&Value::Null);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.get("arguments").unwrap_or(&Value::Null);
            tool_result(id, call_tool(name, arguments))
        }
        _ => json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": format!("method not found: {method}")},
        }),
    };
    Some(response)
}

/// Serve newline-delimited JSON-RPC on stdin until EOF.
///
/// # Errors
/// Returns an I/O error only; protocol errors are replied to inline.
pub fn serve() -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                writeln!(writer, "{}", json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}))
                    .map_err(|e| e.to_string())?;
                continue;
            }
        };
        if let Some(response) = dispatch(&message) {
            writeln!(
                writer,
                "{}",
                serde_json::to_string(&response).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
