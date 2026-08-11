// CodeRadar v3.6 — Mutation: WriteGuard — Watcher Self-Write Suppression (§11.5)
// Prevents the file watcher from triggering on files the mutation engine wrote.

use std::path::PathBuf;
use std::time::Instant;

use dashmap::DashMap;

/// Guards against double-indexing files that the mutation engine writes.
///
/// When the engine writes mutated files, the watcher would otherwise trigger
/// on them. WriteGuard.suppress() inserts a TTL entry. The watcher checks
/// should_drop() for every debounced event.
pub struct WriteGuard {
    suppressed: DashMap<PathBuf, (String, Instant)>,
}

impl WriteGuard {
    pub fn new() -> Self {
        Self {
            suppressed: DashMap::new(),
        }
    }

    /// Register a file path with its expected content hash.
    /// The watcher will suppress events for this path for TTL duration (default 5s).
    pub fn suppress(&self, path: PathBuf, expected_hash: String, ttl_secs: u64) {
        self.suppressed
            .insert(path, (expected_hash, Instant::now() + std::time::Duration::from_secs(ttl_secs)));
    }

    /// Check if a file event should be dropped (the engine just wrote it).
    /// Returns true if the event should be suppressed.
    pub fn should_drop(&self, path: &PathBuf, current_hash: &str) -> bool {
        if let Some(entry) = self.suppressed.get(path) {
            let (expected_hash, expiry) = entry.value();
            if Instant::now() > *expiry {
                // TTL expired — remove stale entry
                drop(entry);
                self.suppressed.remove(path);
                return false;
            }
            // If hash matches expected, suppress
            if expected_hash == current_hash || expected_hash.is_empty() {
                return true;
            }
        }
        false
    }

    /// Clean up expired entries.
    pub fn prune_expired(&self) {
        let now = Instant::now();
        self.suppressed
            .retain(|_, (_, expiry)| now <= *expiry);
    }

    /// Remove a path from the guard (e.g., after successful reindex).
    pub fn clear(&self, path: &PathBuf) {
        self.suppressed.remove(path);
    }

    /// Number of active suppressed paths.
    pub fn len(&self) -> usize {
        self.suppressed.len()
    }
}

impl Default for WriteGuard {
    fn default() -> Self {
        Self::new()
    }
}
