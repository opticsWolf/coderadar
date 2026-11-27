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
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct ManagedServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
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
impl ManagedServer {
    fn spawn(command: &str, root: &std::path::Path) -> Result<Self, String> {
        let mut parts = command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| "empty LSP command".to_string())?;
        let mut cmd = Command::new(program);
        cmd.args(parts)
            .current_dir(root)
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
        let mut server = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            opened: Vec::new(),
            last_activity: Instant::now(),
        };
        server.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": Some(file_uri(root)),
                "capabilities": {},
            }),
        )?;
        server.notify("initialized", json!({}))?;
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

    fn read_message(&mut self) -> Result<Value, String> {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self
                .stdout
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
        self.stdout
            .read_exact(&mut body)
            .map_err(|e| format!("EngineError: LSP read: {e}"))?;
        serde_json::from_slice(&body).map_err(|e| format!("EngineError: LSP JSON: {e}"))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        loop {
            let message = self.read_message()?;
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(format!("EngineError: LSP {method}: {error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
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
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
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
            guard
                .servers
                .insert(key.clone(), ManagedServer::spawn(&command, workspace)?);
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
        // `-32801 content modified` before that work finishes. Retry each of
        // those until a real location appears or the budget expires. Other
        // server errors are returned immediately.
        fn location_of(result: &Value) -> Value {
            match result {
                Value::Array(rows) => rows.first().cloned().unwrap_or(Value::Null),
                other => other.clone(),
            }
        }
        let mut attempt = server.request("textDocument/definition", params.clone());
        let mut location = Value::Null;
        let mut transient_error = None;
        for _ in 0..24 {
            match attempt {
                Ok(result) => {
                    location = location_of(&result);
                    if !location.is_null() {
                        transient_error = None;
                        break;
                    }
                }
                Err(error) if error.contains("content modified") => {
                    transient_error = Some(error);
                }
                Err(error) => return Err(error),
            }
            std::thread::sleep(Duration::from_millis(250));
            attempt = server.request("textDocument/definition", params.clone());
        }
        if location.is_null() {
            if let Some(error) = transient_error {
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
        _ => Err("usage: lsp status | lsp definition LANGUAGE FILE LINE COL".to_string()),
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
}
