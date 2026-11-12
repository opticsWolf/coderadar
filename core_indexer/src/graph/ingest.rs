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

/// First-import completion per lineage (P5b.3a, §3.1.1): the recorded
/// instant at which the lineage's first import into this store completed,
/// caught up to its head at that time. Written once, never moved. Absent
/// on pre-phase stores (the backfill writes it) and on analyze-built
/// stores (no horizon — reads unchanged).
pub fn horizon_key(lineage: &str) -> String {
    format!("coderadar.horizon.{lineage}")
}

/// Per-commit completion instant (P5b.3a, §3.1.2): the recorded instant
/// after the commit's last sub-generation (flush, edges, sweep) and before
/// the next commit's first write. Forward ingest stamps it with the write
/// clock; the backfill recomputes it from the ledger's recorded stamps.
pub fn commit_done_key(lineage: &str, sha: &str) -> String {
    format!("coderadar.commit_done.{lineage}.{sha}")
}

/// Commit-time store format (D-7): `1` marks the settled shape P5b.1/3a
/// write (one open interval per concept, horizon + commit index). A store
/// without it predates the shape; temporal reads name the rebuild.
pub const COMMIT_TIME_FORMAT_KEY: &str = "coderadar.commit_time.format";
pub const COMMIT_TIME_FORMAT: &str = "1";

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
    /// Files the run adopted under D-315 (a first version minted on a
    /// foreign lineage; M6-g). The failure alleys' shape so the audit
    /// reads beside them, one entry per adopted file.
    pub adoptions: Vec<FailedFile>,
    pub commits_adopted_concepts: usize,
    pub inherited_cuts: usize,
    pub files_excluded: usize,
    pub blobs_stored: usize,
    pub blobs_deduped: usize,
    pub stamps_nudged: usize,
    pub ghost_commits: usize,
    /// Commits whose completion instants this run (re)built from the
    /// ledger (P5b.3a): the first import's inline backfill, or the heal's
    /// refresh. Zero on a steady-state run (per-commit writes land inline).
    pub commits_reindexed: usize,
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
        adoptions: Vec::new(),
        commits_adopted_concepts: 0,
        inherited_cuts: 0,
        files_excluded: 0,
        blobs_stored: 0,
        blobs_deduped: 0,
        stamps_nudged: 0,
        ghost_commits: 0,
        commits_reindexed: 0,
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
        // P5a.4, superseded by D-315 (macrame-db 0.22.0, M6-g): the old
        // `cross-lineage-id` skip is an adoption path now. Files whose
        // module id was minted on a lineage this one cannot see (a
        // branch-born id brought in by a merge) mint entity concepts as
        // ordinary first versions — nothing copied from the minting
        // lineage, nothing closed, cut or superseded, the id's provenance
        // row unmoved — and the write joins the commit's own bulk at the
        // commit's stamps. A hard refusal still audits in
        // `files_failed`, and NOT sweep-gating carries over (the write
        // joins the lineage's own set below as it lands).
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
            // macrame-db 0.22.0 (D-315, M6-g item 1): a module id minted
            // on a foreign lineage (branch-born, brought in by a merge)
            // is an adoption — the trunk asserts its OWN first version
            // from this commit's own bytes, and the adoption is auditable
            // in the report rather than a skip. Since schema v26 the
            // version is an overlay row (a first version in
            // `concept_versions` on the writer's own lineage); nothing is
            // copied and the id's provenance row stays where D-214 put
            // it.
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
            if foreign.is_some() {
                report.adoptions.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path.clone(),
                    reason: "adopted-first-version".to_string(),
                });
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
            let (_, _, _, retirement_error) =
                graph.apply_diff_update(&mut projection, units, canonical_path, lang, &stamp, true);
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

        // The cursor advances per commit (H-20): an interrupted ingest
        // resumes after the last completed commit, never inside one.
        store
            .kv_put_sync(&ingest_head_key(lineage), &sha)
            .map_err(|e| IngestError::Store(format!("cannot advance cursor: {e:?}")))?;
        store
            .kv_put_sync(&ingest_stamp_key(lineage), &stamp)
            .map_err(|e| IngestError::Store(format!("cannot advance stamp: {e:?}")))?;
        // P5b.3a: the commit's completion instant — recorded after the
        // sweep and cursor, before the next commit's first write. A fold
        // at this instant sees exactly this commit's state (INV-10).
        let completion = crate::storage::now_iso8601();
        store
            .kv_put_sync(&commit_done_key(lineage, &sha), &completion)
            .map_err(|e| IngestError::Store(format!("cannot record completion: {e:?}")))?;
        // The next commit's first write must stamp strictly later (below).
        wait_past_instant(&completion);
        stamp_prev = Some(stamp);
        report.head = Some(sha);
        report.commits_ingested += 1;
    }
    // P5b.3a: the first import that finishes on this phase backfills the
    // index it never wrote — horizon, completions, format flag — from the
    // ledger in one auditable pass (the pre-phase store's own repair;
    // steady-state runs land their completions inline, above).
    if report.commits_ingested > 0 {
        let store_ref = graph
            .store
            .as_ref()
            .expect("ingest graph always holds a store");
        let horizon = horizon_key(lineage);
        let known = store_ref
            .kv_get_sync(&horizon)
            .map_err(|e| IngestError::Store(format!("cannot read horizon: {e:?}")))?;
        if known.is_none() {
            report.commits_reindexed =
                backfill_commit_index(store_ref, lineage, false)?.commits_indexed;
        }
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
/// (since macrame-db 0.22.0 / schema v26, the entity arm: the branch
/// writes its own TAKEOVER of the entities with `supersede(true)` — the
/// cut + own version at one stamp on the branch's own lineage, U-5's
/// one-write supersession, and the trunk row never moves); files the
/// branch itself minted (branch-born, or re-changed after minting)
/// re-mint entity concepts on the lineage like the trunk does; files no
/// lineage holds are minted new, and a sibling lineage holding the id is
/// an ADOPTION now (macrame-db 0.22.0, D-315, U-4: an ordinary first
/// version at the commit's stamps, nothing copied, the provenance row
/// unmoved — the `cross-lineage-id` skip is over). Deletions of
/// inherited files mint tombstones beside the module cut (absence, not
/// trunk fallback — the cut is the branch's own rejection of the
/// inheritance); deletions of branch-minted files retire through the
/// sweep.
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
        adoptions: Vec::new(),
        commits_adopted_concepts: 0,
        inherited_cuts: 0,
        files_excluded: 0,
        blobs_stored: 0,
        blobs_deduped: 0,
        stamps_nudged: 0,
        ghost_commits: 0,
        commits_reindexed: 0,
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
        let mut ids_adopted: usize = 0;
        // Pointer and tombstone upserts join the commit's single bulk
        // (one flush, one generation) instead of writing immediately:
        // bytes are already put above, and `filter_unchanged` drops a
        // re-point to identical bytes for free.
        let mut pointer_upserts: Vec<macrame::ConceptUpsert> = Vec::new();
        // Trunk-route files whose entity rows are the branch's takeovers
        // (the cut + own version) rather than plain first mints.
        let mut trunk_divergence: HashSet<String> = HashSet::new();
        // Deletions of inherited files mint the module cut beside the
        // tombstone marker (M6-g §10 item 2).
        let mut entity_cuts: Vec<macrame::ConceptUpsert> = Vec::new();
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
            // Sibling-minted ids (D-315, M6-g): bytes and spine land, and
            // the entity concepts mint as the lineage's adoption — the
            // parse below builds them from THIS commit's own bytes
            // (D-315: content asserted by the caller, the other lineage
            // never read). A hard write refusal still audits, and the
            // write joining the lineage's own set keeps the later routes
            // honest.
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
                if foreign.is_some() {
                    ids_adopted += 1;
                    report.adoptions.push(FailedFile {
                        sha: sha.clone(),
                        path: canonical_path.clone(),
                        reason: "adopted-first-version".to_string(),
                    });
                }
            }
            // Trunk-route files (inherited, changed by this commit): the
            // pointer upsert joins the commit's single bulk; the ENTITY
            // arm (macrame-db 0.22.0, M6-g §10 item 2) writes the branch's
            // own versions of the entities with `supersede(true)` — the
            // cut + own version at one stamp on the branch's own lineage
            // (the takeover the overlay exists for), no trunk row moved.
            // The entities build below from THIS commit's own parse; the
            // pointer upsert keeps read-side shape `filter_unchanged`
            // drops a re-point to identical bytes.
            if route == Route::Trunk {
                match digests.get(&canonical_path).cloned() {
                    Some(digest) => {
                        pointer_upserts.push(
                            branch_pointer_upsert(&canonical_path, &digest, lineage, &stamp)
                                .expect("lineage validated at fork-ensure"),
                        );
                    }
                    None => {
                        commit_failed_files.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path.clone(),
                            reason: "pointer-failed:no-blob".to_string(),
                        });
                    }
                }
                trunk_divergence.insert(canonical_path.clone());
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
        // M6-g review (macrame-db 0.22.0): the projection's exact removals
        // (in-file deletes, stale respellings) are the sweep's entity-level
        // candidates - their file may stay in the tree, so the tree filter
        // alone would keep the ids alive.
        let mut removed_ids: Vec<String> = Vec::new();
        for (canonical_path, lang, units) in &parsed_units {
            let (_, _, removed, retirement_error) = graph.apply_diff_update(
                &mut projection,
                units,
                canonical_path,
                lang,
                &stamp,
                false,
            );
            removed_ids.extend(removed);
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
                // macrame-db 0.22.0 (M6-g §10 item 2): the tombstone marker
                // keeps the read side's shape, and the module cut is the
                // branch's own rejection of the inheritance — `retired(true)`
                // on the branch is W2's bare cut row, the inheritance hidden
                // from the cut's instant on whatever the ancestor's rows do
                // later. Entities join the commit's bulk below; the cut rides
                // the overlay's own law inside macrame.
                entity_cuts.push(
                    macrame::ConceptUpsert::new(module_id.clone(), String::new())
                        .valid_from(stamp.clone())
                        .valid_to(crate::storage::TS_OPEN.to_string())
                        .retired(true)
                        .on_branch(lineage_id.clone()),
                );
                report.inherited_cuts += 1;
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
        // The commit's own-parse entities: every id this tree still
        // actively holds for the touched paths (class (c)'s keep set).
        let mut kept_ids: HashSet<String> = HashSet::new();
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
                // M6-g §10 item 2 (macrame-db 0.22.0): an inherited file
                // changed by this commit writes the branch's own TAKEOVER
                // — the cut + own version at one stamp on the branch's own
                // lineage (D-312's discipline, the overlay's turn off the
                // trunk). A first branch write of the same id (no overlay
                // row yet) rides the same marker: Macrame's route answers
                // the plain version row when nothing is open its side, so
                // the marker never invents a cut that is not there.
                let built = if trunk_divergence.contains(canonical_path) {
                    built.supersede(true)
                } else {
                    built
                };
                kept_ids.insert(built.id.clone());
                concepts.push(built.on_branch(lineage_id.clone()));
            }
        }
        report.commits_adopted_concepts += ids_adopted;
        concepts.push(
            commit_concept(&sha, lineage, &stamp, &commit.author_ts, &meta, was_nudged)
                .on_branch(lineage_id.clone()),
        );
        // Views join the same bulk: pointers, tombstones, entity cuts,
        // entities, the commit concept and the spine write together or
        // not at all — one flush, one generation per commit.
        concepts.extend(pointer_upserts);
        concepts.extend(entity_cuts);
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
            // M6-g review (macrame-db 0.22.0): three candidate classes,
            // all disposed by the sweep's own resolved reads (an id the
            // branch answers absent skips its second cut):
            //   (a) the lineage's ever-universe ids whose file left the
            //       tree (deleted own files, W1/overlay rows);
            //   (b) the projection diff's exact removals (an entity
            //       deleted from a touched file: its file STAYS in the
            //       tree, so (a)'s file filter would keep it);
            //   (c) the trunk-held entities of every touched path the
            //       commit no longer keeps - inherited ids the lineage
            //       never wrote (a deleted inherited file's functions, an
            //       in-file delete of an inherited file on first touch)
            //       are not in the ever-universe at all.
            let mut retire: HashSet<String> = store
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
            retire.extend(removed_ids.iter().filter(|id| id.contains("::")).cloned());
            for canonical_path in parsed_units.iter().map(|(p, _, _)| p).chain(deleted.iter()) {
                for lineages in ["IN ('', 'main')", &format!("= '{lineage}'")] {
                    match store.ids_under_path_live(canonical_path, lineages) {
                        Ok(ids) => {
                            for id in ids {
                                if !kept_ids.contains(&id) {
                                    retire.insert(id);
                                }
                            }
                        }
                        Err(e) => eprintln!(
                            "[ingest] ids under {canonical_path} ({lineages}) failed: {e:?}"
                        ),
                    }
                }
            }
            let retire: Vec<String> = retire.into_iter().collect();
            if let Err(e) = store.retire_entities_on_branch_at(&retire, lineage, &stamp) {
                eprintln!("[ingest] branch sweep failed at {sha}: {e:?}");
            }
        }
        report.files_failed.extend(commit_failed_files);

        // Stamp before head: a crash between the two re-ingests one
        // commit (idempotent) instead of skipping one (a hole).
        store
            .kv_put_sync(&ingest_stamp_key(lineage), &stamp)
            .map_err(|e| IngestError::Store(format!("cannot advance stamp: {e:?}")))?;
        store
            .kv_put_sync(&ingest_head_key(lineage), &sha)
            .map_err(|e| IngestError::Store(format!("cannot advance cursor: {e:?}")))?;
        // P5b.3a: the commit's completion instant (trunk loop carries
        // the same write — one rule for both lineages).
        let completion = crate::storage::now_iso8601();
        store
            .kv_put_sync(&commit_done_key(lineage, &sha), &completion)
            .map_err(|e| IngestError::Store(format!("cannot record completion: {e:?}")))?;
        // The next commit's first write must stamp strictly later (below).
        wait_past_instant(&completion);
        stamp_prev = Some(stamp);
        report.head = Some(sha);
        report.commits_ingested += 1;
    }
    // P5b.3a: same first-import backfill as the trunk loop — one rule
    // for both lineages.
    if report.commits_ingested > 0 {
        let store_ref = graph
            .store
            .as_ref()
            .expect("ingest graph always holds a store");
        let horizon = horizon_key(lineage);
        let known = store_ref
            .kv_get_sync(&horizon)
            .map_err(|e| IngestError::Store(format!("cannot read horizon: {e:?}")))?;
        if known.is_none() {
            report.commits_reindexed =
                backfill_commit_index(store_ref, lineage, false)?.commits_indexed;
        }
    }
    report.elapsed_secs = started.elapsed().as_secs_f64();
    Ok(report)
}

/// K8 (Amendment 2 §10 item 4): re-ingest the trunk's MERGE commits whose
/// files the original trunk walk skipped hollow — bytes and spine landed,
/// the entity concepts did not — so the D-315 adoption path mints them
/// now, at the merge commits' own stamps, on stores that predate the
/// adoption law. The trunk cursor sits past those commits, so a resume
/// never reaches them; this op is the re-ingest WITHOUT a cursor rewind:
/// the rewind is not a carrier (it would double every history row the
/// chain already holds and re-run its sweeps), and the hollow state is
/// otherwise unreachable under an adoption-law engine (a trunk walk
/// meeting a branch-born id either adopts inline, when the branch lineage
/// is already minted, or mints it as ordinary when nothing is minted).
///
/// The walk keeps the full first-parent chain in plan (from the root) so
/// the INV-11 nudge chain state is replayed across every commit — the
/// stamps the heal uses are the stamps a full pass would have used, which
/// is what keeps a heal row's start equal to its spine row's start from
/// the original walk. Only the files whose module id is **hollow on the
/// main lineage** process; a file whose trunk side the store already
/// holds past this commit's stamp is named and skipped (the chain's own
/// history is there, nothing to heal):
///
/// - `mint_lineage(id)` names another lineage → the vintage skip: the
///   trunk's own version of the id at this commit lands on the trunk.
///   The first claim is an adoption (an ordinary first version at the
///   commit's stamp, nothing copied, the provenance row unmoved,
///   audited in `adoptions`); a re-walk that finds the trunk already
///   claiming a healed id re-asserts this commit's content through the
///   same resolution a full pass uses (the trunk's supersede chain, the
///   fix the adopted-id read camp had to make first), so a later trunk
///   edit of a hollow file heals at its own commit — the healed set
///   carries the trunk's content, not just merge-day's. Re-asserts land
///   silently beyond `files_parsed`: the write's own unchanged filter
///   drops a byte-identical one, which is where the op's idempotence
///   lives.
/// - `mint_lineage(id)` names `main` → the walk wrote it → untouched.
/// - no mint anywhere → not the vintage-skip shape → named `no-mint` in
///   `files_failed`, untouched (the regular walk's own business).
///
/// Every commit on the chain walks: the hollow shape is per FILE — a
/// post-merge trunk commit that edits a hollow id was skipped by the old
/// walk exactly like the merge itself, so healing only merges would leave
/// those files frozen at merge-day content. And only files still present
/// in HEAD's tree heal; the absent ones are named `gone-by-head`, never
/// half-minted (a heal's correct history for them needs the closes of
/// later commits, which is not this op's shape).
///
/// Per commit: blob puts (content-addressed dedup as usual), the same
/// extraction and cascade the walk runs — over an own-files projection
/// (the branch walk's rule; edges between a healed file and files outside
/// the heal's set do not resolve here) — one bulk flush of the healed
/// entities at the commit's stamp (no commit concept, no spine, no sweep:
/// all three landed at the original walk), the trunk retire of the ids a
/// healed file dropped at this commit (main's own version closes; the
/// minting lineage is never touched), and scoped edge persistence for
/// the healed files. The `kv` cursor and
/// the head/stamp keys are untouched by definition: this op writes no
/// history windows beyond the entities the vintage skip left out.
///
/// A failed bulk is audited loudly (ghost-commit precedent's spirit) and
/// the run continues — there is no cursor to stop behind — so the report
/// is the operator's repair list; a partially written bulk is re-read as
/// `main`-minted by the next run (already) and the residual ids are named
/// in the audit. What the healed set will not carry lives in wording, not
/// fields: edges between a healed entity and entities of files outside
/// the heal's set are not written by the heal (the own-files projection
/// cannot see the other side) and neither the JSON nor `adoptions`
/// announces them — a healed file's link to an unchanged trunk file
/// arrives the next time that file's own ingest re-asserts it. The counts
/// only grow with a settled flush:
/// `commits_ingested` and `commits_adopted_concepts` move for a commit
/// whose bulk did not fail, while `head` still names the commit the run
/// reached (the audit names the failure's bulk; an inflated count would
/// report the merge-day shape as healed when it is not).
pub fn reingest_merged_commits(
    store_path: &str,
    repo: &str,
    branch: &str,
    dry_run: bool,
) -> Result<IngestReport, IngestError> {
    // The heal is trunk-only: merges land entity concepts on the lineage
    // the merge commit belongs to, and that is the trunk.
    if branch != "main" {
        return Err(IngestError::TrunkBranchMismatch {
            branch: branch.to_string(),
        });
    }
    let started = Instant::now();
    let lineage = "main";
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
        adoptions: Vec::new(),
        commits_adopted_concepts: 0,
        inherited_cuts: 0,
        files_excluded: 0,
        blobs_stored: 0,
        blobs_deduped: 0,
        stamps_nudged: 0,
        ghost_commits: 0,
        commits_reindexed: 0,
        elapsed_secs: 0.0,
    };
    let store_file = std::path::Path::new(store_path);
    if !store_file.exists() {
        return Err(IngestError::Store(format!(
            "the heal runs on an existing commit-time store; {store_path:?} does not exist — ingest_history builds stores"
        )));
    }
    let store = CodeGraphStore::open(store_file)
        .map_err(|e| IngestError::Store(format!("cannot open {store_path:?}: {e:?}")))?;
    // INV-12, same as every ingest surface: commit time is the only mode
    // the heal writes under.
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
        None => {
            return Err(IngestError::Store(format!(
                "no stamping mode on {store_path:?} — the pre-flag era's store shape, which the heal does not guess at"
            )))
        }
    }

    // The heal's graph: the same store-backed CodeGraph the walk builds.
    // The projection starts empty and accumulates the heal's files across
    // its commits (the walk's running-projection rule, narrowed to the
    // merge commits' own files).
    let graph = CodeGraph::new(GraphConfig::default()).with_store(store);
    {
        let mut projection = (*graph.snapshot()).clone();
        projection.path_maps =
            crate::graph::path_maps::PathMaps::discover(repo, &graph.config.project, &[]);
        graph.commit_projection(projection);
    }

    // The full first-parent chain, purely in git: the nudge chain state
    // replays across every commit, so the heal's stamps are the stamps a
    // full pass would have used (the vintage walks ran exactly one).
    let plans = plan_ingest(repo, branch, None)?;
    if plans.is_empty() {
        report.elapsed_secs = started.elapsed().as_secs_f64();
        return Ok(report);
    }
    // HEAD's tree, in the canonical spelling the same way `changed` is
    // compared: the heal's presence gate (files gone by the tip are named
    // before anything writes).
    let head_tree: std::collections::HashSet<String> = plans
        .last()
        .expect("non-empty")
        .tree
        .iter()
        .map(|p| {
            canonical_file_form_with_root(&p.to_string_lossy().replace('\\', "/"), Path::new(repo))
        })
        .collect();

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
    let mut query_cache: HashMap<crate::types::Language, crate::extract::tagger::CompiledQuery> =
        HashMap::new();
    let mut parser = tree_sitter::Parser::new();
    let mut seen_digests: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut stamp_prev: Option<String> = None;
    // P5b.3a: commits this run rewrites get their completion instants
    // refreshed at the end (the heal's rows carry old stamps with new
    // recorded instants).
    let mut healed_shas: Vec<String> = Vec::new();
    for commit in plans.iter() {
        let sha = commit.sha.clone();
        let (nudged, was_nudged) = nudge_valid_from(stamp_prev.as_deref(), &commit.committer_ts);
        let stamp = zulu_valid_from(&nudged).expect("nudge output is always a timestamp");
        if was_nudged {
            report.stamps_nudged += 1;
        }
        stamp_prev = Some(stamp.clone());
        // Every commit: the hollow shape is per FILE (the walk's
        // classification names it per commit), and only the ids the mint
        // check decides are the walk's skips — a merge commit is just the
        // first commit on the chain that reaches that shape.
        if dry_run {
            report.commits_skipped.push(SkippedCommit {
                sha: sha.clone(),
                reason: "dry-run".to_string(),
            });
        }
        // The heal's file set, classified from the ledger before anything
        // writes: hollow → adopt; main → already (untouched); no mint →
        // no-mint (named, untouched); gone from HEAD's tree → named.
        let mut parsed_units: Vec<(
            String,
            crate::types::Language,
            Vec<crate::types::ExtractedUnit>,
        )> = Vec::new();
        let mut bad_languages: std::collections::HashSet<crate::types::Language> =
            std::collections::HashSet::new();
        let mut digests = SourceBlobDigests::new();
        let mut commit_failed_files: Vec<FailedFile> = Vec::new();
        let mut ghost_detail: Option<String> = None;
        for rel in commit.changed.iter().filter(|p| !excluded(p.as_path())) {
            let canonical_path = canonical(rel);
            let module_id = format!("{canonical_path}::module");
            match graph
                .store
                .as_ref()
                .expect("heal graph always holds a store")
                .mint_lineage(&module_id)
            {
                Ok(Some(mint)) if mint == lineage => {
                    // Ingested at the original walk: nothing to heal. A
                    // re-run reads every id it healed last time exactly
                    // here.
                    continue;
                }
                Ok(Some(_)) => {
                    // The mint names ANOTHER lineage: the file the walk
                    // would have skipped. The head-tree gate first — one
                    // verdict per file — then the trunk-spine claim tells
                    // the write's shape: a first version (adoption) when
                    // the spine holds nothing, a re-assert (the trunk's
                    // own supersede chain) when it does.
                    let taken = graph
                        .store
                        .as_ref()
                        .expect("heal graph always holds a store")
                        .trunk_version_live(&module_id)
                        .unwrap_or(false);
                    if !head_tree.contains(&canonical_path) {
                        // A file the trunk deleted afterwards: named,
                        // never half-minted. Both verdicts (the real
                        // run's adopt and the dry run's would-adopt
                        // naming) drop to the same auditable name.
                        commit_failed_files.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path,
                            reason: if dry_run {
                                "dry-run:gone-by-head".to_string()
                            } else {
                                "gone-by-head".to_string()
                            },
                        });
                        continue;
                    }
                    if !taken {
                        report.adoptions.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path.clone(),
                            reason: if dry_run {
                                "dry-run".to_string()
                            } else {
                                "adopted-first-version".to_string()
                            },
                        });
                    } else if dry_run {
                        // A re-run (or a store where the trunk's walk
                        // already healed a copy) would re-assert, and
                        // the write's filter may drop it: named here so
                        // a dry run does not promise a fresh adoption.
                        commit_failed_files.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path.clone(),
                            reason: "dry-run:already-adopted".to_string(),
                        });
                    }
                    // A dry run names its would-heal set and writes
                    // nothing - no blob puts, no parse, no flush.
                    if dry_run {
                        continue;
                    }
                }
                Ok(None) => {
                    if dry_run {
                        commit_failed_files.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path.clone(),
                            reason: "dry-run:no-mint".to_string(),
                        });
                    } else {
                        commit_failed_files.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path.clone(),
                            reason: "no-mint".to_string(),
                        });
                    }
                    continue;
                }
                Err(e) => {
                    eprintln!("[ingest] mint-lineage check failed for {canonical_path}: {e:?}");
                    commit_failed_files.push(FailedFile {
                        sha: sha.clone(),
                        path: canonical_path.clone(),
                        reason: "mint-lineage-check".to_string(),
                    });
                    continue;
                }
            }
            // Blob puts first (put-then-assert, per commit), the walk's
            // own order.
            let bytes = match blob_bytes(repo, &sha, rel) {
                Ok(bytes) => bytes,
                Err(e) => {
                    ghost_detail = Some(format!("bytes for {} vanished: {e:?}", rel.display()));
                    break;
                }
            };
            let put_outcome = graph
                .store
                .as_ref()
                .expect("heal graph always holds a store")
                .put_source_blob(&canonical_path, &bytes);
            match put_outcome {
                Ok(BlobOutcome::Stored { digest, .. }) => {
                    if seen_digests.insert(digest.clone()) {
                        report.blobs_stored += 1;
                    } else {
                        report.blobs_deduped += 1;
                    }
                    digests.insert(canonical_path.clone(), digest);
                }
                Ok(BlobOutcome::SkippedOversize { .. }) => {
                    // No digest, no spine row — the walk's same degrade.
                    continue;
                }
                Ok(BlobOutcome::SkippedExcluded) => {
                    report.files_excluded += 1;
                    continue;
                }
                Ok(BlobOutcome::Disabled) => {}
                Err(e) => {
                    eprintln!("[ingest] blob put failed for {canonical_path}: {e:?} — continuing");
                }
            }
            // Extraction from the same bytes (P5-D9: bytes land even when
            // the file does not parse exactly as the walk reads it).
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
            match String::from_utf8(bytes) {
                Ok(source) => {
                    parser
                        .set_language(&ts_lang)
                        .expect("ts_language agreed, so the parser accepts it");
                    if let Some(tree) = parser.parse(&source, None) {
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
                    } else {
                        commit_failed_files.push(FailedFile {
                            sha: sha.clone(),
                            path: canonical_path,
                            reason: "parse".to_string(),
                        });
                    }
                }
                Err(_) => {
                    commit_failed_files.push(FailedFile {
                        sha: sha.clone(),
                        path: canonical_path,
                        reason: "non-utf8".to_string(),
                    });
                }
            }
        }
        report.files_excluded += commit
            .changed
            .iter()
            .chain(commit.deleted.iter())
            .filter(|p| excluded(p.as_path()))
            .count();
        if let Some(detail) = ghost_detail {
            // P5-D8: the commit's bytes vanished mid-run. The ledger keeps
            // its prefix; this commit is skipped auditable, and the next
            // run re-names it.
            commit_failed_files.push(FailedFile {
                sha: sha.clone(),
                path: "<whole-commit>".to_string(),
                reason: format!("ghost: {detail}"),
            });
            eprintln!("[ingest] ghost commit {sha} in the heal: {detail}");
            report.files_failed.extend(commit_failed_files);
            continue;
        }
        if dry_run {
            report.files_failed.extend(commit_failed_files);
            continue;
        }
        // The heal's own projection: empty at the run's start, the heal's
        // files accumulate across its commits (the walk's running-
        // projection rule, narrowed to the heal's own set — edges between
        // a healed file and files outside the heal's set do not resolve
        // here, the branch walk's own-file cascade rule).
        let mut projection = (*graph.snapshot()).clone();
        for (canonical_path, lang, units) in &parsed_units {
            // retire=true: an id this file held at the heal's previous
            // commit and no longer holds was deleted HERE, and no sweep
            // runs over the trunk after the heal. The trunk retire closes
            // main's own (adopted) version at this stamp and leaves the
            // minting lineage alone; an id main no longer holds (a re-run,
            // or a full walk's own tombstone) is a no-op.
            let (_, _, _, retire_err) =
                graph.apply_diff_update(&mut projection, units, canonical_path, lang, &stamp, true);
            if let Some(e) = retire_err {
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: canonical_path.clone(),
                    reason: format!("retire-failed: {e}"),
                });
            }
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

        // One flush, one commit: the healed entities only.
        let mut concepts: Vec<macrame::ConceptUpsert> = Vec::new();
        for (canonical_path, _, _) in &parsed_units {
            concepts.extend(crate::storage::build_v2_concepts_for_file_at(
                &projection,
                canonical_path,
                &digests,
                &stamp,
            ));
        }
        let mut flush_ok = true;
        if !concepts.is_empty() {
            // A failed flush is audited loud, not silent: there is no
            // cursor to stop behind, so the report is the repair list and
            // the next run re-names the commit's residual ids.
            if let Err(e) = graph
                .store
                .as_ref()
                .expect("heal graph always holds a store")
                .upsert_concepts_bulk(&concepts)
            {
                flush_ok = false;
                eprintln!(
                    "[ingest] heal concept flush failed at {sha}: {e:?} — audited, continuing"
                );
                commit_failed_files.push(FailedFile {
                    sha: sha.clone(),
                    path: "<bulk>".to_string(),
                    reason: format!("flush-failed-partial: {e:?}"),
                });
            }
        }
        for (canonical_path, _, _) in &parsed_units {
            if let Err(e) = graph.persist_edges_scoped_at(&projection, Some(canonical_path), &stamp)
            {
                crate::storage::record_edge_persist_error(format!("{sha}:{canonical_path}: {e:?}"));
                eprintln!("[ingest] heal edge persistence failed for {canonical_path}: {e:?}");
            }
        }
        report.files_failed.extend(commit_failed_files);
        if flush_ok {
            report.commits_adopted_concepts += concepts.len();
            report.commits_ingested += 1;
            healed_shas.push(sha.clone());
        }
        report.head = Some(sha);
    }
    // P5b.3a: the heal's rows land after every recorded completion, so
    // every completion on the lineage predates them — recompute the whole
    // index from the ledger (backfill keeps an existing horizon: the heal
    // completes no import).
    if !healed_shas.is_empty() {
        let store_ref = graph
            .store
            .as_ref()
            .expect("heal graph always holds a store");
        let bf = backfill_commit_index(store_ref, lineage, true)?;
        report.commits_reindexed = bf.commits_indexed;
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
    body: String,
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
        // P6c slice (DR-45): the body rides the same payload as the
        // subject (one bulk, no new generation). `body()` is the message
        // minus the subject line; absent or whitespace-only reads empty.
        body: commit
            .body()
            .ok()
            .flatten()
            .unwrap_or("")
            .trim()
            .to_string(),
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
        "body": meta.body,
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

/// What the backfill rebuilt, auditable per lineage (P5b.3a).
#[derive(Debug, Clone, Serialize)]
pub struct CommitIndexReport {
    pub lineage: String,
    pub horizon: String,
    pub commits_indexed: usize,
    /// Commit stamps whose completion shares a microsecond with the next
    /// commit's first row (pre-phase stores only — forward ingest waits
    /// out the tick, so it never mints these). A fold at such a
    /// completion includes the next commit's same-tick rows: an honest,
    /// bounded imprecision the git-history arm inherits.
    pub collisions: Vec<String>,
}

/// P5b.3a: completion instants must strictly separate consecutive
/// commits' rows — a shared microsecond folds the next commit's rows into
/// this commit's state (the ledger stamps at microsecond precision, and a
/// fast machine fits a commit's tail and the next commit's head into one
/// tick). After recording a completion, wait out its tick, so the next
/// commit's first write stamps strictly later. Cross-process gaps are
/// milliseconds by construction; only the in-run chain needs the wait.
fn wait_past_instant(completion: &str) {
    let mut spins = 0u32;
    while crate::storage::now_iso8601().as_str() <= completion {
        spins += 1;
        if spins & 1023 == 0 {
            std::thread::yield_now();
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Completion instants for a lineage's commits, recomputed from the
/// ledger (P5b.3a, §3.1.2): for each commit, the newest `recorded_at`
/// among the rows carrying its stamp — concept opens (`valid_from`) and
/// closes (`valid_to`, the sweep's rows), plus the `links` rows the
/// scoped edge flush wrote. Under INV-10 a commit's rows are all
/// recorded before the next commit's first row, so the max bounds the
/// commit from above and a fold at the instant sees exactly its state.
/// (sha, committer stamp) per commit concept on a lineage, oldest first
/// (P5b.3a, §3.1.2): the resolution order the git-history arm walks. The
/// stamp is the EARLIEST version's valid_from (the original mint): commit
/// rows superseded later (P6c's body heal, valid_from = now) must not
/// read as new commits.
pub(crate) fn commit_stamps(
    store: &CodeGraphStore,
    lineage: &str,
) -> Result<Vec<(String, String)>, IngestError> {
    let err = |what: &str, e: String| IngestError::Store(format!("commit stamps {what}: {e}"));
    let (_guard, conn) = store
        .open_diagnostic_conn()
        .map_err(|e| err("open", format!("{e:?}")))?;
    let branch = libsql::Value::Text(lineage.to_string());
    let mut first: HashMap<String, String> = HashMap::new();
    crate::storage::runtime()
        .block_on(async {
            let mut rows = conn
                .query(
                    "SELECT entity_id, payload FROM transaction_log \
                     WHERE table_name = 'concepts' AND branch_id = ?1 \
                     AND entity_id LIKE 'commit:%'",
                    vec![branch.clone()],
                )
                .await
                .map_err(|e| format!("{e:?}"))?;
            while let Some(row) = rows.next().await.map_err(|e| format!("{e:?}"))? {
                let entity_id: String = row.get(0).map_err(|e| format!("{e:?}"))?;
                let payload: String = row.get(1).map_err(|e| format!("{e:?}"))?;
                let v: serde_json::Value =
                    serde_json::from_str(&payload).map_err(|e| format!("{e:?}"))?;
                let stamp = v
                    .get("valid_from")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                if stamp.is_empty() {
                    continue;
                }
                let sha = entity_id.strip_prefix("commit:").unwrap_or("").to_string();
                first
                    .entry(sha)
                    .and_modify(|s| {
                        if stamp < *s {
                            *s = stamp.clone();
                        }
                    })
                    .or_insert(stamp);
            }
            Ok::<_, String>(())
        })
        .map_err(|e| err("rows", e))?;
    let mut commits: Vec<(String, String)> = first.into_iter().collect();
    commits.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    Ok(commits)
}

/// `(completions, collisions)` — `completions` is `(sha, completion)`
/// oldest first; `collisions` names stamps whose rows were written in the
/// same tick (only reachable on a pre-horizon backfill).
type CommitCompletionMap = (Vec<(String, String)>, Vec<String>);

fn commit_completion_map(
    store: &CodeGraphStore,
    lineage: &str,
) -> Result<CommitCompletionMap, IngestError> {
    let err = |what: &str, e: String| IngestError::Store(format!("backfill {what}: {e}"));
    // Commits first: `open_diagnostic_conn` takes an exclusive guard, so
    // nesting it around another diagnostic conn would deadlock.
    let commits = commit_stamps(store, lineage)?;
    let (_guard, conn) = store
        .open_diagnostic_conn()
        .map_err(|e| err("open", format!("{e:?}")))?;
    let branch = libsql::Value::Text(lineage.to_string());
    // Newest AND oldest recorded stamp per valid-axis stamp, over concept
    // rows (opens AND closes) and link rows.
    let mut newest: HashMap<String, String> = HashMap::new();
    let mut oldest: HashMap<String, String> = HashMap::new();
    let mut note = |stamp: &str, recorded_at: &str| {
        if stamp.is_empty() {
            return;
        }
        newest
            .entry(stamp.to_string())
            .and_modify(|r| {
                if recorded_at > r.as_str() {
                    *r = recorded_at.to_string();
                }
            })
            .or_insert_with(|| recorded_at.to_string());
        oldest
            .entry(stamp.to_string())
            .and_modify(|r| {
                if recorded_at < r.as_str() {
                    *r = recorded_at.to_string();
                }
            })
            .or_insert_with(|| recorded_at.to_string());
    };
    crate::storage::runtime()
        .block_on(async {
            let mut rows = conn
                .query(
                    "SELECT payload, recorded_at FROM transaction_log \
                     WHERE table_name = 'concepts' AND branch_id = ?1",
                    vec![branch.clone()],
                )
                .await
                .map_err(|e| format!("{e:?}"))?;
            while let Some(row) = rows.next().await.map_err(|e| format!("{e:?}"))? {
                let payload: String = row.get(0).map_err(|e| format!("{e:?}"))?;
                let recorded_at: String = row.get(1).map_err(|e| format!("{e:?}"))?;
                let v: serde_json::Value =
                    serde_json::from_str(&payload).map_err(|e| format!("{e:?}"))?;
                // Write-time snapshot rule: an open still shows the open
                // end (attribute the writer's stamp); a close carries the
                // closing stamp (attribute that). A close row's valid_from
                // is an OLDER commit's stamp and must never join its group.
                let valid_to = v.get("valid_to").and_then(|s| s.as_str());
                let is_open = match valid_to {
                    Some(s) => s == crate::storage::TS_OPEN || s == crate::storage::TS_OPEN_LEGACY,
                    None => true,
                };
                if is_open {
                    if let Some(s) = v.get("valid_from").and_then(|s| s.as_str()) {
                        note(s, &recorded_at);
                    }
                } else if let Some(s) = valid_to {
                    note(s, &recorded_at);
                }
            }
            // P5b.3a: the branch sweep's `concept_cuts` row is a
            // sub-generation of the commit (it lands after the concept
            // flush), keyed by `cut_at` — the commit's own stamp. Ignoring
            // it derives a completion that precedes the cut, and the arm
            // then folds the pre-sweep state (a departed entity still
            // present). Same key rule as concepts: the cut's `cut_at` IS
            // its stamp.
            let mut rows = conn
                .query(
                    "SELECT cut_at, recorded_at FROM concept_cuts WHERE branch_id = ?1",
                    vec![branch.clone()],
                )
                .await
                .map_err(|e| format!("{e:?}"))?;
            while let Some(row) = rows.next().await.map_err(|e| format!("{e:?}"))? {
                let cut_at: String = row.get(0).map_err(|e| format!("{e:?}"))?;
                let recorded_at: String = row.get(1).map_err(|e| format!("{e:?}"))?;
                note(&cut_at, &recorded_at);
            }
            let mut rows = conn
                .query(
                    "SELECT valid_from, valid_to, recorded_at FROM links WHERE branch_id = ?1",
                    vec![branch],
                )
                .await
                .map_err(|e| format!("{e:?}"))?;
            while let Some(row) = rows.next().await.map_err(|e| format!("{e:?}"))? {
                let from: String = row.get(0).map_err(|e| format!("{e:?}"))?;
                let to: String = row.get(1).map_err(|e| format!("{e:?}"))?;
                let at: String = row.get(2).map_err(|e| format!("{e:?}"))?;
                if to == crate::storage::TS_OPEN || to == crate::storage::TS_OPEN_LEGACY {
                    note(&from, &at);
                } else {
                    note(&to, &at);
                }
            }
            Ok::<_, String>(())
        })
        .map_err(|e| err("ledger scan", e))?;
    let mut out = Vec::with_capacity(commits.len());
    for (sha, stamp) in &commits {
        match newest.get(stamp) {
            Some(completion) => out.push((sha.clone(), completion.clone())),
            None => return Err(err("missing stamp", format!("no rows carry {stamp}"))),
        }
    }
    // Same-tick adjacencies: the next commit's first row shares this
    // completion's microsecond, so a fold at the completion includes it.
    // Recorded order bounds every group below the next, so equality is
    // the whole test.
    let mut collisions = Vec::new();
    for i in 0..commits.len().saturating_sub(1) {
        if let Some(next_first) = oldest.get(&commits[i + 1].1) {
            if out[i].1 == *next_first {
                collisions.push(commits[i].1.clone());
            }
        }
    }
    Ok((out, collisions))
}

/// Rebuild one lineage's commit index from the ledger (P5b.3a): every
/// commit's completion instant, the horizon (now — the store is caught up
/// to its head when the backfill runs), and the D-7 format flag. The
/// pre-phase store's own repair, and the first import's inline finish:
/// ingest calls this when the horizon is absent. Auditable — the report
/// names the lineage, the horizon, and the commit count.
///
/// `overwrite` separates the two jobs. A *derived* completion is the
/// newest instant on which the commit's own rows landed: correct, but a
/// lower bound — the loop knows the real instant, after the sweep and
/// before the next commit's first write. So the first import's finish
/// fills only what the ingest did not write (`overwrite = false`), while a
/// rebuild — the pre-phase store's migration, or K8's body heal, whose
/// rows land at old stamps now — must replace what is there.
pub fn backfill_commit_index(
    store: &CodeGraphStore,
    lineage: &str,
    overwrite: bool,
) -> Result<CommitIndexReport, IngestError> {
    let err = |what: &str, e: String| IngestError::Store(format!("backfill {what}: {e}"));
    let (completions, collisions) = commit_completion_map(store, lineage)?;
    for (sha, completion) in &completions {
        let key = commit_done_key(lineage, sha);
        if !overwrite
            && store
                .kv_get_sync(&key)
                .map_err(|e| err("kv", format!("{e:?}")))?
                .is_some()
        {
            continue;
        }
        store
            .kv_put_sync(&key, completion)
            .map_err(|e| err("kv", format!("{e:?}")))?;
    }
    // Written once, never moved: the heal and the explicit op both
    // route here, and neither completes an import.
    let horizon = match store
        .kv_get_sync(&horizon_key(lineage))
        .map_err(|e| err("kv", format!("{e:?}")))?
    {
        Some(known) => known,
        None => {
            let fresh = crate::storage::now_iso8601();
            store
                .kv_put_sync(&horizon_key(lineage), &fresh)
                .map_err(|e| err("kv", format!("{e:?}")))?;
            fresh
        }
    };
    store
        .kv_put_sync(COMMIT_TIME_FORMAT_KEY, COMMIT_TIME_FORMAT)
        .map_err(|e| err("kv", format!("{e:?}")))?;
    Ok(CommitIndexReport {
        lineage: lineage.to_string(),
        horizon,
        commits_indexed: completions.len(),
        collisions,
    })
}

/// Where a temporal read folds, and what it calls the answer (P5b.3a,
/// §3.1.1–3.1.2): the recorded arm past the horizon (or with no
/// horizon), the git-history arm before it — the last commit at or
/// before the read instant, folded at its completion instant.
#[derive(Debug, Clone)]
pub struct ResolvedRead {
    /// The recorded-axis instant the fold runs at.
    pub fold_ts: String,
    /// `"recorded"` or `"git-history"` — every result names its basis.
    pub basis: &'static str,
    /// The addressed commit on the git-history arm, `None` on recorded.
    pub sha: Option<String>,
    /// The lineage horizon, `None` when the store has none.
    pub horizon: Option<String>,
    /// Files observed dirty at the addressed commit (P5b.3a: always
    /// empty — observation rows arrive with P5d; the rule that a
    /// non-empty set refuses under `strict` ships with the field).
    pub dirty_at_commit: Vec<String>,
}

/// The git-history arm's refusals (P5b.3a, §3.1.1): reasons, never
/// blanks. Surfaced as `ValueError` at the `_core` boundary.
#[derive(Debug, Clone)]
pub enum ReadArmError {
    /// Nothing on the lineage is at or before the read instant.
    BeforeHistory {
        lineage: String,
        instant: String,
        first_commit: Option<String>,
        horizon: String,
    },
    /// `strict` asked the recorded question, and the read instant is
    /// before the horizon.
    BeforeHorizon {
        lineage: String,
        instant: String,
        horizon: String,
    },
    /// The index itself is unreadable (infrastructure, not history).
    Store(String),
    /// `at=` names no commit on the lineage.
    UnknownCommit { lineage: String, sha: String },
    /// `at=` names a ref: sha addressing is P5b.3a, refs are P5b.4.
    RefNeedsP5b4 { address: String },
}

impl std::fmt::Display for ReadArmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadArmError::BeforeHistory {
                lineage,
                instant,
                first_commit,
                horizon,
            } => write!(
                f,
                "BeforeHistory: lineage {lineage:?} has no commit at or before {instant} \
                 (first commit {}; horizon {horizon}) — the store's history starts later",
                first_commit.as_deref().unwrap_or("unknown")
            ),
            ReadArmError::BeforeHorizon {
                lineage,
                instant,
                horizon,
            } => write!(
                f,
                "BeforeHorizon: {instant} predates the horizon {horizon} on lineage \
                 {lineage:?}, and strict asked the recorded question, which has no \
                 answer yet"
            ),
            ReadArmError::Store(detail) => write!(f, "commit index unreadable: {detail}"),
            ReadArmError::UnknownCommit { lineage, sha } => write!(
                f,
                "unknown commit {sha:?} on lineage {lineage:?} — it names no commit concept"
            ),
            ReadArmError::RefNeedsP5b4 { address } => write!(
                f,
                "at={address:?} names a ref: sha addressing is P5b.3a, refs arrive with P5b.4"
            ),
        }
    }
}

/// Resolve a temporal read to its fold instant (P5b.3a, §3.1.1–3.1.2).
///
/// `timestamp` is the positional read instant (already normalized),
/// `strict` refuses the git-history arm, `at` addresses a commit
/// directly (sha) or restates the instant (timestamp) — never both with
/// the positional. A lost index rebuilds inline (H-20): the ledger
/// holds every completion, `kv` is only the fast path.
pub fn resolve_temporal_read(
    store: &CodeGraphStore,
    lineage: &str,
    timestamp: &str,
    strict: bool,
    at: Option<&str>,
) -> Result<ResolvedRead, ReadArmError> {
    let horizon = store
        .kv_get_sync(&horizon_key(lineage))
        .map_err(|e| ReadArmError::Store(format!("cannot read horizon: {e:?}")))?;
    // `at=` wins: it is the address, the positional is the default the
    // caller passes when `at` is absent.
    if let Some(address) = at {
        return resolve_commit_address(store, lineage, address, horizon, strict);
    }
    match horizon {
        None => Ok(ResolvedRead {
            fold_ts: timestamp.to_string(),
            basis: "recorded",
            sha: None,
            horizon: None,
            dirty_at_commit: Vec::new(),
        }),
        Some(h) => {
            if timestamp >= h.as_str() {
                return Ok(ResolvedRead {
                    fold_ts: timestamp.to_string(),
                    basis: "recorded",
                    sha: None,
                    horizon: Some(h),
                    dirty_at_commit: Vec::new(),
                });
            }
            let commits = commit_stamps(store, lineage)
                .map_err(|e| ReadArmError::Store(format!("cannot list commits: {e}")))?;
            let target = commits
                .iter()
                .rev()
                .find(|(_, stamp)| stamp.as_str() <= timestamp);
            match target {
                None => Err(ReadArmError::BeforeHistory {
                    lineage: lineage.to_string(),
                    instant: timestamp.to_string(),
                    first_commit: commits.first().map(|(sha, _)| sha.clone()),
                    horizon: h,
                }),
                Some((sha, _)) => {
                    if strict {
                        return Err(ReadArmError::BeforeHorizon {
                            lineage: lineage.to_string(),
                            instant: timestamp.to_string(),
                            horizon: h,
                        });
                    }
                    let fold_ts = completion_or_backfill(store, lineage, sha)?;
                    Ok(ResolvedRead {
                        fold_ts,
                        basis: "git-history",
                        sha: Some(sha.clone()),
                        horizon: Some(h),
                        dirty_at_commit: Vec::new(),
                    })
                }
            }
        }
    }
}

/// `at=` resolution (P5b.3a, A-3): a sha addresses its commit's
/// completion instant; a timestamp restates the read instant; anything
/// else is a ref and refuses until P5b.4.
fn resolve_commit_address(
    store: &CodeGraphStore,
    lineage: &str,
    address: &str,
    horizon: Option<String>,
    strict: bool,
) -> Result<ResolvedRead, ReadArmError> {
    // Timestamp-shaped addresses restate the instant (A-3: `at` takes a
    // timestamp, a sha, or — with P5b.4 — a ref).
    if macrame::util::timestamp::normalize(address).is_ok() {
        return resolve_temporal_read(store, lineage, address, strict, None);
    }
    let is_sha = address.len() == 40 && address.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_sha {
        return Err(ReadArmError::RefNeedsP5b4 {
            address: address.to_string(),
        });
    }
    let commits = commit_stamps(store, lineage)
        .map_err(|e| ReadArmError::Store(format!("cannot list commits: {e}")))?;
    if !commits.iter().any(|(sha, _)| sha == address) {
        return Err(ReadArmError::UnknownCommit {
            lineage: lineage.to_string(),
            sha: address.to_string(),
        });
    }
    let fold_ts = completion_or_backfill(store, lineage, address)?;
    // P5b.3a: no observation rows exist until P5d, so the dirty set is
    // empty and `strict` has nothing to refuse. The check ships with the
    // field so P5d fills the set without changing the rule.
    let dirty_at_commit: Vec<String> = Vec::new();
    if strict && !dirty_at_commit.is_empty() {
        return Err(ReadArmError::BeforeHorizon {
            lineage: lineage.to_string(),
            instant: address.to_string(),
            horizon: horizon.unwrap_or_else(|| "none".to_string()),
        });
    }
    Ok(ResolvedRead {
        fold_ts,
        basis: "git-history",
        sha: Some(address.to_string()),
        horizon,
        dirty_at_commit,
    })
}

/// A commit's completion instant, rebuilding the index inline when `kv`
/// lost it (H-20: the ledger is the slow path that never loses).
fn completion_or_backfill(
    store: &CodeGraphStore,
    lineage: &str,
    sha: &str,
) -> Result<String, ReadArmError> {
    let store_err = |e: String| ReadArmError::Store(e);
    match store
        .kv_get_sync(&commit_done_key(lineage, sha))
        .map_err(|e| store_err(format!("cannot read completion: {e:?}")))?
    {
        Some(done) => Ok(done),
        None => {
            backfill_commit_index(store, lineage, false)
                .map_err(|e| store_err(format!("inline backfill failed: {e}")))?;
            let (completions, _) = commit_completion_map(store, lineage)
                .map_err(|e| store_err(format!("cannot map completions: {e}")))?;
            completions
                .into_iter()
                .find(|(s, _)| s == sha)
                .map(|(_, done)| done)
                .ok_or_else(|| store_err(format!("rebuilt index lacks commit {sha} on {lineage}")))
        }
    }
}

/// Message bodies for commit rows minted before the body field existed
/// (P6c slice, DR-45: P5a.3-era rows carry subject only). Explicit-op
/// only — never automatic, never lazy on read: it needs the git repo
/// present (a store can outlive its checkout), and mixed-version resumes
/// are why it exists as an op rather than a migrate step (new rows arrive
/// with bodies, old rows get healed here).
///
/// Reads `git log` per body-less row and restates the row on its OWN mint
/// lineage via `backfill_commit_bodies` (trunk or branch — the §P5a.5
/// choke-point rule). Unknown shas (shallow clones, gc'd objects) and
/// body-less commits skip auditable, never error. Idempotent: healed rows
/// never re-qualify, so re-runs heal zero.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackfillSkip {
    pub sha: String,
    pub reason: String,
}

/// Report-shape-as-contract (P1/P2 rule): counts plus the auditable
/// skip list, never just a number.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackfillReport {
    pub store: String,
    pub repo: String,
    pub commits_healed: usize,
    pub commits_skipped: Vec<BackfillSkip>,
    pub elapsed_secs: f64,
}

/// Refusals (bad repo, wrong store) vs infrastructure (open/query/write).
/// Mirrors the ingest FFI split: `Repo`/`Refused` are user errors,
/// `Store` is infrastructure.
#[derive(Debug)]
pub enum BackfillError {
    Repo(String),
    Refused(String),
    Store(String),
}

impl std::fmt::Display for BackfillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackfillError::Repo(e) => write!(f, "backfill-commit-messages [repo]: {e}"),
            BackfillError::Refused(e) => write!(f, "backfill-commit-messages [refused]: {e}"),
            BackfillError::Store(e) => write!(f, "backfill-commit-messages [store]: {e}"),
        }
    }
}

pub fn backfill_commit_messages(
    store_path: &str,
    repo_path: &str,
) -> Result<BackfillReport, BackfillError> {
    let started = std::time::Instant::now();
    let store = crate::storage::CodeGraphStore::open(store_path)
        .map_err(|e| BackfillError::Store(format!("cannot open {store_path:?}: {e:?}")))?;
    // INV-12, same as ingest: commit rows only exist on commit-time
    // stores, so a legacy store is a refusal, not an empty success.
    // Flagless stores hold no commit rows by construction (every writer
    // of such rows claims the flag first) — they read as zeros.
    match store
        .kv_get_sync(STAMPING_MODE_KEY)
        .map_err(|e| BackfillError::Store(format!("cannot read stamping mode: {e:?}")))?
        .as_deref()
    {
        Some(MODE_COMMIT_TIME) => {}
        // Flagless with rows is a legacy index-now store (INV-12, same
        // posture as ingest): the operator pointed at the wrong store.
        // Flagless and empty is fresh — zeros, honestly.
        None => {
            let (_guard, conn) = store.open_diagnostic_conn().map_err(|e| {
                BackfillError::Store(format!("cannot inspect {store_path:?}: {e:?}"))
            })?;
            let rows: i64 = crate::storage::runtime()
                .block_on(async {
                    let mut rows = conn
                        .query("SELECT COUNT(*) FROM concepts", Vec::<libsql::Value>::new())
                        .await
                        .map_err(macrame::DbError::Engine)?;
                    let n = match rows.next().await.map_err(macrame::DbError::Engine)? {
                        Some(row) => row.get::<i64>(0).unwrap_or(0),
                        None => 0,
                    };
                    Ok::<_, macrame::DbError>(n)
                })
                .map_err(|e| {
                    BackfillError::Store(format!("cannot inspect {store_path:?}: {e:?}"))
                })?;
            if rows > 0 {
                return Err(BackfillError::Refused(format!(
                    "backfill needs a commit-time store: {store_path:?} has no stamping-mode \
                     flag but holds {rows} concept rows, so it is a legacy index-now store"
                )));
            }
        }
        Some(MODE_INDEX_NOW) => {
            return Err(BackfillError::Refused(format!(
                "backfill needs a commit-time store: {store_path:?} is index-now"
            )))
        }
        Some(other) => {
            return Err(BackfillError::Store(format!(
                "unknown stamping mode {other:?} on {store_path:?}"
            )))
        }
    }
    let git = git2::Repository::open(repo_path)
        .map_err(|e| BackfillError::Repo(format!("cannot open {repo_path:?}: {e}")))?;
    let (healed, skipped) = store
        .backfill_commit_bodies(&|sha| {
            let oid = git2::Oid::from_str(sha).map_err(|_| "unknown-sha".to_string())?;
            let commit = git
                .find_commit(oid)
                .map_err(|_| "unknown-sha".to_string())?;
            match commit
                .body()
                .ok()
                .flatten()
                .map(str::trim)
                .filter(|b| !b.is_empty())
            {
                Some(body) => Ok(Some(body.to_string())),
                None => Ok(None),
            }
        })
        .map_err(|e| BackfillError::Store(format!("backfill failed: {e:?}")))?;
    Ok(BackfillReport {
        store: store_path.to_string(),
        repo: repo_path.to_string(),
        commits_healed: healed,
        commits_skipped: skipped
            .into_iter()
            .map(|(sha, reason)| BackfillSkip { sha, reason })
            .collect(),
        elapsed_secs: started.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod backfill_tests {
    use super::*;

    fn write_and_stage(repo: &git2::Repository, name: &str, contents: &str) {
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join(name), contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }

    fn commit_on(
        repo: &git2::Repository,
        reference: &str,
        message: &str,
        parents: &[&git2::Commit],
    ) -> String {
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t.t").unwrap();
        repo.commit(Some(reference), &sig, &sig, message, &tree, parents)
            .unwrap()
            .to_string()
    }

    /// main: c1 subject-only, c2 with a two-line body. Returns (dir, c1, c2).
    fn tiny_repo() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_and_stage(&repo, "a.py", "x = 1\n");
        let c1 = commit_on(&repo, "refs/heads/main", "first subject", &[]);
        let p1 = repo.find_commit(git2::Oid::from_str(&c1).unwrap()).unwrap();
        write_and_stage(&repo, "a.py", "x = 2\n");
        let c2 = commit_on(
            &repo,
            "refs/heads/main",
            "second subject\n\nsecond body line one\nsecond body line two",
            &[&p1],
        );
        (dir, c1, c2)
    }

    /// A P5a.3-era row: subject, no body — exactly what the scratch
    /// migration minted before the body field existed.
    fn legacy_row(sha: &str) -> macrame::ConceptUpsert {
        macrame::ConceptUpsert::new(format!("commit:{sha}"), format!("commit:{sha}"))
            .content(
                serde_json::json!({
                    "meta_version": 2, "kind": "commit", "sha": sha,
                    "lineage": "main", "committer_ts": "2026-01-01T00:00:00.000000Z",
                    "subject": "legacy subject",
                })
                .to_string(),
            )
            .extra(r#"{"coderadar":{"kind":"commit"}}"#.to_string())
            .valid_from("2026-01-01T00:00:00.000000Z".to_string())
            .valid_to(crate::storage::TS_OPEN.to_string())
            .retired(false)
    }

    /// A backfilled store IS a commit-time store: claim the flag the way
    /// a real ingest would, so the INV-12 gate reads it as one.
    fn commit_time_store(path: &std::path::Path) -> crate::storage::CodeGraphStore {
        let store = crate::storage::CodeGraphStore::open(path).unwrap();
        store
            .kv_put_sync(STAMPING_MODE_KEY, MODE_COMMIT_TIME)
            .unwrap();
        store
    }

    fn read_row(store: &crate::storage::CodeGraphStore, id: &str) -> (String, String) {
        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        crate::storage::runtime()
            .block_on(async {
                let mut rows = conn
                    .query(
                        "SELECT content, branch_id FROM concepts WHERE id = ?1",
                        libsql::params![id],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                let row = rows
                    .next()
                    .await
                    .map_err(macrame::DbError::Engine)?
                    .expect("row exists");
                Ok::<_, macrame::DbError>((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                ))
            })
            .unwrap()
    }

    #[test]
    fn heals_a_subject_only_row() {
        let (dir, _c1, c2) = tiny_repo();
        let store_path = dir.path().join("bf.db");
        let store = commit_time_store(&store_path);
        store.upsert_concepts_bulk(&[legacy_row(&c2)]).unwrap();
        let report =
            backfill_commit_messages(store_path.to_str().unwrap(), dir.path().to_str().unwrap())
                .unwrap();
        assert_eq!(report.commits_healed, 1);
        assert!(report.commits_skipped.is_empty());
        let (content, branch_id) = read_row(&store, &format!("commit:{c2}"));
        assert!(content.contains("second body line one"), "{content}");
        assert!(
            content.contains("legacy subject"),
            "subject kept: {content}"
        );
        assert_ne!(branch_id, "", "trunk rows carry the trunk branch");
    }

    #[test]
    fn rerun_heals_zero() {
        let (dir, _c1, c2) = tiny_repo();
        let store_path = dir.path().join("bf2.db");
        commit_time_store(&store_path)
            .upsert_concepts_bulk(&[legacy_row(&c2)])
            .unwrap();
        let run = || {
            backfill_commit_messages(store_path.to_str().unwrap(), dir.path().to_str().unwrap())
                .unwrap()
        };
        assert_eq!(run().commits_healed, 1);
        let second = run();
        assert_eq!(second.commits_healed, 0);
        assert!(second.commits_skipped.is_empty());
    }

    #[test]
    fn restate_rides_the_mint_lineage() {
        // The §P5a.5 choke-point rule covers every restate, including this
        // one: a branch-held commit row healed on the trunk would trip
        // the cross-lineage trigger on its own id.
        let (dir, _c1, c2) = tiny_repo();
        let store_path = dir.path().join("bf3.db");
        let store = commit_time_store(&store_path);
        store.fork_branch("feat", "main").unwrap();
        let branched = legacy_row(&c2).on_branch(macrame::BranchId::new("feat").unwrap());
        store.upsert_concepts_bulk(&[branched]).unwrap();
        let report =
            backfill_commit_messages(store_path.to_str().unwrap(), dir.path().to_str().unwrap())
                .unwrap();
        assert_eq!(report.commits_healed, 1);
        let (content, branch_id) = read_row(&store, &format!("commit:{c2}"));
        assert!(content.contains("second body line two"), "{content}");
        assert_eq!(branch_id, "feat");
    }

    #[test]
    fn unknown_sha_and_bodiless_skip_auditable() {
        let (dir, c1, _c2) = tiny_repo();
        let store_path = dir.path().join("bf4.db");
        let store = commit_time_store(&store_path);
        // c1's git message is subject-only: nothing to heal.
        store
            .upsert_concepts_bulk(&[legacy_row(&c1), legacy_row(&"0".repeat(40))])
            .unwrap();
        let report =
            backfill_commit_messages(store_path.to_str().unwrap(), dir.path().to_str().unwrap())
                .unwrap();
        assert_eq!(report.commits_healed, 0);
        let mut reasons: Vec<_> = report
            .commits_skipped
            .iter()
            .map(|s| (s.sha.clone(), s.reason.clone()))
            .collect();
        reasons.sort();
        assert_eq!(
            reasons,
            vec![
                ("0".repeat(40), "unknown-sha".to_string()),
                (c1, "no-body".to_string()),
            ]
        );
    }

    #[test]
    fn index_now_store_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("bf5.db");
        let store = crate::storage::CodeGraphStore::open(&store_path).unwrap();
        store
            .kv_put_sync(STAMPING_MODE_KEY, MODE_INDEX_NOW)
            .unwrap();
        assert!(matches!(
            backfill_commit_messages(store_path.to_str().unwrap(), dir.path().to_str().unwrap(),),
            Err(BackfillError::Refused(_))
        ));
    }

    #[test]
    fn flagless_store_with_rows_refuses_as_legacy() {
        // No flag + concept rows = a legacy index-now store (INV-12, same
        // posture as ingest): the operator pointed at the wrong store.
        // Flagless and empty stays zeros, honestly.
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("bf6.db");
        let store = crate::storage::CodeGraphStore::open(&store_path).unwrap();
        store
            .upsert_concepts_bulk(
                &[macrame::ConceptUpsert::new("a.py::module", "a.py::module")
                    .content(r#"{"meta_version":2,"kind":"module"}"#.to_string())
                    .valid_from(crate::storage::now_iso8601())
                    .valid_to(crate::storage::TS_OPEN.to_string())
                    .retired(false)],
            )
            .unwrap();
        assert!(matches!(
            backfill_commit_messages(store_path.to_str().unwrap(), dir.path().to_str().unwrap(),),
            Err(BackfillError::Refused(_))
        ));
    }
    // ── K8: the merge-commit heal (Amendment 2 §10 item 4, spec'd in
    // CodeRadar's temporal plan §8, 2026-11-09) ─────────────────────────

    /// A→B(feat: lib_x.py + gone.py, main still at A). The merge itself
    /// (adding fresh.py) and the tail (deleting gone.py) build **after**
    /// the branch ingest this fixture pairs with — the operator order
    /// the heal serves: `fork_point` slides to the branch tip through a
    /// merged main (its walk accepts the first branch-chain commit main's
    /// tip still reaches, and a merge makes that the tip), so a
    /// post-merged branch ingest walks nothing, and the mint the heal
    /// adopts has to predate the merge's arrival in the ledger.
    fn pre_merge_repo() -> (tempfile::TempDir, git2::Repository, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        write_and_stage(
            &repo, "a.py", "x = 1
",
        );
        let a = commit_on(&repo, "refs/heads/main", "one", &[]);
        let b = {
            let pa = repo.find_commit(git2::Oid::from_str(&a).unwrap()).unwrap();
            write_and_stage(
                &repo,
                "lib_x.py",
                "def x_fn():
    return 1
",
            );
            write_and_stage(
                &repo,
                "gone.py",
                "def gone_fn():
    return 2
",
            );
            commit_on(&repo, "refs/heads/feat", "feature", &[&pa])
        };
        (dir, repo, a, b)
    }

    /// The trunk's side of the story, built after the branch ingest: M
    /// merges feat onto main and adds a file of its own, C deletes
    /// gone.py. `a` and `b` name both merge parents.
    fn apply_merge(repo: &git2::Repository, a: &str, b: &str) -> (String, String) {
        let pa = repo.find_commit(git2::Oid::from_str(a).unwrap()).unwrap();
        let pb = repo.find_commit(git2::Oid::from_str(b).unwrap()).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let a_tree = pa.tree().unwrap();
        let mut index = repo.index().unwrap();
        index.read_tree(&a_tree).unwrap();
        index.write().unwrap();
        write_and_stage(repo, "lib_x.py", "def x_fn():\n    return 1\n");
        write_and_stage(repo, "gone.py", "def gone_fn():\n    return 2\n");
        write_and_stage(repo, "fresh.py", "def fresh_fn():\n    return 3\n");
        let m = commit_on(repo, "refs/heads/main", "merge", &[&pa, &pb]);
        let pm = repo.find_commit(git2::Oid::from_str(&m).unwrap()).unwrap();
        write_and_stage(
            repo,
            "lib_x.py",
            "def x_fn():
    return 5
",
        );
        let mut index = repo.index().unwrap();
        index.remove_path(std::path::Path::new("gone.py")).unwrap();
        index.write().unwrap();
        let _ = std::fs::remove_file(repo.workdir().unwrap().join("gone.py"));
        let e = commit_on(repo, "refs/heads/main", "tail", &[&pm]);
        (m, e)
    }

    /// The feat lineage, ingested while main still sits at the merge
    /// base: the store the heal's vintage shape needs, with the mode
    /// claim a real write carries.
    fn ingest_feat(store_path: &std::path::Path, repo: &str) {
        crate::graph::ingest::ingest_branch_history(
            &store_path.to_string_lossy(),
            repo,
            "feat",
            "feat",
            "main",
            None,
            false,
        )
        .unwrap();
    }

    /// The trunk walk's own run (findings 1 and 2's shape): the whole
    /// plan, commit-time stamps, everything the walk writes.
    fn ingest_main(store_path: &std::path::Path, repo: &str) {
        crate::graph::ingest::ingest_history(
            &store_path.to_string_lossy(),
            repo,
            "main",
            None,
            false,
        )
        .unwrap();
    }

    /// The vintage shape, forged on a store where the walk already ran
    /// (under the fixed read): the trunk-side evidence for every id under
    /// each prefix removed under the archive-session marker the delete
    /// guards arm from — overlay rows, their FTS shadow rows, the
    /// projection rows, and the log rows both spellings write. The branch
    /// mints (their own branch_id) are untouched: that is the vintage
    /// shape's story, the file's whole id tree hollow on the trunk.
    fn forge_hollow(store_path: &std::path::Path, ids: &[&str]) {
        let path = store_path.to_string_lossy().to_string();
        crate::storage::runtime().block_on(async {
            // The diagnostic conn is readonly; the forge writes through a
            // throwaway local connection against the live store (no
            // reopen: `open` verifies and would refuse the tampered file
            // first).
            let db = libsql::Builder::new_local(&path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute("CREATE TABLE macrame_archive_session (x)", ())
                .await
                .unwrap();
            for id in ids {
                let id = id.to_string();
                for sql in [
                    "DELETE FROM concept_version_fts_rows                       WHERE id LIKE ?1 AND branch_id IN ('', 'main')",
                    "DELETE FROM concept_versions                       WHERE id LIKE ?1 AND branch_id IN ('', 'main')",
                    "DELETE FROM transaction_log                       WHERE entity_id LIKE ?1 AND branch_id IN ('', 'main')",
                    "DELETE FROM concepts                       WHERE id LIKE ?1 AND branch_id IN ('', 'main')",
                ] {
                    conn.execute(sql, libsql::params![id.clone()])
                        .await
                        .unwrap();
                }
            }
            conn.execute("DROP TABLE macrame_archive_session", ())
                .await
                .unwrap();
        });
    }

    /// The overlay rows a foreign-minted id's trunk version holds: a
    /// content-equality reader for the tests (the rows' own payloads).
    fn overlay_rows_at(store: &crate::storage::CodeGraphStore, id: &str) -> Vec<(String, String)> {
        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        crate::storage::runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT valid_from, content FROM concept_versions                       WHERE id = ?1 AND branch_id IN ('', 'main') ORDER BY valid_from",
                    libsql::params![id],
                )
                .await
                .unwrap();
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                out.push((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                ));
            }
            out
        })
    }

    #[test]
    fn the_heal_adopts_the_vintage_skip_at_the_merge_s_stamp() {
        let (dir, repo, a, b) = pre_merge_repo();
        let repo_s = dir.path().to_string_lossy().to_string();
        let store_path = dir.path().join("heal.db");
        ingest_feat(&store_path, &repo_s);
        let (m, _e) = apply_merge(&repo, &a, &b);
        // the precondition: the mint is away from the trunk
        let store_check = crate::storage::CodeGraphStore::open(&store_path).unwrap();
        assert_eq!(
            store_check
                .mint_lineage("lib_x.py::module")
                .unwrap()
                .unwrap(),
            "feat",
            "the branch-born id is minted on feat before either walk touched it"
        );
        let report = crate::graph::ingest::reingest_merged_commits(
            &store_path.to_string_lossy(),
            &repo_s,
            "main",
            false,
        )
        .unwrap();
        // Every chain commit walks: the vintage skip is per FILE, so the
        // tail's lib_x edit belongs to the heal too (finding 2).
        assert_eq!(report.commits_ingested, 3);
        let adoptions: Vec<&FailedFile> = report
            .adoptions
            .iter()
            .filter(|f| f.path == "lib_x.py")
            .collect();
        assert_eq!(adoptions.len(), 1, "the healed id is audited once");
        assert_eq!(adoptions[0].reason, "adopted-first-version");
        assert_eq!(
            adoptions[0].sha, m,
            "the first trunk version rides the merge"
        );
        let reasons: Vec<(String, String)> = report
            .files_failed
            .iter()
            .map(|f| (f.path.clone(), f.reason.clone()))
            .collect();
        assert!(
            reasons.contains(&("gone.py".to_string(), "gone-by-head".to_string())),
            "the merge's deleted-by-head file is named: {reasons:?}"
        );
        assert!(
            reasons.contains(&("fresh.py".to_string(), "no-mint".to_string())),
            "a merge-only add with no mint anywhere is named, not half-minted: {reasons:?}"
        );
        // The healed shape: the mint keeps its lineage, the trunk spine
        // claims the id, and the tail's edit re-asserted at its own stamp
        // (two overlay rows, different contents).
        assert_eq!(
            read_row(&store_check, "lib_x.py::module").1,
            "feat",
            "the provenance row keeps the mint lineage after the heal"
        );
        assert!(store_check.trunk_version_live("lib_x.py::module").unwrap());
        let rows = overlay_rows_at(&store_check, "lib_x.py::module");
        assert_eq!(rows.len(), 2, "the merge's row and the tail's re-assert");
        assert_ne!(rows[0].1, rows[1].1, "the values differ per commit");
    }

    #[test]
    fn the_heal_is_idempotent_a_second_run_adopts_nothing() {
        let (dir, repo, a, b) = pre_merge_repo();
        let repo_s = dir.path().to_string_lossy().to_string();
        let store_path = dir.path().join("heal.db");
        ingest_feat(&store_path, &repo_s);
        let (m, _e) = apply_merge(&repo, &a, &b);
        let first = crate::graph::ingest::reingest_merged_commits(
            &store_path.to_string_lossy(),
            &repo_s,
            "main",
            false,
        )
        .unwrap();
        assert!(first.adoptions.iter().any(|f| f.path == "lib_x.py"));
        let second = crate::graph::ingest::reingest_merged_commits(
            &store_path.to_string_lossy(),
            &repo_s,
            "main",
            false,
        )
        .unwrap();
        // Both commits walk again; nothing is adopted twice, and the
        // re-walk's writes land byte-identically (the filter drops them),
        // which is why neither `adoptions` nor `files_failed` carries a
        // lib_x entry.
        assert_eq!(second.commits_ingested, 3);
        assert!(second.adoptions.iter().all(|f| f.path != "lib_x.py"));
        let named: Vec<(String, String)> = second
            .files_failed
            .iter()
            .map(|f| (f.path.clone(), f.reason.clone()))
            .collect();
        assert!(
            named.contains(&("gone.py".to_string(), "gone-by-head".to_string())),
            "the gone file stays named"
        );
        assert!(
            !named.iter().any(|(p, _)| p == "lib_x.py"),
            "a re-assert of what the spine holds is no failure and no news"
        );
    }

    #[test]
    fn the_dry_run_names_the_set_and_writes_nothing() {
        let (dir, repo, a, b) = pre_merge_repo();
        let repo_s = dir.path().to_string_lossy().to_string();
        let store_path = dir.path().join("heal.db");
        ingest_feat(&store_path, &repo_s);
        let (m, _e) = apply_merge(&repo, &a, &b);
        let _ = m;
        let report = crate::graph::ingest::reingest_merged_commits(
            &store_path.to_string_lossy(),
            &repo_s,
            "main",
            true,
        )
        .unwrap();
        assert!(
            report
                .adoptions
                .iter()
                .any(|f| f.path == "lib_x.py" && f.reason == "dry-run"),
            "the dry run names the would-heal"
        );
        // Finding 3: the dry run and the real run agree about a file the
        // trunk deleted afterwards — the same auditable verdict, not a
        // promised adoption.
        let named: Vec<(String, String)> = report
            .files_failed
            .iter()
            .map(|f| (f.path.clone(), f.reason.clone()))
            .collect();
        assert!(
            named.contains(&("gone.py".to_string(), "dry-run:gone-by-head".to_string())),
            "the dry run names the gone file the way the real run would: {named:?}"
        );
        // The dry run's writes: none.
        let store_check = crate::storage::CodeGraphStore::open(&store_path).unwrap();
        assert_eq!(
            store_check
                .mint_lineage("lib_x.py::module")
                .unwrap()
                .unwrap(),
            "feat",
            "the dry run wrote nothing"
        );
        assert!(
            !store_check.trunk_version_live("lib_x.py::module").unwrap(),
            "no overlay claim exists, so the dry run's verdict is the only answer"
        );
    }

    #[test]
    fn the_heal_refuses_a_missing_store_and_a_non_trunk_branch() {
        let (dir, repo, a, b) = pre_merge_repo();
        let repo_s = dir.path().to_string_lossy().to_string();
        let missing = dir.path().join("absent.db");
        let report = crate::graph::ingest::reingest_merged_commits(
            &missing.to_string_lossy(),
            &repo_s,
            "main",
            false,
        );
        assert!(matches!(report, Err(IngestError::Store(_))));
        let store_path = dir.path().join("feats.db");
        ingest_feat(&store_path, &repo_s);
        let (m, _e) = apply_merge(&repo, &a, &b);
        let _ = m;
        let report = crate::graph::ingest::reingest_merged_commits(
            &store_path.to_string_lossy(),
            &repo_s,
            "feat",
            false,
        );
        assert!(matches!(
            report,
            Err(IngestError::TrunkBranchMismatch { .. })
        ));
    }

    #[test]
    fn the_trunk_edits_a_merged_file_after_the_merge_adopts_it() {
        let (dir, repo, a, b) = pre_merge_repo();
        let store_path = dir.path().join("walk.db");
        let repo_s = dir.path().to_string_lossy().to_string();
        ingest_feat(&store_path, &repo_s);
        let (m, e) = apply_merge(&repo, &a, &b);
        // The trunk's own walks read the write's own resolution: the
        // merged file's next edit lands the trunk's next version there,
        // instead of a refused whole-commit bulk halting the walk.
        let report = crate::graph::ingest::ingest_history(
            &store_path.to_string_lossy(),
            &repo_s,
            "main",
            None,
            false,
        )
        .unwrap();
        assert_eq!(report.commits_ingested, 3, "one, the merge, the edit");
        assert_eq!(
            report.head.as_deref(),
            Some(e.as_str()),
            "the walk reached the tip, not the refusal's position"
        );
        assert!(report.files_failed.is_empty(), "{:?}", report.files_failed);
        let store_check = crate::storage::CodeGraphStore::open(&store_path).unwrap();
        let rows = overlay_rows_at(&store_check, "lib_x.py::module");
        assert_eq!(rows.len(), 2, "the merge's adoption, the edit's version");
        assert!(
            rows[0].0 < rows[1].0,
            "the overlay rows order by valid_from cleanly",
        );
        assert_ne!(
            rows[0].1, rows[1].1,
            "the edit's content, not merge-day's, is the trunk's version"
        );
        assert!(
            store_check.trunk_version_live("lib_x.py::module").unwrap(),
            "the trunk spine claims the adopted id, which is the read's own truth"
        );
    }

    #[test]
    fn the_heal_restores_the_trunk_s_later_edits_of_a_hollow_file() {
        // The shape a real vintage store carries: the trunk walk ran past
        // the merge day under the old shape — the merge and every later
        // edit of the hollow file were skipped — and the trunk's cursor
        // sits past all of it, so a resume heals nothing. Under the fixed
        // read the whole chain (whole commits) re-heals against the forge:
        // the merge day's adoption, then the tail's content through the
        // trunk's own supersede chain — the trunk's content matches what a
        // full pass would have written, at every commit, not merge-day.
        let (dir, repo, a, b) = pre_merge_repo();
        let store_path = dir.path().join("hollow.db");
        let repo_s = dir.path().to_string_lossy().to_string();
        ingest_feat(&store_path, &repo_s);
        let (m, e) = apply_merge(&repo, &a, &b);
        let _ = m;
        let walk = crate::graph::ingest::ingest_history(
            &store_path.to_string_lossy(),
            &repo_s,
            "main",
            None,
            false,
        )
        .unwrap();
        assert_eq!(walk.commits_ingested, 3);
        let truth = {
            let store_check = crate::storage::CodeGraphStore::open(&store_path).unwrap();
            overlay_rows_at(&store_check, "lib_x.py::module")
        };
        assert_eq!(truth.len(), 2, "the walk wrote both versions");
        // The forge: the trunk-side evidence erased (the vintage shape the
        // store carries), the branch mint untouched.
        forge_hollow(&store_path, &["lib_x.py::%"]);
        {
            let store_check = crate::storage::CodeGraphStore::open(&store_path).unwrap();
            assert!(
                !store_check.trunk_version_live("lib_x.py::module").unwrap(),
                "the trunk side of the id is hollow before the heal"
            );
            assert_eq!(
                store_check
                    .mint_lineage("lib_x.py::module")
                    .unwrap()
                    .unwrap(),
                "feat",
                "the mint row keeps the branch lineage"
            );
        }
        let report = crate::graph::ingest::reingest_merged_commits(
            &store_path.to_string_lossy(),
            &repo_s,
            "main",
            false,
        )
        .unwrap();
        assert_eq!(
            report.commits_ingested, 3,
            "the chain walks: the trunk commit, the merge, the edit"
        );
        assert_eq!(
            report
                .adoptions
                .iter()
                .filter(|f| f.path == "lib_x.py")
                .count(),
            1,
            "the first trunk version is the merge's adoption"
        );
        let store_check = crate::storage::CodeGraphStore::open(&store_path).unwrap();
        let healed = overlay_rows_at(&store_check, "lib_x.py::module");
        assert_eq!(
            healed.len(),
            truth.len(),
            "the heal wrote one row per skipped commit"
        );
        assert_eq!(
            healed.last().expect("rows").1,
            truth.last().expect("rows").1,
            "the healed content at the tip equals what a full pass wrote"
        );
        assert_eq!(
            report.head.as_deref(),
            Some(e.as_str()),
            "the heal reached the tip"
        );
    }
}
