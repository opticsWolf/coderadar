//! Pure-Rust persistent LSP client skeleton.
//!
//! Python's `lsp/pool.py` is optional and, in this repo, never constructed.
//! This module gives the Rust app the same configuration surface plus a real
//! `textDocument/definition` round-trip when `[resolution.lsp] enabled = true`.
//! Servers are one per language/workspace, kept warm, idled out, and cached by
//! the same `(path, line, col, content_hash)` key as the Python pool.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct ManagedServer {
    child: Child,
    stdin: ChildStdin,
    inbox: Receiver<Result<Value, String>>,
    next_id: i64,
    opened: Vec<String>,
    last_activity: Instant,
}
impl Drop for ManagedServer {
    fn drop(&mut self) {
        let _ = self.stdin.write_all(
            b"Content-Length: 55\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":999999,\"method\":\"shutdown\"}",
        );
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
/// Build the process described by a configured command string.
///
/// Commands are shell-expanded so quotes, environment variables, and npm's
/// Windows `.cmd` shims behave the way the Python configuration intends.
fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/D", "/S", "/C", command]);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", command]);
        cmd
    }
}

/// Read one framed JSON-RPC message from a language server's stdout.
fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value, String> {
    let mut length = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| format!("EngineError: LSP read: {e}"))?;
        if read == 0 {
            return Err("EngineError: LSP server closed stdout".to_string());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| "EngineError: LSP response has no length".to_string())?;
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("EngineError: LSP read: {e}"))?;
    serde_json::from_slice(&body).map_err(|e| format!("EngineError: LSP JSON: {e}"))
}

impl ManagedServer {
    fn spawn(command: &str, root: &std::path::Path, timeout: Duration) -> Result<Self, String> {
        let mut cmd = shell_command(command);
        cmd.current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|error| format!("EngineError: cannot start LSP {command:?}: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "EngineError: LSP stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "EngineError: LSP stdout unavailable".to_string())?;
        let (tx, inbox) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader) {
                    Ok(message) => {
                        if tx.send(Ok(message)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
            }
        });
        let mut server = Self {
            child,
            stdin,
            inbox,
            next_id: 1,
            opened: Vec::new(),
            last_activity: Instant::now(),
        };
        if let Err(error) = server.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": Some(file_uri(root)),
                "capabilities": {},
            }),
            timeout,
        ) {
            server.terminate();
            return Err(error);
        }
        if let Err(error) = server.notify("initialized", json!({})) {
            server.terminate();
            return Err(error);
        }
        Ok(server)
    }

    fn write_message(&mut self, value: &Value) -> Result<(), String> {
        let body = serde_json::to_vec(value).map_err(|e| format!("EngineError: {e}"))?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())
            .map_err(|e| format!("EngineError: LSP write: {e}"))?;
        self.stdin
            .write_all(&body)
            .map_err(|e| format!("EngineError: LSP write: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("EngineError: LSP write: {e}"))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn terminate(&mut self) {
        #[cfg(windows)]
        {
            // Shell-expanded commands spawn a `cmd.exe` parent; killing only
            // that parent can leave the language server holding the stdout
            // pipe. End the whole tree first.
            let pid = self.child.id().to_string();
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        loop {
            let message = match self.inbox.recv_timeout(timeout) {
                Ok(Ok(message)) => message,
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("EngineError: LSP server disconnected".to_string());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // The caller decides whether a slow answer is transient;
                    // killing here would prevent the retry budget from ever
                    // recovering a busy language server.
                    return Err(format!(
                        "EngineError: LSP {method} timed out after {}ms",
                        timeout.as_millis()
                    ));
                }
            };
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(format!("EngineError: LSP {method}: {error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }
}

/// First definition from an LSP result, or null when none was returned.
fn first_location(result: &Value) -> Value {
    match result {
        Value::Array(rows) => rows.first().cloned().unwrap_or(Value::Null),
        other => other.clone(),
    }
}

#[derive(Default)]
struct Pools {
    servers: HashMap<String, ManagedServer>,
    cache: HashMap<String, (Instant, Value)>,
}

static POOLS: OnceLock<Mutex<Pools>> = OnceLock::new();

fn pools() -> &'static Mutex<Pools> {
    POOLS.get_or_init(|| Mutex::new(Pools::default()))
}

fn file_uri(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn content_hash(text: &str) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn config() -> crate::graph::LSPConfig {
    crate::active_config().resolution.lsp.clone()
}

/// The configured LSP surface, without starting any server.
pub fn status() -> Value {
    let cfg = config();
    let languages: Vec<Value> = cfg
        .servers
        .iter()
        .map(|(language, command)| {
            json!({
                "language": language,
                "command": command,
                "enabled": cfg.enabled,
            })
        })
        .collect();
    json!({
        "enabled": cfg.enabled,
        "result_ttl_s": cfg.result_ttl_s,
        "idle_timeout_s": cfg.idle_timeout_s,
        "timeout_ms": cfg.timeout_ms,
        "override_threshold": cfg.override_threshold,
        "languages": languages,
    })
}

/// Whether a graph edge's confidence is low enough to consult LSP.
///
/// The threshold comes from `[resolution.lsp].override_threshold`; disabled
/// configurations never override an edge.
pub fn should_override(confidence: f32) -> bool {
    let cfg = config();
    cfg.enabled && confidence < cfg.override_threshold
}

fn override_target_id(location: &Value, source: &std::path::Path) -> String {
    let target = location
        .get("uri")
        .and_then(Value::as_str)
        .and_then(crate::mcp_runtime::uri_to_file_path)
        .unwrap_or_else(|| source.to_path_buf());
    let start = location.get("range").and_then(|range| range.get("start"));
    let line = start
        .and_then(|pos| pos.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1);
    let column = start
        .and_then(|pos| pos.get("character"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1);
    format!("{}#{}:{}", target.display(), line, column)
}

/// Apply the configured threshold to one graph-edge position.
///
/// # Errors
/// Returns an explicit message for a spawn, protocol, or server failure. A
/// disabled config or a confidence at/above the threshold returns `None`
/// without starting a server.
pub fn override_edge(
    language: &str,
    workspace: &std::path::Path,
    path: &std::path::Path,
    line: u32,
    column: u32,
    confidence: f32,
) -> Result<Option<Value>, String> {
    if !should_override(confidence) {
        return Ok(None);
    }
    let Some(location) = definition(language, workspace, path, line, column)? else {
        return Ok(None);
    };
    if location.is_null() {
        return Ok(None);
    }
    Ok(Some(json!({
        "uri": location.get("uri").cloned().unwrap_or(Value::Null),
        "range": location.get("range").cloned().unwrap_or(Value::Null),
        "target_id": override_target_id(&location, path),
        "confidence": 1.0,
    })))
}

/// Apply the configured threshold to a list of graph-edge rows.
///
/// Rows below the threshold are checked; a real definition rewrites
/// `target_id`, `method`, `provenance`, and `confidence` in place. Rows at or
/// above the threshold are returned untouched.
///
/// # Errors
/// Returns `InvalidRequest` when `edges` is not an array, or the server error
/// when a checked lookup fails.
pub fn override_edges(workspace: &std::path::Path, edges: &Value) -> Result<Value, String> {
    let rows = edges
        .as_array()
        .ok_or_else(|| "InvalidRequest: edges must be an array".to_string())?;
    let cfg = config();
    let mut output = Vec::with_capacity(rows.len());
    let mut checked = 0usize;
    let mut applied = 0usize;
    for row in rows {
        let mut row = row.clone();
        let confidence = row.get("confidence").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        if cfg.enabled && confidence < cfg.override_threshold {
            checked += 1;
            let language = row.get("language").and_then(Value::as_str).unwrap_or("");
            let file = row.get("file").and_then(Value::as_str).unwrap_or("");
            let line = row.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let column = row.get("column").and_then(Value::as_u64).unwrap_or(0) as u32;
            if let Some(overridden) = override_edge(
                language,
                workspace,
                std::path::Path::new(file),
                line,
                column,
                confidence,
            )? {
                if let Some(map) = row.as_object_mut() {
                    if let Some(target) = overridden.get("target_id").and_then(Value::as_str) {
                        map.insert("target_id".into(), json!(target));
                    }
                    map.insert("method".into(), json!("lsp"));
                    map.insert("provenance".into(), json!("lsp"));
                    map.insert("confidence".into(), json!(1.0));
                }
                applied += 1;
            }
        }
        output.push(row);
    }
    Ok(json!({
        "threshold": cfg.override_threshold,
        "checked": checked,
        "applied": applied,
        "edges": output,
    }))
}

/// Ask the configured server for a definition, or return `None` when disabled.
///
/// # Errors
/// Returns an explicit message for a spawn, protocol or server error. A
/// missing or stale server is never reported as "no definition".
pub fn definition(
    language: &str,
    workspace: &std::path::Path,
    path: &std::path::Path,
    line: u32,
    column: u32,
) -> Result<Option<Value>, String> {
    let cfg = config();
    let Some(command) = cfg.servers.get(language).cloned() else {
        return Ok(None);
    };
    if !cfg.enabled {
        return Ok(None);
    }
    // A zero timeout would make every request fail immediately rather than
    // block forever, so keep it at one millisecond.
    let timeout = Duration::from_millis(cfg.timeout_ms.max(1));
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    // Language servers expect absolute URIs. Resolve a relative workspace
    // path before the URI is built so Windows drives are not rewritten as
    // filesystem-root paths.
    let absolute =
        crate::fs::strip_verbatim_prefix(std::fs::canonicalize(&joined).unwrap_or(joined));
    let text = std::fs::read_to_string(&absolute)
        .map_err(|e| format!("EngineError: LSP source {}: {e}", absolute.display()))?;
    let hash = content_hash(&text);
    let cache_key = format!(
        "{}:{}:{}:{}:{}",
        absolute.display(),
        line,
        column,
        hash,
        command
    );
    let now = Instant::now();
    {
        let mut guard = pools().lock().map_err(|e| format!("EngineError: {e}"))?;
        if let Some((stamp, value)) = guard.cache.get(&cache_key) {
            if now.duration_since(*stamp) < Duration::from_secs(cfg.result_ttl_s) {
                return Ok(Some(value.clone()));
            }
        }
        let idle = Duration::from_secs(cfg.idle_timeout_s);
        guard
            .servers
            .retain(|_, server| now.duration_since(server.last_activity) < idle);
        let key = format!("{language}:{}", workspace.display());
        if !guard.servers.contains_key(&key) {
            guard.servers.insert(
                key.clone(),
                ManagedServer::spawn(&command, workspace, timeout)?,
            );
        }
        let server = guard
            .servers
            .get_mut(&key)
            .ok_or_else(|| "EngineError: LSP server vanished".to_string())?;
        server.last_activity = now;
        let uri = file_uri(&absolute);
        if !server.opened.contains(&uri) {
            server.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": language,
                        "version": 1,
                        "text": text,
                    }
                }),
            )?;
            server.opened.push(uri.clone());
        } else {
            server.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": 2 },
                    "contentChanges": [{ "text": text }],
                }),
            )?;
        }
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": column },
        });
        // rust-analyzer loads Cargo metadata after initialize; an immediate
        // definition can legitimately be null, an empty array, or a transient
        // `-32801 content modified` before that work finishes. A per-request
        // timeout can also fire while the server is busy. Retry all four
        // until a location appears or the overall budget expires; the budget
        // is what makes a server that never answers an error, not a hang.
        let budget = Duration::from_millis(cfg.timeout_ms.saturating_mul(4).max(10_000));
        let deadline = Instant::now() + budget;
        let mut attempt = server.request("textDocument/definition", params.clone(), timeout);
        let mut location = Value::Null;
        let mut transient_error = None;
        loop {
            match attempt {
                Ok(result) => {
                    location = first_location(&result);
                    if !location.is_null() {
                        transient_error = None;
                        break;
                    }
                }
                Err(error) if error.contains("content modified") || error.contains("timed out") => {
                    transient_error = Some(error);
                }
                Err(error) => return Err(error),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250).min(remaining));
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            attempt = server.request(
                "textDocument/definition",
                params.clone(),
                timeout.min(remaining),
            );
        }
        if location.is_null() {
            if let Some(error) = transient_error {
                server.terminate();
                return Err(error);
            }
        }
        guard.cache.insert(cache_key, (now, location.clone()));
        Ok(if location.is_null() {
            None
        } else {
            Some(location)
        })
    }
}

/// Parse the CLI `lsp` surface in the engine so the app stays thin.
pub fn cli(args: &[String], workspace: &std::path::Path) -> Result<Value, String> {
    match args.first().map(String::as_str) {
        Some("status") => Ok(status()),
        Some("definition") if args.len() >= 5 => {
            let line = args[3]
                .parse::<u32>()
                .map_err(|e| format!("InvalidRequest: {e}"))?;
            let column = args[4]
                .parse::<u32>()
                .map_err(|e| format!("InvalidRequest: {e}"))?;
            Ok(definition(
                &args[1],
                workspace,
                std::path::Path::new(&args[2]),
                line,
                column,
            )?
            .unwrap_or(Value::Null))
        }
        Some("override") if args.len() >= 6 => {
            let line = args[3]
                .parse::<u32>()
                .map_err(|e| format!("InvalidRequest: {e}"))?;
            let column = args[4]
                .parse::<u32>()
                .map_err(|e| format!("InvalidRequest: {e}"))?;
            let confidence = args[5]
                .parse::<f32>()
                .map_err(|e| format!("InvalidRequest: {e}"))?;
            Ok(override_edge(
                &args[1],
                workspace,
                std::path::Path::new(&args[2]),
                line,
                column,
                confidence,
            )?
            .unwrap_or(Value::Null))
        }
        _ => Err(
            "usage: lsp status | lsp definition LANGUAGE FILE LINE COL | lsp override LANGUAGE FILE LINE COL CONFIDENCE"
                .to_string(),
        ),
    }
}

/// Parse the MCP `coderadar_lsp_definition` arguments.
pub fn definition_args(workspace: &std::path::Path, args: &Value) -> Result<Value, String> {
    let language = args.get("language").and_then(Value::as_str).unwrap_or("");
    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
    let line = args.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
    let column = args.get("column").and_then(Value::as_u64).unwrap_or(0) as u32;
    Ok(definition(
        language,
        workspace,
        std::path::Path::new(path),
        line,
        column,
    )?
    .unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reports_defaults_without_spawning() {
        let value = status();
        assert_eq!(value["enabled"], false);
        assert!(value["languages"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()));
    }

    #[test]
    fn disabled_config_does_not_override_low_confidence_edges() {
        assert!(!should_override(0.1));
        let result = override_edge(
            "python",
            std::path::Path::new("."),
            std::path::Path::new("missing.py"),
            0,
            0,
            0.1,
        );
        assert_eq!(result.expect("disabled override"), None);
    }

    #[test]
    fn override_edges_requires_an_array() {
        let error = override_edges(std::path::Path::new("."), &json!({}))
            .expect_err("object is not an edge list");
        assert!(error.contains("InvalidRequest"), "{error}");
    }
}
