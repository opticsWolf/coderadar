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
        other => Err(format!("unknown tool `{other}`")),
    }
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
                "capabilities": {"tools": {}},
            },
        }),
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        "tools/list" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"tools": tool_names()},
        }),
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
