// CodeRadar v3.6 — File Watcher (§17)
// Uses notify + notify-debouncer-mini for cross-platform file events.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_mini::{
    DebounceEventHandler, DebounceEventResult, DebouncedEvent, DebouncedEventKind,
    new_debouncer,
};

/// A batch of file changes emitted together.
#[derive(Clone, Debug)]
pub struct BatchEvent {
    pub trace_id: String,
    pub changes: Vec<FileChange>,
}

#[derive(Clone, Debug)]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChangeKind {
    Create,
    Modify,
    Delete,
    Any,
}

/// Configuration for the file watcher.
#[derive(Clone, Debug)]
pub struct WatcherConfig {
    pub watch_paths: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub debounce_ms: u64,
    pub max_file_size_bytes: u64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            watch_paths: vec!["src/".into(), "tests/".into()],
            exclude_patterns: vec![
                "__pycache__".into(),
                ".git".into(),
                "node_modules".into(),
                ".generated".into(),
                ".pb.go".into(),
                ".g.dart".into(),
            ],
            debounce_ms: 100,
            max_file_size_bytes: 1_048_576,
        }
    }
}

/// Decide what actually happened to `path`.
///
/// `notify-debouncer-mini` only ever reports `Any` / `AnyContinuous` — it
/// collapses create, write and remove into one kind — so `FileChangeKind::
/// Delete` was declared and never constructed, and the Python loop's delete
/// branch was dead. Delete a file in watch mode and its entities lived on in
/// the projection, in the ledger, and in every query result.
///
/// The filesystem still knows: a path that is gone when the event is handled
/// was removed (or renamed away, which is a removal of this path either way).
/// The race — deleted then recreated within the debounce window — resolves as
/// "still there", which is the correct answer.
fn classify(path: &std::path::Path, kind: DebouncedEventKind) -> FileChangeKind {
    if !path.exists() {
        return FileChangeKind::Delete;
    }
    match kind {
        DebouncedEventKind::AnyContinuous => FileChangeKind::Modify,
        _ => FileChangeKind::Any,
    }
}

/// Bridge: converts debouncer events into channel messages.
struct EventBridge {
    tx: Sender<BatchEvent>,
    exclude_patterns: Arc<Vec<String>>,
    write_guard: std::sync::Arc<crate::mutation::write_guard::WriteGuard>,
    batch_counter: u64,
}

impl DebounceEventHandler for EventBridge {
    fn handle_event(&mut self, result: DebounceEventResult) {
        if let Ok(events) = result {
            let changes: Vec<FileChange> = events
                .iter()
                .filter_map(|e| {
                    let path = e.path.to_string_lossy().to_string();

                    // Skip excluded paths
                    for pattern in self.exclude_patterns.iter() {
                        if path.contains(pattern.as_str()) {
                            return None;
                        }
                    }

                    // Skip non-source files
                    let ext = std::path::Path::new(&path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    if !ext.is_empty() && !is_source_extension(ext) {
                        return None;
                    }

                    // Suppress events for files the mutation engine just wrote
                    // (WriteGuard). Only hash content if the path is actively
                    // suppressed, to avoid hashing every file on every event.
                    if self.write_guard.is_suppressed(&e.path) {
                        let hash = std::fs::read(&e.path)
                            .ok()
                            .map(|b| format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&b)))
                            .unwrap_or_default();
                        if self.write_guard.should_drop(&e.path, &hash) {
                            return None;
                        }
                    }

                    let kind = classify(&e.path, e.kind);

                    Some(FileChange { path, kind })
                })
                .collect();

            if !changes.is_empty() {
                self.batch_counter += 1;
                let batch = BatchEvent {
                    trace_id: format!("batch-{}", self.batch_counter),
                    changes,
                };
                let _ = self.tx.send(batch);
            }
        }
    }
}

/// Live file watcher using notify + debouncer.
pub struct FileWatcher {
    _debouncer: notify_debouncer_mini::Debouncer<RecommendedWatcher>,
    receiver: Receiver<BatchEvent>,
}

impl FileWatcher {
    /// Start watching paths. Returns immediately; use `next_batch()` for events.
    pub fn start(config: WatcherConfig) -> Result<Self, WatcherError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let exclude_patterns = Arc::new(config.exclude_patterns.clone());
        let debounce_ms = config.debounce_ms;

        let bridge = EventBridge {
            tx,
            exclude_patterns,
            write_guard: crate::mutation::shared_write_guard(),
            batch_counter: 0,
        };

        let mut debouncer = new_debouncer(Duration::from_millis(debounce_ms), bridge)
            .map_err(|e| WatcherError::Init(format!("Failed to create debouncer: {}", e)))?;

        // Watch all configured paths
        for path_str in &config.watch_paths {
            let path = PathBuf::from(path_str);
            if !path.exists() {
                eprintln!("Watcher: path does not exist: {}", path_str);
                continue;
            }
            debouncer
                .watcher()
                .watch(&path, RecursiveMode::Recursive)
                .map_err(|e| WatcherError::Watch(path_str.clone(), e.to_string()))?;
        }

        Ok(Self {
            _debouncer: debouncer,
            receiver: rx,
        })
    }

    /// Block until the next batch of changes arrives.
    pub fn next_batch(&self) -> Option<BatchEvent> {
        self.receiver.recv().ok()
    }

    /// Get the next batch with a timeout in milliseconds.
    /// Returns None if the timeout expires before a batch arrives.
    pub fn next_batch_timeout(&self, timeout_ms: u64) -> Option<BatchEvent> {
        self.receiver
            .recv_timeout(Duration::from_millis(timeout_ms))
            .ok()
    }
}

/// Check if a file extension is a recognized source code extension.
fn is_source_extension(ext: &str) -> bool {
    matches!(
        ext,
        "py" | "pyi" | "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
            | "go" | "java" | "kt" | "kts" | "c" | "h" | "cpp" | "cc" | "cxx"
            | "hpp" | "hxx" | "rb" | "php" | "cs" | "swift" | "scala" | "sc"
            | "lua" | "ex" | "exs" | "zig" | "zon" | "r" | "R" | "sql" | "toml" | "yaml" | "yml" | "json"
            | "md" | "rst" | "sh" | "bash" | "html" | "css" | "vue" | "svelte"
    )
}

#[derive(Debug)]
pub enum WatcherError {
    Init(String),
    Watch(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watcher_config_default() {
        let config = WatcherConfig::default();
        assert_eq!(config.debounce_ms, 100);
        assert!(!config.watch_paths.is_empty());
        assert_eq!(config.max_file_size_bytes, 1_048_576);
    }

    #[test]
    fn test_source_extensions() {
        assert!(is_source_extension("py"));
        assert!(is_source_extension("rs"));
        assert!(is_source_extension("go"));
        assert!(is_source_extension("kt"));
        assert!(is_source_extension("ts"));
        assert!(is_source_extension("cpp"));
        assert!(!is_source_extension("exe"));
        assert!(!is_source_extension("png"));
        assert!(!is_source_extension("zip"));
    }

    #[test]
    fn test_watcher_start_nonexistent_path() {
        let config = WatcherConfig {
            watch_paths: vec!["/nonexistent/path/12345".into()],
            ..WatcherConfig::default()
        };
        // Should succeed but warn about missing path
        // (debouncer may fail if no paths are valid, so we check gracefully)
        let result = FileWatcher::start(config);
        // OK either way — the key is it doesn't panic
        let _ = result;
    }

    // ── Deletion classification (plan §1.3) ──────────────────────────────

    /// The debouncer reports `Any` for a removal exactly as it does for a
    /// write, so the filesystem is the only thing that can tell them apart.
    #[test]
    fn test_a_vanished_path_is_a_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gone.py");

        assert_eq!(
            classify(&path, DebouncedEventKind::Any),
            FileChangeKind::Delete
        );
    }

    #[test]
    fn test_an_existing_path_is_never_a_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("here.py");
        std::fs::write(&path, "x = 1
").unwrap();

        assert_eq!(classify(&path, DebouncedEventKind::Any), FileChangeKind::Any);
        assert_eq!(
            classify(&path, DebouncedEventKind::AnyContinuous),
            FileChangeKind::Modify
        );
    }

    /// Deleted and recreated inside the debounce window: the file is there
    /// when the event is handled, so it is a modification, not a removal.
    #[test]
    fn test_a_recreated_path_is_not_a_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("churn.py");
        std::fs::write(&path, "x = 1
").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "x = 2
").unwrap();

        assert_eq!(classify(&path, DebouncedEventKind::Any), FileChangeKind::Any);
    }
}
