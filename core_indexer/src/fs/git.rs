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
    old_rev: Option<&str>,
    new_rev: Option<&str>,
) -> Result<Vec<String>, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    // R2-6: unknown revisions used to degrade to None (two `.ok()`
    // swallows) and report an empty diff -- indistinguishable from "no
    // changes". Resolve strictly instead, accepting rev syntax (HEAD,
    // HEAD~1, branches, tags) that Oid::from_str never could.
    let resolve = |rev: &str| -> Result<git2::Tree, GitError> {
        repo.revparse_single(rev)
            .map_err(|_| GitError::UnknownRevision(rev.to_string()))?
            .peel_to_tree()
            .map_err(|_| GitError::UnknownRevision(rev.to_string()))
    };
    let old_tree = old_rev.map(resolve).transpose()?;
    // Documented CLI default: --new falls back to HEAD. A repo without HEAD
    // (fresh, nothing committed) has nothing to diff against: None, which
    // yields the same empty result as before.
    let new_tree = match new_rev {
        Some(rev) => Some(resolve(rev)?),
        None => repo.head().ok().and_then(|h| h.peel_to_tree().ok()),
    };
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
    /// R2-6: a revision string that resolves to nothing (bad hex, unknown
    /// object, unpeelable type). Surfaced instead of diffing empty.
    UnknownRevision(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shared by the R2-8 (clean) and R2-6 (revision) regression tests.
    pub(super) fn init_commit_repo(dir: &std::path::Path) {
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

#[cfg(test)]
mod revision_tests {
    use super::tests::init_commit_repo;
    use super::*;

    fn commit_file(dir: &std::path::Path, name: &str, msg: &str) {
        std::fs::write(dir.join(name), "v\n").unwrap();
        let repo = Repository::open(dir).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new(name))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t.t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
            .unwrap();
    }

    #[test]
    fn unknown_revisions_error_instead_of_diffing_empty() {
        // R2-6: garbage hex AND well-formed-but-unknown OIDs errored into
        // an empty diff. Both must surface UnknownRevision now.
        let dir = tempfile::tempdir().unwrap();
        init_commit_repo(dir.path());
        let root = dir.path().to_str().unwrap();
        assert!(matches!(
            changed_files_between(root, Some("deadbeef"), None),
            Err(GitError::UnknownRevision(_))
        ));
        assert!(matches!(
            changed_files_between(root, Some(&"a".repeat(40)), None),
            Err(GitError::UnknownRevision(_))
        ));
        // Same-rev diff is honestly empty (not an error).
        let files = changed_files_between(root, Some("HEAD"), Some("HEAD")).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn rev_syntax_and_head_default_work() {
        // R2-6: HEAD~1 never parsed as hex (silently diffed nothing); a bare
        // --old now diffs against the documented HEAD default.
        let dir = tempfile::tempdir().unwrap();
        init_commit_repo(dir.path());
        commit_file(dir.path(), "b.txt", "t2");
        let root = dir.path().to_str().unwrap();
        let files = changed_files_between(root, Some("HEAD~1"), None).unwrap();
        assert!(
            files.iter().any(|f| f == "b.txt"),
            "HEAD~1..HEAD must list b.txt, got {files:?}"
        );
    }
}
