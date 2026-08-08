// CodeRadar v3.3 — File Watcher (§17)
// Uses notify for cross-platform file events with debounce, dedup, and batching.

use std::path::PathBuf;

use crate::mutation::write_guard::WriteGuard;

/// Description of a file change event.
#[derive(Clone, Debug)]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub content_hash: Option<String>,
    pub trace_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChangeKind {
    Create,
    Modify,
    Delete,
}

/// A batch of file changes emitted together.
#[derive(Clone, Debug)]
pub struct BatchEvent {
    pub trace_id: String,
    pub changes: Vec<FileChange>,
    pub trigger: String,
    pub timestamp: u64,
}

/// Configuration for the file watcher.
#[derive(Clone, Debug)]
pub struct WatcherConfig {
    pub watch_paths: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub debounce_ms: u64,
    pub max_file_size_bytes: u64,
    pub log_level: String,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            watch_paths: vec!["src/".into(), "tests/".into()],
            exclude_patterns: vec![
                ".generated".into(),
                ".pb.go".into(),
                ".g.dart".into(),
            ],
            debounce_ms: 50,
            max_file_size_bytes: 1_048_576,
            log_level: "info".into(),
        }
    }
}

/// File watcher stub — full notify+debounce implementation gated behind 'watcher' feature.
pub struct FileWatcher {
    config: WatcherConfig,
}

impl FileWatcher {
    pub fn new(config: WatcherConfig, _write_guard: Option<std::sync::Arc<WriteGuard>>) -> Result<Self, WatcherError> {
        Ok(Self { config })
    }

    pub fn next_batch(&mut self) -> Option<BatchEvent> {
        None
    }

    pub fn is_excluded(&self, path: &str) -> bool {
        for pattern in &self.config.exclude_patterns {
            if path.contains(pattern.as_str()) {
                return true;
            }
        }
        false
    }
}

#[derive(Debug)]
pub enum WatcherError {
    Init(String),
    Watch(String, String),
    NoEvents,
}
