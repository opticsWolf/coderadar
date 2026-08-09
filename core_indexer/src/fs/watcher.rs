// CodeRadar v3.5 — File Watcher (§17)
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

/// Bridge: converts debouncer events into channel messages.
struct EventBridge {
    tx: Sender<BatchEvent>,
    exclude_patterns: Arc<Vec<String>>,
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

                    let kind = match e.kind {
                        DebouncedEventKind::Any => FileChangeKind::Any,
                        DebouncedEventKind::AnyContinuous => FileChangeKind::Modify,
                        _ => FileChangeKind::Any,
                    };

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
}
