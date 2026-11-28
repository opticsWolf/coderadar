//! Unit 5: a stdio JSON-RPC server over the same library as the CLI.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use core_indexer::mcp_catalogue::{Args, find, specs};
use core_indexer::mcp_runtime::{HandshakeGuard, McpSession, ParentWatchdog, stdin_messages};
use serde_json::{Value, json};

static SESSION: OnceLock<McpSession> = OnceLock::new();

fn coldstart(root: PathBuf) -> Result<Value, String> {
    let path = root.to_string_lossy().into_owned();
    let report = core_indexer::api::coldstart_build_graph(&path, true)?;
    if let Some(store) = report.get("store").and_then(Value::as_str) {
        crate::state::load_from(std::path::Path::new(store), &root)?;
    }
    Ok(report)
}

fn tool_result(id: Value, result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            let mut payload = json!({
                "content": [{"type": "text", "text": text}],
                "isError": false,
            });
            if value.is_object() {
                payload["structuredContent"] = value;
            }
            json!({"jsonrpc": "2.0", "id": id, "result": payload})
        }
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": op_error_message(&message)}],
                "isError": true,
            },
        }),
    }
}

/// MCP protocol versions this server can serve. The newest is the default
/// when a client asks for something unknown.
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

fn negotiated_protocol(requested: &str) -> &'static str {
    SUPPORTED_PROTOCOLS
        .iter()
        .copied()
        .find(|version| *version == requested)
        .unwrap_or(SUPPORTED_PROTOCOLS[0])
}

fn tool_catalogue() -> Value {
    Value::Array(
        specs()
            .iter()
            .map(core_indexer::mcp_catalogue::ToolSpec::to_mcp)
            .collect(),
    )
}

fn canonical(path: &std::path::Path) -> std::path::PathBuf {
    core_indexer::fs::strip_verbatim_prefix(
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
    )
}

fn compare_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    let spell = |path: &std::path::Path| {
        let text = path.to_string_lossy().replace('/', "\\");
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    spell(left) == spell(right)
}

/// Resolve a tool call's `project_path` the way the original does: a file
/// selects its directory, and the nearest enclosing marker selects the root.
fn selected_project(input: &str) -> Option<std::path::PathBuf> {
    let path = canonical(std::path::Path::new(input));
    let start = if path.is_dir() {
        path
    } else {
        path.parent()?.to_path_buf()
    };
    let mut candidate = start.clone();
    loop {
        if candidate.join(".coderadar").exists() || candidate.join(".coderadar.toml").exists() {
            return Some(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }
    Some(start)
}

fn served_root() -> std::path::PathBuf {
    canonical(&crate::state::root().unwrap_or_else(|| std::env::current_dir().unwrap_or_default()))
}

/// The MCP project guard. Without it a tool would silently answer about the
/// one served graph while an agent asked about a different checkout.
fn wrong_project(args: &Value) -> Option<String> {
    let input = args.get("project_path")?.as_str()?;
    if input.is_empty() {
        return None;
    }
    let Some(selected) = selected_project(input) else {
        return Some(format!(
            "`{input}` names no readable directory, so no project can be selected from it. Pass a directory (or any file inside one) and retry."
        ));
    };
    let served = served_root();
    if compare_path(&selected, &served) {
        return None;
    }
    let served = served.display();
    Some(format!(
        "This server is serving `{served}` and cannot answer for `{}`.\n\nOne project at a time in this build: the index is a single in-process graph. To ask about that other project instead, call `coderadar_set_project` with `{}` — this server switches there and re-indexes. Or drop the `project_path` argument to keep asking about `{served}`.",
        selected.display(),
        selected.display()
    ))
}

/// Word an engine error for an agent, matching the Python MCP error helper.
fn op_error_message(error: &str) -> String {
    if let Some(detail) = error.strip_prefix("NoIndex:") {
        let root = served_root();
        let root = root.display();
        return format!(
            "No code was indexed under `{root}`, which is where this server is serving from.\n\nThat directory does carry a `.coderadar` marker, so it is very likely the right project and simply has no indexed code yet.\n\nRun `coderadar_reindex` to index it now.\n\n{detail}"
        );
    }
    if let Some(detail) = error.strip_prefix("InvalidRequest: ") {
        return detail.to_string();
    }
    error.to_string()
}

fn dry_run(raw: &Value) -> bool {
    if let Some(explicit) = raw.get("dry_run").and_then(Value::as_bool) {
        return explicit;
    }
    if let Some(apply) = raw.get("apply").and_then(Value::as_bool) {
        return !apply;
    }
    true
}

fn call_tool(name: &str, raw: &Value) -> Result<Value, String> {
    let spec = find(name).ok_or_else(|| format!("unknown tool `{name}`"))?;
    if name != "coderadar_set_project"
        && let Some(message) = wrong_project(raw)
    {
        return Err(message);
    }
    let a = Args::new(spec, raw)?;
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
        "coderadar_search" => {
            crate::port::search(a.str("query"), a.opt_str("kind"), a.i64("top_k")).map(Value::Array)
        }
        "coderadar_node" => crate::port::node(a.str("entity_id"), a.bool("include_neighbors")),
        "coderadar_callers" => Ok(Value::Array(crate::port::callers(a.str("entity_id"))?)),
        "coderadar_callees" => Ok(Value::Array(crate::port::callees(a.str("entity_id"))?)),
        "coderadar_affected" => crate::port::affected(a.str("entity_id"), a.i64("max_depth")),
        "coderadar_explore" => crate::port::explore(
            a.str("query"),
            a.strings("symbols"),
            a.str("direction"),
            a.i64("max_files"),
        ),
        "coderadar_traverse" => crate::port::traverse(
            a.str("entity_id"),
            a.str("direction"),
            a.strings("edge_kinds"),
            a.i64("max_depth"),
        ),
        "coderadar_query" => crate::port::query(a.str("query")).map(Value::Array),
        "coderadar_annotations_on" => crate::annotations_ops::annotations_on(&a.str("target")),
        "coderadar_open_items" => crate::annotations_ops::open_items(a.opt_str("scope").as_deref()),
        "coderadar_dead_code" => crate::port::dead_code(
            a.f64("min_confidence"),
            a.bool("include_test_reachable"),
            a.i64("max_findings"),
        )
        .map(Value::Array),
        "coderadar_get_smells" => crate::port::get_smells(
            a.opt_str("entity_id"),
            a.opt_str("rule_id"),
            a.str("strictness"),
        )
        .map(Value::Array),
        "coderadar_find_clones" => crate::port::find_clones(
            a.i64("min_lines"),
            a.f64("min_similarity"),
            a.i64("max_groups"),
        )
        .map(Value::Array),
        "coderadar_find_scaffolding" => {
            crate::port::find_scaffolding(a.bool("include_secrets"), a.i64("max_findings"))
                .map(Value::Array)
        }
        "coderadar_blob_get" => {
            crate::port::blob_get(a.str("digest")).map(|v| v.unwrap_or(Value::Null))
        }
        "coderadar_archive" => crate::port::archive(a.opt_str("cutoff")),
        "coderadar_stale_files" => {
            crate::port::stale_files(a.strings("paths").unwrap_or_default()).map(Value::Array)
        }
        "coderadar_read_source" => {
            let entity = crate::port::find_entity(a.str("entity_id"))?
                .ok_or_else(|| "NotFound: entity does not exist".to_string())?;
            crate::port::read_source(entity)?
                .map(Value::String)
                .ok_or_else(|| "ContentUnavailable: no source for that entity".to_string())
        }
        "coderadar_as_of" => crate::port::as_of(
            a.str("timestamp"),
            a.str("query"),
            a.strings("symbols"),
            a.str("branch"),
            a.bool("strict"),
            a.opt_str("at"),
            a.bool("valid"),
            None,
        ),
        "coderadar_verify_history" => crate::port::verify_history(
            a.opt_str("from_ts"),
            a.opt_str("to_ts"),
            a.i64("generations"),
            raw.get("all")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| a.bool("all_generations")),
            a.str("branch"),
            a.i64("offenders"),
            a.raw("max_generations").and_then(Value::as_i64),
            None,
        ),
        "coderadar_history_of" => crate::port::history_of(a.str("file"), None),
        "coderadar_changes_between" => crate::port::changes_between(
            a.str("t1"),
            a.str("t2"),
            a.opt_str("scope"),
            a.str("branch"),
            a.bool("valid"),
            None,
        ),
        "coderadar_branches" => crate::port::branches(None).map(Value::Array),
        "coderadar_ingest_status" => crate::port::ingest_status(a.opt_str("store_path")),
        "coderadar_ingest_history" => crate::port::ingest_history(
            a.str("repo"),
            a.str("branch"),
            a.opt_str("from_sha"),
            a.bool("dry_run"),
            a.opt_str("lineage"),
            a.str("parent_ref"),
            a.opt_str("to_sha"),
            a.opt_str("store_path"),
        ),
        "coderadar_reingest_merged_commits" => crate::port::reingest_merged_commits(
            a.str("repo"),
            a.str("branch"),
            a.bool("dry_run"),
            a.opt_str("store_path"),
        ),
        "coderadar_backfill_commit_index" => {
            crate::port::backfill_commit_index(a.str("branch"), a.opt_str("store_path"))
        }
        "coderadar_backfill_commit_messages" => {
            crate::port::backfill_commit_messages(a.str("repo"), a.opt_str("store_path"))
        }
        "coderadar_replace_body" => crate::port::replace_body(
            a.str("entity_id"),
            a.str("new_body"),
            a.opt_str("expected_hash"),
            dry_run(raw),
            None,
        ),
        "coderadar_update_signature" => crate::port::update_signature(
            a.str("entity_id"),
            a.str("new_signature"),
            a.bool("inject_defaults"),
            dry_run(raw),
            None,
        ),
        "coderadar_rename" => {
            crate::port::rename(a.str("entity_id"), a.str("new_name"), dry_run(raw), None)
        }
        "coderadar_create_entity" => crate::port::create_entity(
            a.str("file_path"),
            a.str("language"),
            a.str("kind"),
            a.str("name"),
            a.str("body"),
            a.strings("decorators"),
            a.str("anchor"),
            a.opt_str("signature"),
            dry_run(raw),
            None,
        ),
        "coderadar_update_file" => {
            crate::port::update_file(a.str("file_path"), a.opt_str("content"), None)
        }
        "coderadar_compute_embeddings" => crate::port::compute_embeddings_op(
            a.opt_str("model_name"),
            a.i64("batch_size"),
            a.bool("recompute"),
        ),
        "coderadar_search_similar" => crate::port::search_similar_op(
            a.raw("query").cloned().unwrap_or(Value::Null),
            a.i64("top_k"),
        ),
        "coderadar_search_hybrid" => crate::port::search_hybrid_op(
            a.raw("query").cloned().unwrap_or(Value::Null),
            a.i64("top_k"),
            a.str("query_text"),
        ),
        "coderadar_comments_on" => crate::annotations_ops::comments_on(&a.str("target")),
        "coderadar_annotation_history" => crate::annotations_ops::annotation_history(&a.str("id")),
        "coderadar_comment_history" => crate::annotations_ops::comment_history(&a.str("id")),
        "coderadar_lsp_status" => Ok(core_indexer::api::lsp_status()),
        "coderadar_lsp_definition" => {
            let root = crate::state::root()
                .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
            core_indexer::api::lsp_definition_args(&root.to_string_lossy(), raw)
        }
        "coderadar_lsp_override" => {
            let root = crate::state::root()
                .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
            let edges = a.raw("edges").unwrap_or(&Value::Null);
            core_indexer::api::lsp_override_edges(&root.to_string_lossy(), edges)
        }
        "coderadar_context" => core_indexer::api::graphrag_mcp(raw),
        "coderadar_diagnose" => {
            crate::port::diagnose(a.bool("unresolved"), a.bool("low_confidence"))
        }
        "coderadar_module_children" => crate::port::module_children(a.str("module_id")),
        "coderadar_resolve" => crate::port::resolve(a.str("name"), a.i64("limit")),
        "coderadar_reindex" => {
            let root = crate::state::root()
                .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
            crate::port::reindex_with_op(&root, a.bool("with_embeddings"), a.bool("full"))
        }
        "coderadar_set_project" => {
            let raw_path = a.str("project_path");
            let path = match SESSION.get() {
                Some(session) => session.set_project(&raw_path, a.bool("confirm"), coldstart)?,
                None => PathBuf::from(&raw_path),
            };
            let _activation = core_indexer::api::activate_config(&path.to_string_lossy());
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
    if method.starts_with("notifications/") {
        return None;
    }
    let response = match method {
        "initialize" => {
            let requested = message
                .get("params")
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": negotiated_protocol(requested),
                    "serverInfo": {"name": "CodeRadar", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                    "instructions": crate::assets::SERVER_INSTRUCTIONS,
                },
            })
        }
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        "tools/list" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"tools": tool_catalogue()},
        }),
        "prompts/list" => json!({
            "jsonrpc": "2.0", "id": id, "result": {"prompts": []},
        }),
        "resources/templates/list" => json!({
            "jsonrpc": "2.0", "id": id, "result": {"resourceTemplates": []},
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
            if find(name).is_none() {
                return Some(tool_result(id, Err(format!("unknown tool `{name}`"))));
            }
            if let Some(session) = SESSION.get() {
                if let Err(error) = session.ensure_ready() {
                    return Some(tool_result(id, Err(error)));
                }
                if let Some(path) = session.retarget(coldstart)
                    && let Err(error) = crate::state::load(&path)
                {
                    return Some(tool_result(id, Err(error)));
                }
            }
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
    let launch = std::env::current_dir().unwrap_or_default();
    let path_flag = std::env::var("CODERADAR_MCP_PATH").ok();
    let session = McpSession::new(launch, path_flag.as_deref());
    eprintln!("{}", session.describe());
    let root = PathBuf::from(session.root()["path"].as_str().unwrap_or("."));
    let _ = SESSION.set(session.clone());
    let _activation = core_indexer::api::activate_config(&root.to_string_lossy());
    session.begin(coldstart);
    let timeout = std::env::var("CODERADAR_HANDSHAKE_TIMEOUT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
    let guard = HandshakeGuard::arm(timeout);
    let _watchdog = ParentWatchdog::start();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let rx = stdin_messages(&session);

    for parsed in rx {
        guard.disarm();
        let message: Value = match parsed {
            Ok(value) => value,
            Err(error) => {
                writeln!(writer, "{}", json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": error}}))
                    .map_err(|e| e.to_string())?;
                continue;
            }
        };
        let roots_line = if message.get("method").and_then(Value::as_str) == Some("tools/call") {
            session.roots_request_line()
        } else {
            None
        };
        if let Some(line) = roots_line {
            writeln!(writer, "{line}").map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
            session.wait_for_roots();
        }
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
    guard.disarm();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_project_path_selects_the_marker_root() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let repo = canonical(&repo);
        let nested = repo.join("py_agent").join("src");
        assert_eq!(selected_project(&nested.to_string_lossy()), Some(repo));
    }

    #[test]
    fn invalid_request_errors_drop_the_exception_prefix() {
        assert_eq!(
            op_error_message("InvalidRequest: Please provide a query."),
            "Please provide a query."
        );
    }

    #[test]
    fn path_comparison_is_case_insensitive_on_windows() {
        let left = std::path::Path::new("D:/User/Project");
        let right = std::path::Path::new(r"d:\user\project");
        assert_eq!(compare_path(left, right), cfg!(windows));
    }
}
