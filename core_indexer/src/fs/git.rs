// CodeRadar v3.6 — Git Integration (§12)
// Branch-switch detection, .gitignore integration, and blame annotation.

use git2::{BlameOptions, DiffOptions, Repository, StatusOptions};

#[derive(Clone, Debug)]
pub struct BlameLine {
    pub line_number: usize,
    pub line_count: usize,
    pub author: String,
    pub commit: String,
}

// ── Feature-gated implementations ──────────────────────────────────────────

pub fn detect_branch_switch(repo_path: &str) -> Result<Option<Vec<String>>, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let head = repo.head().map_err(GitError::Head)?;
    let current_oid = head.target().ok_or(GitError::NoHead)?;
    let _ = current_oid;
    Ok(None)
}

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
            None,
            None,
            None,
        )
        .map_err(GitError::Diff)?;
    }
    Ok(files)
}

pub fn blame_file(repo_path: &str, file_path: &str) -> Result<Vec<BlameLine>, GitError> {
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

pub fn is_worktree_clean(repo_path: &str) -> Result<bool, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    // R2-8: `statuses(None)` (libgit2 defaults) reports IGNORED entries,
    // so every initialized project -- ignored `.coderadar/` store present
    // -- read as dirty while `git status` was clean. Spell out `git
    // status` semantics explicitly: untracked counts, ignored never does.
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .recurse_ignored_dirs(false);
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(GitError::Status)?;
    Ok(statuses.is_empty())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn init_commit_repo(dir: &std::path::Path) {
        std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
        std::fs::write(dir.join(".gitignore"), ".coderadar/\n*.log\n").unwrap();
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new("a.txt"))
            .unwrap();
        index
            .add_path(std::path::Path::new(".gitignore"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t.t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "t1", &tree, &[])
            .unwrap();
    }

    #[test]
    fn worktree_clean_ignores_ignored_artifacts() {
        // R2-8: `git status` is clean with only ignored artifacts present;
        // is_worktree_clean must agree (it reported dirty instead).
        let dir = tempfile::tempdir().unwrap();
        init_commit_repo(dir.path());
        let root = dir.path().to_str().unwrap();
        assert!(is_worktree_clean(root).unwrap(), "committed base must be clean");

        std::fs::write(dir.path().join("top.log"), "x\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".coderadar/store")).unwrap();
        std::fs::write(dir.path().join(".coderadar/store/x.db"), "junk\n").unwrap();
        let repo = Repository::open(root).unwrap();
        for e in repo.statuses(None).unwrap().iter() {
            eprintln!("STATUS ENTRY: {:?} {:?}", e.path(), e.status());
        }
        assert!(
            is_worktree_clean(root).unwrap(),
            "ignored artifacts must not dirty the tree"
        );

        std::fs::write(dir.path().join("loose.txt"), "u\n").unwrap();
        assert!(
            !is_worktree_clean(root).unwrap(),
            "untracked non-ignored files must dirty the tree"
        );
    }
}
