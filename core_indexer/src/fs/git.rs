// CodeRadar v3.6 — Git Integration (§12)
// Branch-switch detection, .gitignore integration, and blame annotation.

use std::path::{Path, PathBuf};

use git2::{
    BlameOptions, Delta, DiffOptions, ObjectType, Repository, StatusOptions, TreeWalkMode,
    TreeWalkResult,
};

use crate::graph::CodeGraph;
use crate::types::Language;

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
    let statuses = repo.statuses(Some(&mut opts)).map_err(GitError::Status)?;
    Ok(statuses.is_empty())
}

/// One commit's ingest input (P5a.1, DR-41): the walker output an ingest
/// consumes, with no ledger writes. `changed` holds added/modified paths
/// that are both indexable and blob-eligible; `deleted` holds indexable
/// paths the commit removed (blob exclusion does not spare a deletion — a
/// retired module must retire whether or not its bytes were stored);
/// `tree` is the commit's full indexable tree, the sweep universe (§0).
/// Times are RFC3339 strings; ties are passed through un-nudged (INV-11's
/// nudge is the writer's job, P5a.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitPlan {
    pub sha: String,
    pub committer_ts: String,
    pub author_ts: String,
    pub parents: Vec<String>,
    pub changed: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    pub tree: Vec<PathBuf>,
}

/// Walk first-parent history, oldest-first, from `from` (exclusive) to the
/// tip of `branch` (P5a.1). `from` is the ingest high-water mark: a commit
/// already ingested on this lineage. Unknown branches refuse; a `from`
/// that is not on the first-parent chain refuses rather than silently
/// re-ingesting or skipping history.
pub fn plan_ingest(
    repo_path: &str,
    branch: &str,
    from: Option<&str>,
) -> Result<Vec<CommitPlan>, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let tip = repo
        .revparse_single(branch)
        .map_err(|_| GitError::UnknownBranch(branch.to_string()))?
        .peel_to_commit()
        .map_err(|_| GitError::UnknownBranch(branch.to_string()))?;
    let mut chain = Vec::new();
    let mut current = Some(tip);
    while let Some(commit) = current {
        let parent = commit.parent(0).ok();
        chain.push(commit);
        current = parent;
    }
    chain.reverse();
    let start = match from {
        None => 0,
        Some(rev) => {
            let oid = repo
                .revparse_single(rev)
                .map_err(|_| GitError::UnknownRevision(rev.to_string()))?
                .peel_to_commit()
                .map_err(|_| GitError::UnknownRevision(rev.to_string()))?
                .id();
            match chain.iter().position(|commit| commit.id() == oid) {
                Some(index) => index + 1,
                None => return Err(GitError::NotAncestor(rev.to_string())),
            }
        }
    };
    let excludes = blob_exclusion_matcher();
    let mut plans = Vec::with_capacity(chain.len().saturating_sub(start));
    for commit in chain.iter().skip(start) {
        plans.push(commit_plan_for_commit(&repo, commit, &excludes)?);
    }
    Ok(plans)
}

/// Newest commit on `branch`'s first-parent chain that the parent ref's tip
/// still reaches (P5a.4): the ingest's first-run window start, and the fork
/// commit recorded in `kv`.
///
/// The scan walks newest-first and takes the first hit, so it is robust to
/// the two shapes where a merge-base is not the answer: a branch that merged
/// the trunk into itself (the merge commit is not parent-reachable, the fork
/// stays where the branch left the trunk) and criss-cross merges (newest
/// shared point wins — it is the INV-10 boundary, not a semantic merge-base).
/// An orphan branch with nothing in common refuses (`NoCommonHistory`);
/// unknown names surface as the walker's existing typed errors.
pub fn fork_point(repo_path: &str, branch: &str, parent_ref: &str) -> Result<String, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let tip = repo
        .revparse_single(branch)
        .map_err(|_| GitError::UnknownBranch(branch.to_string()))?
        .peel_to_commit()
        .map_err(|_| GitError::UnknownBranch(branch.to_string()))?;
    let parent_tip = repo
        .revparse_single(parent_ref)
        .map_err(|_| GitError::UnknownRevision(parent_ref.to_string()))?
        .peel_to_commit()
        .map_err(|_| GitError::UnknownRevision(parent_ref.to_string()))?
        .id();
    let mut current = Some(tip);
    while let Some(commit) = current {
        // `graph_descendant_of` is strict: a commit is not its own
        // descendant, so the parent tip itself (the common case — the
        // branch forked straight off the trunk) needs the equality arm.
        // Without it the scan slides past the true fork to an older
        // commit, and the ingest re-writes the fork commit onto the
        // branch until D-225 refuses it mid-bulk.
        if commit.id() == parent_tip
            || repo
                .graph_descendant_of(parent_tip, commit.id())
                .unwrap_or(false)
        {
            return Ok(commit.id().to_string());
        }
        current = commit.parent(0).ok();
    }
    Err(GitError::NoCommonHistory {
        branch: branch.to_string(),
        parent: parent_ref.to_string(),
    })
}

/// Raw bytes of `path` as committed in `sha` (P5a.1): the walker feeds git
/// blob bytes, so no commit is ever checked out. `sha` must resolve to a
/// commit and `path` must name a blob in that commit's tree.
pub fn blob_bytes(repo_path: &str, sha: &str, path: &Path) -> Result<Vec<u8>, GitError> {
    let repo = Repository::open(repo_path).map_err(GitError::Open)?;
    let commit = repo
        .revparse_single(sha)
        .map_err(|_| GitError::UnknownRevision(sha.to_string()))?
        .peel_to_commit()
        .map_err(|_| GitError::UnknownRevision(sha.to_string()))?;
    let tree = commit.tree().map_err(GitError::Commit)?;
    let entry = tree.get_path(path).map_err(|_| GitError::PathNotFound {
        sha: sha.to_string(),
        path: path.to_string_lossy().to_string(),
    })?;
    let blob = entry
        .to_object(&repo)
        .map_err(GitError::Commit)?
        .peel_to_blob()
        .map_err(|_| GitError::PathNotFound {
            sha: sha.to_string(),
            path: path.to_string_lossy().to_string(),
        })?;
    Ok(blob.content().to_vec())
}

fn commit_plan_for_commit(
    repo: &Repository,
    commit: &git2::Commit,
    excludes: &Option<ignore::gitignore::Gitignore>,
) -> Result<CommitPlan, GitError> {
    let tree = commit.tree().map_err(GitError::Commit)?;
    let mut changed = Vec::new();
    let mut deleted = Vec::new();
    if commit.parent_count() == 0 {
        for path in indexable_tree_paths(&tree)? {
            if is_blob_eligible(&path, excludes) {
                changed.push(path);
            }
        }
    } else {
        let parent = commit.parent(0).map_err(GitError::Commit)?;
        let parent_tree = parent.tree().map_err(GitError::Commit)?;
        let mut diff_opts = DiffOptions::new();
        let diff = repo
            .diff_tree_to_tree(Some(&parent_tree), Some(&tree), Some(&mut diff_opts))
            .map_err(GitError::Diff)?;
        diff.foreach(
            &mut |delta, _| {
                match delta.status() {
                    Delta::Added | Delta::Modified | Delta::Copied => {
                        if let Some(path) = delta.new_file().path() {
                            let path = PathBuf::from(path);
                            if is_blob_eligible(&path, excludes) {
                                changed.push(path);
                            }
                        }
                    }
                    Delta::Deleted => {
                        if let Some(path) = delta.old_file().path() {
                            let path = PathBuf::from(path);
                            if is_indexable_path(&path) {
                                deleted.push(path);
                            }
                        }
                    }
                    Delta::Renamed => {
                        if let Some(path) = delta.new_file().path() {
                            let path = PathBuf::from(path);
                            if is_blob_eligible(&path, excludes) {
                                changed.push(path);
                            }
                        }
                        if let Some(path) = delta.old_file().path() {
                            let path = PathBuf::from(path);
                            if is_indexable_path(&path) {
                                deleted.push(path);
                            }
                        }
                    }
                    Delta::Typechange => {
                        if let Some(path) = delta.new_file().path() {
                            let path = PathBuf::from(path);
                            if is_blob_eligible(&path, excludes) {
                                changed.push(path);
                            } else if let Some(old) = delta.old_file().path() {
                                let old = PathBuf::from(old);
                                if is_indexable_path(&old) {
                                    deleted.push(old);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                true
            },
            None,
            None,
            None,
        )
        .map_err(GitError::Diff)?;
    }
    let mut tree_paths = indexable_tree_paths(&tree)?;
    changed.sort();
    changed.dedup();
    deleted.sort();
    deleted.dedup();
    tree_paths.sort();
    tree_paths.dedup();
    Ok(CommitPlan {
        sha: commit.id().to_string(),
        committer_ts: rfc3339_from_git_time(&commit.committer().when()),
        author_ts: rfc3339_from_git_time(&commit.author().when()),
        parents: commit.parent_ids().map(|oid| oid.to_string()).collect(),
        changed,
        deleted,
        tree: tree_paths,
    })
}

fn indexable_tree_paths(tree: &git2::Tree) -> Result<Vec<PathBuf>, GitError> {
    let mut paths = Vec::new();
    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            if let Ok(name) = entry.name() {
                let mut full = PathBuf::from(root);
                full.push(name);
                if is_indexable_path(&full) {
                    paths.push(full);
                }
            }
        }
        TreeWalkResult::Ok
    })
    .map_err(GitError::Diff)?;
    Ok(paths)
}

fn is_indexable_path(path: &Path) -> bool {
    let name = path.to_string_lossy();
    let mut language = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(Language::from_extension)
        .unwrap_or(Language::OtherTen);
    if language == Language::OtherTen {
        language = Language::from_filename(&name);
    }
    if language == Language::OtherTen {
        return false;
    }
    CodeGraph::ts_language(&language).is_some()
}

fn is_blob_eligible(path: &Path, excludes: &Option<ignore::gitignore::Gitignore>) -> bool {
    if !is_indexable_path(path) {
        return false;
    }
    match excludes {
        None => true,
        Some(matcher) => !crate::path_excluded(matcher, path, false),
    }
}

fn blob_exclusion_matcher() -> Option<ignore::gitignore::Gitignore> {
    let patterns = crate::active_config().database.blob_exclude.clone();
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    for pattern in &patterns {
        let _ = builder.add_line(None, pattern);
    }
    builder.build().ok()
}

fn rfc3339_from_git_time(when: &git2::Time) -> String {
    let offset = when.offset_minutes();
    let local = when.seconds() + i64::from(offset) * 60;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let sign = if offset < 0 { '-' } else { '+' };
    let magnitude = offset.abs();
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60,
        magnitude / 60,
        magnitude % 60
    )
}

fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u8;
    let month = if month_prime < 10 {
        (month_prime + 3) as u8
    } else {
        (month_prime - 9) as u8
    };
    let year = if month <= 2 { year + 1 } else { year } as i32;
    (year, month, day)
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
    /// P5a.1: the branch argument resolves to nothing committish.
    UnknownBranch(String),
    /// P5a.1: `from` resolved, but is not on the branch's first-parent
    /// chain — refusing beats silently re-ingesting or skipping history.
    NotAncestor(String),
    /// P5a.1: `path` names no blob in `sha`'s tree (or names a tree).
    PathNotFound {
        sha: String,
        path: String,
    },
    /// P5a.4: `branch` shares no commit with `parent_ref` — an orphan or
    /// unrelated history. A lineage forked from the trunk with nothing in
    /// common is almost certainly a wrong ref, so refusing beats
    /// ingesting an unrelated tree as branch history.
    NoCommonHistory {
        branch: String,
        parent: String,
    },
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
        index.add_path(std::path::Path::new("a.txt")).unwrap();
        index.add_path(std::path::Path::new(".gitignore")).unwrap();
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
        assert!(
            is_worktree_clean(root).unwrap(),
            "committed base must be clean"
        );

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
        index.add_path(std::path::Path::new(name)).unwrap();
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

#[cfg(test)]
mod ingest_plan_tests {
    use super::*;

    fn write_files(dir: &Path, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
    }

    fn stage_paths(repo: &git2::Repository, paths: &[&str]) {
        let mut index = repo.index().unwrap();
        for path in paths {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
    }

    fn remove_paths(repo: &git2::Repository, paths: &[&str]) {
        let mut index = repo.index().unwrap();
        for path in paths {
            index.remove_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
    }

    fn commit_index_to_ref<'a>(
        repo: &'a git2::Repository,
        reference: &str,
        message: &str,
        sig: &git2::Signature<'_>,
        parents: &[&git2::Commit<'_>],
    ) -> git2::Commit<'a> {
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(Some(reference), sig, sig, message, &tree, parents)
            .unwrap();
        repo.find_commit(oid).unwrap()
    }

    fn commit_index<'a>(
        repo: &'a git2::Repository,
        message: &str,
        sig: &git2::Signature<'_>,
        parents: &[&git2::Commit<'_>],
    ) -> git2::Commit<'a> {
        commit_index_to_ref(repo, "HEAD", message, sig, parents)
    }

    fn now_sig() -> git2::Signature<'static> {
        git2::Signature::now("t", "t@t.t").unwrap()
    }

    fn fixed_sig(secs: i64) -> git2::Signature<'static> {
        git2::Signature::new("t", "t@t.t", &git2::Time::new(secs, 0)).unwrap()
    }

    fn linear_repo() -> (tempfile::TempDir, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_files(dir.path(), &[("a.py", "x = 1\n")]);
        stage_paths(&repo, &["a.py"]);
        let c1 = commit_index(&repo, "c1", &now_sig(), &[]);
        write_files(dir.path(), &[("a.py", "x = 2\n"), ("b.py", "y = 3\n")]);
        stage_paths(&repo, &["a.py", "b.py"]);
        let c2 = commit_index(&repo, "c2", &now_sig(), &[&c1]);
        std::fs::remove_file(dir.path().join("b.py")).unwrap();
        remove_paths(&repo, &["b.py"]);
        let c3 = commit_index(&repo, "c3", &now_sig(), &[&c2]);
        let shas = vec![
            c1.id().to_string(),
            c2.id().to_string(),
            c3.id().to_string(),
        ];
        (dir, shas)
    }

    fn assert_rfc3339_shape(ts: &str) {
        assert_eq!(
            ts.len(),
            25,
            "RFC3339 second precision with offset, got {ts}"
        );
        assert_eq!(ts.as_bytes()[10], b'T');
        assert!(ts.ends_with(":00"), "offset must carry minutes, got {ts}");
    }

    #[test]
    fn linear_history_plans_oldest_first_with_delta_and_tree() {
        let (dir, shas) = linear_repo();
        let root = dir.path().to_str().unwrap();
        let plans = plan_ingest(root, "HEAD", None).unwrap();
        assert_eq!(plans.len(), 3);
        for (plan, sha) in plans.iter().zip(shas.iter()) {
            assert_eq!(&plan.sha, sha);
            assert_rfc3339_shape(&plan.committer_ts);
            assert_rfc3339_shape(&plan.author_ts);
        }
        assert!(plans[0].parents.is_empty());
        assert_eq!(plans[1].parents, vec![plans[0].sha.clone()]);
        assert_eq!(plans[2].parents, vec![plans[1].sha.clone()]);
        assert_eq!(plans[0].changed, vec![PathBuf::from("a.py")]);
        assert!(plans[0].deleted.is_empty());
        assert_eq!(plans[0].tree, vec![PathBuf::from("a.py")]);
        assert_eq!(
            plans[1].changed,
            vec![PathBuf::from("a.py"), PathBuf::from("b.py")]
        );
        assert_eq!(
            plans[1].tree,
            vec![PathBuf::from("a.py"), PathBuf::from("b.py")]
        );
        assert!(plans[2].changed.is_empty());
        assert_eq!(plans[2].deleted, vec![PathBuf::from("b.py")]);
        assert_eq!(plans[2].tree, vec![PathBuf::from("a.py")]);
    }

    #[test]
    fn from_is_exclusive_and_must_be_an_ancestor() {
        let (dir, shas) = linear_repo();
        let root = dir.path().to_str().unwrap();
        let after_first = plan_ingest(root, "HEAD", Some(&shas[0])).unwrap();
        assert_eq!(after_first.len(), 2);
        assert_eq!(after_first[0].sha, shas[1]);
        let after_tip = plan_ingest(root, "HEAD", Some(&shas[2])).unwrap();
        assert!(after_tip.is_empty());
        assert!(matches!(
            plan_ingest(root, "nope", None),
            Err(GitError::UnknownBranch(_))
        ));
        assert!(matches!(
            plan_ingest(root, "HEAD", Some("deadbeef")),
            Err(GitError::UnknownRevision(_))
        ));
    }

    #[test]
    fn blob_bytes_round_trips_each_commit() {
        let (dir, shas) = linear_repo();
        let root = dir.path().to_str().unwrap();
        let first = blob_bytes(root, &shas[0], Path::new("a.py")).unwrap();
        assert_eq!(first, b"x = 1\n");
        let second = blob_bytes(root, &shas[1], Path::new("a.py")).unwrap();
        assert_eq!(second, b"x = 2\n");
        assert!(matches!(
            blob_bytes(root, &shas[0], Path::new("missing.py")),
            Err(GitError::PathNotFound { .. })
        ));
    }

    #[test]
    fn rename_lists_new_path_changed_and_old_path_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_files(dir.path(), &[("old.py", "x = 1\n")]);
        stage_paths(&repo, &["old.py"]);
        let c1 = commit_index(&repo, "c1", &now_sig(), &[]);
        write_files(dir.path(), &[("new.py", "x = 1\n")]);
        std::fs::remove_file(dir.path().join("old.py")).unwrap();
        stage_paths(&repo, &["new.py"]);
        remove_paths(&repo, &["old.py"]);
        let c2 = commit_index(&repo, "rename", &now_sig(), &[&c1]);
        let root = dir.path().to_str().unwrap();
        let plans = plan_ingest(root, "HEAD", None).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[1].sha, c2.id().to_string());
        assert_eq!(plans[1].changed, vec![PathBuf::from("new.py")]);
        assert_eq!(plans[1].deleted, vec![PathBuf::from("old.py")]);
        assert_eq!(plans[1].tree, vec![PathBuf::from("new.py")]);
    }

    #[test]
    fn merge_without_first_parent_change_plans_empty_while_resolution_plans() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_files(dir.path(), &[("a.py", "x = 1\n")]);
        stage_paths(&repo, &["a.py"]);
        let base = commit_index(&repo, "base", &now_sig(), &[]);
        write_files(dir.path(), &[("a.py", "x = 2\n")]);
        stage_paths(&repo, &["a.py"]);
        let main = commit_index(&repo, "main", &now_sig(), &[&base]);
        // Side history off the first-parent chain: reset the index to base,
        // add an unrelated file, and commit it directly to its own ref.
        repo.index()
            .unwrap()
            .read_tree(&base.tree().unwrap())
            .unwrap();
        write_files(dir.path(), &[("side.py", "s = 1\n")]);
        stage_paths(&repo, &["side.py"]);
        let side = commit_index_to_ref(&repo, "refs/heads/side", "side", &now_sig(), &[&base]);
        // Merge whose tree equals the first parent's tree: no delta to plan.
        repo.index()
            .unwrap()
            .read_tree(&main.tree().unwrap())
            .unwrap();
        let merged = commit_index(&repo, "merge", &now_sig(), &[&main, &side]);
        // Conflict-resolving merge: the tree differs from the first parent.
        write_files(dir.path(), &[("resolved.py", "r = 1\n")]);
        stage_paths(&repo, &["resolved.py"]);
        let resolved = commit_index(&repo, "resolve", &now_sig(), &[&merged, &side]);
        let root = dir.path().to_str().unwrap();
        let plans = plan_ingest(root, "HEAD", None).unwrap();
        let shas: Vec<String> = plans.iter().map(|plan| plan.sha.clone()).collect();
        assert_eq!(
            shas,
            vec![
                base.id().to_string(),
                main.id().to_string(),
                merged.id().to_string(),
                resolved.id().to_string(),
            ]
        );
        let empty_merge = plans
            .iter()
            .find(|plan| plan.sha == merged.id().to_string())
            .unwrap();
        assert!(empty_merge.changed.is_empty());
        assert!(empty_merge.deleted.is_empty());
        assert_eq!(empty_merge.tree, vec![PathBuf::from("a.py")]);
        let resolution = plans
            .iter()
            .find(|plan| plan.sha == resolved.id().to_string())
            .unwrap();
        assert_eq!(resolution.changed, vec![PathBuf::from("resolved.py")]);
        assert_eq!(
            resolution.tree,
            vec![PathBuf::from("a.py"), PathBuf::from("resolved.py")]
        );
        assert!(matches!(
            plan_ingest(root, "HEAD", Some(&side.id().to_string())),
            Err(GitError::NotAncestor(_))
        ));
    }

    #[test]
    fn tied_stamps_reach_the_caller_unnudged() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = fixed_sig(1_700_000_000);
        write_files(dir.path(), &[("a.py", "x = 1\n")]);
        stage_paths(&repo, &["a.py"]);
        let c1 = commit_index(&repo, "c1", &sig, &[]);
        write_files(dir.path(), &[("a.py", "x = 2\n")]);
        stage_paths(&repo, &["a.py"]);
        let _c2 = commit_index(&repo, "c2", &sig, &[&c1]);
        let root = dir.path().to_str().unwrap();
        let plans = plan_ingest(root, "HEAD", None).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].committer_ts, plans[1].committer_ts);
        assert_eq!(plans[0].committer_ts, "2023-11-14T22:13:20+00:00");
    }

    #[test]
    fn secret_shaped_paths_stay_in_tree_but_leave_changed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_files(
            dir.path(),
            &[("a.py", "x = 1\n"), ("my_secret_helper.py", "token = 1\n")],
        );
        stage_paths(&repo, &["a.py", "my_secret_helper.py"]);
        commit_index(&repo, "c1", &now_sig(), &[]);
        let root = dir.path().to_str().unwrap();
        let plans = plan_ingest(root, "HEAD", None).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].changed, vec![PathBuf::from("a.py")]);
        assert_eq!(
            plans[0].tree,
            vec![PathBuf::from("a.py"), PathBuf::from("my_secret_helper.py")]
        );
    }
}

#[cfg(test)]
mod fork_point_tests {
    use super::*;

    fn write_files_local(dir: &Path, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
    }

    fn stage_paths_local(repo: &git2::Repository, paths: &[&str]) {
        let mut index = repo.index().unwrap();
        for path in paths {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
    }

    // NOTE: the ingest_plan_tests helpers are private to that module; this
    // module re-declares the two it needs rather than widening visibility.
    fn now_sig_local() -> git2::Signature<'static> {
        git2::Signature::now("t", "t@t.t").unwrap()
    }

    fn commit_to<'a>(
        repo: &'a git2::Repository,
        reference: &str,
        message: &str,
        parents: &[&git2::Commit<'_>],
    ) -> git2::Commit<'a> {
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = now_sig_local();
        let oid = repo
            .commit(Some(reference), &sig, &sig, message, &tree, parents)
            .unwrap();
        repo.find_commit(oid).unwrap()
    }

    /// main: m1 - m2 (- m3); feature branched at m2, one commit f1.
    fn forked_repo() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let root = dir.path();
        write_files_local(root, &[("a.py", "x = 1\n")]);
        stage_paths_local(&repo, &["a.py"]);
        let m1 = commit_to(&repo, "refs/heads/main", "m1", &[]);
        write_files_local(root, &[("a.py", "x = 2\n")]);
        stage_paths_local(&repo, &["a.py"]);
        let m2 = commit_to(&repo, "refs/heads/main", "m2", &[&m1]);
        write_files_local(root, &[("b.py", "y = 1\n")]);
        stage_paths_local(&repo, &["b.py"]);
        let f1 = commit_to(&repo, "refs/heads/feature", "f1", &[&m2]);
        write_files_local(root, &[("a.py", "x = 3\n")]);
        stage_paths_local(&repo, &["a.py"]);
        let m3 = commit_to(&repo, "refs/heads/main", "m3", &[&m2]);
        let _ = (f1, m3);
        (dir, m2.id().to_string(), String::new())
    }

    #[test]
    fn fork_point_is_the_branch_point_despite_later_main() {
        let (dir, m2, _) = forked_repo();
        let root = dir.path().to_str().unwrap();
        // main has advanced past the fork (m3); the fork stays at m2.
        assert_eq!(fork_point(root, "feature", "main").unwrap(), m2);
    }

    #[test]
    fn fork_point_survives_a_trunk_merge_into_the_branch() {
        let (dir, m2, _) = forked_repo();
        let root = dir.path().to_str().unwrap();
        let repo = git2::Repository::open(dir.path()).unwrap();
        // feature merges main (second parent): the merge commit is not
        // main-reachable, so the fork stays at m2 and the resolution
        // commit itself is ingested (P5-D7).
        let f1 = repo
            .revparse_single("feature")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let m3 = repo
            .revparse_single("main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        repo.index().unwrap().write_tree().unwrap();
        commit_to(&repo, "refs/heads/feature", "merge main", &[&f1, &m3]);
        assert_eq!(fork_point(root, "feature", "main").unwrap(), m2);
        let plan = plan_ingest(root, "feature", Some(&m2)).unwrap();
        assert_eq!(plan.len(), 2, "merge resolution + branch tip: {plan:?}");
    }

    #[test]
    fn fork_point_unknown_names_and_orphans_refuse() {
        let (dir, _, _) = forked_repo();
        let root = dir.path().to_str().unwrap();
        assert!(matches!(
            fork_point(root, "nope", "main"),
            Err(GitError::UnknownBranch(_))
        ));
        assert!(matches!(
            fork_point(root, "feature", "nope"),
            Err(GitError::UnknownRevision(_))
        ));
        // An orphan root shares nothing with main: almost certainly a
        // wrong ref, so refusing beats ingesting an unrelated tree.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let sig = now_sig_local();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/orphan"), &sig, &sig, "o1", &tree, &[])
            .unwrap();
        assert!(matches!(
            fork_point(root, "orphan", "main"),
            Err(GitError::NoCommonHistory { .. })
        ));
    }
}
