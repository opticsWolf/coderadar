//! MCP process lifecycle helpers (roots ladder, background index, watchdogs).
//!
//! These are engine-shaped process primitives: pure Rust, no Python objects,
//! and safe to call from the thin stdio MCP crate. Keeping them here also
//! keeps the port crate's line count inside the gate's size heuristic.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde_json::{json, Value};

/// A project root and the evidence that chose it.
#[derive(Clone, Debug)]
pub struct ResolvedRoot {
    pub path: PathBuf,
    pub source: String,
    pub marker: Option<PathBuf>,
}

impl ResolvedRoot {
    pub fn confirmed(&self) -> bool {
        self.marker.is_some()
    }

    pub fn json(&self) -> Value {
        json!({
            "path": self.path.to_string_lossy(),
            "source": self.source,
            "marker": self.marker.as_ref().map(|p| p.to_string_lossy()),
            "confirmed": self.confirmed(),
        })
    }

    pub fn describe(&self) -> String {
        match &self.marker {
            Some(marker) => format!(
                "Project root {} (from {}, marker {})",
                self.path.display(),
                self.source,
                marker
                    .file_name()
                    .map(|v| v.to_string_lossy().into_owned())
                    .unwrap_or_else(|| marker.display().to_string())
            ),
            None => format!(
                "Project root {} (from {}, no .coderadar marker found — run `coderadar init` there if this is the wrong project)",
                self.path.display(),
                self.source
            ),
        }
    }
}

/// Walk up from `path` looking for `.coderadar/` or `.coderadar.toml`.
pub fn find_marker(path: &Path) -> Option<PathBuf> {
    let mut dir = path.to_path_buf();
    loop {
        if dir.join(".coderadar").is_dir() || dir.join(".coderadar.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn canonical_dir(path: &Path) -> Option<PathBuf> {
    let mut resolved = std::fs::canonicalize(path).ok()?;
    if !resolved.is_dir() {
        resolved.pop();
        if !resolved.is_dir() {
            return None;
        }
    }
    Some(resolved)
}

/// Decode a `file://` URI that names a file, without reducing it to its
/// containing directory.
pub fn uri_to_file_path(uri: &str) -> Option<PathBuf> {
    if uri.is_empty() {
        return None;
    }
    if let Some(rest) = uri.strip_prefix("file://") {
        let mut text = percent_decode(rest);
        if text.starts_with('/') && text.as_bytes().get(2) == Some(&b':') {
            text.remove(0);
        }
        let path = PathBuf::from(text);
        let resolved = std::fs::canonicalize(&path).unwrap_or(path);
        return Some(crate::fs::strip_verbatim_prefix(resolved));
    }
    if uri.starts_with("file:") {
        return None;
    }
    Some(PathBuf::from(uri))
}

/// Accept `file://` URIs and bare absolute paths, as the Python ladder does.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    if uri.is_empty() {
        return None;
    }
    if let Some(rest) = uri.strip_prefix("file://") {
        let mut text = percent_decode(rest);
        if text.starts_with('/') && text.as_bytes().get(2) == Some(&b':') {
            text.remove(0);
        }
        return canonical_dir(Path::new(&text));
    }
    if uri.starts_with("file:") {
        return None;
    }
    let path = Path::new(uri);
    if path.is_absolute() {
        canonical_dir(path)
    } else {
        None
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push(hi * 16 + lo);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn state_file() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| {
            PathBuf::from(home)
                .join(".coderadar")
                .join("mcp")
                .join("last_projects.json")
        })
}

fn key(directory: &Path) -> String {
    let resolved = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    let text = resolved.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// Remember the last explicit project choice for this launch directory.
/// Best effort: every failure is swallowed by design.
pub fn record_project(launch_cwd: &Path, root: &Path) {
    let Some(path) = state_file() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut data: BTreeMap<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    data.insert(key(launch_cwd), json!(root.to_string_lossy()));
    if let Ok(text) = serde_json::to_string_pretty(&data) {
        let _ = std::fs::write(path, text);
    }
}

pub fn last_project_for(launch_cwd: &Path) -> Option<PathBuf> {
    let path = state_file()?;
    let text = std::fs::read_to_string(path).ok()?;
    let data: BTreeMap<String, Value> = serde_json::from_str(&text).ok()?;
    let value = data.get(&key(launch_cwd))?.as_str()?;
    let candidate = PathBuf::from(value);
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

fn resolve_one(path: PathBuf, source: &str) -> Option<ResolvedRoot> {
    let canonical = canonical_dir(&path)?;
    Some(match find_marker(&canonical) {
        Some(marker) => ResolvedRoot {
            path: marker.clone(),
            source: source.to_string(),
            marker: Some(marker),
        },
        None => ResolvedRoot {
            path: canonical,
            source: source.to_string(),
            marker: None,
        },
    })
}

/// Two-pass root ladder: a lower-rung marker beats a higher-rung guess.
pub fn resolve_root(
    client_roots: &[String],
    path_flag: Option<&str>,
    cwd: Option<&str>,
    launch_cwd: Option<&Path>,
) -> ResolvedRoot {
    let raw_cwd = PathBuf::from(cwd.unwrap_or("."));
    let mut candidates: Vec<(PathBuf, &'static str)> = Vec::new();
    for uri in client_roots {
        if let Some(path) = uri_to_path(uri) {
            candidates.push((path, "client root"));
        }
    }
    if let Some(flag) = path_flag {
        candidates.push((PathBuf::from(flag), "--path"));
    }
    if let Some(launch) = launch_cwd {
        if let Some(recorded) = last_project_for(launch) {
            candidates.push((recorded, "previous session"));
        }
    }
    candidates.push((raw_cwd.clone(), "cwd"));

    let canonical: Vec<(PathBuf, &'static str)> = candidates
        .into_iter()
        .filter_map(|(path, source)| canonical_dir(&path).map(|dir| (dir, source)))
        .collect();
    for (path, source) in &canonical {
        if let Some(marker) = find_marker(path) {
            let root = marker.clone();
            return ResolvedRoot {
                path: root,
                source: (*source).to_string(),
                marker: Some(marker),
            };
        }
    }
    if let Some((path, source)) = canonical.first() {
        return ResolvedRoot {
            path: path.clone(),
            source: (*source).to_string(),
            marker: None,
        };
    }
    ResolvedRoot {
        path: raw_cwd,
        source: "cwd".to_string(),
        marker: None,
    }
}

/// Resolve a tool call's `project_path`, including files and subdirectories.
pub fn resolve_selector(path: &str) -> Option<ResolvedRoot> {
    resolve_one(PathBuf::from(path), "project_path")
}

/// Resolve only over client roots, for the lazy first-tool retry.
pub fn resolve_client_roots(uris: &[String]) -> Option<ResolvedRoot> {
    for uri in uris {
        if let Some(path) = uri_to_path(uri) {
            if let Some(marker) = find_marker(&path) {
                let root = marker.clone();
                return Some(ResolvedRoot {
                    path: root,
                    source: "client root".to_string(),
                    marker: Some(marker),
                });
            }
        }
    }
    uris.iter()
        .filter_map(|uri| uri_to_path(uri))
        .map(|path| ResolvedRoot {
            path,
            source: "client root".to_string(),
            marker: None,
        })
        .next()
}

/// One connection's "should we ask the client again?" decision.
#[derive(Default)]
pub struct LazyRootRetry {
    attempted: Mutex<bool>,
    client_roots: Vec<String>,
}
impl LazyRootRetry {
    pub fn new(client_roots: Vec<String>) -> Self {
        Self {
            attempted: Mutex::new(false),
            client_roots,
        }
    }
    pub fn should_ask(&self, confirmed: bool, index_empty: bool) -> bool {
        !*self.attempted.lock() && (!confirmed || index_empty)
    }
    pub fn mark_user_chosen(&self) {
        *self.attempted.lock() = true;
    }
    pub fn set_client_roots(&mut self, uris: Vec<String>) {
        self.client_roots = uris;
    }
    pub fn attempt(&self, current: &ResolvedRoot) -> Option<ResolvedRoot> {
        let mut attempted = self.attempted.lock();
        if *attempted {
            return None;
        }
        *attempted = true;
        let candidate = resolve_client_roots(&self.client_roots)?;
        if candidate.path == current.path {
            return None;
        }
        if current.confirmed() && !candidate.confirmed() {
            return None;
        }
        Some(candidate)
    }
}
impl Clone for LazyRootRetry {
    fn clone(&self) -> Self {
        Self {
            attempted: Mutex::new(*self.attempted.lock()),
            client_roots: self.client_roots.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IndexStatus {
    #[default]
    NotStarted,
    Indexing,
    Ready,
    Failed,
}
impl IndexStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Default)]
struct IndexInner {
    root: PathBuf,
    status: IndexStatus,
    error: Option<String>,
    generation: u64,
    started: Option<Instant>,
    finished: Option<Instant>,
}

static BUILD_LOCK: Mutex<()> = Mutex::new(());
static BUILD_SEQ: OnceLock<Mutex<u64>> = OnceLock::new();

fn next_seq() -> u64 {
    let slot = BUILD_SEQ.get_or_init(|| Mutex::new(0));
    let mut guard = slot.lock();
    *guard += 1;
    *guard
}

/// One generation-tracked background build, with a bounded waiter.
#[derive(Clone)]
pub struct BackgroundIndex {
    inner: Arc<(Mutex<IndexInner>, Condvar)>,
}
impl BackgroundIndex {
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(IndexInner {
                    root,
                    status: IndexStatus::NotStarted,
                    ..IndexInner::default()
                }),
                Condvar::new(),
            )),
        }
    }

    pub fn start<F>(&self, build: F)
    where
        F: Fn(String) -> Result<Value, String> + Send + 'static,
    {
        let (lock, _wake) = &*self.inner;
        let (root, generation) = {
            let mut state = lock.lock();
            if state.status == IndexStatus::Indexing {
                return;
            }
            state.status = IndexStatus::Indexing;
            state.started = Some(Instant::now());
            state.error = None;
            (state.root.clone(), state.generation)
        };
        let inner = Arc::clone(&self.inner);
        let seq = next_seq();
        std::thread::spawn(move || {
            let status;
            let error;
            {
                let _guard = BUILD_LOCK.lock();
                let current = *BUILD_SEQ.get_or_init(|| Mutex::new(0)).lock();
                if seq != current {
                    return;
                }
                match build(root.to_string_lossy().into_owned()) {
                    Ok(_) => {
                        status = IndexStatus::Ready;
                        error = None;
                    }
                    Err(message) => {
                        status = IndexStatus::Failed;
                        error = Some(message);
                    }
                }
            }
            let (lock, wake) = &*inner;
            let mut state = lock.lock();
            if state.generation == generation {
                state.status = status;
                state.error = error;
                state.finished = Some(Instant::now());
            }
            wake.notify_all();
        });
    }

    pub fn restart<F>(&self, root: PathBuf, build: F)
    where
        F: Fn(String) -> Result<Value, String> + Send + 'static,
    {
        {
            let (lock, _) = &*self.inner;
            let mut state = lock.lock();
            state.root = root;
            state.generation += 1;
            state.status = IndexStatus::NotStarted;
            state.started = None;
            state.finished = None;
            state.error = None;
        }
        self.start(build);
    }

    pub fn status(&self) -> Value {
        let (lock, _) = &*self.inner;
        let state = lock.lock();
        json!({
            "status": state.status.as_str(),
            "root": state.root.to_string_lossy(),
            "error": state.error,
            "elapsed": state.started.map(|start| {
                state.finished.unwrap_or_else(Instant::now).duration_since(start).as_secs_f64()
            }),
        })
    }

    /// Wait up to `budget`, emitting a stderr heartbeat every `heartbeat`.
    pub fn wait(&self, budget: Duration, heartbeat: Duration) -> Value {
        let (lock, wake) = &*self.inner;
        let deadline = Instant::now() + budget;
        let mut last_beat = Instant::now();
        loop {
            let state = lock.lock();
            if state.status != IndexStatus::Indexing {
                // parking_lot mutexes are not reentrant: drop this guard
                // before `status()` takes the same lock.
                drop(state);
                return self.status();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return json!({
                    "status": "indexing",
                    "ready": false,
                    "root": state.root.to_string_lossy(),
                    "elapsed": state.started.map(|s| s.elapsed().as_secs_f64()),
                    "message": "still indexing",
                });
            }
            let mut state = state;
            let _ = wake.wait_for(&mut state, remaining.min(heartbeat));
            if last_beat.elapsed() >= heartbeat {
                eprintln!("[coderadar-rs] still indexing...");
                let _ = std::io::Write::flush(&mut std::io::stderr());
                last_beat = Instant::now();
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        !matches!(self.status()["status"].as_str(), Some("ready"))
    }
}

/// Keep a stdio MCP server tied to its launching process.
///
/// The watchdog polls once a second and exits cleanly when the parent is
/// gone. `CODERADAR_PARENT_WATCHDOG=0` disables it for embedded callers.
pub struct ParentWatchdog {
    parent_pid: u32,
}
impl ParentWatchdog {
    /// Start the watchdog when a parent pid can be discovered.
    pub fn start() -> Option<Self> {
        if std::env::var("CODERADAR_PARENT_WATCHDOG").as_deref() == Ok("0") {
            return None;
        }
        let Some(parent_pid) = parent_pid() else {
            if std::env::var_os("CODERADAR_WATCHDOG_DEBUG").is_some() {
                eprintln!("[coderadar-rs] parent pid unavailable; watchdog disabled");
            }
            return None;
        };
        if std::env::var_os("CODERADAR_WATCHDOG_DEBUG").is_some() {
            eprintln!("[coderadar-rs] parent watchdog follows {parent_pid}");
        }
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let alive = process_alive(parent_pid);
            if std::env::var_os("CODERADAR_WATCHDOG_DEBUG").is_some() {
                eprintln!("[coderadar-rs] parent {parent_pid} alive={alive}");
            }
            if !alive {
                eprintln!("[coderadar-rs] MCP parent {parent_pid} exited");
                std::process::exit(0);
            }
        });
        Some(Self { parent_pid })
    }

    /// The pid this watchdog follows.
    pub fn parent_pid(&self) -> u32 {
        self.parent_pid
    }
}

fn parent_pid() -> Option<u32> {
    #[cfg(windows)]
    {
        let script = format!(
            "(Get-CimInstance Win32_Process -Filter \"ProcessId={}\").ParentProcessId",
            std::process::id()
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
    #[cfg(not(windows))]
    {
        if let Ok(text) = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())) {
            return text.split_whitespace().nth(3)?.parse().ok();
        }
        let output = Command::new("ps")
            .args(["-o", "ppid=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
}

fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(true)
    }
    #[cfg(not(windows))]
    {
        if std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(true)
    }
}

/// Exit if no inbound MCP message arrives within the handshake budget.
pub struct HandshakeGuard {
    stop: Arc<(Mutex<bool>, Condvar)>,
}
impl HandshakeGuard {
    pub fn arm(seconds: u64) -> Self {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        if seconds > 0 {
            let watcher = Arc::clone(&stop);
            std::thread::spawn(move || {
                let (lock, wake) = &*watcher;
                let mut stopped = lock.lock();
                if !*stopped {
                    let _ = wake.wait_for(&mut stopped, Duration::from_secs(seconds));
                }
                if !*stopped {
                    eprintln!("[coderadar-rs] no MCP handshake within {seconds}s");
                    std::process::exit(0);
                }
            });
        }
        Self { stop }
    }
    pub fn disarm(&self) {
        let (lock, wake) = &*self.stop;
        *lock.lock() = true;
        wake.notify_all();
    }
}

/// One active `roots/list` probe for a stdio session.
///
/// The server asks the client once, on the first tool call, when the current
/// root is unconfirmed or the background index is not ready. The caller owns
/// the transport: this type only allocates the id and waits for the matching
/// reply.
struct RootProbe {
    asked: Mutex<bool>,
    pending: Arc<(Mutex<Option<i64>>, Condvar)>,
    next_id: AtomicI64,
}
impl Default for RootProbe {
    fn default() -> Self {
        Self {
            asked: Mutex::new(false),
            pending: Arc::new((Mutex::new(None), Condvar::new())),
            // Negative ids keep server-to-client requests distinct from the
            // client's own tool-call ids.
            next_id: AtomicI64::new(-1_000_000),
        }
    }
}
impl RootProbe {
    fn request(&self, needed: bool) -> Option<Value> {
        if !needed || *self.asked.lock() {
            return None;
        }
        *self.asked.lock() = true;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        *self.pending.0.lock() = Some(id);
        Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "roots/list",
            "params": {},
        }))
    }

    fn accept(&self, message: &Value) -> Option<Vec<String>> {
        let id = message.get("id").and_then(Value::as_i64)?;
        if *self.pending.0.lock() != Some(id) {
            return None;
        }
        let uris = message
            .get("result")
            .and_then(|result| result.get("roots"))
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("uri").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        *self.pending.0.lock() = None;
        self.pending.1.notify_all();
        Some(uris)
    }

    fn wait(&self, timeout: Duration) {
        let (lock, wake) = &*self.pending;
        let mut pending = lock.lock();
        if pending.is_some() {
            let _ = wake.wait_for(&mut pending, timeout);
        }
    }
}

/// The process-wide MCP session: resolved root, lazy retry, index.
///
/// The stdio crate supplies one build callback that performs the cold start
/// and loads the projection; this type owns every decision around it.
#[derive(Clone)]
pub struct McpSession {
    launch_cwd: PathBuf,
    root: Arc<Mutex<Value>>,
    lazy: Arc<Mutex<Option<LazyRootRetry>>>,
    roots_probe: Arc<RootProbe>,
    pub index: BackgroundIndex,
}
impl McpSession {
    pub fn new(launch_cwd: PathBuf, path_flag: Option<&str>) -> Self {
        let resolved = resolve_root(&[], path_flag, None, Some(&launch_cwd));
        Self {
            launch_cwd,
            root: Arc::new(Mutex::new(resolved.json())),
            lazy: Arc::new(Mutex::new(None)),
            roots_probe: Arc::new(RootProbe::default()),
            index: BackgroundIndex::new(resolved.path),
        }
    }

    pub fn root(&self) -> Value {
        self.root.lock().clone()
    }

    pub fn describe(&self) -> String {
        super::api::mcp_describe(&self.root())
    }

    pub fn begin<F>(&self, build: F)
    where
        F: Fn(PathBuf) -> Result<Value, String> + Send + 'static,
    {
        self.index.start(move |root| build(PathBuf::from(root)));
    }

    fn client_roots(message: &Value) -> Vec<String> {
        let params = message.get("params").unwrap_or(&Value::Null);
        for key in ["roots", "clientRoots"] {
            if let Some(rows) = params.get(key).and_then(Value::as_array) {
                return rows
                    .iter()
                    .filter_map(|row| row.as_str().map(str::to_string))
                    .collect();
            }
        }
        Vec::new()
    }

    pub fn on_message(&self, message: &Value) {
        if let Some(uris) = self.roots_probe.accept(message) {
            if let Some(lazy) = self.lazy.lock().as_mut() {
                lazy.set_client_roots(uris);
            }
            return;
        }
        if message.get("method").and_then(Value::as_str) != Some("initialize") {
            return;
        }
        let mut lazy = self.lazy.lock();
        if lazy.is_none() {
            *lazy = Some(LazyRootRetry::new(Self::client_roots(message)));
        }
    }

    /// Build the one `roots/list` request for a first tool call.
    pub fn roots_request(&self) -> Option<Value> {
        let root = self.root();
        let confirmed = root["confirmed"] == true;
        let empty = self.index.is_empty();
        let needed = self
            .lazy
            .lock()
            .as_ref()
            .is_some_and(|lazy| lazy.should_ask(confirmed, empty));
        self.roots_probe.request(needed)
    }

    /// Serialized `roots/list` request, ready for the stdio writer.
    pub fn roots_request_line(&self) -> Option<String> {
        self.roots_request()
            .and_then(|request| serde_json::to_string(&request).ok())
    }

    /// Wait briefly for the client's `roots/list` response.
    pub fn wait_for_roots(&self) {
        let timeout = std::env::var("CODERADAR_ROOTS_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1000);
        self.roots_probe.wait(Duration::from_millis(timeout));
    }

    pub fn ensure_ready(&self) -> Result<(), String> {
        let wait = std::env::var("CODERADAR_INDEX_WAIT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(25);
        let outcome = self
            .index
            .wait(Duration::from_secs(wait), Duration::from_secs(5));
        if outcome["status"] == "failed" {
            return Err(outcome["error"]
                .as_str()
                .unwrap_or("indexing failed")
                .to_string());
        }
        if outcome["ready"] == false && outcome["status"] == "indexing" {
            return Err(format!(
                "Still indexing {} for {:.0}s — keep the session open",
                outcome["root"].as_str().unwrap_or("?"),
                outcome["elapsed"].as_f64().unwrap_or(0.0)
            ));
        }
        Ok(())
    }

    pub fn mark_user_chosen(&self) {
        if let Some(lazy) = self.lazy.lock().as_ref() {
            lazy.mark_user_chosen();
        }
    }

    pub fn retarget<F>(&self, build: F) -> Option<PathBuf>
    where
        F: Fn(PathBuf) -> Result<Value, String> + Send + 'static,
    {
        let lazy = self.lazy.lock().clone()?;
        let current = self.root();
        let empty = self.index.is_empty();
        let confirmed = current["confirmed"] == true;
        if !lazy.should_ask(confirmed, empty) {
            return None;
        }
        let candidate = lazy.attempt(&ResolvedRoot {
            path: PathBuf::from(current["path"].as_str().unwrap_or(".")),
            source: current["source"].as_str().unwrap_or("cwd").to_string(),
            marker: current["marker"].as_str().map(PathBuf::from),
        })?;
        *self.root.lock() = candidate.json();
        record_project(&self.launch_cwd, &candidate.path);
        self.index.restart(candidate.path.clone(), move |root| {
            build(PathBuf::from(root))
        });
        Some(candidate.path)
    }

    pub fn set_project<F>(&self, raw: &str, confirm: bool, build: F) -> Result<PathBuf, String>
    where
        F: Fn(PathBuf) -> Result<Value, String> + Send + 'static,
    {
        let selected = resolve_selector(raw)
            .map(|root| root.json())
            .unwrap_or_else(|| json!({"path": raw, "source": "project_path", "confirmed": false}));
        let path = PathBuf::from(selected["path"].as_str().unwrap_or(raw));
        if selected.get("confirmed").and_then(Value::as_bool) != Some(true) && !confirm {
            return Err(format!(
                "InvalidRequest: No `.coderadar/` or `.coderadar.toml` was found at or above `{}`, so nothing confirms that directory as a project root. Run `coderadar init` there if it should be one, or re-call with confirm=true to serve it anyway.",
                path.display()
            ));
        }
        *self.root.lock() = selected;
        record_project(&self.launch_cwd, &path);
        self.mark_user_chosen();
        self.index
            .restart(path.clone(), move |root| build(PathBuf::from(root)));
        Ok(path)
    }
}

/// Parse stdio JSON-RPC messages off the dispatcher thread.
///
/// The session clone handles `roots/list` replies as they arrive so a tool
/// call can wait for them without blocking the reader.
pub fn stdin_messages(session: &McpSession) -> Receiver<Result<Value, String>> {
    let (tx, rx) = mpsc::channel();
    let reader = session.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let parsed = match line {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => serde_json::from_str::<Value>(&line).map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if let Ok(message) = &parsed {
                reader.on_message(message);
            }
            if tx.send(parsed).is_err() {
                break;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_wins_over_a_higher_unconfirmed_rung() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".coderadar")).expect("marker");
        std::fs::create_dir_all(project.join("src/deep")).expect("nested");
        let root = resolve_root(
            &[],
            Some(project.join("src/deep").to_str().unwrap()),
            None,
            None,
        );
        assert!(root.confirmed());
        assert_eq!(
            root.path,
            std::fs::canonicalize(&project).expect("canonical")
        );
        assert_eq!(root.source, "--path");
    }

    #[test]
    fn directory_candidate_is_canonical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = resolve_root(&[], Some(dir.path().to_str().unwrap()), None, None);
        assert_eq!(root.source, "--path");
        assert!(root.path.is_dir());
    }

    #[test]
    fn client_roots_are_decoded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uri = format!("file://{}", dir.path().display());
        let path = uri_to_path(&uri).expect("uri");
        assert_eq!(path, std::fs::canonicalize(dir.path()).expect("canonical"));
    }

    #[test]
    fn wait_returns_for_a_non_running_index() {
        let index = BackgroundIndex::new(PathBuf::from("."));
        let outcome = index.wait(Duration::from_millis(50), Duration::from_millis(10));
        assert_eq!(outcome["status"], "not_started");
    }

    #[test]
    fn roots_probe_is_one_shot_and_accepts_the_reply() {
        let probe = RootProbe::default();
        let request = probe.request(true).expect("roots request");
        let id = request["id"].as_i64().expect("request id");
        assert!(id < 0, "server requests use negative ids");
        assert_eq!(probe.request(true), None);
        let reply = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"roots": [{"uri": "file:///tmp/example", "name": "example"}]}
        });
        assert_eq!(
            probe.accept(&reply).expect("matching reply"),
            vec!["file:///tmp/example"]
        );
    }

    #[test]
    fn parent_probe_sees_a_live_process() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(u32::MAX));
    }
}
