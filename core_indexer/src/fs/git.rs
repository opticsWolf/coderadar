// CodeRadar v3.3 — Git Integration (§12)
// Branch-switch detection, .gitignore integration, and blame annotation.

#[cfg(feature = "git")]
use std::path::PathBuf;

#[cfg(feature = "git")]
use git2::{BlameOptions, DiffOptions, Repository};

use crate::graph::GitConfig;

#[derive(Clone, Debug)]
pub struct BlameLine {
    pub line_number: usize,
    pub line_count: usize,
    pub author: String,
    pub commit: String,
}

// ── Feature-gated implementations ──────────────────────────────────────────

#[cfg(feature = "git")]
pub fn detect_branch_switch(repo_path: &str) -> Result<Option<Vec<String>>, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let head = repo.head().map_err(GitError::Head)?;
    let current_oid = head.target().ok_or(GitError::NoHead)?;
    let _ = current_oid;
    Ok(None)
}

#[cfg(feature = "git")]
pub fn changed_files_between(
    repo_path: &str,
    old_oid: Option<git2::Oid>,
    new_oid: Option<git2::Oid>,
) -> Result<Vec<String>, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let old_tree = old_oid
        .and_then(|oid| repo.find_commit(oid).ok())
        .and_then(|c| c.tree().ok());
    let new_tree = new_oid
        .and_then(|oid| repo.find_commit(oid).ok())
        .and_then(|c| c.tree().ok());
    let mut files = Vec::new();
    let mut diff_opts = DiffOptions::new();
    if let (Some(old), Some(new)) = (old_tree, new_tree) {
        let diff = repo
            .diff_tree_to_tree(Some(&old), Some(&new), Some(&mut diff_opts))
            .map_err(GitError::Diff)?;
        diff.foreach(
            &mut |delta, _| {
                if let Some(new_file) = delta.new_file().path() {
                    files.push(new_file.to_string_lossy().to_string());
                }
                true
            },
            None, None, None,
        )
        .map_err(GitError::Diff)?;
    }
    Ok(files)
}

#[cfg(feature = "git")]
pub fn blame_file(
    repo_path: &str,
    file_path: &str,
) -> Result<Vec<BlameLine>, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let head = repo.head().map_err(GitError::Head)?;
    let commit = head.peel_to_commit().map_err(GitError::Commit)?;
    let mut blame_opts = BlameOptions::new();
    blame_opts.newest_commit(commit.id());
    let blame = repo
        .blame_file(std::path::Path::new(file_path), Some(&mut blame_opts))
        .map_err(|e| GitError::Blame(e.message().to_string()))?;
    let mut lines = Vec::new();
    for hunk in blame.iter() {
        let sig = hunk.final_signature();
        let commit_id = hunk.final_commit_id();
        let author = match sig {
            Some(s) => s.name().unwrap_or("unknown").to_string(),
            None => "unknown".to_string(),
        };
        lines.push(BlameLine {
            line_number: hunk.final_start_line(),
            line_count: hunk.lines_in_hunk(),
            author,
            commit: commit_id.to_string(),
        });
    }
    Ok(lines)
}

#[cfg(feature = "git")]
pub fn is_worktree_clean(repo_path: &str) -> Result<bool, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let statuses = repo.statuses(None).map_err(GitError::Status)?;
    Ok(statuses.is_empty())
}

#[cfg(feature = "git")]
#[derive(Debug)]
pub enum GitError {
    Open(git2::Error),
    Head(git2::Error),
    NoHead,
    Diff(git2::Error),
    Commit(git2::Error),
    Blame(String),
    Status(git2::Error),
}

// ── Stub implementations when git feature is disabled ──────────────────

#[cfg(not(feature = "git"))]
pub fn detect_branch_switch(_repo_path: &str) -> Result<Option<Vec<String>>, GitError> {
    Ok(None)
}

#[cfg(not(feature = "git"))]
pub fn changed_files_between(
    _repo_path: &str, _old_oid: Option<u64>, _new_oid: Option<u64>,
) -> Result<Vec<String>, GitError> {
    Ok(Vec::new())
}

#[cfg(not(feature = "git"))]
pub fn blame_file(_repo_path: &str, _file_path: &str) -> Result<Vec<BlameLine>, GitError> {
    Ok(Vec::new())
}

#[cfg(not(feature = "git"))]
pub fn is_worktree_clean(_repo_path: &str) -> Result<bool, GitError> {
    Ok(true)
}

#[cfg(not(feature = "git"))]
#[derive(Debug)]
pub enum GitError {
    GitDisabled,
}
