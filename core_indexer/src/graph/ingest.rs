//! Commit-driven history ingest (P5a.3, DR-41).
//!
//! One op that takes a repository and a git branch and writes history:
//! per commit, in first-parent order, blob puts for the changed files,
//! one stamped flush (entity concepts + spine + commit concept), scoped
//! edge persistence, and a sweep scoped to the commit's tree — then the
//! `kv` high-water advance that makes an interrupted ingest resumable.
//!
//! The lineage is always the trunk in P5a.3. A non-trunk lineage cannot
//! restate trunk concepts (Macrame D-225 refuses the write), so branch
//! ingestion — fork, branch-minted concepts, branch deletion semantics —
//! is P5a.4's work, and this op stays on the one lineage it can legally
//! write. Any git branch may be walked: INV-10 (recorded order equals
//! commit topological order) refuses whatever is not a descendant of the
//! high-water mark, so a diverged branch fails closed instead of writing
//! a history that never existed.
//!
//! Reads never change: the ingest walks forward, so recorded order and
//! commit order coincide and every shipped temporal read answers in
//! commit order with no read-path change (P5a §2). Out-of-order history
//! is refused, not reordered (INV-10); tied or regressing committer
//! stamps are nudged, never written backwards (INV-11); one store holds
//! one stamping mode, declared in `kv` (INV-12).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use super::module_resolution::{canonical_file_form_with_root, rebuild_module_path_index};
use super::{CodeGraph, GraphConfig};
use crate::fs::git::{blob_bytes, fork_point, plan_ingest, CommitPlan, GitError};
use crate::storage::{
    branch_pointer_upsert, branch_tombstone_upsert, nudge_valid_from, zulu_valid_from, BlobOutcome,
    CodeGraphStore, SourceBlobDigests,
};

// ── kv mode + cursor keys (P5-D5, INV-12) ─────────────────────────────────

/// Per-store stamping-mode flag: `index-now` (today's analyze) or
/// `commit-time` (this ingest). Written once, checked on every write.
pub const STAMPING_MODE_KEY: &str = "coderadar.stamping_mode";
/// Today's analyze stamping: the clock, per flush.
pub const MODE_INDEX_NOW: &str = "index-now";
/// P5 ingest stamping: the commit's time, per commit.
pub const MODE_COMMIT_TIME: &str = "commit-time";

/// Last ingested commit on a lineage: INV-10's comparison point and what
/// makes an interrupted ingest resumable (H-20: a cursor, never a blob
/// reference, so losing it repeats work instead of losing bytes).
pub fn ingest_head_key(lineage: &str) -> String {
    format!("coderadar.ingest.head.{lineage}")
}

/// The final `valid_from` written for the head commit: INV-11's `prev`
/// across runs, so a resume nudges against the true last stamp.
pub fn ingest_stamp_key(lineage: &str) -> String {
    format!("coderadar.ingest.stamp.{lineage}")
}

/// The fork commit recorded once per branch lineage (P5a.4, DR-41): the
/// newest commit on the branch's first-parent chain the trunk ref still
/// reaches. The ingest window always starts here or at the cursor past
/// it — a lineage's history is complete from its fork, never partial.
/// A cursor, like the head: losing it repeats work instead of losing
/// bytes (H-20).
pub fn ingest_fork_key(lineage: &str) -> String {
    format!("coderadar.ingest.fork.{lineage}")
}

// ── report + errors ───────────────────────────────────────────────────────

/// One skipped commit and why it never wrote.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedCommit {
    pub sha: String,
    pub reason: String,
}

/// One file that got bytes but no concepts, and why.
#[derive(Debug, Clone, Serialize)]
pub struct FailedFile {
    pub sha: String,
    pub path: String,
    pub reason: String,
}

/// The P5a.3 report shape (Q-4: shape as contract). Counts first, then the
/// two auditable lists: `commits_skipped` says which commits never wrote
/// and why, `files_failed` says which files got bytes but no concepts
/// (P5-D9: the sweep stays off for their commit, so the gap is visible
/// rather than silently healed over). P5a.4 adds `fork_point`: the fork
/// commit a branch lineage's window starts at (`None` on the trunk).
/// Additive, never renamed: readers that ignore unknown keys keep working.
#[derive(Debug, Clone, Serialize)]
pub struct IngestReport {
    pub branch: String,
    pub lineage: String,
    pub fork_point: Option<String>,
    pub from: Option<String>,
    pub head: Option<String>,
    pub dry_run: bool,
    pub commits_ingested: usize,
    pub commits_skipped: Vec<SkippedCommit>,
    pub files_parsed: usize,
    pub files_failed: Vec<FailedFile>,
    pub files_excluded: usize,
    pub blobs_stored: usize,
    pub blobs_deduped: usize,
    pub stamps_nudged: usize,
    pub ghost_commits: usize,
    pub elapsed_secs: f64,
}

/// Typed refusals and failures. User-caused variants (unknown branch,
/// unknown revision, out-of-order, legacy store) map to `InvalidRequest`
/// at the FFI boundary; store and git-infrastructure failures map to
/// `EngineError`.
#[derive(Debug)]
pub enum IngestError {
    Git(GitError),
    Store(String),
    UnknownHighWater {
        head: String,
    },
    OutOfOrder {
        head: String,
        from: String,
    },
    NotDescendant {
        from: String,
        first_parent: String,
    },
    IndexNowRefused {
        path: String,
        legacy: bool,
    },
    /// P5a.4: a non-main git branch walked into the trunk lineage. The
    /// trunk is main's first-parent history; branch content belongs on
    /// the branch's lineage (pass `lineage`), where pointers and
    /// branch-minted concepts can hold it without restating trunk rows.
    TrunkBranchMismatch {
        branch: String,
    },
    /// P5a.4: the recorded fork commit and the computed one disagree —
    /// history moved under the lineage (rebase, or the branch now forks
    /// elsewhere). Refusing beats writing two histories onto one lineage.
    ForkMismatch {
        lineage: String,
        recorded: String,
        computed: String,
    },
    /// P5a.4: the lineage exists in the ledger but ingest never forked it
    /// (a manual fork, or a lost `kv` row). The fork commit is unknown,
    /// so the window cannot be validated — refusing beats guessing.
    LineageExists {
        lineage: String,
    },
    /// P5a.4: the lineage name is not a legal `BranchId` (blank,
    /// over-long, control characters, surrounding whitespace).
    InvalidLineage {
        lineage: String,
    },
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::Git(e) => write!(f, "ingest-history [git]: {e:?}"),
            IngestError::Store(e) => write!(f, "ingest-history [store]: {e}"),
            IngestError::UnknownHighWater { head } => write!(
                f,
                "ingest-history [unknown-high-water]: {head:?} is no longer in this repository \
                 (rebase, force-push, or shallow fetch moved history out from under the cursor); \
                 nothing was written"
            ),
            IngestError::OutOfOrder { head, from } => write!(
                f,
                "ingest-history [out-of-order]: refusing to ingest from {from:?}: the store's \
                 head on this lineage is already {head:?} (INV-10: recorded order must equal \
                 commit topological order; pass no --from to resume, or ingest into a fresh store)"
            ),
            IngestError::NotDescendant { from, first_parent } => write!(
                f,
                "ingest-history [not-descendant]: the first planned commit's parent \
                 {first_parent:?} is not the window start {from:?}; refusing rather than \
                 writing a history with a hole in it"
            ),
            IngestError::IndexNowRefused { path, legacy } => {
                let why = if *legacy {
                    "it has no stamping-mode flag, so it is a legacy index-now store"
                } else {
                    "its stamping mode is index-now"
                };
                write!(
                    f,
                    "ingest-history [index-now]: refusing {path:?}: {why} (INV-12: one store, \
                     one stamping mode — ingest into a fresh store path instead)"
                )
            }
            IngestError::TrunkBranchMismatch { branch } => write!(
                f,
                "ingest-history [trunk-branch]: refusing to walk git branch {branch:?} \
                 into the trunk lineage (P5a.4: the trunk is main's first-parent history; \
                 pass lineage={branch:?} to ingest it onto its own lineage)"
            ),
            IngestError::ForkMismatch {
                lineage,
                recorded,
                computed,
            } => write!(
                f,
                "ingest-history [fork-mismatch]: lineage {lineage:?} forked at \
                 {recorded:?}, but the branch now forks at {computed:?} (rebase or a \
                 moved branch point); refusing rather than writing two histories onto \
                 one lineage"
            ),
            IngestError::LineageExists { lineage } => write!(
                f,
                "ingest-history [lineage-exists]: lineage {lineage:?} exists in the \
                 ledger but ingest never forked it (a manual fork, or a lost cursor row); \
                 the fork commit is unknown, so the window cannot be validated"
            ),
            IngestError::InvalidLineage { lineage } => write!(
                f,
                "ingest-history [invalid-lineage]: {lineage:?} is not a legal lineage \
                 name (blank, over-long, control characters, or surrounding whitespace)"
            ),
        }
    }
}

impl From<GitError> for IngestError {
    fn from(e: GitError) -> Self {
        IngestError::Git(e)
    }
}

// ── the op ────────────────────────────────────────────────────────────────

/// Ingest one git branch's first-parent history into the trunk lineage.
///
/// See the module docs for the contract. `from` is an exclusive window
/// start (a commit sha, or `None` to resume from the `kv` cursor / ingest
/// from the root); `dry_run` plans without writing anything, including no
/// store creation and no cursor advance.
pub fn ingest_history(
    store_path: &str,
    repo: &str,
    branch: &str,
    from: Option<&str>,
    dry_run: bool,
) -> Result<IngestReport, IngestError> {
    let started = Instant::now();
    // P5a.3 writes the trunk only (module docs). The lineage rides the
    // report so P5a.4's multi-lineage ingest has the field to fill.
    let lineage = "main";
    // P5a.4: the trunk lineage is main's first-parent history. Walking a
    // diverged branch here would mint trunk entity concepts for branch
    // content — and the branch's own ingest could never mint its own
    // (same id, D-225). Branch content belongs on the branch's lineage.
    if branch != "main" {
        return Err(IngestError::TrunkBranchMismatch {
            branch: branch.to_string(),
        });
    }
    let mut report = IngestReport {
        branch: branch.to_string(),
        lineage: lineage.to_string(),
        fork_point: None,
        from: None,
        head: None,
        dry_run,
        commits_ingested: 0,
        commits_skipped: Vec::new(),
        files_parsed: 0,
        files_failed: Vec::new(),
        files_excluded: 0,
        blobs_stored: 0,
        blobs_deduped: 0,
        stamps_nudged: 0,
        ghost_commits: 0,
        elapsed_secs: 0.0,
    };

    // The store stays closed on a dry run against a path that does not
    // exist yet: planning needs git, not the ledger. An existing store is
    // still opened (and its cursor still checked) so a dry run answers
    // whether the real run would be accepted.
    let store_file = Path::new(store_path);
    let store_exists = store_file.exists();
    if !dry_run && !store_exists {
        if let Some(parent) = store_file.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| IngestError::Store(format!("cannot create {parent:?}: {e}")))?;
            }
        }
    }
    let store: Option<CodeGraphStore> = if dry_run && !store_exists {
        None
    } else {
        Some(
            CodeGraphStore::open(store_file)
                .map_err(|e| IngestError::Store(format!("cannot open {store_path:?}: {e:?}")))?,
        )
    };

    // INV-12: one store, one stamping mode. A store file that predates the
    // flag is a legacy index-now store by definition — mixed stamping is
    // the thing the flag exists to refuse. A fresh file is claimed for
    // commit time (never in a dry run: planning writes nothing).
    if let Some(ref store) = store {
        let mode = store
            .kv_get_sync(STAMPING_MODE_KEY)
            .map_err(|e| IngestError::Store(format!("cannot read stamping mode: {e:?}")))?;
        match mode.as_deref() {
            Some(MODE_COMMIT_TIME) => {}
            Some(MODE_INDEX_NOW) => {
                return Err(IngestError::IndexNowRefused {
                    path: store_path.to_string(),
                    legacy: false,
                })
            }
            Some(other) => {
                return Err(IngestError::Store(format!(
                    "unknown stamping mode {other:?} on {store_path:?}"
                )))
            }
            None if !store_exists => {
                if !dry_run {
                    store
                        .kv_put_sync(STAMPING_MODE_KEY, MODE_COMMIT_TIME)
                        .map_err(|e| IngestError::Store(format!("cannot claim mode: {e:?}")))?;
                }
            }
            None => {
                return Err(IngestError::IndexNowRefused {
                    path: store_path.to_string(),
                    legacy: true,
                })
            }
        }
    }

    // INV-10's comparison points. `stamp_prev` seeds INV-11 across runs.
    let (head, mut stamp_prev): (Option<String>, Option<String>) = match &store {
        Some(store) => (
            store
                .kv_get_sync(&ingest_head_key(lineage))
                .map_err(|e| IngestError::Store(format!("cannot read cursor: {e:?}")))?,
            store
                .kv_get_sync(&ingest_stamp_key(lineage))
                .map_err(|e| IngestError::Store(format!("cannot read stamp: {e:?}")))?,
        ),
        None => (None, None),
    };
    if let Some(ref h) = head {
        // The cursor names a commit the repository no longer has: rebased,
        // force-pushed, or shallow-fetched away (P5-D8). The ledger keeps
        // what it was told; the ingest refuses rather than writing after
        // a commit it can no longer see.
        let git = git2::Repository::open(repo).map_err(GitError::Open)?;
        if git.revparse_single(h).is_err() {
            return Err(IngestError::UnknownHighWater { head: h.clone() });
        }
    }
    let effective_from: Option<String> = match (from, &head) {
        (Some(f), Some(h)) if f != h => {
            return Err(IngestError::OutOfOrder {
                head: h.clone(),
                from: f.to_string(),
            })
        }
        (Some(f), _) => Some(f.to_string()),
        (None, h) => h.clone(),
    };
    report.from = effective_from.clone();
    report.head = head.clone();

    // The walk itself. Unknown branch / revision / ancestry surface here
    // as the walker's typed errors, before anything is written.
    let plan: Vec<CommitPlan> = plan_ingest(repo, branch, effective_from.as_deref())?;
    if let (Some(ef), Some(first)) = (effective_from.as_ref(), plan.first()) {
        if !first.parents.contains(ef) {
            return Err(IngestError::NotDescendant {
                from: ef.clone(),
                first_parent: first.parents.first().cloned().unwrap_or_default(),
            });
        }
    }

    if dry_run {
        for commit in &plan {
            report.commits_skipped.push(SkippedCommit {
                sha: commit.sha.clone(),
                reason: "dry-run".to_string(),
            });
        }
        report.elapsed_secs = started.elapsed().as_secs_f64();
        return Ok(report);
    }
    if plan.is_empty() {
        // Resumed past the tip, or asked to start at it: nothing to do,
        // cursor untouched.
        report.elapsed_secs = started.elapsed().as_secs_f64();
        return Ok(report);
    }
    let store = store.expect("store is opened for every non-dry run");

    // The running projection: empty at the start (a fresh commit-time
    // store holds no entities), accumulated commit by commit. The cascade
    // runs per commit over the whole running projection — full resolution
    // is always correct, and the plan says to measure before optimising.
    let graph = CodeGraph::new(GraphConfig::default()).with_store(store);
    {
        let mut projection = (*graph.snapshot()).clone();
        projection.path_maps =
            crate::graph::path_maps::PathMaps::discover(repo, &graph.config.project, &[]);
        graph.commit_projection(projection);
    }
    // Standard excludes (build dirs, always-on defaults — the same matcher
    // analyze walks with) apply on top of the walker's blob-exclusion and
    // language filtering. A committed `node_modules` tree is not history
    // worth keeping, exactly as it is not a tree worth indexing.
    let std_excludes = crate::exclusion_gitignore(&[]);
    let excluded = |rel: &Path| -> bool {
        match &std_excludes {
            Some(matcher) => crate::path_excluded(matcher, rel, false),
            None => false,
        }
    };

    let repo_root = Path::new(repo);
    let canonical = |rel: &Path| -> String {
        canonical_file_form_with_root(&rel.to_string_lossy().replace('\\', "/"), repo_root)
    };
    let git = git2::Repository::open(repo).map_err(GitError::Open)?;
    let mut query_cache: HashMap<crate::types::Language, crate::extract::tagger::CompiledQuery> =
        HashMap::new();
    let mut parser = tree_sitter::Parser::new();
    // Content-addressed sharing across commits (a revert re-puts bytes the
    // store already holds): the first put of a digest stores, repeats
    // dedupe — within and across runs, since a resume never re-puts.
    let mut seen_digests: HashSet<String> = HashSet::new();

    let total = plan.len();
    for (index, commit) in plan.iter().enumerate() {
        let sha = commit.sha.clone();
        // INV-11: the stamp strictly increases along the lineage. Ties
        // (second-resolution committer time) and regressions advance by
        // the smallest representable step, flagged for `stamp_nudged`.
        let (nudged, was_nudged) = nudge_valid_from(stamp_prev.as_deref(), &commit.committer_ts);
        let stamp = zulu_valid_from(&nudged).expect("nudge output is always a timestamp");
        if was_nudged {
            report.stamps_nudged += 1;
        }
        // Commit metadata (P5-D2): author time and both identities ride in
        // `extra` — numbers and strings, never digests — so a later reader
        // can choose a different clock without a re-ingest.
        let meta = match read_commit_meta(&git, &sha) {
            Ok(meta) => meta,
            Err(detail) => {
                // P5-D8: the commit's objects vanished mid-run. The ledger
                // keeps its prefix; everything from here on is unattempted
                // rather than written over a hole.
                report.ghost_commits += 1;
                for rest in plan.iter().skip(index) {
                    report.commits_skipped.push(SkippedCommit {
                        sha: rest.sha.clone(),
                        reason: "halted-after-ghost".to_string(),
                    });
                }
                eprintln!(
                    "[ingest] ghost commit {sha}: {detail}; halting with {} of {} ingested",
                    report.commits_ingested, total
                );
                break;
            }
        };

        // Blob puts first (put-then-assert, per commit): bytes land before
        // any concept names their digest, so a crash between leaves orphan
        // blobs at worst — never a live digest pointing at absent bytes.
        let mut digests = SourceBlobDigests::new();
        let mut commit_failed_files: Vec<FailedFile> = Vec::new();
        // P5a.4: files whose module id was minted on another lineage
        // (branch-born, brought in by a merge). Bytes and spine land;
        // entities stay on the mint lineage (D-225 refuses the restate,
        // D-214 names the overlay that would allow it). Auditable in
        // `files_failed`, but NOT sweep-gating: the file has no trunk
        // rows, so the sweep has nothing it could wrongly retire.
        let mut cross_lineage: Vec<FailedFile> = Vec::new();
        let mut parsed_units: Vec<(
            String,
            crate::types::Language,
            Vec<crate::types::ExtractedUnit>,
        )> = Vec::new();
        // A language whose query fails to compile degrades per file, the
        // same way a file that does not parse does — one bad grammar
        // must not sink the commit.
        let mut bad_languages: HashSet<crate::types::Language> = HashSet::new();
        let mut ghost_detail: Option<String> = None;
        for rel in commit.changed.iter().filter(|p| !excluded(p.as_path())) {
            let bytes = match blob_bytes(repo, &sha, rel) {
                Ok(bytes) => bytes,
                Err(e) => {
                    ghost_detail = Some(format!("bytes for {} vanished: {e:?}", rel.display()));
                    break;
                }
            };
            let canonical_path = canonical(rel);
            match graph
                .store
                .as_ref()
                .expect("ingest graph always holds a store")
                .put_source_blob(&canonical_path, &bytes)
            {
                Ok(BlobOutcome::Stored { digest, .. }) => {
                    if seen_digests.insert(digest.clone()) {
                        report.blobs_stored += 1;
                    } else {
                        report.blobs_deduped += 1;
                    }
                    digests.insert(canonical_path.clone(), digest);
                }
                Ok(BlobOutcome::SkippedOversize { .. }) => {
                    // Policy, not failure: concepts without a digest, no
                    // spine row — the same degrade analyze applies.
                }
                Ok(BlobOutcome::SkippedExcluded) => {
                    // Unreachable (the walker filtered these), counted all
                    // the same if a config ever disagrees with it.
                    report.files_excluded += 1;
                    continue;
                }
                Ok(BlobOutcome::Disabled) => {}
                Err(e) => {
                    eprintln!("[ingest] blob put failed for {canonical_path}: {e:?} — continuing");
                }
            }
            // P5a.4 (D-225 backstop position): the mint-lineage check.
            // A foreign-minted module id refuses the upsert at the
            // trigger; skipping here keeps the rest of the commit's flush
            // intact instead of aborting the whole bulk. A failed check
            // degrades to proceeding (the trigger stays loud if it must).
            let foreign = graph
                .store
                .as_ref()
                .expect("ingest graph always holds a store")
                .mint_lineage(&format!("{canonical_path}::module"))
                .unwrap_or_else(|e| {
                    eprintln!("[ingest] mint-lineage check failed for {canonical_path}: {e:?} — proceeding");
                    None
                })
                .filter(|minted| minted != lineage);
            if let Some(minted) = foreign {
                cross_lineage.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path,
                    reason: format!("cross-lineage-id:{minted}"),
                });
                continue;
            }
            // Extraction from the same bytes (P5-D9: bytes land even when
            // the file no longer parses — historical commits do that).
            let lang = crate::types::Language::from_extension(
                rel.extension().and_then(|e| e.to_str()).unwrap_or(""),
            );
            if bad_languages.contains(&lang) {
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path,
                    reason: "query-compile".to_string(),
                });
                continue;
            }
            let Some(ts_lang) = CodeGraph::ts_language(&lang) else {
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path,
                    reason: "no-grammar".to_string(),
                });
                continue;
            };
            let query = match query_cache.entry(lang) {
                std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    match crate::extract::tagger::CompiledQuery::new(lang, &ts_lang) {
                        Some(compiled) => v.insert(compiled),
                        None => {
                            bad_languages.insert(lang);
                            commit_failed_files.push(FailedFile {
                                sha: sha.clone(),
                                path: canonical_path,
                                reason: "query-compile".to_string(),
                            });
                            continue;
                        }
                    }
                }
            };
            let source = match String::from_utf8(bytes) {
                Ok(source) => source,
                Err(_) => {
                    commit_failed_files.push(FailedFile {
                        sha: sha.clone(),
                        path: canonical_path,
                        reason: "non-utf8".to_string(),
                    });
                    continue;
                }
            };
            parser
                .set_language(&ts_lang)
                .expect("ts_language agreed, so the parser accepts it");
            let Some(tree) = parser.parse(&source, None) else {
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path,
                    reason: "parse".to_string(),
                });
                continue;
            };
            let mut units = crate::extract::single_pass::extract_single_pass(
                &source,
                tree.root_node(),
                query,
                &canonical_path,
            );
            units.insert(
                0,
                CodeGraph::synthesize_module_unit(
                    &canonical_path,
                    &lang,
                    &source,
                    tree.root_node(),
                ),
            );
            report.files_parsed += 1;
            parsed_units.push((canonical_path, lang, units));
        }
        if let Some(detail) = ghost_detail {
            // P5-D8: the commit's bytes vanished mid-run. The ledger keeps
            // its prefix; everything from here on is unattempted rather
            // than written over a hole.
            report.ghost_commits += 1;
            for rest in plan.iter().skip(index) {
                report.commits_skipped.push(SkippedCommit {
                    sha: rest.sha.clone(),
                    reason: "halted-after-ghost".to_string(),
                });
            }
            eprintln!(
                "[ingest] ghost commit {sha}: {detail}; halting with {} of {} ingested",
                report.commits_ingested, total
            );
            break;
        }
        let deleted: Vec<String> = commit
            .deleted
            .iter()
            .filter(|p| !excluded(p.as_path()))
            .map(|p| canonical(p.as_path()))
            .collect();
        report.files_excluded += commit
            .changed
            .iter()
            .chain(commit.deleted.iter())
            .filter(|p| excluded(p.as_path()))
            .count();

        // Deleted paths leave the running projection; every surviving
        // unit of every changed path is diffed in. One clone per commit,
        // one commit back — diffs touch disjoint id sets.
        let mut projection = (*graph.snapshot()).clone();
        for (canonical_path, lang, units) in &parsed_units {
            let (_, _, retirement_error) =
                graph.apply_diff_update(&mut projection, units, canonical_path, lang, &stamp);
            if let Some(error) = retirement_error {
                eprintln!("[ingest] retire-after-diff failed for {canonical_path}: {error}");
            }
        }
        for canonical_path in &deleted {
            graph.remove_file_entities(&mut projection, canonical_path, &stamp);
            projection
                .file_to_modules
                .remove(&PathBuf::from(canonical_path));
        }
        rebuild_module_path_index(&mut projection);
        graph.resolve_imports(&mut projection);
        graph.populate_class_methods(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_decorators(&mut projection);
        graph.resolve_all_calls(&mut projection);
        graph.resolve_instantiations(&mut projection);
        graph.resolve_references(&mut projection);
        graph.commit_projection(projection);
        let projection = graph.snapshot();

        // One flush, one commit, one generation: the commit concept (P5-D2
        // metadata), the changed files' concepts, and the spine rows for
        // the changed modules — a single bulk, stamped with commit time.
        let store = graph
            .store
            .as_ref()
            .expect("ingest graph always holds a store");
        let mut concepts: Vec<macrame::ConceptUpsert> = Vec::new();
        for (canonical_path, _, _) in &parsed_units {
            concepts.extend(crate::storage::build_v2_concepts_for_file_at(
                &projection,
                canonical_path,
                &digests,
                &stamp,
            ));
        }
        concepts.push(commit_concept(
            &sha,
            lineage,
            &stamp,
            &commit.author_ts,
            &meta,
            was_nudged,
        ));
        let spine_inputs: Vec<(String, String)> = digests
            .iter()
            .map(|(file, digest)| (format!("{file}::module"), digest.clone()))
            .collect();
        if !spine_inputs.is_empty() {
            match store.spine_upserts_for(&spine_inputs, &stamp) {
                Ok(extra) => concepts.extend(extra),
                Err(e) => eprintln!("[ingest] spine computation failed at {sha}: {e:?}"),
            }
        }
        // A failed flush halts the run (ghost-commit precedent): the
        // bulk is chunked, so a mid-bulk abort is a partial write —
        // counting the commit as ingested would bake the gap in, while
        // halting leaves the cursor behind it for a retry.
        if let Err(e) = store.upsert_concepts_bulk(&concepts) {
            eprintln!(
                "[ingest] concept flush failed at {sha}: {e:?} — halting with {} of {} ingested",
                report.commits_ingested, total
            );
            for rest in plan.iter().skip(index) {
                report.commits_skipped.push(SkippedCommit {
                    sha: rest.sha.clone(),
                    reason: "halted-after-flush-failure".to_string(),
                });
            }
            break;
        }
        // Scoped edge flush (H-12): an in-scope edge with an open interval
        // is not re-asserted, so unchanged edges keep their original
        // commit stamp — only new facts write. Deletions retire through
        // the cascade below, not here.
        for (canonical_path, _, _) in &parsed_units {
            if let Err(e) = graph.persist_edges_scoped_at(&projection, Some(canonical_path), &stamp)
            {
                crate::storage::record_edge_persist_error(format!("{sha}:{canonical_path}: {e:?}"));
                eprintln!("[ingest] scoped edge persistence failed for {canonical_path}: {e:?}");
            }
        }
        // The sweep, scoped to the running projection (the commit's full
        // tree, accumulated): concepts and structural edges the commit no
        // longer believes close at the commit's stamp. Gated per P5-D9 —
        // a commit that does not fully parse must not retire anything.
        // (A failed flush halts above, so the flush always succeeded here.)
        if commit_failed_files.is_empty() {
            let mut keep_ids: HashSet<String> = HashSet::new();
            for id in projection.modules.keys() {
                keep_ids.insert(id.clone());
            }
            for id in projection.classes.keys() {
                keep_ids.insert(id.clone());
            }
            for id in projection.functions.keys() {
                keep_ids.insert(id.clone());
            }
            for id in projection.imports.keys() {
                keep_ids.insert(id.clone());
            }
            for id in projection.constants.keys() {
                keep_ids.insert(id.clone());
            }
            for id in projection.type_aliases.keys() {
                keep_ids.insert(id.clone());
            }
            for id in projection.routes.keys() {
                keep_ids.insert(id.clone());
            }
            // Commit concepts carry no file-path annotation, so the sweep
            // (which only retires file concepts) passes over them without
            // needing their ids in the keep set.
            if let Err(e) = store.retire_stale_file_concepts_at(&keep_ids, &stamp, lineage) {
                eprintln!("[ingest] stale concept sweep failed at {sha}: {e:?}");
            }
            if let Err(e) =
                store.retire_stale_edges_at(&crate::stale_edge_keep_set(&projection), &stamp)
            {
                eprintln!("[ingest] stale edge sweep failed at {sha}: {e:?}");
            }
        }
        report.files_failed.extend(commit_failed_files);
        // P5a.4: cross-lineage skips are auditable but never sweep-gating
        // (there are no trunk rows to protect — the entities live on the
        // mint lineage).
        report.files_failed.extend(cross_lineage);

        // The cursor advances per commit (H-20): an interrupted ingest
        // resumes after the last completed commit, never inside one.
        store
            .kv_put_sync(&ingest_head_key(lineage), &sha)
            .map_err(|e| IngestError::Store(format!("cannot advance cursor: {e:?}")))?;
        store
            .kv_put_sync(&ingest_stamp_key(lineage), &stamp)
            .map_err(|e| IngestError::Store(format!("cannot advance stamp: {e:?}")))?;
        stamp_prev = Some(stamp);
        report.head = Some(sha);
        report.commits_ingested += 1;
    }
    report.elapsed_secs = started.elapsed().as_secs_f64();
    Ok(report)
}

/// Ingest one git branch's first-parent history onto its own lineage
/// (P5a.4, DR-41): the fork-aware twin of [`ingest_history`].
///
/// Each commit is ingested in first-parent order from the fork point.
/// For every commit the op puts blobs for the changed files, writes one
/// stamped flush (the branch's own entity concepts plus the shared spine
/// plus the commit concept), and runs a sweep scoped to the commit's
/// tree and the lineage — then it advances the `kv` cursor. There is no
/// cascade and no edge persistence, because relations on branches need
/// cross-lineage edge semantics the ledger does not have (D-225 forbids
/// restating trunk rows, and cross-lineage edges are deferred, not smuggled).
///
/// What each changed file becomes depends on who minted its module id:
/// trunk-inherited files diverge through `write_branch_content_at`
/// (pointer turnover, bytes only — the entities stay trunk rows the
/// branch reads through fallback); files the branch itself minted
/// (branch-born, or re-changed after minting) re-mint entity concepts
/// on the lineage like the trunk does; files no lineage holds are
/// minted new, unless a sibling lineage holds the id (then bytes land
/// and the concepts are skipped with `cross-lineage-id` — D-225 refuses
/// the write, D-214 names the overlay that would allow it). Deletions of
/// inherited files mint tombstones (absence, not trunk fallback);
/// deletions of branch-minted files retire through the sweep.
///
/// The fork is recorded once (`kv` fork commit + ledger parentage) and
/// never re-derived: a recorded fork that disagrees with the computed
/// one refuses (`ForkMismatch`), and a lineage the ledger knows but
/// ingest never forked refuses (`LineageExists`). The window always
/// starts at the fork or the cursor past it — a lineage's history is
/// complete, never partial, which is what makes the recorded order
/// commit order with no read-path change (INV-10, same as the trunk).
/// A dry run plans without forking, writing, or advancing anything.
pub fn ingest_branch_history(
    store_path: &str,
    repo: &str,
    branch: &str,
    lineage: &str,
    parent_ref: &str,
    from: Option<&str>,
    dry_run: bool,
) -> Result<IngestReport, IngestError> {
    let started = Instant::now();
    let lineage_id = macrame::BranchId::new(lineage).map_err(|_| IngestError::InvalidLineage {
        lineage: lineage.to_string(),
    })?;
    let mut report = IngestReport {
        branch: branch.to_string(),
        lineage: lineage.to_string(),
        fork_point: None,
        from: None,
        head: None,
        dry_run,
        commits_ingested: 0,
        commits_skipped: Vec::new(),
        files_parsed: 0,
        files_failed: Vec::new(),
        files_excluded: 0,
        blobs_stored: 0,
        blobs_deduped: 0,
        stamps_nudged: 0,
        ghost_commits: 0,
        elapsed_secs: 0.0,
    };

    let store_file = Path::new(store_path);
    let store_exists = store_file.exists();
    if !dry_run && !store_exists {
        if let Some(parent) = store_file.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| IngestError::Store(format!("cannot create {parent:?}: {e}")))?;
            }
        }
    }
    let store: Option<CodeGraphStore> = if dry_run && !store_exists {
        None
    } else {
        Some(
            CodeGraphStore::open(store_file)
                .map_err(|e| IngestError::Store(format!("cannot open {store_path:?}: {e:?}")))?,
        )
    };

    // INV-12, same as the trunk: one store, one stamping mode.
    if let Some(ref store) = store {
        let mode = store
            .kv_get_sync(STAMPING_MODE_KEY)
            .map_err(|e| IngestError::Store(format!("cannot read stamping mode: {e:?}")))?;
        match mode.as_deref() {
            Some(MODE_COMMIT_TIME) => {}
            Some(MODE_INDEX_NOW) => {
                return Err(IngestError::IndexNowRefused {
                    path: store_path.to_string(),
                    legacy: false,
                })
            }
            Some(other) => {
                return Err(IngestError::Store(format!(
                    "unknown stamping mode {other:?} on {store_path:?}"
                )))
            }
            None if !store_exists => {
                if !dry_run {
                    store
                        .kv_put_sync(STAMPING_MODE_KEY, MODE_COMMIT_TIME)
                        .map_err(|e| IngestError::Store(format!("cannot claim mode: {e:?}")))?;
                }
            }
            None => {
                return Err(IngestError::IndexNowRefused {
                    path: store_path.to_string(),
                    legacy: true,
                })
            }
        }
    }

    // The fork boundary: newest branch-chain commit the parent ref still
    // reaches. Unknown names surface as the walker's typed errors before
    // anything is written.
    let computed_fork = fork_point(repo, branch, parent_ref)?;
    report.fork_point = Some(computed_fork.clone());
    if let Some(ref store) = store {
        match store
            .kv_get_sync(&ingest_fork_key(lineage))
            .map_err(|e| IngestError::Store(format!("cannot read fork: {e:?}")))?
        {
            Some(recorded) if recorded != computed_fork => {
                return Err(IngestError::ForkMismatch {
                    lineage: lineage.to_string(),
                    recorded,
                    computed: computed_fork,
                })
            }
            Some(_) => {}
            None => {
                let known = store
                    .branches()
                    .map_err(|e| IngestError::Store(format!("cannot list branches: {e:?}")))?
                    .into_iter()
                    .any(|b| b.name == lineage);
                if known {
                    return Err(IngestError::LineageExists {
                        lineage: lineage.to_string(),
                    });
                }
                if !dry_run {
                    store.fork_branch(lineage, "main").map_err(|e| match &e {
                        macrame::DbError::InvalidBranchId(_) => IngestError::InvalidLineage {
                            lineage: lineage.to_string(),
                        },
                        macrame::DbError::BranchExists(_) => IngestError::LineageExists {
                            lineage: lineage.to_string(),
                        },
                        _ => IngestError::Store(format!("cannot fork {lineage:?}: {e:?}")),
                    })?;
                    store
                        .kv_put_sync(&ingest_fork_key(lineage), &computed_fork)
                        .map_err(|e| IngestError::Store(format!("cannot record fork: {e:?}")))?;
                }
            }
        }
    }

    // INV-10 per lineage, INV-11's `prev` across runs. A first run starts
    // at the fork — never mid-branch, never partial.
    let (head, mut stamp_prev): (Option<String>, Option<String>) = match &store {
        Some(store) => (
            store
                .kv_get_sync(&ingest_head_key(lineage))
                .map_err(|e| IngestError::Store(format!("cannot read cursor: {e:?}")))?,
            store
                .kv_get_sync(&ingest_stamp_key(lineage))
                .map_err(|e| IngestError::Store(format!("cannot read stamp: {e:?}")))?,
        ),
        None => (None, None),
    };
    if let Some(ref h) = head {
        let git = git2::Repository::open(repo).map_err(GitError::Open)?;
        if git.revparse_single(h).is_err() {
            return Err(IngestError::UnknownHighWater { head: h.clone() });
        }
    }
    let effective_from: Option<String> = match (from, &head) {
        (Some(f), Some(h)) if f != h => {
            return Err(IngestError::OutOfOrder {
                head: h.clone(),
                from: f.to_string(),
            })
        }
        (Some(f), None) if f != computed_fork => {
            return Err(IngestError::OutOfOrder {
                head: format!("(fork {})", computed_fork),
                from: f.to_string(),
            })
        }
        (Some(f), _) => Some(f.to_string()),
        (None, h) => h.clone().or(Some(computed_fork.clone())),
    };
    report.from = effective_from.clone();
    report.head = head.clone();

    let plan: Vec<CommitPlan> = plan_ingest(repo, branch, effective_from.as_deref())?;
    if let (Some(ef), Some(first)) = (effective_from.as_ref(), plan.first()) {
        if !first.parents.contains(ef) {
            return Err(IngestError::NotDescendant {
                from: ef.clone(),
                first_parent: first.parents.first().cloned().unwrap_or_default(),
            });
        }
    }

    if dry_run {
        for commit in &plan {
            report.commits_skipped.push(SkippedCommit {
                sha: commit.sha.clone(),
                reason: "dry-run".to_string(),
            });
        }
        report.elapsed_secs = started.elapsed().as_secs_f64();
        return Ok(report);
    }
    if plan.is_empty() {
        report.elapsed_secs = started.elapsed().as_secs_f64();
        return Ok(report);
    }
    let store = store.expect("store is opened for every non-dry run");

    // Classification sets. `own` is every module id this lineage minted
    // (ever, not live — retired rows still route here, so a re-added
    // file re-mints on its lineage instead of shadowing the trunk);
    // `inherited` is the visible union minus the lineage's own — trunk
    // rows the branch reads through fallback. Both refresh from the
    // ledger per run; in-run mints join `own` as they land.
    let mut own: HashSet<String> = store
        .module_concept_ids(lineage)
        .map_err(|e| IngestError::Store(format!("cannot list own modules: {e:?}")))?
        .into_iter()
        .collect();
    let inherited: HashSet<String> = store
        .visible_module_concept_ids(lineage)
        .map_err(|e| IngestError::Store(format!("cannot list visible modules: {e:?}")))?
        .into_iter()
        .filter(|id| !own.contains(id))
        .collect();

    // The running projection starts empty and accumulates this run's own
    // files only — INV-10 makes every run a pure append of never-before-
    // diffed commits, so no cross-run diff memory is needed (same reason
    // the trunk starts empty). Trunk-owned files never touch it.
    let graph = CodeGraph::new(GraphConfig::default()).with_store(store);
    {
        let mut projection = (*graph.snapshot()).clone();
        projection.path_maps =
            crate::graph::path_maps::PathMaps::discover(repo, &graph.config.project, &[]);
        graph.commit_projection(projection);
    }
    let std_excludes = crate::exclusion_gitignore(&[]);
    let excluded = |rel: &Path| -> bool {
        match &std_excludes {
            Some(matcher) => crate::path_excluded(matcher, rel, false),
            None => false,
        }
    };

    let repo_root = Path::new(repo);
    let canonical = |rel: &Path| -> String {
        canonical_file_form_with_root(&rel.to_string_lossy().replace('\\', "/"), repo_root)
    };
    let git = git2::Repository::open(repo).map_err(GitError::Open)?;
    let mut query_cache: HashMap<crate::types::Language, crate::extract::tagger::CompiledQuery> =
        HashMap::new();
    let mut parser = tree_sitter::Parser::new();
    let mut seen_digests: HashSet<String> = HashSet::new();

    // Own-file routes, decided per file: OWN = the lineage minted the
    // module id (entity re-mint, same lineage, always legal); TRUNK =
    // inherited from an ancestor (pointer divergence, bytes only — the
    // entities stay the ancestor's rows); NEW = no lineage holds the id
    // (full mint, unless a sibling holds it).
    #[derive(PartialEq)]
    enum Route {
        Own,
        Trunk,
        New,
    }
    let total = plan.len();
    for (index, commit) in plan.iter().enumerate() {
        let sha = commit.sha.clone();
        let (nudged, was_nudged) = nudge_valid_from(stamp_prev.as_deref(), &commit.committer_ts);
        let stamp = zulu_valid_from(&nudged).expect("nudge output is always a timestamp");
        if was_nudged {
            report.stamps_nudged += 1;
        }
        let meta = match read_commit_meta(&git, &sha) {
            Ok(meta) => meta,
            Err(detail) => {
                report.ghost_commits += 1;
                for rest in plan.iter().skip(index) {
                    report.commits_skipped.push(SkippedCommit {
                        sha: rest.sha.clone(),
                        reason: "halted-after-ghost".to_string(),
                    });
                }
                eprintln!(
                    "[ingest] ghost commit {sha}: {detail}; halting with {} of {} ingested",
                    report.commits_ingested, total
                );
                break;
            }
        };

        let mut digests = SourceBlobDigests::new();
        let mut commit_failed_files: Vec<FailedFile> = Vec::new();
        let mut cross_lineage: Vec<FailedFile> = Vec::new();
        // Pointer and tombstone upserts join the commit's single bulk
        // (one flush, one generation) instead of writing immediately:
        // bytes are already put above, and `filter_unchanged` drops a
        // re-point to identical bytes for free.
        let mut pointer_upserts: Vec<macrame::ConceptUpsert> = Vec::new();
        let mut parsed_units: Vec<(
            String,
            crate::types::Language,
            Vec<crate::types::ExtractedUnit>,
        )> = Vec::new();
        let mut bad_languages: HashSet<crate::types::Language> = HashSet::new();
        let mut ghost_detail: Option<String> = None;
        for rel in commit.changed.iter().filter(|p| !excluded(p.as_path())) {
            let bytes = match blob_bytes(repo, &sha, rel) {
                Ok(bytes) => bytes,
                Err(e) => {
                    ghost_detail = Some(format!("bytes for {} vanished: {e:?}", rel.display()));
                    break;
                }
            };
            let canonical_path = canonical(rel);
            let module_id = format!("{canonical_path}::module");
            let route = if own.contains(&module_id) {
                Route::Own
            } else if inherited.contains(&module_id) {
                Route::Trunk
            } else {
                Route::New
            };
            let put_outcome = graph
                .store
                .as_ref()
                .expect("ingest graph always holds a store")
                .put_source_blob(&canonical_path, &bytes);
            let digest = match put_outcome {
                Ok(BlobOutcome::Stored { digest, .. }) => {
                    if seen_digests.insert(digest.clone()) {
                        report.blobs_stored += 1;
                    } else {
                        report.blobs_deduped += 1;
                    }
                    Some(digest)
                }
                Ok(BlobOutcome::SkippedOversize { .. }) => None,
                Ok(BlobOutcome::SkippedExcluded) => {
                    report.files_excluded += 1;
                    continue;
                }
                Ok(BlobOutcome::Disabled) => None,
                Err(e) => {
                    eprintln!("[ingest] blob put failed for {canonical_path}: {e:?} — continuing");
                    None
                }
            };
            if let Some(digest) = digest {
                digests.insert(canonical_path.clone(), digest);
            }
            // Sibling-minted ids (D-225 backstop position): bytes and
            // spine land, concepts stay on the mint lineage.
            if route == Route::New {
                let foreign = graph
                    .store
                    .as_ref()
                    .expect("ingest graph always holds a store")
                    .mint_lineage(&module_id)
                    .unwrap_or_else(|e| {
                        eprintln!("[ingest] mint-lineage check failed for {canonical_path}: {e:?} — proceeding");
                        None
                    })
                    .filter(|minted| minted != lineage);
                if let Some(minted) = foreign {
                    cross_lineage.push(FailedFile {
                        sha: sha.clone(),
                        path: canonical_path,
                        reason: format!("cross-lineage-id:{minted}"),
                    });
                    continue;
                }
            }
            // Trunk-route files diverge by pointer: bytes only, no parse,
            // no entities — the entities stay the ancestor's rows, which
            // branch reads reach through fallback. The pointer upsert joins
            // the commit's single bulk (one flush, one generation); the
            // no-op gate comes free via `filter_unchanged`, which drops a
            // re-point to identical bytes.
            if route == Route::Trunk {
                match digests.get(&canonical_path) {
                    Some(sha) => {
                        pointer_upserts.push(
                            branch_pointer_upsert(&canonical_path, sha, lineage, &stamp)
                                .expect("lineage validated at fork-ensure"),
                        );
                    }
                    None => {
                        commit_failed_files.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path,
                            reason: "pointer-failed:no-blob".to_string(),
                        });
                    }
                }
                continue;
            }
            // Own and new files mint entity concepts: extraction from the
            // same bytes (P5-D9: bytes land even when the file no longer
            // parses — historical commits do that).
            let lang = crate::types::Language::from_extension(
                rel.extension().and_then(|e| e.to_str()).unwrap_or(""),
            );
            if bad_languages.contains(&lang) {
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path,
                    reason: "query-compile".to_string(),
                });
                continue;
            }
            let Some(ts_lang) = CodeGraph::ts_language(&lang) else {
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path,
                    reason: "no-grammar".to_string(),
                });
                continue;
            };
            let query = match query_cache.entry(lang) {
                std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    match crate::extract::tagger::CompiledQuery::new(lang, &ts_lang) {
                        Some(compiled) => v.insert(compiled),
                        None => {
                            bad_languages.insert(lang);
                            commit_failed_files.push(FailedFile {
                                sha: sha.clone(),
                                path: canonical_path,
                                reason: "query-compile".to_string(),
                            });
                            continue;
                        }
                    }
                }
            };
            let source = match String::from_utf8(bytes) {
                Ok(source) => source,
                Err(_) => {
                    commit_failed_files.push(FailedFile {
                        sha: sha.clone(),
                        path: canonical_path,
                        reason: "non-utf8".to_string(),
                    });
                    continue;
                }
            };
            parser
                .set_language(&ts_lang)
                .expect("ts_language agreed, so the parser accepts it");
            let Some(tree) = parser.parse(&source, None) else {
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path,
                    reason: "parse".to_string(),
                });
                continue;
            };
            let mut units = crate::extract::single_pass::extract_single_pass(
                &source,
                tree.root_node(),
                query,
                &canonical_path,
            );
            units.insert(
                0,
                CodeGraph::synthesize_module_unit(
                    &canonical_path,
                    &lang,
                    &source,
                    tree.root_node(),
                ),
            );
            report.files_parsed += 1;
            parsed_units.push((canonical_path, lang, units));
        }
        if let Some(detail) = ghost_detail {
            report.ghost_commits += 1;
            for rest in plan.iter().skip(index) {
                report.commits_skipped.push(SkippedCommit {
                    sha: rest.sha.clone(),
                    reason: "halted-after-ghost".to_string(),
                });
            }
            eprintln!(
                "[ingest] ghost commit {sha}: {detail}; halting with {} of {} ingested",
                report.commits_ingested, total
            );
            break;
        }
        let deleted: Vec<String> = commit
            .deleted
            .iter()
            .filter(|p| !excluded(p.as_path()))
            .map(|p| canonical(p.as_path()))
            .collect();
        report.files_excluded += commit
            .changed
            .iter()
            .chain(commit.deleted.iter())
            .filter(|p| excluded(p.as_path()))
            .count();

        // Own-file diffs only: trunk-owned files never touch the running
        // projection (their entities stay the ancestor's rows). Deleted
        // own files leave the projection AND the lineage (the sweep
        // retires what the tree no longer holds); deleted inherited
        // files mint tombstones — absence, not trunk fallback.
        let mut projection = (*graph.snapshot()).clone();
        for (canonical_path, lang, units) in &parsed_units {
            let (_, _, retirement_error) =
                graph.apply_diff_update(&mut projection, units, canonical_path, lang, &stamp);
            if let Some(error) = retirement_error {
                eprintln!("[ingest] retire-after-diff failed for {canonical_path}: {error}");
            }
        }
        for canonical_path in &deleted {
            let module_id = format!("{canonical_path}::module");
            if own.contains(&module_id) {
                graph.remove_file_entities(&mut projection, canonical_path, &stamp);
                projection
                    .file_to_modules
                    .remove(&PathBuf::from(canonical_path));
            } else if inherited.contains(&module_id) {
                pointer_upserts.push(
                    branch_tombstone_upsert(canonical_path, lineage, &stamp)
                        .expect("lineage validated at fork-ensure"),
                );
            }
        }
        graph.commit_projection(projection);
        let projection = graph.snapshot();

        // One flush, one commit: the branch's own entity concepts (mapped
        // onto the lineage — the bulk respects per-upsert lineage, so the
        // shared spine rows stay lineage-agnostic exactly as P4 mints
        // them), the commit concept, and the spine rows. No edges: branch
        // relations need cross-lineage semantics the ledger refuses to
        // smuggle (D-225).
        let store = graph
            .store
            .as_ref()
            .expect("ingest graph always holds a store");
        let mut concepts: Vec<macrame::ConceptUpsert> = Vec::new();
        for (canonical_path, _, _) in &parsed_units {
            // The module id joins the lineage's own set as it lands, so a
            // later commit in the same run routes the file OWN (entity
            // re-mint, same lineage) instead of re-checking the ledger.
            own.insert(format!("{canonical_path}::module"));
            for built in crate::storage::build_v2_concepts_for_file_at(
                &projection,
                canonical_path,
                &digests,
                &stamp,
            ) {
                concepts.push(built.on_branch(lineage_id.clone()));
            }
        }
        concepts.push(
            commit_concept(&sha, lineage, &stamp, &commit.author_ts, &meta, was_nudged)
                .on_branch(lineage_id.clone()),
        );
        // Views join the same bulk: pointers, tombstones, entities, the
        // commit concept and the spine write together or not at all — one
        // flush, one generation per commit.
        concepts.extend(pointer_upserts);
        let spine_inputs: Vec<(String, String)> = digests
            .iter()
            .map(|(file, digest)| (format!("{file}::module"), digest.clone()))
            .collect();
        if !spine_inputs.is_empty() {
            match store.spine_upserts_for(&spine_inputs, &stamp) {
                Ok(extra) => concepts.extend(extra),
                Err(e) => eprintln!("[ingest] spine computation failed at {sha}: {e:?}"),
            }
        }
        // A failed flush halts the run (ghost-commit precedent): the
        // bulk is chunked, so a mid-bulk abort is a partial write —
        // counting the commit as ingested would bake the gap in, while
        // halting leaves the cursor behind it for a retry.
        if let Err(e) = store.upsert_concepts_bulk(&concepts) {
            eprintln!(
                "[ingest] concept flush failed at {sha}: {e:?} — halting with {} of {} ingested",
                report.commits_ingested, total
            );
            for rest in plan.iter().skip(index) {
                report.commits_skipped.push(SkippedCommit {
                    sha: rest.sha.clone(),
                    reason: "halted-after-flush-failure".to_string(),
                });
            }
            break;
        }
        // The sweep, scoped to the lineage AND the commit's tree (P5-D9
        // gate, same as the trunk): candidates are file-anchored ids this
        // lineage ever minted — trunk rows are never candidates, commit
        // concepts and pointers carry no file path, spine rows are
        // timeless. What the tree no longer holds retires at the commit's
        // stamp; the lineage-carrying retire keeps every row legal.
        // (A failed flush halts above, so the flush always succeeded here.)
        if commit_failed_files.is_empty() {
            let tree: HashSet<String> = commit
                .tree
                .iter()
                .filter(|p| !excluded(p.as_path()))
                .map(|p| canonical(p.as_path()))
                .collect();
            let retire: Vec<String> = store
                .all_concept_ids(lineage)
                .unwrap_or_else(|e| {
                    eprintln!("[ingest] branch universe listing failed at {sha}: {e:?}");
                    Vec::new()
                })
                .into_iter()
                .filter(|id| id.contains("::"))
                .filter(|id| {
                    let file = id.split("::").next().unwrap_or("");
                    !tree.contains(file)
                })
                .collect();
            if let Err(e) = store.retire_entities_at(&retire, &stamp) {
                eprintln!("[ingest] branch sweep failed at {sha}: {e:?}");
            }
        }
        report.files_failed.extend(commit_failed_files);
        report.files_failed.extend(cross_lineage);

        // Stamp before head: a crash between the two re-ingests one
        // commit (idempotent) instead of skipping one (a hole).
        store
            .kv_put_sync(&ingest_stamp_key(lineage), &stamp)
            .map_err(|e| IngestError::Store(format!("cannot advance stamp: {e:?}")))?;
        store
            .kv_put_sync(&ingest_head_key(lineage), &sha)
            .map_err(|e| IngestError::Store(format!("cannot advance cursor: {e:?}")))?;
        stamp_prev = Some(stamp);
        report.head = Some(sha);
        report.commits_ingested += 1;
    }
    report.elapsed_secs = started.elapsed().as_secs_f64();
    Ok(report)
}

/// Author/committer identities and the commit subject for the commit
/// concept's `extra` (P5-D2). Stamps come from the plan (already RFC3339);
/// everything here is display metadata, never an address.
struct CommitMeta {
    author_name: String,
    author_email: String,
    committer_name: String,
    committer_email: String,
    subject: String,
}

fn read_commit_meta(git: &git2::Repository, sha: &str) -> Result<CommitMeta, String> {
    let oid = git2::Oid::from_str(sha).map_err(|e| format!("bad sha: {e}"))?;
    let commit = git
        .find_commit(oid)
        .map_err(|e| format!("commit object gone: {e}"))?;
    let author = commit.author();
    let committer = commit.committer();
    Ok(CommitMeta {
        author_name: author.name().unwrap_or("").to_string(),
        author_email: author.email().unwrap_or("").to_string(),
        committer_name: committer.name().unwrap_or("").to_string(),
        committer_email: committer.email().unwrap_or("").to_string(),
        subject: commit.summary().ok().flatten().unwrap_or("").to_string(),
    })
}

/// One ledger row per ingested commit (`kind = 'commit'`): the ingest's
/// own record of what it did, when the commit says it happened. No
/// file-path annotation (so the stale sweep passes over it), no edges
/// (P3's lesson: pointers would pin blobs hot), no digests in payloads
/// (the retention scan's discipline). Excluded from the entity listings
/// and the cold-start loader exactly like the spine and the pointers.
fn commit_concept(
    sha: &str,
    lineage: &str,
    valid_from: &str,
    author_ts: &str,
    meta: &CommitMeta,
    nudged: bool,
) -> macrame::ConceptUpsert {
    let content = serde_json::json!({
        "meta_version": 2,
        "kind": "commit",
        "sha": sha,
        "lineage": lineage,
        "committer_ts": valid_from,
        "subject": meta.subject,
    });
    let extra = serde_json::json!({
        "coderadar": {
            "kind": "commit",
            "author_ts": author_ts,
            "author_name": meta.author_name,
            "author_email": meta.author_email,
            "committer_name": meta.committer_name,
            "committer_email": meta.committer_email,
            "stamp_nudged": nudged,
        }
    });
    macrame::ConceptUpsert::new(format!("commit:{sha}"), format!("commit:{sha}"))
        .content(content.to_string())
        .extra(extra.to_string())
        .valid_from(valid_from.to_string())
        .valid_to(crate::storage::TS_OPEN.to_string())
        .retired(false)
}
