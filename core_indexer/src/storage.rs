// CodeRadar v3.6 — Macrame Storage Interface (§10)
// Bridges CodeRadar's entity model to Macrame's concept+assertion model.
// Macrame is tokio-based; CodeRadar wraps it with block_on behind a sync API.

use macrame::graph::{Subgraph, TraversalBuilder};
use macrame::prelude::*;
use macrame::temporal::{MaterializedState, SnapshotCadence};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tokio::runtime::Runtime;

use crate::types::*;

// ── Annotation / Edge Property Constants ────────────────────────────────────

/// Keys for JSON metadata stored in ConceptUpsert.content (entity annotations).
pub mod annotation {
    pub const KIND: &str = "kind";
    pub const FILE_PATH: &str = "file_path";
    pub const LANGUAGE: &str = "language";
    pub const LINE: &str = "line";
    pub const END_LINE: &str = "end_line";
    pub const START_BYTE: &str = "start_byte";
    pub const END_BYTE: &str = "end_byte";
    pub const NAME_SPAN: &str = "name_span";
    pub const BODY_SPAN: &str = "body_span";
    pub const PARAMS_SPAN: &str = "params_span";
    pub const SIGNATURE: &str = "signature";
    pub const DOCSTRING: &str = "docstring";
    pub const IS_ASYNC: &str = "is_async";
    pub const IS_STATIC: &str = "is_static";
    pub const DECORATORS: &str = "decorators";
    pub const CONTENT_HASH: &str = "content_hash";
    pub const PARSE_QUALITY: &str = "parse_quality";
    pub const RETURN_TYPE: &str = "return_type";
    pub const GRAMMAR_KIND: &str = "grammar_kind"; // v3.6: tree-sitter node kind
}

/// Edge types for Macrame edge assertions.
pub mod edge_type {
    pub const CONTAINS: &str = "CONTAINS";
    pub const CALLS: &str = "CALLS";
    pub const IMPORTS: &str = "IMPORTS";
    pub const EXTENDS: &str = "EXTENDS";
    pub const IMPLEMENTS: &str = "IMPLEMENTS";
    pub const REFERENCES: &str = "REFERENCES";
    pub const DECORATES: &str = "DECORATES";
    pub const INSTANTIATES: &str = "INSTANTIATES";
    pub const OVERRIDES: &str = "OVERRIDES";
    /// P3 (DR-38): the content-version spine edge. Ledger-only — never in
    /// the projection, never structural, never `synthetic:` — so neither
    /// `retire_stale_edges`, nor the DR-42 hook sweep, nor the single-open
    /// guard's fresh set touches it; `maintain_content_spine` owns it.
    ///
    /// RETIRED WITH THE EDGE (P3 as shipped, DR-38): the spine is
    /// concepts-only. A `HAD_CONTENT` edge target names its concept
    /// (`{path}@{digest}`), and Macrame's archive reference scan reads
    /// link payloads — so every closed edge version would pin its blob
    /// hot forever and kill retention (§3.4). The chain needs no edge:
    /// concepts order by `valid_from` and intervals derive from chain
    /// order. Kept as a reserved name, not a live kind.
    pub const HAD_CONTENT: &str = "HAD_CONTENT";
    /// P4 (DR-40): the branch-content pointer concept kind. Ledger-only —
    /// never in the projection, never traversed, never swept: the id
    /// (`branch_pointer_id`) sits outside every file-prefix range, the
    /// row carries no `file_path`, and the kind is excluded from the
    /// entity listings beside `content_version`.
    ///
    /// Why a concept and not an edge: Macrame concepts are
    /// lineage-frozen — a branch inherits its parent's concepts and
    /// cannot restate one (D-225; the overlay is deferred as D-214) — so
    /// divergent bytes cannot live in the module concept. A branch
    /// therefore mints its OWN pointer concept per path, id
    /// `branch:{branch}:content:{path}` (stable per lineage+path, so
    /// every re-point is same-id turnover and the archive reclaims
    /// superseded bytes exactly like superseded module content). An
    /// earlier edge-shaped pointer died on retention evidence: link log
    /// rows are terminal (a closed row is latest-for-its-entity
    /// forever), so every superseded branch version would pin its blob
    /// hot forever; concept turnover has no such trap.
    ///
    /// The read side is a direct map lookup (no scan): the branch's
    /// pointer wins when present, else the module concept (trunk-frozen
    /// fallback — one read algorithm, no special case). The trunk never
    /// mints one. The row names its blob in content AND extra (the live
    /// pin, same as module `extra`); superseded rows release on turnover.
    pub const BRANCH_CONTENT_KIND: &str = "branch_content";
    /// P5 commit concept (`kind = 'commit'`): one row per ingested commit,
    /// carrying author/committer metadata in `extra`. Ledger infrastructure
    /// like the spine — never an entity, never swept, never listed.
    pub const COMMIT_KIND: &str = "commit";
}

/// One row of `branches()`: the lineage's name, its parent (`None` for
/// the trunk), and the two stamps Macrame records (fork instant and row
/// write — equal for every fork this release can cut; see the `Branch`
/// docs for why they are separate columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub parent: Option<String>,
    pub forked_at: Option<String>,
    pub created_at: String,
}

/// Outcome of one [`CodeGraphStore::maintain_content_spine`] pass: how
/// many flushed modules were seen, and how many spine concepts were
/// upserted. Unchanged files report only in `files_seen` — the
/// zero-growth property the P3 tests pin.
#[derive(Debug, Default)]
pub struct SpineReport {
    pub files_seen: usize,
    pub concepts_upserted: usize,
}

/// One link in a file's content chain, as [`CodeGraphStore::history_of`]
/// returns it: the digest, its ledger interval, the previous digest (or
/// `None` for the first content), and the payload size. `open` is true
/// exactly for the head of the chain (`valid_to` still the open
/// sentinel); a deleted file's chain ends closed via the retire cascade
/// (`retire_entities` closes edges on either side of a retired
/// endpoint), never by spine deletion — the concept is timeless.
#[derive(Debug)]
pub struct SpineVersion {
    pub digest: String,
    pub valid_from: String,
    pub valid_to: String,
    pub open: bool,
    pub parent_digest: Option<String>,
    pub size_bytes: u64,
    pub line_count: u64,
}

/// Build the timeless `{path}@{digest}` spine concept upsert. The id is
/// content-addressed, so re-indexing unchanged content re-upserts an
/// identical row the [`CodeGraphStore::filter_unchanged`] gate absorbs;
/// the payload is metadata-sized (path, size, lines) —
/// never program text, which stays in the blob (P3b constraint) — and,
/// critically, names no blob digest anywhere the archive reference scan
/// reads. Concept payloads DO log `extra` (verified against the ledger,
/// not assumed), so `extra` carries no digests at all: no `source_blob`
/// (the digest is the id suffix) and no `parent_digest` (the parent is
/// the next-older chain link, derived at read time). Title and content
/// carry the path only; the digest lives solely in the id — an
/// unscanned `entity_id` column, since the scan reads payloads. A
/// superseded blob therefore stays archivable (§3.4); the spine pins
/// metadata, never bytes.
///
/// Stable pointer-concept id for one path on one lineage (P4, DR-40):
/// `branch:{branch}:content:{path}`. Deliberately outside every
/// file-prefix range (`{path}::`, `{path}@`): no sweep, no listing, no
/// history chain matches it by accident — the kind exclusion is belt.
fn branch_pointer_id(path: &str, branch: &str) -> String {
    format!("branch:{branch}:content:{path}")
}

/// One branch-content pointer concept (P4, DR-40): the lineage's view of
/// one path's bytes. Same-id turnover by construction (the id is stable
/// per lineage+path), so re-pointing supersedes same-entity and the
/// archive reclaims — the module-content release path, not a new one.
/// The blob address rides content AND extra (the read boundary is
/// `source_blob_of_extra`, mirrored from modules); `file_path` appears
/// nowhere (H-1: a sweep matching it would retire live branch views).
/// One branch-content pointer concept (P4, DR-40): the lineage's view of
/// one path's bytes. Same-id turnover by construction (the id is stable
/// per lineage+path), so re-pointing supersedes same-entity and the
/// archive reclaims exactly like superseded module content.
///
/// The deletion twin (P5a.4, DR-41): the same stable id with a `deleted`
/// marker and NO blob address anywhere — not in content, not in extra
/// (P3's payload discipline: a bool is not an address, and there is no
/// blob to pin). The read side (`source_ref_from_state`) checks the
/// marker before the digest, so a tombstone reads as absent — never as
/// trunk fallback, never as empty bytes. Un-deleting is the same id
/// turned over to content again; the archive reclaims the tombstone row
/// exactly like any other superseded version.
pub(crate) fn branch_tombstone_upsert(
    path: &str,
    branch: &str,
    now: &str,
) -> macrame::Result<ConceptUpsert> {
    let branch_id = macrame::BranchId::new(branch)?;
    let content = serde_json::json!({
        "meta_version": V2_META_VERSION,
        "kind": edge_type::BRANCH_CONTENT_KIND,
        "branch": branch,
        "deleted": true,
    });
    let extra = serde_json::json!({
        "coderadar": {
            "format": 1,
            "kind": edge_type::BRANCH_CONTENT_KIND,
            "deleted": true,
        },
    });
    Ok(
        ConceptUpsert::new(branch_pointer_id(path, branch), path.to_string())
            .content(content.to_string())
            .extra(extra.to_string())
            .valid_from(now.to_string())
            .valid_to(TS_OPEN.to_string())
            .retired(false)
            .on_branch(branch_id),
    )
}

pub(crate) fn branch_pointer_upsert(
    path: &str,
    sha: &str,
    branch: &str,
    now: &str,
) -> macrame::Result<ConceptUpsert> {
    let branch_id = macrame::BranchId::new(branch)?;
    let content = serde_json::json!({
        "meta_version": V2_META_VERSION,
        "kind": edge_type::BRANCH_CONTENT_KIND,
        "branch": branch,
        "source_blob": sha,
    });
    let extra = serde_json::json!({
        "coderadar": {
            "format": 1,
            "kind": edge_type::BRANCH_CONTENT_KIND,
            "source_blob": sha,
        },
    });
    Ok(
        ConceptUpsert::new(branch_pointer_id(path, branch), path.to_string())
            .content(content.to_string())
            .extra(extra.to_string())
            .valid_from(now.to_string())
            .valid_to(TS_OPEN.to_string())
            .retired(false)
            .on_branch(branch_id),
    )
}

fn spine_concept_upsert(
    path: &str,
    digest: &str,
    size: u64,
    line_count: u64,
    now: &str,
) -> ConceptUpsert {
    let id = format!("{path}@{digest}");
    let content = serde_json::json!({
        "meta_version": V2_META_VERSION,
        "kind": "content_version",
        "name": path,
        "file_path": path,
    });
    let extra = serde_json::json!({
        "coderadar": {
            "format": 1,
            "kind": "content_version",
            "file_path": path,
            "content_hash": serde_json::Value::Null,
            "size": size,
            "line_count": line_count,
        }
    });
    ConceptUpsert::new(id, path.to_string())
        .content(content.to_string())
        .extra(extra.to_string())
        .valid_from(now.to_string())
        .valid_to(TS_OPEN.to_string())
        .retired(false)
}

/// Serialises every libSQL open in this process — `Database::open` and
/// `diagnostic_conn` alike (see [`CodeGraphStore::open_diagnostic_conn`]).
static DIAGNOSTIC_OPEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Key lifespan timestamp — the Macrame open sentinel for "still true".
pub const TS_OPEN: &str = "9999-12-31T00:00:00.000000Z";

/// Edge-kind namespace owned by the framework hook's scoped pair-retire
/// (§1.3, DR-42). Minted only by `framework.py::_ledger_synthetic_edge_kind`;
/// see `open_synthetic_triples` for the membership rule and the audit.
pub(crate) const FRAMEWORK_SYNTHETIC_EDGE_PREFIX: &str = "synthetic:";

/// Indexed application metadata namespace carried in Macrame's `concepts.extra`.
const CODERADAR_EXTRA_FILE_PATH: &str = "$.coderadar.file_path";

const LIVE_IDS_BY_EXTRA_FILE_PATH_SQL: &str = "SELECT id FROM concepts WHERE retired = 0 \
    AND json_extract(extra, '$.coderadar.file_path') = ?1 \
    AND (json_extract(extra, '$.coderadar.kind') IS NULL \
         OR json_extract(extra, '$.coderadar.kind') != 'content_version')";
const LIVE_IDS_BY_ID_FILE_RANGE_SQL: &str = "SELECT id FROM concepts WHERE retired = 0 \
    AND id >= ?1 AND id < ?2";

/// Current UTC time as a canonical Macrame timestamp (no chrono dependency).
/// Used as `valid_from` for concepts AND edges so the bitemporal ledger can
/// distinguish *when* a fact was asserted (temporal traversal depends on it),
/// and as the `ts` argument of `Connection::reconstruct` on cold start.
///
/// The microsecond digits are load-bearing, not decorative: Macrame's write
/// actor stamps `transaction_log.recorded_at` through `SystemClock` at full
/// microsecond precision, and `reconstruct(ts)` folds only rows with
/// `recorded_at <= ts` (a `ts` below the log floor returns the empty
/// `predates_recorded_history` state without error). A second-precision `now`
/// truncates up to a millisecond into the past, so a cold start in the same
/// second as the last write silently restores nothing.
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let micros = now.subsec_micros();
    let days_since_epoch = secs / 86400;
    let secs_of_day = secs % 86400;
    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    let seconds = secs_of_day % 60;
    let (y, m, d) = civil_from_days(days_since_epoch as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        y, m, d, hours, minutes, seconds, micros
    )
}

/// Nudge a candidate stamp past a lineage's high-water mark (P5a.2, INV-11).
///
/// Returns `(stamp, nudged)`: the candidate unchanged when it is strictly
/// after `prev`, else `prev` advanced by one microsecond with `nudged =
/// true`, so the caller can record `stamp_nudged` in the commit concept's
/// extra (P5a.3). Comparison is on instants, not strings — committer
/// stamps carry arbitrary offsets, and lexicographic order would misrank
/// them. An unparseable candidate is returned as-is (Macrame refuses it
/// at the write); an unparseable `prev` counts as no bound.
pub fn nudge_valid_from(prev: Option<&str>, candidate: &str) -> (String, bool) {
    let prev_us = prev.and_then(parse_rfc3339_epoch_micros);
    let candidate_us = parse_rfc3339_epoch_micros(candidate);
    match (prev_us, candidate_us) {
        (Some(prev_us), Some(candidate_us)) if candidate_us <= prev_us => {
            (format_epoch_micros_zulu(prev_us + 1), true)
        }
        _ => (candidate.to_string(), false),
    }
}

/// Normalize any parseable stamp to the Zulu microsecond shape Macrame's
/// `normalize_timestamp` accepts (`...SS.ffffffZ`). Committer stamps carry
/// arbitrary offsets; the write boundary speaks only Zulu, so the offset is
/// folded into the instant before the write. The offset itself rides in the
/// commit concept's extra per P5-D2, not in the stamp. `None` when the
/// shape is not a timestamp at all.
pub fn zulu_valid_from(ts: &str) -> Option<String> {
    parse_rfc3339_epoch_micros(ts).map(format_epoch_micros_zulu)
}

/// Epoch microseconds of an RFC3339 stamp (`Z` or `±HH:MM`, optional
/// fractional seconds). `None` when the shape is not a timestamp at all.
fn parse_rfc3339_epoch_micros(stamp: &str) -> Option<i128> {
    let (date, time) = stamp.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let (clock, offset_minutes) = if let Some(bare) = time.strip_suffix(['Z', 'z']) {
        (bare, 0i64)
    } else {
        let at = time.rfind(['+', '-'])?;
        let (bare, zone) = time.split_at(at);
        let (sign, digits) = zone.split_at(1);
        let mut zone_parts = digits.split(':');
        let hours: i64 = zone_parts.next()?.parse().ok()?;
        let minutes: i64 = zone_parts.next()?.parse().ok()?;
        if zone_parts.next().is_some() {
            return None;
        }
        let magnitude = hours * 60 + minutes;
        (bare, if sign == "-" { -magnitude } else { magnitude })
    };
    let mut clock_parts = clock.split(':');
    let hours: i128 = clock_parts.next()?.parse().ok()?;
    let minutes: i128 = clock_parts.next()?.parse().ok()?;
    let seconds = clock_parts.next()?;
    if clock_parts.next().is_some() {
        return None;
    }
    let (whole, micros) = match seconds.split_once('.') {
        Some((whole, frac)) => {
            let mut padded = frac.to_string();
            padded.truncate(6);
            while padded.len() < 6 {
                padded.push('0');
            }
            (whole.parse::<i128>().ok()?, padded.parse::<i128>().ok()?)
        }
        None => (seconds.parse::<i128>().ok()?, 0),
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hours > 23 || minutes > 59 || whole > 60 {
        return None;
    }
    let days = days_since_epoch(year, month, day)?;
    Some(
        (days as i128 * 86_400 + hours * 3_600 + minutes * 60 + whole) * 1_000_000 + micros
            - offset_minutes as i128 * 60 * 1_000_000,
    )
}

/// Days from 1970-01-01 to a civil date (the inverse of
/// [`civil_from_days`]).
fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// Render epoch microseconds as Zulu microsecond RFC3339 (the
/// [`now_iso8601`] shape), so a nudged stamp stays comparable with every
/// stamp the write path ever minted.
fn format_epoch_micros_zulu(micros: i128) -> String {
    let secs = micros.div_euclid(1_000_000);
    let subsec = micros.rem_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Epoch values from real stamps fit i64 comfortably; saturate rather
    // than wrap on absurd input.
    let (year, month, day) = civil_from_days(days.clamp(i64::MIN as i128, i64::MAX as i128) as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{subsec:06}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

// ── CodeGraphStore — owns the Macrame Database + Runtime ────────────────────

pub struct CodeGraphStore {
    pub db: Database,
}

/// One tokio runtime for every store in the process.
///
/// Each `CodeGraphStore` used to build its own, so N stores meant N runtimes
/// and N worker-thread pools, all opening and tearing down libSQL databases.
/// That teardown is where the suite faulted with STATUS_ACCESS_VIOLATION once
/// enough stores existed in one process (5 bad runs in 15; 0 in 30 with a
/// shared runtime). Production only ever opens one store, so this costs it
/// nothing and gives the tests the same shape production has.
pub(crate) fn runtime() -> &'static Runtime {
    static RUNTIME: std::sync::LazyLock<Runtime> =
        std::sync::LazyLock::new(|| Runtime::new().expect("tokio runtime for Macrame"));
    &RUNTIME
}

/// Normalize a Python-side embedding model id to a macrame [`ModelName`]
/// (DR-11 slice 2).
///
/// fastembed ids (`BAAI/bge-small-en-v1.5`) are not bare identifiers and
/// SQLite cannot bind a table name, so the mapping is total and pinned:
/// lowercase, every non-`[a-z0-9]` char becomes `_`, then `ModelName`
/// enforces leading-letter + ≤48 chars. Distinct raw ids CAN collide
/// (`a-b` vs `a/b` → `a_b`); a same-dimension collision would share a
/// table, which is why every persist/rehydrate answer reports the derived
/// table name. The error carries the RAW id (what was configured), not
/// the normalized form.
pub fn macrame_model_name(raw: &str) -> macrame::Result<macrame::vector::ModelName> {
    let normalized: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    macrame::vector::ModelName::new(&normalized)
        .map_err(|_| macrame::DbError::InvalidModelName(raw.to_string()))
}

/// What one persist run wrote (DR-11 slice 2). `healed` marks the
/// count-gate path: no new vectors this run, but the table disagreed with
/// memory (crash between memory commit and table write, or staged vectors
/// that never persisted), so the whole memory set was rewritten.
#[derive(Debug)]
pub struct PersistReport {
    pub model_table: String,
    pub dimension: usize,
    pub persisted: usize,
    pub healed: bool,
}

/// What one model-table read returned (DR-11 slice 3).
/// `dimension` is the table's declared width; 0 with no rows means the
/// table is absent (fresh store), not that the model is zero-width.
#[derive(Debug)]
pub struct ModelVectors {
    pub model_table: String,
    pub dimension: usize,
    pub rows: Vec<(String, Vec<f32>)>,
}

impl CodeGraphStore {
    /// Open or create a Macrame database at `path`.
    pub fn open(path: impl AsRef<Path>) -> macrame::Result<Self> {
        Self::open_with_cadence(path, None)
    }

    /// Open with a snapshot cadence (production).
    ///
    /// Serialised with every other libSQL open in this process — see
    /// [`DIAGNOSTIC_OPEN_LOCK`]. Opening is the racing operation, and it does
    /// not care whether the opener is a diagnostic read or a whole database.
    pub fn open_with_cadence(
        path: impl AsRef<Path>,
        cadence: Option<SnapshotCadence>,
    ) -> macrame::Result<Self> {
        let db = {
            let _open_guard = DIAGNOSTIC_OPEN_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            runtime().block_on(Database::open_with_cadence(path, cadence))?
        };
        // Exact-file lookups clean up stale ledger concepts that are absent
        // from the in-memory projection. Re-assert on every open so restored
        // database backups regain the derived index automatically.
        runtime().block_on(db.register_extra_index(CODERADAR_EXTRA_FILE_PATH))?;
        Ok(Self { db })
    }

    /// §1.9 write-through: content-addressed put of one file's bytes.
    ///
    /// Bytes are put BEFORE any digest is asserted (callers persist after),
    /// so a crash between leaves an orphan blob at worst — never a live
    /// digest pointing at absent bytes; retry converges on the same digest
    /// (idempotent by construction). Re-put refreshes `put_at` without
    /// rewriting: 0 new bytes on unchanged-tree reindex (the DR-25
    /// zero-growth property). Every outcome feeds the cumulative counters.
    pub fn put_source_blob(&self, file_path: &str, bytes: &[u8]) -> macrame::Result<BlobOutcome> {
        let cfg = crate::active_config();
        if !cfg.database.store_source_blobs {
            return Ok(BlobOutcome::Disabled);
        }
        // Match against the canonical id-form path; normalize belt for
        // native-separator callers.
        let normalized = file_path.replace('\\', "/");
        if !cfg.database.blob_exclude.is_empty() {
            let mut builder = ignore::gitignore::GitignoreBuilder::new("");
            for pat in &cfg.database.blob_exclude {
                let _ = builder.add_line(None, pat);
            }
            if let Ok(matcher) = builder.build() {
                if crate::path_excluded(&matcher, std::path::Path::new(&normalized), false) {
                    let outcome = BlobOutcome::SkippedExcluded;
                    record_blob_outcome(&outcome);
                    return Ok(outcome);
                }
            }
        }
        if bytes.len() > BLOB_MAX_BYTES {
            let outcome = BlobOutcome::SkippedOversize {
                bytes: bytes.len() as u64,
            };
            record_blob_outcome(&outcome);
            return Ok(outcome);
        }
        match runtime().block_on(self.db.blob_put(bytes)) {
            Ok(digest) => {
                validate_digest(&digest).map_err(|e| {
                    macrame::DbError::InvalidDigest(format!(
                        "blob_put returned {e} for {file_path}"
                    ))
                })?;
                let outcome = BlobOutcome::Stored {
                    digest,
                    bytes: bytes.len() as u64,
                };
                record_blob_outcome(&outcome);
                Ok(outcome)
            }
            Err(macrame::DbError::BlobTooLarge { size, .. }) => {
                // Engine belt (custom tuning): same skip, same count.
                let outcome = BlobOutcome::SkippedOversize { bytes: size as u64 };
                record_blob_outcome(&outcome);
                Ok(outcome)
            }
            Err(e) => Err(e),
        }
    }

    /// Synchronous wrapper around Macrame's async upsert.
    pub fn upsert_entity(
        &self,
        unit: &ExtractedUnit,
        file_path: &str,
        language: &str,
    ) -> macrame::Result<()> {
        let concept = build_concept(unit, file_path, language);
        if self
            .filter_unchanged(std::slice::from_ref(&concept))
            .is_empty()
        {
            return Ok(());
        }
        runtime().block_on(self.db.upsert_concept(concept))
    }

    /// Synchronous wrapper — bulk upsert entities from one file.
    /// Uses Macrame's `write_concepts` chunked path (70 concepts/chunk, one
    /// transaction per chunk) instead of per-concept `upsert_concept` calls.
    /// Per quickref.md:688, per-transaction overhead is ~0.8ms; batching
    /// 2,191 concepts into ~32 chunks saves ~1,700ms vs per-concept commits.
    pub fn upsert_entities(
        &self,
        units: &[ExtractedUnit],
        file_path: &str,
        language: &str,
    ) -> macrame::Result<()> {
        self.upsert_entities_at(units, file_path, language, &now_iso8601())
    }

    /// Stamped twin (P5a.2, DR-41): the same bulk upsert at an explicit
    /// `valid_from` — commit time on the P5 path.
    pub fn upsert_entities_at(
        &self,
        units: &[ExtractedUnit],
        file_path: &str,
        language: &str,
        valid_from: &str,
    ) -> macrame::Result<()> {
        if units.is_empty() {
            return Ok(());
        }
        let concepts: Vec<ConceptUpsert> = units
            .iter()
            .map(|unit| build_concept_at(unit, file_path, language, valid_from))
            .collect();
        let changed = self.filter_unchanged(&concepts);
        if changed.is_empty() {
            return Ok(());
        }
        runtime().block_on(self.db.write_concepts(changed))?;
        Ok(())
    }

    /// Bulk-persist pre-built concepts via Macrame's `write_concepts`.
    /// Chunked internally at 70/chunk, one transaction per chunk.
    /// For batch index: 2,191 concepts ≈ 32 chunks ≈ 75ms.
    ///
    /// Only changed concepts reach the ledger ([`Self::filter_unchanged`]);
    /// the count returned is concepts actually written.
    pub fn upsert_concepts_bulk(&self, concepts: &[ConceptUpsert]) -> macrame::Result<usize> {
        if concepts.is_empty() {
            return Ok(0);
        }
        let changed = self.filter_unchanged(concepts);
        if changed.is_empty() {
            return Ok(0);
        }
        // macrame 0.14+: bulk writes report interruption distinctly. `?`
        // converts via From (the committed prefix stays committed; upserts
        // are idempotent so the next run heals it). CodeRadar never passes
        // a cancel token, so interruption here is a real chunk failure.
        Ok(runtime().block_on(self.db.write_concepts(changed))?)
    }

    /// Ids with no row in the store's current-state projection.
    ///
    /// Fail-open: on any read error every id counts as absent, i.e. today's
    /// write-everything behavior. A failed comparison must never lose a fact.
    pub fn absent_concept_ids(&self, ids: &[String]) -> HashSet<String> {
        if ids.is_empty() {
            return HashSet::new();
        }
        match self.fetch_current_states(ids) {
            Ok(present) => ids
                .iter()
                .filter(|id| !present.contains_key(*id))
                .cloned()
                .collect(),
            Err(e) => {
                eprintln!("[diag] absent_concept_ids failed, writing all: {e:?}");
                ids.iter().cloned().collect()
            }
        }
    }

    /// Drop concepts the ledger already holds byte-identically.
    ///
    /// A concept is unchanged when the open row agrees on title,
    /// `content_hash` annotation, and retired flag; `valid_from`/`valid_to`
    /// are write mechanics, not facts, and are deliberately not compared
    /// (every run stamps a fresh `valid_from`, which is what used to mint a
    /// new version per analyze — +26 rows per no-op analyze on the 7-file
    /// fixture). Unhashable on exactly one side always rewrites (the safe
    /// direction — a v1 row without a hash still upgrades); unhashable on
    /// both sides falls back to full-content equality, so hashless
    /// concepts (the P3 spine, which must not carry a digest in its
    /// content) do not churn a fresh version per run. Fail-open like
    /// [`Self::absent_concept_ids`]..
    pub fn filter_unchanged(&self, concepts: &[ConceptUpsert]) -> Vec<ConceptUpsert> {
        if concepts.is_empty() {
            return Vec::new();
        }
        let ids: Vec<String> = concepts.iter().map(|c| c.id.clone()).collect();
        let current = match self.fetch_current_states(&ids) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[diag] filter_unchanged failed, writing all: {e:?}");
                return concepts.to_vec();
            }
        };
        concepts
            .iter()
            .filter(|c| concept_changed(c, current.get(&c.id)))
            .cloned()
            .collect()
    }

    /// Current `(title, content, extra, retired)` per id, one chunked PK lookup.
    /// Mint lineage per concept id, in bulk (P5a.5, DR-41): the choke-point
    /// companion to `fetch_current_states` for every writer that restates
    /// rows it did not mint (sweep retire, embedding-hash flush). One
    /// query per 500 ids; ids with no row or an empty branch read absent.
    fn fetch_mint_lineages(&self, ids: &[String]) -> macrame::Result<HashMap<String, String>> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT id, branch_id FROM concepts WHERE id IN ({placeholders})");
            let params: Vec<libsql::Value> = chunk
                .iter()
                .map(|s| libsql::Value::Text(s.clone()))
                .collect();
            let mut rows = runtime()
                .block_on(conn.query(&sql, params))
                .map_err(macrame::DbError::Engine)?;
            while let Some(row) = runtime()
                .block_on(rows.next())
                .map_err(macrame::DbError::Engine)?
            {
                let branch: String = row.get::<String>(1).unwrap_or_default();
                if !branch.is_empty() {
                    out.insert(row.get::<String>(0).unwrap_or_default(), branch);
                }
            }
        }
        Ok(out)
    }

    fn fetch_current_states(
        &self,
        ids: &[String],
    ) -> macrame::Result<HashMap<String, (String, String, String, bool)>> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, title, content, extra, retired FROM concepts WHERE id IN ({placeholders})"
            );
            let params: Vec<libsql::Value> = chunk
                .iter()
                .map(|s| libsql::Value::Text(s.clone()))
                .collect();
            let mut rows = runtime()
                .block_on(conn.query(&sql, params))
                .map_err(macrame::DbError::Engine)?;
            while let Some(row) = runtime()
                .block_on(rows.next())
                .map_err(macrame::DbError::Engine)?
            {
                out.insert(
                    row.get::<String>(0).unwrap_or_default(),
                    (
                        row.get::<String>(1).unwrap_or_default(),
                        row.get::<String>(2).unwrap_or_default(),
                        row.get::<String>(3).unwrap_or_else(|_| "{}".to_string()),
                        row.get::<i64>(4).unwrap_or(0) != 0,
                    ),
                );
            }
        }
        Ok(out)
    }

    /// Refresh the query planner statistics after a bulk load (macrame
    /// 0.13+, W2/D-149). Without ANALYZE, SQLite costs every plan against
    /// built-in defaults (~1M rows) and this schema's covering indexes
    /// capture queries without discriminating. Bounded internally
    /// (`PRAGMA analysis_limit`), ~19ms at 40k edges upstream.
    pub fn analyze(&self) -> macrame::Result<()> {
        runtime().block_on(self.db.analyze())
    }

    /// Open a read-only diagnostic connection, one at a time process-wide.
    ///
    /// `diagnostic_conn` is the only `Database` method that opens the file, so
    /// N callers is N concurrent libSQL opens — upstream risk R15, which
    /// presents as an access violation (0xC0000005) or, worse, as a *returned*
    /// SQLite error ("database is locked") that reads like a fact about the
    /// data. macrame documents this and deliberately does not serialise it for
    /// you, on the grounds that the connection is the caller's own. So this is
    /// the caller doing it: one outstanding open at a time, which is enough.
    ///
    /// The guard is returned rather than dropped here, and callers hold it for
    /// as long as they use the connection, so a diagnostic read is one
    /// outstanding handle rather than one outstanding `open`.
    ///
    /// This is not what fixed the access violation the store tests were
    /// hitting — sharing one tokio runtime across stores was (see
    /// [`runtime`]). It stays because the risk it bounds is real and
    /// documented upstream, and because it costs a diagnostic path nothing.
    pub(crate) fn open_diagnostic_conn(
        &self,
    ) -> macrame::Result<(std::sync::MutexGuard<'static, ()>, libsql::Connection)> {
        // Poisoning carries no meaning here: the guard protects nothing but
        // access to a file, so a panicking caller leaves no inconsistent state
        // behind for the next one to find.
        let guard = DIAGNOSTIC_OPEN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let conn = runtime().block_on(self.db.diagnostic_conn())?;
        Ok((guard, conn))
    }

    /// FTS5 keyword search over concept text (§1.10, DR-34).
    ///
    /// Zero storage change: `concepts_fts` (title+content, trigger-maintained)
    /// already indexes every v2 concept's JSON, so names, docstrings, file
    /// paths and member lists are all searchable with no new write path.
    /// Escaped-by-default (`raw=false` quotes each token via macrame's
    /// `escape_fts5_query`, so hostile input like `cats not dogs` or an
    /// unbalanced quote degrades to safe empty, never an error); `raw=true`
    /// passes the MATCH expression through for power MATCH syntax.
    /// Live-only: the engine filters retired rows, and history search is
    /// out of scope — FTS indexes current rows, the ledger holds the past.
    /// Empty queries short-circuit to `[]` without touching the store.
    /// `top_k` clamps to [1, 50].
    ///
    /// Foreign/old stores may predate the FTS table: that is an honest
    /// error naming `rebuild_fts`, never a silent empty.
    pub fn search_symbols(
        &self,
        query: &str,
        top_k: usize,
        raw: bool,
    ) -> macrame::Result<Vec<(String, f64)>> {
        let top_k = top_k.clamp(1, 50);
        let matched = if raw {
            query.trim().to_string()
        } else {
            macrame::vector::escape_fts5_query(query)
        };
        if matched.is_empty() {
            return Ok(Vec::new());
        }
        let res = (|| -> macrame::Result<Vec<(String, f64)>> {
            let (_diag_guard, conn) = self.open_diagnostic_conn()?;
            runtime().block_on(async {
                let mut rows = conn
                    .query(
                        "SELECT name FROM sqlite_master WHERE name='concepts_fts'",
                        libsql::params![],
                    )
                    .await?;
                if rows.next().await?.is_none() {
                    return Err(Self::missing_fts_err("sqlite_master lists no concepts_fts"));
                }
                macrame::vector::keyword_search(&conn, &matched, top_k, None, None).await
            })
        })();
        // Whatever layer notices the missing table (our pre-check, the
        // MATCH itself, or open-time schema verification), the answer names
        // the repair, never a bare table name or a silent empty.
        res.map_err(|e| {
            if format!("{e:?}").contains("concepts_fts") {
                Self::missing_fts_err(&format!("{e:?}"))
            } else {
                e
            }
        })
    }

    /// Persist embedding vectors to the ledger's per-model table (DR-11
    /// slice 2). `rows` are `(id, hash)` pairs for vectors written to
    /// memory this run — the vectors themselves are read from `projection`
    /// (they already live there; passing them back across FFI would copy
    /// megabytes pointlessly). A listed id with no in-memory vector is an
    /// honest error, never a silent skip.
    ///
    /// Empty `rows` + matching table count = no-op. Empty `rows` +
    /// missing/divergent table = heal (rewrite the whole memory set): the
    /// crash window between memory commit and table write cannot be atomic
    /// across two systems, so it is healable instead — and a full regen
    /// with matching counts that somehow skipped persist is repaired by
    /// `compute_embeddings(recompute=True)`, stated here rather than
    /// discovered. No memory rows at all = nothing registered (tables are
    /// never created empty).
    ///
    /// Stale-row note: rows are upserted, never deleted — macrame has no
    /// vector delete. Edits clear memory + ledger hash immediately and the
    /// next compute regenerates and overwrites; deletions retire the
    /// concept, which the ANN/hybrid arm filters. Between edit and
    /// recompute the table serves last-computed vectors.
    pub fn persist_projection_embeddings(
        &self,
        model_raw: &str,
        projection: &ProjectedGraph,
        rows: &[(String, String)],
    ) -> macrame::Result<PersistReport> {
        let model = macrame_model_name(model_raw)?;
        // (id, f32 vector, dedup hash): the hash rides along so the
        // ledger flush below can record provenance without re-reading it.
        let mut memory: Vec<(String, Vec<f32>, String)> = Vec::new();
        {
            let mut collect = |id: &String, emb: &EmbeddingVec| {
                if !emb.vec.is_empty() {
                    memory.push((
                        id.clone(),
                        emb.vec.iter().map(|x| *x as f32).collect(),
                        emb.hash.clone(),
                    ));
                }
            };
            for (id, e) in &projection.functions {
                collect(id, &e.embedding);
            }
            for (id, e) in &projection.classes {
                collect(id, &e.embedding);
            }
            for (id, e) in &projection.modules {
                collect(id, &e.embedding);
            }
            for (id, e) in &projection.imports {
                collect(id, &e.embedding);
            }
            for (id, e) in &projection.constants {
                collect(id, &e.embedding);
            }
            for (id, e) in &projection.type_aliases {
                collect(id, &e.embedding);
            }
        }
        let uniform_dim = |vs: &[(String, Vec<f32>, String)]| -> macrame::Result<usize> {
            let mut dim = 0usize;
            for (id, v, _) in vs {
                if dim == 0 {
                    dim = v.len();
                } else if v.len() != dim {
                    return Err(macrame::DbError::DimMismatch {
                        got: v.len(),
                        expected: dim,
                        model: model.to_string(),
                    });
                }
                let _ = id;
            }
            Ok(dim)
        };
        let write_all = |vs: &[(String, Vec<f32>, String)]| -> macrame::Result<usize> {
            let dim = uniform_dim(vs)?;
            if dim == 0 {
                return Ok(0);
            }
            runtime().block_on(self.db.register_model(&model, dim))?;
            let plain: Vec<(String, Vec<f32>)> = vs
                .iter()
                .map(|(id, v, _)| (id.clone(), v.clone()))
                .collect();
            runtime()
                .block_on(self.db.upsert_embeddings(&model, plain))
                .map_err(|e| {
                    // BulkInterrupted carries the written count; the cause is
                    // what the caller answers. A foreign-key failure means
                    // vectors for concepts the ledger never recorded — name
                    // the repair (flush v2 concepts first), never bare 787.
                    let cause = e.cause;
                    if format!("{cause:?}").contains("FOREIGN KEY") {
                        macrame::DbError::InvalidId {
                            id: model.table(),
                            reason: "persist found vectors for concepts the ledger never \
                                     recorded — flush v2 concepts (analyze/update_file) \
                                     before persisting embeddings"
                                .to_string(),
                        }
                    } else {
                        cause
                    }
                })
        };
        if !rows.is_empty() {
            let mut fresh: Vec<(String, Vec<f32>, String)> = Vec::with_capacity(rows.len());
            for (id, hash) in rows {
                match memory.iter().find(|(mid, _, _)| mid == id) {
                    Some((_, v, _)) => fresh.push((id.clone(), v.clone(), hash.clone())),
                    None => {
                        return Err(macrame::DbError::InvalidId {
                            id: id.clone(),
                            reason: "persist lists an id with no in-memory vector \
                                     (cleared between set and persist?)"
                                .to_string(),
                        });
                    }
                }
            }
            let dim = uniform_dim(&fresh)?;
            let persisted = write_all(&fresh)?;
            // The S1 contract: compute never touches files, so without this
            // flush a restart loses hashes (the ledger keeps the last
            // flushed generation) and warm dedup regenerates everything
            // the table already holds. Fail-open with a diagnostic:
            // vectors are durable already, and the next persist heals.
            if let Err(e) = self.flush_embedding_hashes(
                &fresh
                    .iter()
                    .map(|(id, _, h)| (id.clone(), h.clone()))
                    .collect::<Vec<_>>(),
            ) {
                eprintln!("[diag] embedding hash flush failed: {e:?}");
            }
            return Ok(PersistReport {
                model_table: model.table(),
                dimension: dim,
                persisted,
                healed: false,
            });
        }
        // No fresh rows: verify the table agrees with memory, heal if not.
        if memory.is_empty() {
            return Ok(PersistReport {
                model_table: model.table(),
                dimension: 0,
                persisted: 0,
                healed: false,
            });
        }
        let dim = uniform_dim(&memory)?;
        // Scoped: the diagnostic lock is process-wide and non-reentrant —
        // it must drop before the heal write below re-enters the store
        // through flush_embedding_hashes (same-thread relock deadlocks).
        let table_count: i64 = {
            let (_diag_guard, conn) = self.open_diagnostic_conn()?;
            let table_exists: bool = {
                let mut rows = runtime()
                    .block_on(conn.query(
                        "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
                        vec![libsql::Value::Text(model.table())],
                    ))
                    .map_err(macrame::DbError::Engine)?;
                runtime()
                    .block_on(rows.next())
                    .map_err(macrame::DbError::Engine)?
                    .is_some()
            };
            if table_exists {
                let mut rows = runtime()
                    .block_on(conn.query(
                        &format!("SELECT COUNT(*) FROM {}", model.table()),
                        Vec::<libsql::Value>::new(),
                    ))
                    .map_err(macrame::DbError::Engine)?;
                match runtime()
                    .block_on(rows.next())
                    .map_err(macrame::DbError::Engine)?
                {
                    Some(row) => row.get::<i64>(0).map_err(macrame::DbError::Engine)?,
                    None => 0,
                }
            } else {
                0
            }
        };
        if table_count == memory.len() as i64 {
            return Ok(PersistReport {
                model_table: model.table(),
                dimension: dim,
                persisted: 0,
                healed: false,
            });
        }
        let persisted = write_all(&memory)?;
        // Heal rewrites vectors; the ledger hashes come along for the same
        // S1 reason as the fresh path above.
        if let Err(e) = self.flush_embedding_hashes(
            &memory
                .iter()
                .map(|(id, _, h)| (id.clone(), h.clone()))
                .collect::<Vec<_>>(),
        ) {
            eprintln!("[diag] embedding hash flush failed: {e:?}");
        }
        Ok(PersistReport {
            model_table: model.table(),
            dimension: dim,
            persisted,
            healed: true,
        })
    }

    /// Read a model's whole vector table back (DR-11 slice 3). Missing
    /// table = honest empty (fresh store), never an error — the caller
    /// decides what absence means. A non-blob cell or undecodable bytes
    /// fail loudly naming the table: silent skips here become phantom
    /// search misses downstream.
    pub fn read_model_vectors(&self, model_raw: &str) -> macrame::Result<ModelVectors> {
        let model = macrame_model_name(model_raw)?;
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        let (dim, rows) = runtime().block_on(async {
            let mut probe = conn
                .query(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
                    vec![libsql::Value::Text(model.table())],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            if probe
                .next()
                .await
                .map_err(macrame::DbError::Engine)?
                .is_none()
            {
                return Ok::<_, macrame::DbError>((0usize, Vec::new()));
            }
            let dim = macrame::vector::declared_dimension(&conn, &model).await?;
            let mut q = conn
                .query(
                    &format!("SELECT concept_id, embedding FROM {}", model.table()),
                    Vec::<libsql::Value>::new(),
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
            while let Some(row) = q.next().await.map_err(macrame::DbError::Engine)? {
                let id: String = row.get(0).map_err(macrame::DbError::Engine)?;
                let cell: libsql::Value = row.get(1).map_err(macrame::DbError::Engine)?;
                let bytes = match cell {
                    libsql::Value::Blob(b) => b,
                    other => {
                        return Err(macrame::DbError::InvalidId {
                            id: model.table(),
                            reason: format!("embedding cell for {id} is not a blob ({other:?})"),
                        });
                    }
                };
                let vec = macrame::vector::EmbeddingCodec::decode(&bytes).map_err(|e| {
                    macrame::DbError::InvalidId {
                        id: model.table(),
                        reason: format!("cannot decode embedding for {id}: {e:?}"),
                    }
                })?;
                rows.push((id, vec));
            }
            Ok::<_, macrame::DbError>((dim, rows))
        })?;
        Ok(ModelVectors {
            model_table: model.table(),
            dimension: dim,
            rows,
        })
    }

    /// Tables registered for one embedding width (DR-11 epilogue).
    ///
    /// `search_similar` must name its vector arm without Python passing a
    /// model: tables whose declared dimension matches the query width are
    /// candidates. Returns inner normalized model names, feedable back
    /// into `read_model_vectors` (normalization is idempotent). A store
    /// with no embedding tables yields empty, never an error — the caller
    /// falls back to the legacy memory scan.
    pub fn model_tables_at_dim(&self, dim: usize) -> macrame::Result<Vec<String>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut q = conn
                .query(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'embeddings\\_%' ESCAPE '\\'",
                    Vec::<libsql::Value>::new(),
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut hits = Vec::new();
            while let Some(row) = q.next().await.map_err(macrame::DbError::Engine)? {
                let table: String = row.get(0).map_err(macrame::DbError::Engine)?;
                let inner = table
                    .strip_prefix("embeddings_")
                    .unwrap_or(&table)
                    .to_string();
                let model = macrame_model_name(&inner)?;
                if macrame::vector::declared_dimension(&conn, &model).await? == dim {
                    hits.push(inner);
                }
            }
            Ok(hits)
        })
    }

    /// Write `embedding_hash` into stored concept JSON for `(id, hash)`
    /// pairs (DR-11 slice 3). Read-modify-write preserving title and
    /// retired: only the `embedding_hash` key changes. `extra` is
    /// recomputed for non-modules (its content-hash mirror would go stale)
    /// and preserved for modules (it carries the source-blob digest, which
    /// cannot be recomputed here). Unconditional write — it bypasses
    /// `filter_unchanged`, which is blind to this key on source-hashed
    /// kinds — so callers must bound the set (fresh or heal set only).
    /// Already-correct rows are skipped; returns concepts written.
    fn flush_embedding_hashes(&self, pairs: &[(String, String)]) -> macrame::Result<usize> {
        if pairs.is_empty() {
            return Ok(0);
        }
        let ids: Vec<String> = pairs.iter().map(|(id, _)| id.clone()).collect();
        let current = self.fetch_current_states(&ids)?;
        // P5a.5 (DR-41): the hash flush restates every touched concept, so
        // each upsert must ride the row's mint lineage — a branch-minted
        // entity restated on the trunk trips the cross-lineage trigger on
        // its own id (the same choke-point rule as the sweep retire).
        let lineages = self.fetch_mint_lineages(&ids)?;
        let hashes: std::collections::HashMap<&str, &str> = pairs
            .iter()
            .map(|(id, h)| (id.as_str(), h.as_str()))
            .collect();
        let now = now_iso8601();
        let mut concepts = Vec::new();
        for (id, (title, content, extra, retired)) in &current {
            let Some(hash) = hashes.get(id.as_str()) else {
                continue;
            };
            let mut v: serde_json::Value = match serde_json::from_str(content) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[diag] embedding hash flush skips {id}: content is not JSON ({e})");
                    continue;
                }
            };
            if v.get("embedding_hash").and_then(|h| h.as_str()) == Some(hash) {
                continue;
            }
            v["embedding_hash"] = serde_json::json!(hash);
            let is_module = v.get("kind").and_then(|k| k.as_str()) == Some("module");
            let new_extra = if is_module {
                extra.clone()
            } else {
                coderadar_extra(&v, None).to_string()
            };
            let mut upsert = macrame::ConceptUpsert::new(id.clone(), title.clone())
                .content(v.to_string())
                .extra(new_extra)
                .valid_from(now.clone())
                .valid_to(TS_OPEN.to_string())
                .retired(*retired);
            if let Some(branch) = lineages.get(id.as_str()) {
                if let Ok(bid) = macrame::BranchId::new(branch) {
                    upsert = upsert.on_branch(bid);
                }
            }
            concepts.push(upsert);
        }
        if concepts.is_empty() {
            return Ok(0);
        }
        Ok(runtime().block_on(self.db.write_concepts(concepts))?)
    }

    /// Message bodies for commit rows minted before the body field existed
    /// (P6c slice, DR-45: subject-only rows from P5a.3-era ingests).
    /// `message_for(sha)` is the git boundary (owned by the caller — this
    /// file never touches git2): `Ok(Some(body))` heals, `Ok(None)` skips
    /// as `no-body`, `Err(reason)` skips auditable (unknown shas from
    /// shallow clones and gc'd objects land here, never as errors).
    /// Each restate rides the row's OWN mint lineage (trunk or branch —
    /// the §P5a.5 choke-point rule covers every restate, including this
    /// one). Selection is `body IS NULL`-at-read, so a healed row never
    /// qualifies again: re-runs heal zero without relying on content
    /// filtering. Returns `(healed, [(sha, reason)])`.
    pub(crate) fn backfill_commit_bodies(
        &self,
        message_for: &dyn Fn(&str) -> std::result::Result<Option<String>, String>,
    ) -> macrame::Result<(usize, Vec<(String, String)>)> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        let mut pending: Vec<(String, String, String, String, bool, String)> = Vec::new();
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT id, title, content, extra, retired, branch_id FROM concepts \
                     WHERE id LIKE 'commit:%' AND retired = 0 \
                     AND (json_extract(content, '$.body') IS NULL \
                          OR json_extract(content, '$.body') = '')",
                    Vec::<libsql::Value>::new(),
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                pending.push((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                    row.get::<String>(2).unwrap_or_default(),
                    row.get::<String>(3).unwrap_or_else(|_| "{}".to_string()),
                    row.get::<i64>(4).unwrap_or(0) != 0,
                    row.get::<String>(5).unwrap_or_default(),
                ));
            }
            Ok::<_, macrame::DbError>(())
        })?;
        drop(_guard);
        let now = now_iso8601();
        let mut concepts = Vec::new();
        let mut skipped = Vec::new();
        for (id, title, content, extra, retired, branch_id) in &pending {
            let sha = id.strip_prefix("commit:").unwrap_or(id.as_str());
            match message_for(sha) {
                Err(reason) => skipped.push((sha.to_string(), reason)),
                Ok(None) => skipped.push((sha.to_string(), "no-body".to_string())),
                Ok(Some(body)) => {
                    let mut v: serde_json::Value = match serde_json::from_str(content) {
                        Ok(v) => v,
                        Err(e) => {
                            skipped.push((sha.to_string(), format!("unparseable-content:{e}")));
                            continue;
                        }
                    };
                    v["body"] = serde_json::json!(body);
                    let mut upsert = macrame::ConceptUpsert::new(id.clone(), title.clone())
                        .content(v.to_string())
                        .extra(extra.clone())
                        .valid_from(now.clone())
                        .valid_to(TS_OPEN.to_string())
                        .retired(*retired);
                    if !branch_id.is_empty() {
                        if let Ok(bid) = macrame::BranchId::new(branch_id) {
                            upsert = upsert.on_branch(bid);
                        }
                    }
                    concepts.push(upsert);
                }
            }
        }
        let healed = concepts.len();
        if !concepts.is_empty() {
            runtime().block_on(self.db.write_concepts(concepts))?;
        }
        Ok((healed, skipped))
    }

    /// Hybrid keyword+vector search (DR-11 slice 4 / §2.10): RRF fusion
    /// (`RRF_K=60`, depth `max(5×top_k,50)`) of the FTS5 keyword arm and
    /// the DiskANN vector arm over one model's table. `top_k` clamps to
    /// [1, 50]; empty text AND empty vector short-circuit to `[]`.
    /// Escaped-by-default like `search_symbols`. A query vector is
    /// required — hybrid without the vector arm is just `search_symbols`.
    /// Unregistered model / missing FTS are honest errors naming the
    /// repair (`compute_embeddings` / `rebuild_fts`), never silent empties.
    pub fn search_hybrid(
        &self,
        model_raw: &str,
        query_text: &str,
        query_vec: &[f64],
        top_k: usize,
    ) -> macrame::Result<Vec<macrame::vector::HybridHit>> {
        let model = macrame_model_name(model_raw)?;
        let top_k = top_k.clamp(1, 50);
        if query_text.trim().is_empty() && query_vec.is_empty() {
            return Ok(Vec::new());
        }
        if query_vec.is_empty() {
            return Err(macrame::DbError::InvalidId {
                id: model.table(),
                reason: "search_hybrid needs a query vector — hybrid without the \
                         vector arm is search_symbols"
                    .to_string(),
            });
        }
        let vec_f32: Vec<f32> = query_vec.iter().map(|x| *x as f32).collect();
        let res = (|| -> macrame::Result<Vec<macrame::vector::HybridHit>> {
            let (_diag_guard, conn) = self.open_diagnostic_conn()?;
            runtime().block_on(async {
                let mut probe = conn
                    .query(
                        "SELECT name FROM sqlite_master WHERE name='concepts_fts'",
                        Vec::<libsql::Value>::new(),
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                if probe
                    .next()
                    .await
                    .map_err(macrame::DbError::Engine)?
                    .is_none()
                {
                    return Err(Self::missing_fts_err("sqlite_master lists no concepts_fts"));
                }
                macrame::vector::HybridSearch::new(model, query_text.trim(), vec_f32)
                    .top_k(top_k)
                    .execute(&conn)
                    .await
            })
        })();
        // Whatever layer notices the missing table (our pre-check, the
        // MATCH itself, or open-time schema verification), the answer names
        // the repair, never a bare table name or a silent empty.
        res.map_err(|e| {
            if format!("{e:?}").contains("concepts_fts") {
                Self::missing_fts_err(&format!("{e:?}"))
            } else {
                e
            }
        })
    }

    /// Raw blob bytes by digest (§3.0, DR-25): the `blob_get` half of the
    /// PyO3 round-trip. `None` = valid address, no bytes (never put, or the
    /// bytes are gone) — the Python layer adds the at-T context and raises
    /// `ContentUnavailable` there, keeping `None` distinct from
    /// "not in the graph at T". Malformed digests refuse via
    /// `validate_digest`; cold-archived blobs resolve through macrame's
    /// hot→cold fallback, so archive never breaks a read.
    pub fn blob_get(&self, digest: &str) -> macrame::Result<Option<Vec<u8>>> {
        macrame::blob::validate_digest(digest)?;
        runtime().block_on(self.db.blob_get(digest))
    }

    /// Ref for one entity out of an already-folded state: the single
    /// implementation behind `entity_source_ref_at` and `source_refs_at`
    /// (P0, DR-36). `timestamp` is only used in error text — the state is
    /// already the fold at T. `None` = not in the graph at T (never
    /// asserted, not yet, or retired) — the same answer as
    /// `lookup_entity_at`, never an error. `digest=None` = no blob recorded
    /// (pre-blob generation, excluded, oversize, or disabled);
    /// `has_span=false` = synthetic (routes) with no source bytes. Takes
    /// the id as given — spelling resolution is the caller's (`Snapshot`
    /// resolves via `find` first).
    ///
    /// P4 (DR-40, INV-4): the digest resolves through the reader's
    /// lineage before the bytes are touched. The branch's pointer concept
    /// (`branch:{branch}:content:{path}`) wins over the module concept
    /// when the fold sees it — own row, or an ancestor's until the reader
    /// mints its own — else the module concept (trunk-frozen fallback).
    /// Never another lineage's bytes: pointer ids name their lineage.
    /// A pointer hit returns the branch's full
    /// file (`full_file = true`, `has_span = false`): spans belong to the
    /// lineage that wrote them, and slicing one lineage's bytes by
    /// another's spans would be a wrong answer, not a partial one.
    fn source_ref_from_state(
        state: &MaterializedState,
        entity_id: &str,
        timestamp: &str,
        branch: &str,
    ) -> macrame::Result<Option<SourceRef>> {
        let concept = match state.concepts.get(entity_id) {
            Some(c) => c,
            None => return Ok(None),
        };
        let content: serde_json::Value = serde_json::from_str(&concept.content).map_err(|e| {
            macrame::DbError::Engine(libsql::Error::Misuse(format!(
                "concept {entity_id} has unparseable content at {timestamp}: {e}",
            )))
        })?;
        let file_path = content
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                macrame::DbError::Engine(libsql::Error::Misuse(format!(
                    "concept {entity_id} has no file_path at {timestamp}",
                )))
            })?;
        let span = span_of(&content, "span");
        let is_module = content
            .get("kind")
            .and_then(|v| v.as_str())
            .is_some_and(|k| k == "module");
        let module_id = format!("{file_path}::module");
        // P4 (DR-40): the branch's view first. A pointer names the blob
        // outright (`properties.source_blob`); the module concept is the
        // trunk-frozen fallback (a branch inherits it as of the fork).
        // P5a.4 (DR-41): a tombstone pointer (deleted marker, no blob)
        // reads as absent — the file is gone on this lineage even though
        // the trunk still holds it. The marker is checked before the
        // digest so a deletion never falls through to trunk content.
        if Self::branch_pointer_deleted(state, file_path, branch) {
            return Ok(None);
        }
        if let Some(sha) = Self::branch_pointer_digest(state, file_path, branch) {
            return Ok(Some(SourceRef {
                id: entity_id.to_string(),
                file_path: file_path.to_string(),
                digest: Some(sha),
                span_start: 0,
                span_end: 0,
                has_span: false,
                full_file: true,
                start_line: 1,
                end_line: 1,
                handler_id: content
                    .get("handler_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            }));
        }
        let digest = state
            .concepts
            .get(&module_id)
            .and_then(|m| source_blob_of_extra(&m.extra));
        Ok(Some(SourceRef {
            id: entity_id.to_string(),
            file_path: file_path.to_string(),
            digest,
            span_start: span.start,
            span_end: span.end,
            has_span: content.get("span").is_some(),
            full_file: is_module,
            start_line: content.get("line").and_then(|v| v.as_u64()).unwrap_or(1),
            end_line: content
                .get("exit_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(1),
            handler_id: content
                .get("handler_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }))
    }

    /// The blob address a branch-content pointer concept names for `path`
    /// on `branch`, if any (P4, DR-40): a direct map lookup — the id is
    /// deterministic (`branch:{branch}:content:{path}`), so there is no
    /// scan and no lineage guessing. The row is visible exactly when the
    /// reader's ancestry holds its mint lineage (own row, or an
    /// ancestor's until the reader mints its own): a `reconstruct_on`
    /// fold narrows to the ancestry, so a hit IS the lineage's view and
    /// a miss falls through to the module concept. Never another
    /// lineage's bytes: a sibling's pointer id names the sibling, and
    /// the trunk never mints one. A tombstone (P5a.4: deleted marker,
    /// no blob) names nothing — the deleted check above runs first, so
    /// this returning `None` still means "no pointer", never "deleted".
    fn branch_pointer_digest(
        state: &MaterializedState,
        path: &str,
        branch: &str,
    ) -> Option<String> {
        let pointer = state.concepts.get(&branch_pointer_id(path, branch))?;
        source_blob_of_extra(&pointer.extra)
    }

    /// Whether the lineage's pointer for `path` is a deletion tombstone
    /// (P5a.4, DR-41): the marker lives in `extra` — the same read
    /// boundary `branch_pointer_digest` uses — so a corrupt or legacy
    /// pointer (unparseable extra, no marker) keeps the old fail-open
    /// behavior and falls through to the module concept.
    fn branch_pointer_deleted(state: &MaterializedState, path: &str, branch: &str) -> bool {
        let Some(pointer) = state.concepts.get(&branch_pointer_id(path, branch)) else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(&pointer.extra)
            .ok()
            .and_then(|v| v.get("coderadar")?.get("deleted")?.as_bool())
            .unwrap_or(false)
    }

    /// Batched refs for N entities at one timestamp (P0, DR-36): one
    /// `reconstruct_on(T, branch)` serves every id, turning a sweep from
    /// O(files × generations) folds into O(generations). Ids absent at T
    /// are absent from the map — never `None` entries — so "not in the
    /// graph" stays distinct from "in the graph without bytes" (INV-7).
    /// Returns the map plus `predates_recorded_history`, mirroring
    /// `entities_at`. A corrupt concept fails the whole batch, exactly as
    /// the single call fails its one read: a sweep must surface
    /// corruption, not skip it. P4 (DR-40): the fold is lineage-resolved;
    /// an unknown branch refuses with `DbError::UnknownBranch` (refused,
    /// never trunk-fallback — Macrame's own rule).
    pub fn source_refs_at(
        &self,
        entity_ids: &[String],
        timestamp: &str,
        branch: &str,
    ) -> macrame::Result<(HashMap<String, SourceRef>, bool)> {
        let state = self.reconstruct_on(timestamp, branch)?;
        let mut refs = HashMap::with_capacity(entity_ids.len());
        for entity_id in entity_ids {
            if let Some(r) = Self::source_ref_from_state(&state, entity_id, timestamp, branch)? {
                refs.insert(entity_id.clone(), r);
            }
        }
        Ok((refs, state.predates_recorded_history))
    }

    /// Where one entity's bytes live at one timestamp (§3.0, DR-25).
    ///
    /// One lineage-resolved fold over the ledger, then
    /// `source_ref_from_state` — the fold-per-call cost that P0's batched
    /// `source_refs_at` exists to avoid. Same answers as
    /// `lookup_entity_at`: `None` is data, never an error. P4 (DR-40):
    /// `branch` selects the lineage (trunk by default).
    pub fn entity_source_ref_at(
        &self,
        entity_id: &str,
        timestamp: &str,
        branch: &str,
    ) -> macrame::Result<Option<SourceRef>> {
        let state = self.reconstruct_on(timestamp, branch)?;
        Self::source_ref_from_state(&state, entity_id, timestamp, branch)
    }

    /// One archive session at `cutoff` (§3.4, DR-25): moves blobs last-put
    /// before the cutoff and named by no hot log entry to `cold.blobs`,
    /// alongside archivable links/concepts/log rows. Reads never break:
    /// `blob_get` falls back to cold, and hot entries naming cold-only
    /// blobs copy them back (`blobs_restored`). The cold file is the
    /// `<store-stem>_archive.db` sibling — backup means the hot+cold pair,
    /// a hot-only copy is pointers (recorded digests, no bytes).
    pub fn archive(&self, cutoff: &str) -> macrame::Result<macrame::temporal::ArchiveReport> {
        runtime().block_on(self.db.archive(cutoff))
    }

    /// The honest error for a store without the FTS index: names the table
    /// symptom and the `rebuild_fts` repair, never a silent empty.
    fn missing_fts_err(detail: &str) -> macrame::DbError {
        macrame::DbError::Engine(libsql::Error::Misuse(format!(
            "concepts_fts is absent (foreign/old store; {detail}): run \
             Database::rebuild_fts() in macrame, then reindex, before keyword search",
        )))
    }

    /// Retire concepts and close every open edge that touches them.
    ///
    /// `retired: true` appeared nowhere in the codebase: a deleted function, a
    /// renamed class, a removed import all stayed "currently true" in the
    /// ledger forever, so `as_of` for any past instant returned a superset
    /// that only ever grew. Retirement is what makes the temporal claim true.
    ///
    /// Bitemporally this is an assertion, not an erasure: the current row is
    /// replaced by one carrying `valid_to = now` and `retired = 1`, while the
    /// original row survives in the log, so `reconstruct` at an earlier
    /// instant still sees the entity alive.
    ///
    /// Returns `(concepts_retired, edges_retired)`.
    /// Stamped twin (P5a.3, DR-41): the same retirement at an explicit
    /// `valid_from` — commit time on the P5 path. The un-stamped form
    /// below keeps the live paths on the clock.
    pub fn retire_entities_at(
        &self,
        entity_ids: &[String],
        valid_from: &str,
    ) -> macrame::Result<(usize, usize)> {
        if entity_ids.is_empty() {
            return Ok((0, 0));
        }
        let ts = valid_from.to_string();
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            // Title, content and extra are carried over: the upsert overwrites
            // these columns, so the retired row remains a record of the entity
            // rather than a blank tombstone.
            let mut concepts: Vec<ConceptUpsert> = Vec::new();
            for id in entity_ids {
                // P5a.4 (DR-41): the retire upsert carries the row's mint
                // lineage. Concepts are globally unique by id but
                // lineage-provenanced (D-225: `branch_id` is provenance, not
                // identity), so retiring a branch-minted row through a
                // trunk upsert would trip `trg_concepts_cross_lineage`.
                // Round-tripping the stored value keeps every lineage's
                // retire legal; trunk rows round-trip to the trunk default.
                let mut rows = conn
                    .query(
                        "SELECT title, content, extra, valid_from, branch_id FROM concepts \
                         WHERE id = ?1 AND retired = 0",
                        libsql::params![id.as_str()],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                if let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                    let mut upsert =
                        ConceptUpsert::new(id.clone(), row.get::<String>(0).unwrap_or_default())
                            .content(row.get::<String>(1).unwrap_or_default())
                            .extra(row.get::<String>(2).unwrap_or_else(|_| "{}".to_string()))
                            .valid_from(row.get::<String>(3).unwrap_or_else(|_| ts.clone()))
                            .valid_to(ts.clone())
                            .retired(true);
                    let mint: Option<String> = row.get(4).ok().flatten();
                    if let Some(lineage) = mint.filter(|b| !b.is_empty()) {
                        upsert = upsert.on_branch(macrame::BranchId::new(&lineage)?);
                    }
                    concepts.push(upsert);
                }
            }

            // Open edges on either side. An edge whose endpoint is gone is not
            // a fact about the code any more, whichever end went.
            let mut open_edges: Vec<(String, String, String, String)> = Vec::new();
            for id in entity_ids {
                let mut rows = conn
                    .query(
                        "SELECT source_id, target_id, edge_type, valid_from \
                         FROM links_current \
                         WHERE valid_to > ?2 AND (source_id = ?1 OR target_id = ?1)",
                        // `> now` rather than `= TS_OPEN`: edges carry
                        // macrame's own open sentinel, concepts carry ours,
                        // and the two spellings differ. Openness is the
                        // property that matters, not which sentinel says so.
                        libsql::params![id.as_str(), ts.as_str()],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                    open_edges.push((
                        row.get::<String>(0).unwrap_or_default(),
                        row.get::<String>(1).unwrap_or_default(),
                        row.get::<String>(2).unwrap_or_default(),
                        row.get::<String>(3).unwrap_or_default(),
                    ));
                }
            }
            open_edges.sort();
            open_edges.dedup();

            let mut edges_retired = 0usize;
            for (source, target, etype, valid_from) in &open_edges {
                // NotFound means something else closed it first; that is the
                // desired end state either way.
                if self
                    .db
                    .retire_edge(
                        source.as_str(),
                        target.as_str(),
                        etype.as_str(),
                        valid_from.as_str(),
                        ts.as_str(),
                    )
                    .await
                    .is_ok()
                {
                    edges_retired += 1;
                }
            }

            let concepts_retired = concepts.len();
            if concepts_retired > 0 {
                self.db.write_concepts(concepts).await?;
            }
            Ok((concepts_retired, edges_retired))
        })
    }

    /// Un-stamped wrapper (live paths): retire at the clock.
    pub fn retire_entities(&self, entity_ids: &[String]) -> macrame::Result<(usize, usize)> {
        self.retire_entities_at(entity_ids, &now_iso8601())
    }

    /// `(id, content)` of every concept the ledger still holds to be true.
    ///
    /// The content-carrying counterpart to [`Self::live_concept_ids`], for
    /// health checks that must classify rows (v1-upgrade repair).
    pub fn live_concept_contents(&self) -> macrame::Result<Vec<(String, String)>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query("SELECT id, content FROM concepts WHERE retired = 0", ())
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                ));
            }
            Ok(out)
        })
    }

    /// Retire every live concept that cold start would hard-reject as a v1
    /// leftover (F6 fix).
    ///
    /// v1 stores keyed entities by ABSOLUTE path while v2 uses
    /// project-relative ids, so a re-analyze writes v2 rows *alongside*
    /// the v1 ones and the loader kept failing on the orphans — the
    /// documented "re-run analyze to upgrade" path was a no-op (3857
    /// leftovers surviving every analyze in the dogfood review). A full
    /// analyze now calls this after the v2 flush, which makes the upgrade
    /// path real. Retired rows stay in the log for `as_of` but leave the
    /// current state, so the next cold load succeeds.
    /// Returns `(v1_retired, edges_retired)`.
    pub fn retire_v1_leftovers(&self) -> macrame::Result<(usize, usize)> {
        let v1_ids: Vec<String> = self
            .live_concept_contents()?
            .into_iter()
            .filter(|(_, content)| {
                matches!(classify_v2_concept(content), V2ConceptClass::V1Leftover)
            })
            .map(|(id, _)| id)
            .collect();
        self.retire_entities(&v1_ids)
    }

    /// Retire canonical file concepts absent from a complete fresh projection.
    ///
    /// Full analyze calls this only after a successful project walk and a
    /// successful v2 flush. Restricting candidates to recognized file-backed
    /// concepts avoids touching unrelated ledger rows; `keep_ids` is the exact
    /// set written from the new projection. This also removes stale v2 imports
    /// whose line-based ids disappeared between indexing passes.
    /// Stamped twin (P5a.3, DR-41): the same file-concept sweep at an
    /// explicit `valid_from`. The wrapper below keeps analyze on the clock.
    /// Retire file concepts the run no longer believes, scoped to the run's
    /// lineage (P5a.4, DR-41).
    ///
    /// The candidate set is lineage-minted rows only: a trunk sweep must
    /// never retire a branch-minted module (it is not in the trunk's tree
    /// and never will be), and a branch sweep must never touch trunk rows
    /// (global ids — retiring by id is lineage-blind, so the candidate set
    /// is where the scoping lives). Commit concepts carry no file-path
    /// annotation and pointers deliberately neither (H-1), so both pass
    /// over without needing keep-set entries.
    pub fn retire_stale_file_concepts_at(
        &self,
        keep_ids: &HashSet<String>,
        valid_from: &str,
        lineage: &str,
    ) -> macrame::Result<usize> {
        let stale_ids: Vec<String> = self
            .live_concept_contents_on(lineage)?
            .into_iter()
            .filter(|(id, content)| {
                if keep_ids.contains(id) {
                    return false;
                }
                let Ok(attrs) = serde_json::from_str::<serde_json::Value>(content) else {
                    return false;
                };
                if attrs
                    .get(annotation::FILE_PATH)
                    .and_then(|v| v.as_str())
                    .is_none()
                {
                    return false;
                }
                // Routes are owned by the framework hook's scoped diff-retire
                // (§1.3): the generic sweep runs before extraction and would
                // orphan-then-churn every route each full analyze.
                match classify_v2_concept(content) {
                    V2ConceptClass::Canonical(k) => k != "route",
                    V2ConceptClass::StandaloneField => true,
                    _ => false,
                }
            })
            .map(|(id, _)| id)
            .collect();
        Ok(self.retire_entities_at(&stale_ids, valid_from)?.0)
    }

    /// Un-stamped wrapper (live paths: the analyze sweep runs on the trunk).
    pub fn retire_stale_file_concepts(&self, keep_ids: &HashSet<String>) -> macrame::Result<usize> {
        self.retire_stale_file_concepts_at(keep_ids, &now_iso8601(), "main")
    }

    /// `(id, content)` of every concept the ledger still holds to be true
    /// on one lineage (P5a.4, DR-41): the sweep-scoped counterpart to
    /// [`Self::live_concept_contents`]. Sweeps scope candidates by mint
    /// lineage through this; the unscoped listing stays for health checks
    /// that must classify rows wherever they were minted (v1 repair).
    pub fn live_concept_contents_on(
        &self,
        lineage: &str,
    ) -> macrame::Result<Vec<(String, String)>> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT id, content FROM concepts WHERE retired = 0 AND branch_id = ?1",
                    libsql::params![lineage],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                ));
            }
            Ok(out)
        })
    }

    /// Live ids of framework route concepts (§1.3), optionally scoped to
    /// one file. Routes are identified by the mirrored
    /// `extra.coderadar.kind`, not by id pattern, so a coincidence in a
    /// qualifier can never promote an entity into (or hide a route from)
    /// the framework hook's scoped diff-retire.
    pub fn live_route_ids(&self) -> macrame::Result<Vec<String>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT id FROM concepts WHERE retired = 0 \
                     AND json_extract(extra, '$.coderadar.kind') = 'route'",
                    libsql::params![],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                ids.push(row.get::<String>(0).unwrap_or_default());
            }
            Ok::<_, macrame::DbError>(ids)
        })
    }

    pub fn live_route_ids_for_file(&self, file_path: &str) -> macrame::Result<Vec<String>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT id FROM concepts WHERE retired = 0 \
                     AND json_extract(extra, '$.coderadar.kind') = 'route' \
                     AND json_extract(extra, '$.coderadar.file_path') = ?1",
                    libsql::params![file_path],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                ids.push(row.get::<String>(0).unwrap_or_default());
            }
            Ok::<_, macrame::DbError>(ids)
        })
    }

    /// Open framework-owned edge triples with their `valid_from`, for the
    /// framework hook's scoped pair-retire (§1.3). The hook owns exactly
    /// the `synthetic:` namespace: every framework-resolver edge passes
    /// through `framework.py::_ledger_synthetic_edge_kind`, which mints
    /// `synthetic:{resolver}:{edge}` (13 resolvers in `resolvers/`, edge
    /// parts observed in the audit: handles, registers, navigation,
    /// callback, depends_on, association).
    ///
    /// DR-42 (Q-2, H-3): this used to be an exclusion list (`NOT IN
    /// ('CALLS', 'IMPORTS', 'EXTENDS', 'OVERRIDES')`), which silently
    /// swept every future edge type — including the four structural kinds
    /// it did not exclude (DECORATES, IMPLEMENTS, INSTANTIATES,
    /// REFERENCES) — into the hook's retire scope. The inclusion list
    /// under-sweeps visibly instead: structural rows belong to
    /// `retire_stale_edges`, and direct-path synthetic rows with
    /// non-namespaced kinds (e.g. `DEPENDS_ON`) belong to whoever wrote
    /// them. If you add a resolver, its edges are covered automatically
    /// as long as they pass through the `synthetic:` choke point; if you
    /// add a new edge kind outside that namespace, this sweep will not
    /// retire it — that is the point.
    pub fn open_synthetic_triples(
        &self,
        now: &str,
    ) -> macrame::Result<Vec<(String, String, String, String)>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT source_id, target_id, edge_type, valid_from \
                         FROM links_current WHERE valid_to > ?1 \
                         AND edge_type LIKE '{FRAMEWORK_SYNTHETIC_EDGE_PREFIX}%'"
                    ),
                    libsql::params![now],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                    row.get::<String>(2).unwrap_or_default(),
                    row.get::<String>(3).unwrap_or_default(),
                ));
            }
            Ok::<_, macrame::DbError>(out)
        })
    }

    /// Close open synthetic edge triples the framework hook no longer
    /// believes (§1.3 scoped pair-retire). Structural kinds are never
    /// passed here — `retire_stale_edges` owns those. NotFound (something
    /// else closed it first) is the desired end state, same tolerance as
    /// `retire_entities`.
    pub fn retire_synthetic_pairs(
        &self,
        stale: &[(String, String, String, String)],
        ts: &str,
    ) -> macrame::Result<usize> {
        let n = runtime().block_on(async {
            let mut n = 0usize;
            for (source, target, etype, valid_from) in stale {
                if self
                    .db
                    .retire_edge(
                        source.as_str(),
                        target.as_str(),
                        etype.as_str(),
                        valid_from.as_str(),
                        ts,
                    )
                    .await
                    .is_ok()
                {
                    n += 1;
                }
            }
            n
        });
        Ok(n)
    }

    /// Live concept ids belonging to one exact source file.
    ///
    /// Keep the indexed metadata lookup and legacy ID-prefix lookup separate:
    /// combining them with `OR` makes SQLite scan the table because the prefix
    /// expression is not indexable. The second query uses a primary-key range
    /// over `{file_path}::`, so both paths stay bounded to indexed lookups.
    pub fn live_concept_ids_for_file(&self, file_path: &str) -> macrame::Result<Vec<String>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut ids = HashSet::new();
            let mut rows = conn
                .query(LIVE_IDS_BY_EXTRA_FILE_PATH_SQL, libsql::params![file_path])
                .await
                .map_err(macrame::DbError::Engine)?;
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                ids.insert(row.get::<String>(0).unwrap_or_default());
            }

            // The final `:` in the lower bound is replaced with its next ASCII
            // character. Under SQLite's BINARY text ordering this bounds every
            // id beginning with `{file_path}::` without matching sibling paths
            // such as `a.pyx` when looking up `a.py`.
            let lower = format!("{file_path}::");
            let upper = format!("{file_path}:;");
            let mut rows = conn
                .query(LIVE_IDS_BY_ID_FILE_RANGE_SQL, libsql::params![lower, upper])
                .await
                .map_err(macrame::DbError::Engine)?;
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                ids.insert(row.get::<String>(0).unwrap_or_default());
            }

            let mut out: Vec<String> = ids.into_iter().collect();
            out.sort();
            Ok(out)
        })
    }

    /// Ids of every live ENTITY concept the ledger still holds to be true.
    ///
    /// Entities only: the P3 content-version spine (`kind =
    /// 'content_version'`), the P4 branch-content pointers (`kind =
    /// 'branch_content'`), commit markers (`kind = 'commit'`), and the P6a
    /// annotation/comment rows (`kind = 'annotation'` / `'comment'`) are
    /// ledger infrastructure or user metadata, not entities — timeless or
    /// present-tense rows owned by their own writers, never by the
    /// file-lifecycle sweeps. Every file-lifecycle sweep built on this
    /// listing (F14 non-canonical retirement, exclusion retirement) must
    /// never see them, so the exclusion lives here rather than in each
    /// caller. Read the spine through `history_of`, annotations through
    /// `annotations_on` / `comments_on`.
    ///
    /// The counterpart to [`Self::retire_entities`]: retirement is only
    /// meaningful if something can tell the difference, and until now nothing
    /// read the `retired` column at all.
    pub fn live_concept_ids(&self) -> macrame::Result<Vec<String>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT id FROM concepts WHERE retired = 0 \
                     AND (json_extract(content, '$.kind') IS NULL \
                          OR json_extract(content, '$.kind') NOT IN \
                             ('content_version', 'branch_content', 'commit', \
                              'annotation', 'comment')) \
                     ORDER BY id",
                    (),
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push(row.get::<String>(0).unwrap_or_default());
            }
            Ok(out)
        })
    }

    /// Every `(source, target, edge_type)` triple that currently has an open
    /// interval, read from `links_current`.
    ///
    /// `now` is the as-of instant; openness is `valid_to > now`, the same
    /// test [`Self::retire_entities`] uses (the open sentinel and any future
    /// close both compare greater than a real timestamp, a closed interval's
    /// `valid_to` does not).
    ///
    /// The persist path uses this to stay idempotent: an edge that is already
    /// open is the same fact (CodeRadar edges carry no properties and a
    /// constant weight), so re-asserting it would only add a new version for
    /// `as_of` to replay through — and with per-assertion `valid_from` it
    /// would abort on macrame's single-open-interval guard anyway.
    pub fn open_edge_triples(&self, now: &str) -> macrame::Result<Vec<(String, String, String)>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT source_id, target_id, edge_type \
                     FROM links_current WHERE valid_to > ?1",
                    libsql::params![now],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                    row.get::<String>(2).unwrap_or_default(),
                ));
            }
            Ok(out)
        })
    }

    /// R2-2: close open STRUCTURAL edges absent from a fresh full index.
    ///
    /// `persist_edges` is assert-only and `retire_entities` only fires for
    /// removed entities, so an edge whose endpoints both survive but whose
    /// fact vanished — a deleted call, a pre-R2-2 synthetic-as-CALLS row, a
    /// resolver that stopped emitting — stayed open forever (a full rebuild
    /// reported 4 edges on a 3-edge tree). After a full unscoped index,
    /// every open CALLS / IMPORTS / EXTENDS / OVERRIDES triple not present
    /// in the fresh projection is closed with `valid_to = now`: a bitemporal
    /// assertion, not an erasure, so `as_of` history is preserved.
    /// Synthetic-kind rows are NOT touched — the register path owns them.
    ///
    /// `keep` holds the fresh projection's (source, target, kind) triples
    /// for the five structural kinds, with CALLS pairs minus
    /// `synthetic_edges` (built by `stale_edge_keep_set` in lib.rs).
    /// Returns the number of edges retired.
    /// Stamped twin (P5a.3, DR-41): the same structural-edge sweep at an
    /// explicit `valid_from` — a call deleted in a commit closes at the
    /// commit's stamp, not at ingest time. The wrapper keeps analyze on
    /// the clock.
    pub fn retire_stale_edges_at(
        &self,
        keep: &std::collections::HashSet<(String, String, String)>,
        valid_from: &str,
    ) -> macrame::Result<usize> {
        const STRUCTURAL: [&str; 8] = [
            "CALLS",
            "IMPORTS",
            "EXTENDS",
            "IMPLEMENTS",
            "DECORATES",
            "INSTANTIATES",
            "REFERENCES",
            "OVERRIDES",
        ];
        let ts = valid_from.to_string();
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        let stale: Vec<(String, String, String, String)> = runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT source_id, target_id, edge_type, valid_from \
                     FROM links_current WHERE valid_to > ?1",
                    libsql::params![ts.as_str()],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut stale = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                let triple = (
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                    row.get::<String>(2).unwrap_or_default(),
                );
                if !STRUCTURAL.contains(&triple.2.as_str()) {
                    continue;
                }
                if keep.contains(&triple) {
                    continue;
                }
                stale.push((
                    triple.0,
                    triple.1,
                    triple.2,
                    row.get::<String>(3).unwrap_or_default(),
                ));
            }
            Ok::<_, macrame::DbError>(stale)
        })?;
        // NotFound means something else closed it first; that is the
        // desired end state either way (same tolerance as retire_entities).
        let retired = runtime().block_on(async {
            let mut n = 0usize;
            for (source, target, etype, valid_from) in &stale {
                if self
                    .db
                    .retire_edge(
                        source.as_str(),
                        target.as_str(),
                        etype.as_str(),
                        valid_from.as_str(),
                        ts.as_str(),
                    )
                    .await
                    .is_ok()
                {
                    n += 1;
                }
            }
            n
        });
        Ok(retired)
    }

    /// Un-stamped wrapper (live paths): sweep at the clock.
    pub fn retire_stale_edges(
        &self,
        keep: &std::collections::HashSet<(String, String, String)>,
    ) -> macrame::Result<usize> {
        self.retire_stale_edges_at(keep, &now_iso8601())
    }

    /// Assert a single edge.
    pub fn assert_edge(
        &self,
        source: &str,
        target: &str,
        etype: &str,
        properties_json: &str,
        valid_from: &str,
        weight: f64,
    ) -> macrame::Result<()> {
        let edge = EdgeAssertion::new(source, target, etype)
            .valid_from(valid_from)
            .weight(weight)
            .properties(properties_json);
        runtime().block_on(self.db.assert_edge(edge))
    }

    /// Bulk assert edges (atomic — one transaction stamp per D-014).
    pub fn assert_edges_bulk(&self, edges: Vec<EdgeAssertion>) -> macrame::Result<usize> {
        runtime().block_on(self.db.write_bulk_atomic(edges))
    }

    /// Traverse the graph from a source entity (current state).
    pub fn traverse(
        &self,
        start_id: &str,
        max_depth: usize,
        edge_types: &[&str],
    ) -> macrame::Result<Subgraph> {
        let types: Vec<String> = edge_types.iter().map(|s| s.to_string()).collect();
        self.traverse_at(start_id, max_depth, &types, "now")
    }

    /// Traverse the graph from a source entity at `ts` (temporal read).
    ///
    /// Only the `"now"` live path uses this (no builder instants, so no
    /// `attribute_mode` is required): temporal walks BFS over the
    /// reconstructed state instead (see `bfs_over_state`). The historical
    /// loader gap that forced this (present-tense node closure pruning
    /// post-instant retirements, upstream opticsWolf/Macrame#3) is fixed in
    /// macrame-db 0.19.1 via `hydrate_historical` + `AttributeMode`, re-proven
    /// on the rename fixture (`repro_walk_retired`, now un-ignored) — but the
    /// state fold stays the recorded-time primitive; do not route temporal
    /// walks back through the loader without a reason beyond parity.
    pub fn traverse_at(
        &self,
        start_id: &str,
        max_depth: usize,
        edge_types: &[String],
        ts: &str,
    ) -> macrame::Result<Subgraph> {
        let mut traversal = TraversalBuilder::new(start_id).max_depth(max_depth);
        if !edge_types.is_empty() {
            traversal = traversal.edge_types(edge_types.to_vec());
        }
        runtime().block_on(self.db.load_subgraph_with(&traversal, ts, 10_000_000))
    }

    /// Reconstruct state as of a timestamp.
    pub fn reconstruct(&self, ts: &str) -> macrame::Result<MaterializedState> {
        runtime().block_on(self.db.reconstruct(ts))
    }

    /// State at `ts` as `branch` saw it (P4, DR-40): the ancestry
    /// resolved, one belief per edge key from the nearest lineage holding
    /// it. Concepts need no resolution — the schema forbids two lineages
    /// holding one id (D-225), so narrowing is all a concept needs — and
    /// branch-divergent bytes ride branch-minted pointer concepts
    /// (`branch:{branch}:content:{path}`), which ancestry-narrowing
    /// already scopes to the reader's lineage. On an unforked database this delegates to
    /// `reconstruct` unchanged (Macrame's `Trunk` shape pays nothing).
    /// Unknown branches refuse with `DbError::UnknownBranch`.
    pub fn reconstruct_on(&self, ts: &str, branch: &str) -> macrame::Result<MaterializedState> {
        runtime().block_on(self.db.reconstruct_on(ts, branch))
    }

    /// The ledger's high-water mark without folding it.
    ///
    /// `analyze` stamps LEDGER_REVISION after every flush; a full
    /// `reconstruct(now)` fold just to read one integer costs seconds on a
    /// large log (measured 1.7s on a 300MB fixture db under macrame 0.15).
    /// MAX(seq_id) is a single btree probe and exactly the change-token the
    /// stamp needs: it moves on every committed write. Empty log maps to 0,
    /// matching the empty fold's anchor.
    pub fn current_seq_anchor(&self) -> macrame::Result<i64> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        let mut rows = runtime()
            .block_on(conn.query("SELECT MAX(seq_id) FROM transaction_log", ()))
            .map_err(macrame::DbError::Engine)?;
        let anchor = runtime()
            .block_on(rows.next())
            .map_err(macrame::DbError::Engine)?
            .map(|row| row.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);
        Ok(anchor)
    }

    /// Every recorded generation in a window, oldest first (P1, DR-36).
    ///
    /// A listing, not a fold: one indexed `DISTINCT recorded_at` probe on
    /// the `current_seq_anchor` precedent (folding to read scalars costs
    /// seconds on a large log). `recorded_at` is when the ledger was told,
    /// so one stamp is one act of indexing, not one commit of history.
    /// Branch-scoped: the ledger holds every branch and a sweep must not
    /// mix their stamps.
    ///
    /// Schema dependency (Q-3): this names the `transaction_log` columns
    /// directly. It breaks loudly — a renamed column fails the query —
    /// rather than returning a plausible wrong answer, which is why the
    /// coupling is acceptable until it is upstreamed into Macrame.
    pub fn recorded_generations(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        branch: &str,
    ) -> macrame::Result<Vec<String>> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        let mut sql =
            "SELECT DISTINCT recorded_at FROM transaction_log WHERE branch_id = ?1".to_string();
        // Params are positional (?1 is branch); window bounds append in
        // order so the numbering stays obvious.
        let mut params: Vec<String> = vec![branch.to_string()];
        if let Some(lo) = from {
            sql.push_str(&format!(" AND recorded_at >= ?{}", params.len() + 1));
            params.push(lo.to_string());
        }
        if let Some(hi) = to {
            sql.push_str(&format!(" AND recorded_at <= ?{}", params.len() + 1));
            params.push(hi.to_string());
        }
        sql.push_str(" ORDER BY recorded_at");
        let libsql_params: Vec<libsql::Value> =
            params.into_iter().map(libsql::Value::Text).collect();
        runtime().block_on(async {
            let mut rows = conn
                .query(&sql, libsql_params)
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push(row.get::<String>(0).unwrap_or_default());
            }
            Ok(out)
        })
    }

    /// Every lineage the ledger knows, trunk first (P4, DR-40).
    ///
    /// Read through Macrame's read connection (one tiny append-only
    /// table), not a fold — the same reason `recorded_generations` is a
    /// listing. A database that never forked answers exactly the trunk.
    pub fn branches(&self) -> macrame::Result<Vec<BranchInfo>> {
        let out = runtime().block_on(self.db.branches())?;
        Ok(out
            .into_iter()
            .map(|b| BranchInfo {
                name: b.id.as_str().to_string(),
                parent: b.parent.as_ref().map(|p| p.as_str().to_string()),
                forked_at: b.forked_at.clone(),
                created_at: b.created_at.clone(),
            })
            .collect())
    }

    /// Cut a lineage `name` from `from` (P4, DR-40): the write half of
    /// branch reads, and the setup P5's per-git-branch ingestion stands
    /// on. Refuses unknown parents with `DbError::UnknownBranch` and bad
    /// names with `DbError::InvalidBranchId` — a typo'd lineage must
    /// never silently read as the trunk.
    pub fn fork_branch(&self, name: &str, from: &str) -> macrame::Result<BranchInfo> {
        let name = BranchId::new(name)?;
        let from = BranchId::new(from)?;
        let b = runtime().block_on(self.db.fork(name, from))?;
        Ok(BranchInfo {
            name: b.id.as_str().to_string(),
            parent: b.parent.as_ref().map(|p| p.as_str().to_string()),
            forked_at: b.forked_at.clone(),
            created_at: b.created_at.clone(),
        })
    }

    /// Sync `kv_store` read for callers without a runtime handle (P5a.3,
    /// DR-41): the ingest cursor and stamping-mode flag live in Macrame's
    /// branch-global `kv`, outside the ledger by design (H-20 — never a
    /// blob reference, only mode strings and commit shas).
    pub(crate) fn kv_get_sync(&self, key: &str) -> macrame::Result<Option<String>> {
        runtime().block_on(self.db.kv_get(key))
    }

    /// Sync `kv_store` write (P5a.3, DR-41). One or two keys per commit
    /// (head + stamp); the write actor serializes them.
    pub(crate) fn kv_put_sync(&self, key: &str, value: &str) -> macrame::Result<()> {
        runtime().block_on(self.db.kv_put(key, value))
    }

    /// Record divergent content for `path` on `branch` (P4, DR-40): the
    /// one branch-legal way to disagree with the trunk about a file's
    /// bytes. Concepts are lineage-frozen (D-225: a branch inherits and
    /// cannot restate), so this mints the three things a branch MAY own:
    /// blob bytes (content-addressed, branch-global table — identical
    /// content shares one row across lineages), the content-addressed
    /// spine concept (same id the trunk would mint for these bytes, so
    /// `history_of` stays one chain and `all_concept_ids` stays clean),
    /// and the branch-content pointer concept (stable id per lineage +
    /// path — the read side's digest source, same-id turnover on
    /// re-point, so superseded bytes archive exactly like superseded
    /// module content).
    ///
    /// Two commits, honestly: blob put, then spine + pointer in one
    /// bulk (both concepts, one stamp — the P3 one-flush ethos). A
    /// re-point to the same bytes is a gate no-op (the open pointer
    /// already names them — zero-growth). Returns the spine id, so
    /// callers can assert chain continuity.
    ///
    /// Refusals: unknown branch (the pointer mint names it —
    /// `BranchId::new` refuses blanks, the write refuses the
    /// unregistered); disabled or skipped blob puts (no bytes, no
    /// pointer — a pointer without bytes would be a lie the read side
    /// cannot detect).
    pub fn write_branch_content(
        &self,
        path: &str,
        bytes: &[u8],
        branch: &str,
    ) -> macrame::Result<String> {
        self.write_branch_content_at(path, bytes, branch, &now_iso8601())
    }

    /// Stamped twin (P5a.2, DR-41): the same branch write at an explicit
    /// `valid_from` — commit time on the P5 path (P5a.4's first non-`now`
    /// caller). The blob put stays engine-stamped: only the concept rows
    /// carry the commit axis.
    pub fn write_branch_content_at(
        &self,
        path: &str,
        bytes: &[u8],
        branch: &str,
        valid_from: &str,
    ) -> macrame::Result<String> {
        BranchId::new(branch)?;
        let sha = match self.put_source_blob(path, bytes)? {
            BlobOutcome::Stored { digest, .. } => digest,
            outcome => {
                return Err(macrame::DbError::Engine(libsql::Error::Misuse(format!(
                    "write_branch_content: no blob recorded for {path} on {branch} ({outcome:?})"
                ))));
            }
        };
        let now = valid_from.to_string();
        let size = bytes.len() as u64;
        let lines = if bytes.is_empty() {
            0
        } else {
            let breaks = bytes.iter().filter(|b| **b == b'\n').count() as u64;
            breaks + u64::from(!bytes.ends_with(b"\n"))
        };
        // Same id the trunk's `spine_upserts_for` mints for these bytes:
        // `{path}@{sha}` content-addressed, lineage-agnostic.
        let spine_id = format!("{path}@{sha}");
        let spine = spine_concept_upsert(path, &sha, size, lines, &now);
        let pointer = branch_pointer_upsert(path, &sha, branch, &now)?;
        self.upsert_concepts_bulk(&[spine, pointer])?;
        Ok(spine_id)
    }

    /// Record a deletion on `branch` (P5a.4, DR-41): the tombstone twin of
    /// [`Self::write_branch_content_at`]. Same stable pointer id, a
    /// `deleted` marker, no blob — the read side omits the path instead
    /// of falling back to trunk content. One stamped row, same-id
    /// turnover: re-adding the file re-points to content, and the archive
    /// reclaims the tombstone row like any other superseded version.
    /// Unknown branches refuse exactly like the content write.
    pub fn write_branch_tombstone_at(
        &self,
        path: &str,
        branch: &str,
        valid_from: &str,
    ) -> macrame::Result<()> {
        BranchId::new(branch)?;
        let tombstone = branch_tombstone_upsert(path, branch, valid_from)?;
        self.upsert_concepts_bulk(&[tombstone])?;
        Ok(())
    }

    /// Which lineage minted `id`, if any row — current or retired — still
    /// names it (P5a.4, DR-41): the D-225 backstop query. A branch ingest
    /// mints entity concepts only for ids no other lineage holds; the
    /// trunk ingest skips ids a branch minted. `None` is unminted (or
    /// trunk-default, which the callers treat as unminted — the check is
    /// "foreign to my lineage", never "present").
    pub fn mint_lineage(&self, id: &str) -> macrame::Result<Option<String>> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT branch_id FROM concepts WHERE id = ?1",
                    libsql::params![id],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            Ok(rows
                .next()
                .await
                .map_err(macrame::DbError::Engine)?
                .map(|row| row.get::<String>(0).unwrap_or_default())
                .filter(|b| !b.is_empty()))
        })
    }

    /// Ids of every module concept the ledger ever described on a branch
    /// (P1, DR-36): the input universe for a per-generation sweep.
    ///
    /// EVER, not live: a `retired = 0` filter would drop every module that
    /// no longer exists, and the sweep must still visit the generations
    /// where it did. The per-generation `source_refs_at` batch then filters
    /// this universe to present-at-T (absent ids are omitted, never `None`),
    /// so retired and not-yet-asserted ids cost nothing but list membership.
    /// Both id spellings are included (H-11: `{file}::module` changed
    /// spelling mid-history); each generation's batch returns only the
    /// spelling live at that T, so no generation double-counts.
    /// The `LIKE`/`GLOB` pair is the prototype's (`scripts/verify_history.py`):
    /// `LIKE` for the index, `GLOB` because SQLite `LIKE` is
    /// case-insensitive for ASCII and would also match a struct named
    /// `Module`.
    pub fn module_concept_ids(&self, branch: &str) -> macrame::Result<Vec<String>> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT DISTINCT entity_id FROM transaction_log \
                     WHERE table_name = 'concepts' AND branch_id = ?1 \
                     AND entity_id LIKE '%::module' AND entity_id GLOB '*::module' \
                     ORDER BY entity_id",
                    libsql::params![branch],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push(row.get::<String>(0).unwrap_or_default());
            }
            Ok(out)
        })
    }

    /// Ids of every module concept visible on `branch` (P4, DR-40): the
    /// branch's own rows UNION its ancestors' — inheritance, not just
    /// mint lineage. A branch sweep must see the trunk's modules (it
    /// reads them at fork-time content through its own folds); the
    /// single-branch [`Self::module_concept_ids`] lists what was minted
    /// where and stays the honest primitive underneath. Unknown branches
    /// refuse via the ancestry lookup (never trunk-fallback).
    pub fn visible_module_concept_ids(&self, branch: &str) -> macrame::Result<Vec<String>> {
        let mut out = self.module_concept_ids(branch)?;
        for ancestor in runtime().block_on(self.db.ancestry(branch))? {
            if ancestor.branch_id != branch {
                out.extend(self.module_concept_ids(&ancestor.branch_id)?);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Ids of every concept the ledger ever described on `branch` (P2,
    /// DR-37): the input universe for the entity diff. Same ever-not-live
    /// contract as [`Self::module_concept_ids`] without the kind filter —
    /// functions, classes, imports and the rest are in the diff's join,
    /// not just files. Excludes the non-entity kinds (`content_version`
    /// spine, P4 `branch_content` pointers, commit markers, P6a
    /// annotation/comment rows) the way `live_concept_ids`
    /// does — the diff joins entities, not ledger machinery or user
    /// metadata (an annotation has no content hash, so without the
    /// exclusion it would read as a perpetual `uncompared` group).
    /// Both id spellings are included (H-11); each
    /// generation's batch returns only the spelling live at that T, and
    /// `ops.changes_between` groups raw ids by canonical form, so no
    /// generation double-counts and a respelling never reads as churn.
    /// Retired ids stay in (a deletion is a T1-only group); branch scope
    /// matches `transaction_log.branch_id` (`"main"` unless told
    /// otherwise). `ordering` is_id order — deterministic, not meaningful.
    ///
    /// Infrastructure kinds (`content_version` spine, P4
    /// `branch_content` pointers) are excluded by kind (P2's diff joins
    /// entities; a timeless `{path}@{digest}` row would read as a perpetual
    /// `uncompared` group at every T, and a branch pointer is a view, not
    /// an entity). The kind exclusion is branch-uniform: it holds for the
    /// mint branch's own rows too, so pointers never enter any branch's
    /// diff universe. The subquery is branch-agnostic by id, which is
    /// exact here: spine ids are content-addressed, so the same bytes
    /// are the same spine row on every branch.
    pub fn all_concept_ids(&self, branch: &str) -> macrame::Result<Vec<String>> {
        let (_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT DISTINCT entity_id FROM transaction_log \
                     WHERE table_name = 'concepts' AND branch_id = ?1 \
                     AND entity_id NOT IN (SELECT id FROM concepts \
                         WHERE json_extract(content, '$.kind') IN \
                            ('content_version', 'branch_content', 'commit', \
                             'annotation', 'comment')) \
                     ORDER BY entity_id",
                    libsql::params![branch],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                out.push(row.get::<String>(0).unwrap_or_default());
            }
            Ok(out)
        })
    }

    /// Ids of every concept visible on `branch` (P4, DR-40): the
    /// ancestry union, matching [`Self::visible_module_concept_ids`] —
    /// what P2's diff sweeps when it runs on a lineage.
    pub fn visible_all_concept_ids(&self, branch: &str) -> macrame::Result<Vec<String>> {
        let mut out = self.all_concept_ids(branch)?;
        for ancestor in runtime().block_on(self.db.ancestry(branch))? {
            if ancestor.branch_id != branch {
                out.extend(self.all_concept_ids(&ancestor.branch_id)?);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Compute the content-version spine upserts for freshly flushed
    /// modules (P3, DR-38): one timeless `{path}@{digest}` concept per
    /// actual content change, nothing for unchanged files. Pure compute
    /// plus store reads — no writes — so both write sites append the
    /// spine to their v2 bulk and flush ONCE: the spine must not mint
    /// its own commit, because `recorded_generations` (P1) counts
    /// DISTINCT `recorded_at` per commit and one flush stays one
    /// generation.
    ///
    /// Concepts only, deliberately no edge: an edge target names its
    /// concept, and Macrame's archive reference scan reads hot link
    /// payloads — so every closed `HAD_CONTENT` version would pin its
    /// blob hot forever and defeat retention (§3.4, DR-25). The chain
    /// needs no edge: spine concepts order by `valid_from`, intervals
    /// and parents derive from chain order. Likewise the concept
    /// payload names no digest — title and content carry the path only
    /// and `extra` carries numbers only (payloads DO log `extra`,
    /// verified against the ledger, not assumed) — the digest lives
    /// solely in the id, an unscanned `entity_id` column since the scan
    /// reads payloads. A superseded blob therefore stays archivable.
    ///
    /// `module_digests` carries only modules with a validated digest
    /// (degraded puts keep graph coverage but mint no spine; the next
    /// successful put starts the chain). Unchanged content is ALWAYS
    /// re-emitted, never skipped by digest comparison: the timeless id
    /// makes "same digest" look like "nothing to do", but the open
    /// row may still need the rewrite — a v1 row missing `meta_version`
    /// is the case that bit (the v1→v2 upgrade silently skipped every
    /// spine row). The [`CodeGraphStore::filter_unchanged`] gate absorbs
    /// the true no-op (byte-identical re-upsert, no new version, no new
    /// commit); emitting unconditionally also heals any metadata drift
    /// or corruption from ground truth (blob bytes). A missing blob
    /// skips the file loudly, never half-written.
    pub fn spine_upserts_for(
        &self,
        module_digests: &[(String, String)],
        now: &str,
    ) -> macrame::Result<Vec<ConceptUpsert>> {
        let mut out = Vec::new();
        for (module_id, digest) in module_digests {
            let Some(path) = module_id.strip_suffix("::module") else {
                eprintln!("[diag] spine: skipping non-module id {module_id}");
                continue;
            };
            let bytes = match self.blob_get(digest)? {
                Some(b) => b,
                None => {
                    eprintln!("[diag] spine: blob missing for {digest} ({path}) — skipping");
                    continue;
                }
            };
            let size = bytes.len() as u64;
            // Line count: newlines, plus the final unterminated line.
            // A trailing newline does not start a new line.
            let lines = if bytes.is_empty() {
                0
            } else {
                let breaks = bytes.iter().filter(|b| **b == b'\n').count() as u64;
                breaks + u64::from(!bytes.ends_with(b"\n"))
            };
            out.push(spine_concept_upsert(path, digest, size, lines, now));
        }
        Ok(out)
    }

    /// Maintain the content-version spine (P3, DR-38): compute via
    /// [`CodeGraphStore::spine_upserts_for`] and flush in one commit.
    /// The write sites (full analyze, `update_file`) inline the compute
    /// into their v2 bulk instead — one flush, one generation — so this
    /// wrapper exists for small callers and tests, not the hot path.
    pub fn maintain_content_spine(
        &self,
        module_digests: &[(String, String)],
        now: &str,
    ) -> macrame::Result<SpineReport> {
        let concepts = self.spine_upserts_for(module_digests, now)?;
        Ok(SpineReport {
            files_seen: module_digests
                .iter()
                .filter(|(id, _)| id.ends_with("::module"))
                .count(),
            concepts_upserted: self.upsert_concepts_bulk(&concepts)?,
        })
    }

    /// Every content a file ever had, newest first (P3, DR-38): the
    /// spine chain with derived intervals, plus the spine payload's
    /// `parent_digest`/`size`/`line_count`.
    ///
    /// Ledger-direct, not a fold: the chain is one concept per content
    /// change, so a full reconstruct to read it would be pure overhead.
    /// Reads the timeless spine concepts by id prefix (`concepts.id`
    /// is UNIQUE, so one row per content — no version fold); intervals
    /// derive from chain order because maintain stamps one `now` per
    /// content (each version's `valid_from` ends the previous one's
    /// interval, contiguously by construction).
    ///
    /// The head is open while the module concept is live. A deleted
    /// file's chain ends at the module's retire stamp (the retire
    /// cascade owns entity death; the spine owns content, so it never
    /// invents the stamp — it reports the module's). An unknown file
    /// reads as `[]`: no spine rows, no chain.
    ///
    /// Branch is trunk for now — P4 (DR-40) threads lineage through
    /// this read. A corrupt spine payload fails the read (INV-7
    /// honesty, P0 precedent), with the concept id in the message.
    pub fn history_of(&self, file_path: &str) -> macrame::Result<Vec<SpineVersion>> {
        let module_id = format!("{file_path}::module");
        let escaped = file_path
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("{escaped}@%");
        let (_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT id, extra, valid_from FROM concepts \
                     WHERE retired = 0 AND id LIKE ?1 ESCAPE '\\' \
                     AND json_extract(content, '$.kind') = 'content_version' \
                     ORDER BY valid_from DESC",
                    libsql::params![pattern],
                )
                .await
                .map_err(macrame::DbError::Engine)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                let id: String = row.get(0).unwrap_or_default();
                let extra: String = row.get(1).unwrap_or_default();
                let payload: serde_json::Value = serde_json::from_str(&extra).map_err(|e| {
                    macrame::DbError::Engine(libsql::Error::Misuse(format!(
                        "spine concept {id} has unparseable extra: {e}",
                    )))
                })?;
                let inner = payload.get("coderadar");
                let get_u64 = |key: &str| {
                    inner
                        .and_then(|v| v.get(key))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                };
                let prefix = format!("{file_path}@");
                out.push(SpineVersion {
                    digest: id.strip_prefix(&prefix).unwrap_or(&id).to_string(),
                    valid_from: row.get(2).unwrap_or_default(),
                    valid_to: TS_OPEN.to_string(),
                    open: true,
                    // No stored parent: the parent is the next-older
                    // link, derived below from chain order (`extra`
                    // carries no digests — retention coexistence).
                    parent_digest: None,
                    size_bytes: get_u64("size"),
                    line_count: get_u64("line_count"),
                });
            }
            // Intervals and parents derive from chain order: each
            // version's start ends the previous one's interval, and each
            // version's parent is the next-older link (the oldest content
            // is parentless).
            for i in 1..out.len() {
                let start = out[i - 1].valid_from.clone();
                out[i].valid_to = start;
                out[i].open = false;
                let child = i - 1;
                let parent_digest = out[i].digest.clone();
                out[child].parent_digest = Some(parent_digest);
            }
            // The head ends where the file died, if it did: the module
            // concept's retire stamp. A live module (or an untraceable
            // one) leaves the head open.
            if let Some(head) = out.first_mut() {
                let mut mrows = conn
                    .query(
                        "SELECT retired, valid_to FROM concepts WHERE id = ?1",
                        libsql::params![module_id],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                if let Some(mrow) = mrows.next().await.map_err(macrame::DbError::Engine)? {
                    let retired: i64 = mrow.get(0).unwrap_or(0);
                    if retired != 0 {
                        head.valid_to = mrow.get(1).unwrap_or_default();
                        head.open = false;
                    }
                }
            }
            Ok(out)
        })
    }
}

// ── Source-blob write path (§1.9, DR-25/DR-30) ────────────────────────────

/// Blob size cap: Macrame's `Tuning::max_blob_bytes` default (8 MiB).
/// CodeRadar does not plumb tuning, so this const mirrors the engine — a
/// file over it is refused HERE, and the engine refuses it again as belt
/// (both count as `skipped_oversize`, never as an index failure).
///
/// Pairing (documented, not enforced): the watcher skips files over
/// `[watch] max_file_size_bytes` (1 MiB default) before they ever reach
/// this path, so oversize blobs arise only from direct `analyze` /
/// `update_file` calls on files between 1 MiB and 8 MiB. Raising one cap
/// must never silently raise the other.
pub const BLOB_MAX_BYTES: usize = 8 * 1024 * 1024;

/// A sha256 digest is 64 lowercase hex chars — the address and the integrity
/// check in one. Validated at the persistence boundary, both directions:
/// put-returns are checked before their digest is asserted (an engine bug
/// must fail loudly, never mint a live digest pointing at absent bytes),
/// and stored extras are checked on read (legacy generations have no digest
/// at all; malformed ones read as absent, never as errors).
pub fn validate_digest(digest: &str) -> std::result::Result<(), String> {
    if digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(format!("malformed source_blob digest: {digest:?}"))
    }
}

/// The `extra.coderadar.source_blob` read boundary (§3.0 reads through
/// this): a validated digest, or `None` — legacy generations (no key) and
/// malformed ones both read as absent, never as errors.
pub fn source_blob_of_extra(extra_json: &str) -> Option<String> {
    let extra: serde_json::Value = serde_json::from_str(extra_json).ok()?;
    let digest = extra.get("coderadar")?.get("source_blob")?.as_str()?;
    validate_digest(digest).ok()?;
    Some(digest.to_string())
}

/// One file's blob outcome. `Stored` carries the digest + byte length;
/// the policy skips keep full graph coverage with no blob. Anything else
/// (I/O, engine bugs) is a propagated error — a broken store must fail
/// the index honestly, not silently skip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobOutcome {
    Stored { digest: String, bytes: u64 },
    SkippedOversize { bytes: u64 },
    SkippedExcluded,
    Disabled,
}

/// Cumulative blob activity backing the currently loaded graph generation:
/// reset on `analyze`/`load_snapshot`, incremented by every write-through
/// put. Read via `blob_stats()` for the reindex/watcher/status surfaces —
/// the DR-30 notice (counts ARE the notice) without per-call threading.
#[derive(Clone, Debug, Default)]
pub struct BlobStats {
    pub stored: u64,
    pub skipped_oversize: u64,
    pub skipped_excluded: u64,
    pub bytes: u64,
}

static BLOB_STATS: std::sync::LazyLock<std::sync::Mutex<BlobStats>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(BlobStats::default()));

/// Reset the cumulative counters (analyze/load boundaries).
pub fn reset_blob_stats() {
    if let Ok(mut stats) = BLOB_STATS.lock() {
        *stats = BlobStats::default();
    }
}

/// H-12: the last edge-persist failure of this generation, or `None`.
///
/// `persist_edges` is assert-only and flushes in batches, so a failure part
/// way through leaves the batches before it committed — the store's edge
/// state is then incomplete while the index itself looks healthy. That used
/// to be a `[diag]` line on stderr with a zero exit, which is how a partial
/// edge write could pass for a clean analyze. Recorded here on the same
/// terms as `BLOB_STATS`: process-global and reset at the analyze/load
/// boundary, because the cheap reindex path persists edges through
/// `update_file` and per-call threading would miss it.
static EDGE_PERSIST_ERROR: std::sync::LazyLock<std::sync::Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Clear the recorded failure (analyze/load boundaries).
pub fn reset_edge_persist_error() {
    if let Ok(mut slot) = EDGE_PERSIST_ERROR.lock() {
        *slot = None;
    }
}

/// Record that edge persistence failed, with the engine's own message.
pub fn record_edge_persist_error(message: impl Into<String>) {
    if let Ok(mut slot) = EDGE_PERSIST_ERROR.lock() {
        *slot = Some(message.into());
    }
}

/// The recorded failure for the report surfaces, or `None` when edges
/// persisted cleanly.
pub fn edge_persist_error_snapshot() -> Option<String> {
    EDGE_PERSIST_ERROR.lock().ok().and_then(|slot| slot.clone())
}

/// Where one entity's bytes live at one timestamp (§3.0 read path).
///
/// `digest=None` = no blob recorded for the file at T; `has_span=false` =
/// the concept carries no source span (synthetic routes). Modules carry no
/// span either, but they ARE the file: `full_file=true` means the whole
/// blob, no slicing (file-bytes-at-T, plan §3.0). `handler_id` is populated
/// for routes so the honest error can name the fallback.
#[derive(Clone, Debug)]
pub struct SourceRef {
    pub id: String,
    pub file_path: String,
    pub digest: Option<String>,
    pub span_start: usize,
    pub span_end: usize,
    pub has_span: bool,
    pub full_file: bool,
    pub start_line: u64,
    pub end_line: u64,
    pub handler_id: Option<String>,
}

/// Snapshot the cumulative counters for the report surfaces.
pub fn blob_stats_snapshot() -> BlobStats {
    BLOB_STATS
        .lock()
        .map(|stats| stats.clone())
        .unwrap_or_default()
}

fn record_blob_outcome(outcome: &BlobOutcome) {
    let Ok(mut stats) = BLOB_STATS.lock() else {
        return;
    };
    match outcome {
        BlobOutcome::Stored { bytes, .. } => {
            stats.stored += 1;
            stats.bytes += bytes;
        }
        BlobOutcome::SkippedOversize { .. } => stats.skipped_oversize += 1,
        BlobOutcome::SkippedExcluded => stats.skipped_excluded += 1,
        BlobOutcome::Disabled => {}
    }
}

// ── Concept Builder ─────────────────────────────────────────────────────────

/// Build a Macrame ConceptUpsert from a CodeRadar ExtractedUnit.
/// Compact, queryable CodeRadar attributes carried beside canonical entity
/// content. Keep the namespace application-specific so other users of the
/// shared ledger can evolve their own attributes independently.
/// `format` stays 1 (declared §1.9): `source_blob` is additive-optional —
/// present on module concepts whose file bytes were blob-stored, absent on
/// legacy generations and policy-skipped files. Absence is the signal;
/// readers (`source_blob_of_extra`) treat missing and malformed alike.
fn coderadar_extra(content: &serde_json::Value, source_blob: Option<&str>) -> serde_json::Value {
    let mut inner = serde_json::Map::new();
    inner.insert("format".to_string(), serde_json::json!(1));
    inner.insert(
        "kind".to_string(),
        content
            .get("kind")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    inner.insert(
        "file_path".to_string(),
        content
            .get("file_path")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    inner.insert(
        "content_hash".to_string(),
        content
            .get(annotation::CONTENT_HASH)
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    if let Some(digest) = source_blob {
        inner.insert(
            "source_blob".to_string(),
            serde_json::Value::String(digest.to_string()),
        );
    }
    serde_json::Value::Object(
        [("coderadar".to_string(), serde_json::Value::Object(inner))]
            .into_iter()
            .collect(),
    )
}

/// Entity metadata is stored canonically in `content`; the small subset needed
/// for indexed maintenance queries is mirrored in Macrame 0.18's `extra`.
pub fn build_concept(unit: &ExtractedUnit, file_path: &str, language: &str) -> ConceptUpsert {
    build_concept_at(unit, file_path, language, &now_iso8601())
}

/// Stamped twin (P5a.2, DR-41): the same v1 pre-flush row at an explicit
/// `valid_from` — commit time on the P5 path.
pub fn build_concept_at(
    unit: &ExtractedUnit,
    file_path: &str,
    language: &str,
    valid_from: &str,
) -> ConceptUpsert {
    let entity_id = unit.entity_id();
    let (title, _kind, metadata) = entity_meta(unit, file_path, language);

    let content = serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".into());
    // v1 pre-flush rows carry no digest (healed to v2 with digests in the
    // same run); the digest-less shape is also what legacy rows look like.
    let extra = coderadar_extra(&metadata, None).to_string();

    let valid_from = valid_from.to_string();

    ConceptUpsert::new(entity_id, title.to_string())
        .content(content)
        .extra(extra)
        .valid_from(valid_from)
        .valid_to(TS_OPEN.to_string())
        .retired(false)
}

/// Convert days since 0000-03-01 to (year, month, day) using the civil calendar algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Algorithm from Howard Hinnant, based on the proleptic Gregorian calendar
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month phase [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Build entity metadata JSON for a ConceptUpsert.content field.
fn entity_meta<'a>(
    unit: &'a ExtractedUnit,
    file_path: &'a str,
    language: &'a str,
) -> (&'a str, &'a str, serde_json::Value) {
    use serde_json::json;

    let base = json!({
        annotation::FILE_PATH: file_path,
        annotation::LANGUAGE: language,
    });

    match unit {
        ExtractedUnit::Function(f) => {
            let mut meta = base;
            meta[annotation::KIND] = json!("function");
            meta[annotation::LINE] = json!(f.line);
            meta[annotation::END_LINE] = json!(f.exit_line);
            meta[annotation::NAME_SPAN] = json!(span_to_str(f.name_span));
            meta[annotation::BODY_SPAN] = json!(span_to_str(f.body_span));
            meta[annotation::PARAMS_SPAN] = json!(span_to_str(f.params_span));
            meta[annotation::CONTENT_HASH] = json!(format!("{:x}", f.body_hash));
            meta[annotation::PARSE_QUALITY] = json!(format!("{:?}", f.parse_quality));
            if f.is_async {
                meta[annotation::IS_ASYNC] = json!(true);
            }
            if !f.decorators.is_empty() {
                meta[annotation::DECORATORS] = json!(f.decorators.join("\x00"));
            }
            if let Some(ref dt) = f.docstring {
                meta[annotation::DOCSTRING] = json!(dt);
            }
            if let Some(ref rt) = f.return_type {
                meta[annotation::RETURN_TYPE] = json!(rt);
            }
            (&f.name, &f.name, meta)
        }
        ExtractedUnit::Class(c) => {
            let mut meta = base;
            meta[annotation::KIND] = json!("class");
            meta[annotation::LINE] = json!(c.line);
            meta[annotation::END_LINE] = json!(c.exit_line);
            meta[annotation::NAME_SPAN] = json!(span_to_str(c.name_span));
            meta[annotation::BODY_SPAN] = json!(span_to_str(c.body_span));
            meta[annotation::PARSE_QUALITY] = json!(format!("{:?}", c.parse_quality));
            if !c.decorators.is_empty() {
                meta[annotation::DECORATORS] = json!(c.decorators.join("\x00"));
            }
            if let Some(ref dt) = c.docstring {
                meta[annotation::DOCSTRING] = json!(dt);
            }
            if !c.grammar_kind.is_empty() {
                meta[annotation::GRAMMAR_KIND] = json!(&c.grammar_kind);
            }
            (&c.name, &c.name, meta)
        }
        ExtractedUnit::Import(i) => {
            let mut meta = base;
            meta[annotation::KIND] = json!("import");
            meta[annotation::LINE] = json!(i.line);
            meta[annotation::NAME_SPAN] = json!(span_to_str(i.name_span));
            ("__import__", &i.raw, meta)
        }
        ExtractedUnit::Constant(c) => {
            let mut meta = base;
            meta[annotation::KIND] = json!("constant");
            meta[annotation::NAME_SPAN] = json!(span_to_str(c.name_span));
            (&c.name, &c.name, meta)
        }
        ExtractedUnit::TypeAlias(t) => {
            let mut meta = base;
            meta[annotation::KIND] = json!("type_alias");
            meta[annotation::NAME_SPAN] = json!(span_to_str(t.name_span));
            (&t.name, &t.name, meta)
        }
        ExtractedUnit::Field(f) => {
            let mut meta = base;
            meta[annotation::KIND] = json!("field");
            meta[annotation::NAME_SPAN] = json!(span_to_str(f.name_span));
            (&f.name, &f.name, meta)
        }
        ExtractedUnit::Module(m) => {
            let mut meta = base;
            meta[annotation::KIND] = json!("module");
            meta[annotation::FILE_PATH] = json!(m.path.to_string_lossy());
            (&m.name, &m.name, meta)
        }
    }
}

fn span_to_str(span: ByteSpan) -> String {
    format!("{}..{}", span.start, span.end)
}

// ── Concept JSON v2 (v0.8 P1) ──────────────────────────────────────────────
//
// v1 concept JSON (see `entity_meta`) consumed pre-resolution
// `ExtractedUnit`s, so it cannot carry resolved state such as
// `Function.resolved_calls`. v2 is built from the FINAL `ProjectedGraph`
// entities — after the resolution cascade — and is what `load_snapshot`
// parses back (see `graph::cold_start`).
//
// Version gate: every canonical concept carries `"meta_version": 2`. A store
// whose canonical concepts lack it (v1 stores) is rejected on load with a
// hard error, never silently upgraded.

/// The meta_version value concept JSON v2 writes and requires.
pub const V2_META_VERSION: u64 = 2;

/// Canonical entity kinds a v2 concept may declare.
pub const V2_CANONICAL_KINDS: &[&str] = &[
    "module",
    "class",
    "function",
    "import",
    "constant",
    "type_alias",
    "route",
];

fn file_path_of(id: &str) -> &str {
    // Entity ids are `{file_path}::{rest}`; file paths never contain "::".
    match id.split_once("::") {
        Some((file, _)) => file,
        None => id,
    }
}

fn v2_common(
    kind: &str,
    name: &str,
    file_path: &str,
    line: u64,
    exit_line: u64,
    docstring: &Option<String>,
    decorators: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "meta_version": V2_META_VERSION,
        "kind": kind,
        "name": name,
        "file_path": file_path,
        "line": line,
        "exit_line": exit_line,
        "docstring": docstring,
        "decorators": decorators,
    })
}

/// True when `concept` must be written: new id, title/hash/retired drift,
/// or a stale envelope version (a v1 row can agree on all three yet still
/// need the v2 upgrade rewrite — and any future meta_version bump must
/// rewrite every row exactly once).
fn concept_changed(
    concept: &ConceptUpsert,
    current: Option<&(String, String, String, bool)>,
) -> bool {
    let Some((title, content, extra, retired)) = current else {
        return true;
    };
    if title != &concept.title || retired != &concept.retired {
        return true;
    }
    // In Macrame 0.18, `None` means leave app attributes untouched. Compare
    // only explicitly stated metadata so maintenance upserts stay non-erasing.
    if concept
        .extra
        .as_ref()
        .is_some_and(|expected| expected != extra)
    {
        return true;
    }
    let cur: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return true,
    };
    if cur.get("meta_version").and_then(|m| m.as_u64()) != Some(V2_META_VERSION) {
        return true;
    }
    match (
        content_hash_of(&concept.content),
        cur.get(annotation::CONTENT_HASH).and_then(|h| h.as_str()),
    ) {
        (Some(a), Some(b)) => a != b,
        // Neither side hashable: compare the full content (parsed, so
        // key order cannot fake a change). The P3 spine deliberately
        // carries no `content_hash` — a digest in content would land in
        // the logged payload and pin the blob hot against retention —
        // so without this arm every maintain would mint a fresh spine
        // version per analyze, churning `valid_from` and scrambling the
        // chain order the read derives. Byte-identical means no-op.
        (None, None) => {
            serde_json::from_str::<serde_json::Value>(&concept.content).map_or(true, |v| v != cur)
        }
        // Unhashable on exactly one side rewrites (safe direction).
        _ => true,
    }
}

/// The `content_hash` annotation, if present and a string.
fn content_hash_of(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()?
        .get(annotation::CONTENT_HASH)?
        .as_str()
        .map(str::to_string)
}

pub(crate) fn v2_upsert(
    id: &str,
    title: &str,
    content: serde_json::Value,
    now: &str,
    source_blob: Option<&str>,
) -> ConceptUpsert {
    let extra = coderadar_extra(&content, source_blob).to_string();
    ConceptUpsert::new(id.to_string(), title.to_string())
        .content(content.to_string())
        .extra(extra)
        .valid_from(now.to_string())
        .valid_to(TS_OPEN.to_string())
        .retired(false)
}

fn module_content(m: &Module) -> serde_json::Value {
    let file_path = m.id.strip_suffix("::module").unwrap_or(&m.id);
    let mut v = v2_common("module", &m.name, file_path, 0, 0, &None, &[]);
    v["language"] = serde_json::json!(m.language.to_json());
    v["parse_quality"] = serde_json::json!(m.parse_quality.to_json());
    v["file_version"] = serde_json::json!(m.file_version);
    v["content_hash"] = serde_json::json!(format!("{:x}", m.content_hash));
    v["classes"] = serde_json::json!(&m.classes);
    v["functions"] = serde_json::json!(&m.functions);
    v["imports"] = serde_json::json!(&m.imports);
    v["constants"] = serde_json::json!(&m.constants);
    v["type_aliases"] = serde_json::json!(&m.type_aliases);
    // Module-scope liveness (dead-code roots, RTA, property reads). Written
    // only when present: most modules have none, and loads default them.
    if !m.uses.is_empty() {
        v["uses"] = serde_json::json!(&m.uses);
    }
    if !m.resolved_uses.is_empty() {
        v["resolved_uses"] = serde_json::json!(&m.resolved_uses);
    }
    if !m.attr_reads.is_empty() {
        v["attr_reads"] = serde_json::json!(&m.attr_reads);
    }
    // DR-11: the dedup key rides the ledger (vectors persist separately).
    if !m.embedding.hash.is_empty() {
        v["embedding_hash"] = serde_json::json!(&m.embedding.hash);
    }
    v
}

fn class_content(c: &Class) -> serde_json::Value {
    let file_path = file_path_of(&c.id);
    let mut v = v2_common(
        "class",
        &c.name,
        file_path,
        c.line as u64,
        c.exit_line as u64,
        &c.docstring,
        &c.decorators,
    );
    v["grammar_kind"] = serde_json::json!(&c.grammar_kind);
    v["parent_class"] = serde_json::json!(&c.parent_class);
    v["bases"] = serde_json::json!(&c.bases);
    v["is_type_checking_only"] = serde_json::json!(&c.is_type_checking_only);
    v["fields"] = serde_json::json!(&c.fields);
    v["source"] = serde_json::json!(&c.source);
    v["parse_quality"] = serde_json::json!(&c.parse_quality.to_json());
    v["content_hash"] = serde_json::json!(format!("{:x}", c.content_hash));
    v["span"] = serde_json::json!(&c.span);
    v["name_span"] = serde_json::json!(&c.name_span);
    v["body_span"] = serde_json::json!(&c.body_span);
    v["decorators_span"] = serde_json::json!(&c.decorators_span);
    // DR-11: the dedup key rides the ledger (vectors persist separately).
    if !c.embedding.hash.is_empty() {
        v["embedding_hash"] = serde_json::json!(&c.embedding.hash);
    }
    v
}

fn function_content(f: &Function) -> serde_json::Value {
    let file_path = file_path_of(&f.id);
    let mut v = v2_common(
        "function",
        &f.name,
        file_path,
        f.line as u64,
        f.exit_line as u64,
        &f.docstring,
        &f.decorators,
    );
    v["parent_class"] = serde_json::json!(&f.parent_class);
    v["parameters"] = serde_json::json!(&f.parameters);
    v["return_type"] = serde_json::json!(&f.return_type);
    v["fn_kind"] = serde_json::json!(&f.kind);
    v["is_async"] = serde_json::json!(&f.is_async);
    v["is_generator"] = serde_json::json!(&f.is_generator);
    v["source"] = serde_json::json!(&f.source);
    v["signature_hash"] = serde_json::json!(format!("{:x}", f.signature_hash));
    v["body_hash"] = serde_json::json!(format!("{:x}", f.body_hash));
    v["metrics"] = serde_json::json!(&f.metrics);
    v["is_type_checking_only"] = serde_json::json!(&f.is_type_checking_only);
    v["parse_quality"] = serde_json::json!(&f.parse_quality.to_json());
    v["content_hash"] = serde_json::json!(format!("{:x}", f.content_hash));
    v["span"] = serde_json::json!(&f.span);
    v["name_span"] = serde_json::json!(&f.name_span);
    v["params_span"] = serde_json::json!(&f.params_span);
    v["body_span"] = serde_json::json!(&f.body_span);
    v["decorators_span"] = serde_json::json!(&f.decorators_span);
    // Post-resolution state: the ONLY call data the ledger keeps. Cold
    // start rebuilds the call indices from this (see cold_start).
    v["resolved_calls"] = serde_json::json!(&f.resolved_calls);
    // §2.3 step 2: raw call sites ride alongside, parallel to
    // `resolved_calls` (same order). Cold start rehydrates `Function.calls`
    // from this so arg spans survive reload (unlike `call_evidence`).
    // Omitted when empty to keep the ledger small; missing key reads back
    // as empty (pre-§2.3 stores degrade to the unverified fallback).
    if !f.calls.is_empty() {
        let sites: Vec<CallSiteSpan> = f
            .calls
            .iter()
            .map(|c| CallSiteSpan {
                name: c.name.clone(),
                path: c.path.clone(),
                line: c.line,
                col: c.col,
                name_span: c.name_span,
                args_span: c.args_span,
            })
            .collect();
        v["call_sites"] = serde_json::json!(&sites);
    }
    // Callback and template-method liveness, kept for the same reason.
    if !f.resolved_refs.is_empty() {
        v["resolved_refs"] = serde_json::json!(&f.resolved_refs);
    }
    // DR-11: the dedup key rides the ledger (vectors persist separately).
    // Cold start restores it so warm dedup skips without vectors loaded.
    // Omitted when empty; missing key reads back as empty.
    if !f.embedding.hash.is_empty() {
        v["embedding_hash"] = serde_json::json!(&f.embedding.hash);
    }
    v
}

/// Stable content hash for v2 writers whose entity carries no source-derived
/// hash (imports, constants, type aliases). xxh3 over the canonical JSON —
/// deterministic across runs (unlike SipHash with process-random keys), so
/// the write gate in `filter_unchanged` can skip them when nothing changed.
/// Computed before the key is inserted (no circularity).
fn json_content_hash(v: &serde_json::Value) -> String {
    format!("{:x}", xxhash_rust::xxh3::xxh3_64(v.to_string().as_bytes()))
}

fn import_content(i: &Import) -> serde_json::Value {
    let file_path = file_path_of(&i.id);
    // v2 rule: every entity carries an explicit `name`; imports use their
    // raw source text (that is also what import_to_dict surfaces).
    let mut v = v2_common("import", &i.raw, file_path, i.line as u64, 0, &None, &[]);
    v["raw"] = serde_json::json!(&i.raw);
    v["import_kind"] = serde_json::json!(i.kind.to_json());
    v["is_type_only"] = serde_json::json!(&i.is_type_only);
    v["name_span"] = serde_json::json!(&i.name_span);
    v[annotation::CONTENT_HASH] = serde_json::json!(json_content_hash(&v));
    // DR-11: the dedup key rides the ledger (vectors persist separately).
    // AFTER the gate hash: the gate covers source facts, and the hash is
    // derived state flushed by its own path — so a hash-only difference
    // never rewrites, and a hash-less fresh walk never wipes a stored hash.
    if !i.embedding.hash.is_empty() {
        v["embedding_hash"] = serde_json::json!(&i.embedding.hash);
    }
    v
}

fn constant_content(c: &Constant) -> serde_json::Value {
    let file_path = file_path_of(&c.id);
    let mut v = v2_common("constant", &c.name, file_path, 0, 0, &None, &[]);
    v["annotation"] = serde_json::json!(&c.annotation);
    v["source"] = serde_json::json!(&c.source);
    v["default_value"] = serde_json::json!(&c.default_value);
    v["span"] = serde_json::json!(&c.span);
    v["name_span"] = serde_json::json!(&c.name_span);
    v[annotation::CONTENT_HASH] = serde_json::json!(json_content_hash(&v));
    // DR-11: the dedup key rides the ledger — after the gate hash, as
    // above: derived state must not move the source-facts gate.
    if !c.embedding.hash.is_empty() {
        v["embedding_hash"] = serde_json::json!(&c.embedding.hash);
    }
    v
}

fn type_alias_content(t: &TypeAlias) -> serde_json::Value {
    let file_path = file_path_of(&t.id);
    let mut v = v2_common("type_alias", &t.name, file_path, 0, 0, &None, &[]);
    v["target"] = serde_json::json!(&t.target);
    v["source"] = serde_json::json!(&t.source);
    v["span"] = serde_json::json!(&t.span);
    v["name_span"] = serde_json::json!(&t.name_span);
    v[annotation::CONTENT_HASH] = serde_json::json!(json_content_hash(&v));
    // DR-11: the dedup key rides the ledger — after the gate hash, as
    // above: derived state must not move the source-facts gate.
    if !t.embedding.hash.is_empty() {
        v["embedding_hash"] = serde_json::json!(&t.embedding.hash);
    }
    v
}

/// File path → source_blob digest for the module concepts of this flush.
/// Built by the caller from file bytes BEFORE any concept is asserted
/// (put-then-assert ordering); consulted only for modules — every other
/// kind passes `None` and keeps digest-less extras.
pub type SourceBlobDigests = HashMap<String, String>;

/// Module id (`{file}::module`) → digest lookup in a [`SourceBlobDigests`]
/// map keyed by file path.
fn digest_for_module(module_id: &str, digests: &SourceBlobDigests) -> Option<String> {
    let file = module_id.strip_suffix("::module")?;
    digests.get(file).cloned()
}

/// Build concept JSON v2 [`ConceptUpsert`]s for the WHOLE projection.
///
/// Call this on the FINAL projection (post-cascade), BEFORE
/// `persist_edges`, because edges reference concept ids and the concept
/// upsert must land first.
pub fn build_v2_concepts_all(
    projection: &ProjectedGraph,
    digests: &SourceBlobDigests,
) -> Vec<ConceptUpsert> {
    build_v2_concepts_all_at(projection, digests, &now_iso8601())
}

/// Stamped twin (P5a.2, DR-41): the same post-cascade flush at an explicit
/// `valid_from` — commit time on the P5 path. `now` below is the flush
/// stamp, not the clock: wall-clock by default, commit time when told.
pub fn build_v2_concepts_all_at(
    projection: &ProjectedGraph,
    digests: &SourceBlobDigests,
    valid_from: &str,
) -> Vec<ConceptUpsert> {
    let now = valid_from.to_string();
    let mut out: Vec<ConceptUpsert> = Vec::new();
    for m in projection.modules.values() {
        let digest = digest_for_module(&m.id, digests);
        out.push(v2_upsert(
            &m.id,
            &m.name,
            module_content(m),
            &now,
            digest.as_deref(),
        ));
    }
    for c in projection.classes.values() {
        out.push(v2_upsert(&c.id, &c.name, class_content(c), &now, None));
    }
    for f in projection.functions.values() {
        out.push(v2_upsert(&f.id, &f.name, function_content(f), &now, None));
    }
    for i in projection.imports.values() {
        out.push(v2_upsert(&i.id, &i.raw, import_content(i), &now, None));
    }
    for k in projection.constants.values() {
        out.push(v2_upsert(&k.id, &k.name, constant_content(k), &now, None));
    }
    for t in projection.type_aliases.values() {
        out.push(v2_upsert(&t.id, &t.name, type_alias_content(t), &now, None));
    }
    out
}

/// Build concept JSON v2 upserts for the entities of ONE file (id prefix
/// `{file_path}::`). Used by `update_file` after its scoped cascade.
pub fn build_v2_concepts_for_file(
    projection: &ProjectedGraph,
    file_path: &str,
    digests: &SourceBlobDigests,
) -> Vec<ConceptUpsert> {
    build_v2_concepts_for_file_at(projection, file_path, digests, &now_iso8601())
}

/// Stamped twin (P5a.2, DR-41): the same single-file flush at an explicit
/// `valid_from` — commit time on the P5 path. `now` below is the flush
/// stamp, not the clock.
pub fn build_v2_concepts_for_file_at(
    projection: &ProjectedGraph,
    file_path: &str,
    digests: &SourceBlobDigests,
    valid_from: &str,
) -> Vec<ConceptUpsert> {
    let now = valid_from.to_string();
    let prefix = format!("{file_path}::");
    let mut out: Vec<ConceptUpsert> = Vec::new();
    for m in projection.modules.values() {
        if m.id.starts_with(&prefix) {
            let digest = digest_for_module(&m.id, digests);
            out.push(v2_upsert(
                &m.id,
                &m.name,
                module_content(m),
                &now,
                digest.as_deref(),
            ));
        }
    }
    for c in projection.classes.values() {
        if c.id.starts_with(&prefix) {
            out.push(v2_upsert(&c.id, &c.name, class_content(c), &now, None));
        }
    }
    for f in projection.functions.values() {
        if f.id.starts_with(&prefix) {
            out.push(v2_upsert(&f.id, &f.name, function_content(f), &now, None));
        }
    }
    for i in projection.imports.values() {
        if i.id.starts_with(&prefix) {
            out.push(v2_upsert(&i.id, &i.raw, import_content(i), &now, None));
        }
    }
    for k in projection.constants.values() {
        if k.id.starts_with(&prefix) {
            out.push(v2_upsert(&k.id, &k.name, constant_content(k), &now, None));
        }
    }
    for t in projection.type_aliases.values() {
        if t.id.starts_with(&prefix) {
            out.push(v2_upsert(&t.id, &t.name, type_alias_content(t), &now, None));
        }
    }
    out
}
/// A parsed v2 concept, typed by canonical kind.
#[derive(Debug)]
pub enum V2Entity {
    Module(Module),
    Class(Class),
    Function(Function),
    Import(Import),
    Constant(Constant),
    TypeAlias(TypeAlias),
    Route(Route),
}

/// How a cold-start pre-scan should treat one concept's content.
#[derive(Debug)]
pub enum V2ConceptClass {
    /// Canonical v2 entity concept; `kind` is one of [`V2_CANONICAL_KINDS`].
    Canonical(String),
    /// Canonical kind but no `meta_version: 2` — a v1 leftover that must
    /// hard-fail the load (the store needs a re-analyze to upgrade).
    V1Leftover,
    /// v1 standalone `field` concept. v2 never materializes standalone
    /// Field entities (fields ride inside their Class), so these are
    /// skipped with a warning.
    StandaloneField,
    /// P3 content-version spine concept (`kind = 'content_version'`),
    /// P4 branch-content pointer (`kind = 'branch_content'`), and P5
    /// commit concept (`kind = 'commit'`).
    /// Ledger infrastructure, not entities: timeless,
    /// digest-addressed, read through `history_of` / the branch read
    /// path / the ingest report. Skipped by entity loaders exactly like
    /// standalone fields — matching them before the canonical check is
    /// what keeps them out of `V1Leftover` (which hard-fails the load)
    /// and out of `parse_v2_concept` (which has no arm for any of them).
    Spine,
    /// Content that is not JSON or has no kind.
    Unreadable,
}

/// Pre-scan classification for cold start (cheap: parses content once).
pub fn classify_v2_concept(content: &str) -> V2ConceptClass {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
        return V2ConceptClass::Unreadable;
    };
    let Some(kind) = v.get("kind").and_then(|k| k.as_str()) else {
        return V2ConceptClass::Unreadable;
    };
    if kind == "field" {
        return V2ConceptClass::StandaloneField;
    }
    if kind == "content_version"
        || kind == edge_type::BRANCH_CONTENT_KIND
        || kind == edge_type::COMMIT_KIND
    {
        return V2ConceptClass::Spine;
    }
    if V2_CANONICAL_KINDS.contains(&kind)
        && v.get("meta_version").and_then(|m| m.as_u64()) == Some(V2_META_VERSION)
    {
        return V2ConceptClass::Canonical(kind.to_string());
    }
    V2ConceptClass::V1Leftover
}

fn v2_err(id: &str, msg: &str) -> String {
    format!("concept {id}: {msg}")
}

fn req_str(v: &serde_json::Value, key: &str, id: &str) -> std::result::Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| v2_err(id, &format!("missing '{key}'")))
}

fn opt_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn req_bool(v: &serde_json::Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}

fn req_u64(v: &serde_json::Value, key: &str, id: &str) -> std::result::Result<u64, String> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| v2_err(id, &format!("missing '{key}'")))
}

fn hex_u64(v: &serde_json::Value, key: &str, id: &str) -> std::result::Result<u64, String> {
    let s = v
        .get(key)
        .and_then(|x| x.as_str())
        .ok_or_else(|| v2_err(id, &format!("missing '{key}'")))?;
    u64::from_str_radix(s, 16).map_err(|e| v2_err(id, &format!("'{key}' is not hex: {e}")))
}

fn str_list(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn span_of(v: &serde_json::Value, key: &str) -> ByteSpan {
    match v.get(key) {
        Some(obj) if obj.is_object() => ByteSpan {
            start: obj.get("start").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            end: obj.get("end").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
        },
        _ => ByteSpan { start: 0, end: 0 },
    }
}

fn span_opt_of(v: &serde_json::Value, key: &str) -> Option<ByteSpan> {
    match v.get(key) {
        Some(obj) if obj.is_object() => Some(span_of(v, key)),
        _ => None,
    }
}

fn deser<T: serde::de::DeserializeOwned>(v: &serde_json::Value, key: &str) -> Option<T> {
    v.get(key)
        .and_then(|x| serde_json::from_value::<T>(x.clone()).ok())
}

fn deser_or_default<T: serde::de::DeserializeOwned + Default>(
    v: &serde_json::Value,
    key: &str,
) -> T {
    deser(v, key).unwrap_or_default()
}

fn source_of(v: &serde_json::Value, id: &str) -> std::result::Result<SourceType, String> {
    Ok(v.get("source")
        .and_then(|s| s.as_str())
        .map(|s| SourceType::from_json(s).map_err(|e| v2_err(id, &e)))
        .transpose()?
        .unwrap_or(SourceType::Impl))
}

fn parse_v2_module(id: &str, v: &serde_json::Value) -> std::result::Result<Module, String> {
    let file_path = req_str(v, "file_path", id)?;
    let language = req_str(v, "language", id)?;
    let parse_quality = req_str(v, "parse_quality", id)?;
    Ok(Module {
        id: id.to_string(),
        name: req_str(v, "name", id)?,
        path: std::path::PathBuf::from(&file_path),
        language: Language::from_json(&language).map_err(|e| v2_err(id, &e))?,
        package: None,
        exports: vec![],
        star_exports: None,
        uses: deser_or_default(v, "uses"),
        resolved_uses: str_list(v, "resolved_uses"),
        attr_reads: str_list(v, "attr_reads"),
        classes: str_list(v, "classes"),
        functions: str_list(v, "functions"),
        imports: str_list(v, "imports"),
        constants: str_list(v, "constants"),
        type_aliases: str_list(v, "type_aliases"),
        parse_quality: ParseQuality::from_json(&parse_quality).map_err(|e| v2_err(id, &e))?,
        file_version: v.get("file_version").and_then(|x| x.as_u64()).unwrap_or(1),
        content_hash: hex_u64(v, "content_hash", id)?,
        embedding: EmbeddingVec {
            vec: Vec::new(),
            hash: opt_str(v, "embedding_hash").unwrap_or_default(),
        },
    })
}

fn parse_v2_class(id: &str, v: &serde_json::Value) -> std::result::Result<Class, String> {
    let file_path = req_str(v, "file_path", id)?;
    let parse_quality = req_str(v, "parse_quality", id)?;
    Ok(Class {
        id: id.to_string(),
        name: req_str(v, "name", id)?,
        grammar_kind: v
            .get("grammar_kind")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        parent_module: format!("{file_path}::module"),
        parent_class: opt_str(v, "parent_class"),
        bases: deser_or_default(v, "bases"),
        // Cascade-rebuilt, never persisted:
        resolved_bases: vec![],
        resolved_interfaces: vec![],
        resolved_decorators: vec![],
        mro: vec![],
        mro_error: false,
        methods: vec![],
        fields: deser_or_default(v, "fields"),
        source: source_of(v, id)?,
        decorators: str_list(v, "decorators"),
        // Cascade-recomputed (dataclass/protocol/enum detection):
        effective: EffectiveClass::Plain,
        is_type_checking_only: req_bool(v, "is_type_checking_only"),
        line: req_u64(v, "line", id)? as usize,
        exit_line: req_u64(v, "exit_line", id)? as usize,
        docstring: opt_str(v, "docstring"),
        parse_quality: ParseQuality::from_json(&parse_quality).map_err(|e| v2_err(id, &e))?,
        content_hash: hex_u64(v, "content_hash", id)?,
        span: span_of(v, "span"),
        name_span: span_of(v, "name_span"),
        body_span: span_of(v, "body_span"),
        decorators_span: span_opt_of(v, "decorators_span"),
        embedding: EmbeddingVec {
            vec: Vec::new(),
            hash: opt_str(v, "embedding_hash").unwrap_or_default(),
        },
    })
}

fn parse_v2_function(id: &str, v: &serde_json::Value) -> std::result::Result<Function, String> {
    let file_path = req_str(v, "file_path", id)?;
    let parse_quality = req_str(v, "parse_quality", id)?;
    let kind =
        deser::<FunctionKind>(v, "fn_kind").ok_or_else(|| v2_err(id, "missing 'fn_kind'"))?;
    Ok(Function {
        id: id.to_string(),
        name: req_str(v, "name", id)?,
        parent_module: format!("{file_path}::module"),
        parent_class: opt_str(v, "parent_class"),
        parameters: deser_or_default(v, "parameters"),
        return_type: opt_str(v, "return_type"),
        // §2.3 step 2: raw call sites now ride the ledger as `call_sites`
        // (parallel to `resolved_calls`). Missing key = pre-§2.3 store →
        // empty, preserving the old unverified-fallback behavior.
        calls: deser_or_default::<Vec<CallSiteSpan>>(v, "call_sites")
            .into_iter()
            .map(|s| UnresolvedRef {
                name: s.name,
                path: s.path,
                line: s.line,
                col: s.col,
                name_span: s.name_span,
                args_span: s.args_span,
            })
            .collect(),
        resolved_calls: deser_or_default(v, "resolved_calls"),
        decorators: str_list(v, "decorators"),
        setter_of: None,
        bindings: Vec::new(),
        refs: Vec::new(),
        resolved_refs: str_list(v, "resolved_refs"),
        resolved_decorators: vec![],
        line: req_u64(v, "line", id)? as usize,
        exit_line: req_u64(v, "exit_line", id)? as usize,
        docstring: opt_str(v, "docstring"),
        kind,
        is_async: req_bool(v, "is_async"),
        is_generator: req_bool(v, "is_generator"),
        source: source_of(v, id)?,
        signature_hash: hex_u64(v, "signature_hash", id)?,
        body_hash: hex_u64(v, "body_hash", id)?,
        metrics: deser_or_default(v, "metrics"),
        is_type_checking_only: req_bool(v, "is_type_checking_only"),
        parse_quality: ParseQuality::from_json(&parse_quality).map_err(|e| v2_err(id, &e))?,
        content_hash: hex_u64(v, "content_hash", id)?,
        span: span_of(v, "span"),
        name_span: span_of(v, "name_span"),
        params_span: span_of(v, "params_span"),
        body_span: span_of(v, "body_span"),
        decorators_span: span_opt_of(v, "decorators_span"),
        embedding: EmbeddingVec {
            vec: Vec::new(),
            hash: opt_str(v, "embedding_hash").unwrap_or_default(),
        },
    })
}

fn parse_v2_import(id: &str, v: &serde_json::Value) -> std::result::Result<Import, String> {
    let kind_v = v
        .get("import_kind")
        .ok_or_else(|| v2_err(id, "missing 'import_kind'"))?;
    let kind = ImportKind::from_json(kind_v).map_err(|e| v2_err(id, &e))?;
    Ok(Import {
        id: id.to_string(),
        raw: req_str(v, "raw", id)?,
        kind,
        // Re-resolved by the load cascade:
        resolution: ImportResolution::Unresolved,
        line: req_u64(v, "line", id)? as usize,
        is_type_only: req_bool(v, "is_type_only"),
        name_span: span_of(v, "name_span"),
        embedding: EmbeddingVec {
            vec: Vec::new(),
            hash: opt_str(v, "embedding_hash").unwrap_or_default(),
        },
    })
}

fn parse_v2_constant(id: &str, v: &serde_json::Value) -> std::result::Result<Constant, String> {
    Ok(Constant {
        id: id.to_string(),
        name: req_str(v, "name", id)?,
        annotation: opt_str(v, "annotation"),
        source: source_of(v, id)?,
        default_value: opt_str(v, "default_value"),
        span: span_of(v, "span"),
        name_span: span_of(v, "name_span"),
        embedding: EmbeddingVec {
            vec: Vec::new(),
            hash: opt_str(v, "embedding_hash").unwrap_or_default(),
        },
    })
}

fn parse_v2_type_alias(id: &str, v: &serde_json::Value) -> std::result::Result<TypeAlias, String> {
    Ok(TypeAlias {
        id: id.to_string(),
        name: req_str(v, "name", id)?,
        target: req_str(v, "target", id)?,
        source: source_of(v, id)?,
        span: span_of(v, "span"),
        name_span: span_of(v, "name_span"),
        embedding: EmbeddingVec {
            vec: Vec::new(),
            hash: opt_str(v, "embedding_hash").unwrap_or_default(),
        },
    })
}

/// Parse a v2 route concept (§1.3). `name` is the URL pattern; the handler
/// and methods ride as plain fields (no spans — resolvers record files,
/// not offsets).
fn parse_v2_route(id: &str, v: &serde_json::Value) -> std::result::Result<Route, String> {
    let methods = v
        .get("methods")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Route {
        id: id.to_string(),
        pattern: req_str(v, "name", id)?,
        file_path: req_str(v, "file_path", id)?,
        handler_id: req_str(v, "handler", id)?,
        methods,
        framework: opt_str(v, "framework").unwrap_or_default(),
    })
}

/// Build the v2 content JSON for a framework route (§1.3). Mirrors
/// `v2_common` plus the route fields `parse_v2_route` reads back.
pub fn route_content(
    pattern: &str,
    file_path: &str,
    handler: &str,
    methods: &[String],
    framework: &str,
) -> serde_json::Value {
    let mut v = v2_common("route", pattern, file_path, 0, 0, &None, &[]);
    v["handler"] = serde_json::json!(handler);
    v["methods"] = serde_json::json!(methods);
    v["framework"] = serde_json::json!(framework);
    v
}

/// Parse one v2 concept's content into its typed entity.
///
/// Hard-fails (no silent v1 fallback) when a canonical concept lacks
/// `meta_version: 2`.
pub fn parse_v2_concept(id: &str, content: &str) -> std::result::Result<V2Entity, String> {
    let v: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| v2_err(id, &format!("content is not JSON: {e}")))?;
    if v.get("meta_version").and_then(|x| x.as_u64()) != Some(V2_META_VERSION) {
        return Err(format!(
            "concept {id} lacks meta_version: 2 — store predates concept-JSON v2; re-analyze to upgrade it"
        ));
    }
    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or_else(|| v2_err(id, "missing 'kind'"))?;
    match kind {
        "module" => Ok(V2Entity::Module(parse_v2_module(id, &v)?)),
        "class" => Ok(V2Entity::Class(parse_v2_class(id, &v)?)),
        "function" => Ok(V2Entity::Function(parse_v2_function(id, &v)?)),
        "import" => Ok(V2Entity::Import(parse_v2_import(id, &v)?)),
        "constant" => Ok(V2Entity::Constant(parse_v2_constant(id, &v)?)),
        "type_alias" => Ok(V2Entity::TypeAlias(parse_v2_type_alias(id, &v)?)),
        "route" => Ok(V2Entity::Route(parse_v2_route(id, &v)?)),
        other => Err(v2_err(id, &format!("unknown kind '{other}'"))),
    }
}

#[cfg(test)]
mod concept_v2_tests {
    use super::*;

    fn sample_module() -> Module {
        Module {
            id: "pkg/alpha.py::module".to_string(),
            name: "alpha".to_string(),
            path: std::path::PathBuf::from("pkg/alpha.py"),
            language: Language::Python,
            package: None,
            exports: vec![],
            star_exports: None,
            uses: Vec::new(),
            resolved_uses: Vec::new(),
            attr_reads: Vec::new(),
            classes: vec!["pkg/alpha.py::Alpha".into()],
            functions: vec!["pkg/alpha.py::helper".into()],
            imports: vec!["pkg/alpha.py::import os".into()],
            constants: vec![],
            type_aliases: vec!["pkg/alpha.py::Alias".into()],
            parse_quality: ParseQuality::Clean,
            file_version: 1,
            content_hash: 0xDEAD_BEEF,
            embedding: EmbeddingVec::default(),
        }
    }

    fn sample_class() -> Class {
        Class {
            id: "pkg/alpha.py::Alpha".to_string(),
            name: "Alpha".to_string(),
            grammar_kind: "class_definition".to_string(),
            parent_module: "pkg/alpha.py::module".to_string(),
            parent_class: None,
            bases: vec![UnresolvedRef {
                name: "Base".into(),
                path: vec![],
                line: 2,
                col: 0,
                name_span: Default::default(),
                args_span: None,
            }],
            resolved_bases: vec![],
            resolved_interfaces: vec![],
            resolved_decorators: vec![],
            mro: vec![],
            mro_error: false,
            methods: vec![],
            fields: vec![Field {
                name: "x".into(),
                annotation: Some("int".into()),
                source: SourceType::Impl,
                default_value: Some("0".into()),
                is_class_var: false,
                span: ByteSpan { start: 40, end: 48 },
                name_span: ByteSpan { start: 40, end: 41 },
            }],
            source: SourceType::Impl,
            decorators: vec!["dataclass".into()],
            effective: EffectiveClass::Plain,
            is_type_checking_only: false,
            line: 2,
            exit_line: 10,
            docstring: Some("Doc.".into()),
            parse_quality: ParseQuality::Clean,
            content_hash: 0x1234,
            span: ByteSpan {
                start: 30,
                end: 200,
            },
            name_span: ByteSpan { start: 36, end: 41 },
            body_span: ByteSpan {
                start: 42,
                end: 199,
            },
            decorators_span: Some(ByteSpan { start: 20, end: 30 }),
            embedding: EmbeddingVec::default(),
        }
    }

    fn sample_function() -> Function {
        Function {
            id: "pkg/alpha.py::helper".to_string(),
            name: "helper".to_string(),
            parent_module: "pkg/alpha.py::module".to_string(),
            parent_class: None,
            parameters: vec![Parameter {
                name: "a".into(),
                annotation: Some("int".into()),
                default_value: None,
                is_varargs: false,
                is_kwargs: false,
                is_positional_only: false,
                is_keyword_only: false,
            }],
            return_type: Some("str".into()),
            calls: vec![],
            resolved_calls: vec![
                ResolvedCall::Function("pkg/alpha.py::other".into()),
                ResolvedCall::Method {
                    receiver: ReceiverShape::SelfRef,
                    method: "pkg/alpha.py::Alpha.m".into(),
                },
                ResolvedCall::Builtin("len".into()),
                ResolvedCall::External("requests.get".into()),
                ResolvedCall::Unresolved {
                    reason: UnresolvedReason::TypeInferenceRequired,
                    raw: UnresolvedRef {
                        name: "mystery".into(),
                        path: vec![],
                        line: 5,
                        col: 8,
                        name_span: Default::default(),
                        args_span: None,
                    },
                },
            ],
            decorators: vec![],
            setter_of: None,
            bindings: Vec::new(),
            refs: Vec::new(),
            resolved_refs: Vec::new(),
            resolved_decorators: vec![],
            line: 12,
            exit_line: 20,
            docstring: Some("H.".into()),
            kind: FunctionKind::Free,
            is_async: false,
            is_generator: true,
            source: SourceType::Impl,
            signature_hash: 0xABCD,
            body_hash: 0x99,
            metrics: FunctionMetrics {
                cyclomatic: 3,
                nesting_depth: 2,
                return_count: 1,
            },
            is_type_checking_only: false,
            parse_quality: ParseQuality::Partial,
            content_hash: 0x77,
            span: ByteSpan {
                start: 100,
                end: 250,
            },
            name_span: ByteSpan {
                start: 112,
                end: 118,
            },
            params_span: ByteSpan {
                start: 119,
                end: 123,
            },
            body_span: ByteSpan {
                start: 124,
                end: 249,
            },
            decorators_span: None,
            embedding: EmbeddingVec::default(),
        }
    }

    fn sample_import() -> Import {
        Import {
            id: "pkg/alpha.py::from pkg.beta import Base as B, other".to_string(),
            raw: "from pkg.beta import Base as B, other".to_string(),
            kind: ImportKind::FromImport {
                module: "pkg.beta".into(),
                names: vec![("Base".into(), Some("B".into())), ("other".into(), None)],
            },
            resolution: ImportResolution::Unresolved,
            line: 1,
            is_type_only: false,
            name_span: ByteSpan { start: 20, end: 50 },
            embedding: EmbeddingVec::default(),
        }
    }

    #[test]
    fn route_roundtrip() {
        // §1.3: route concepts survive the ledger JSON round-trip.
        let json = route_content(
            "/users/<id>",
            "app.py",
            "app.py::get_user",
            &["GET".to_string()],
            "flask",
        )
        .to_string();
        let id = "app.py::flask:route:/users/<id>";
        match parse_v2_concept(id, &json).unwrap() {
            V2Entity::Route(got) => {
                assert_eq!(got.id, id);
                assert_eq!(got.pattern, "/users/<id>");
                assert_eq!(got.file_path, "app.py");
                assert_eq!(got.handler_id, "app.py::get_user");
                assert_eq!(got.methods, vec!["GET".to_string()]);
                assert_eq!(got.framework, "flask");
            }
            other => panic!("expected Route, got {other:?}"),
        }
        // The new kind classifies canonical, never a v1 leftover.
        assert!(matches!(
            classify_v2_concept(&json),
            V2ConceptClass::Canonical(_)
        ));
    }

    #[test]
    fn module_roundtrip() {
        let mut m = sample_module();
        m.resolved_uses = vec!["pkg/alpha.py::helper".into()];
        m.attr_reads = vec!["enabled".into(), "name".into()];
        let json = module_content(&m).to_string();
        match parse_v2_concept(&m.id, &json).unwrap() {
            V2Entity::Module(got) => {
                assert_eq!(got.id, m.id);
                assert_eq!(got.name, m.name);
                assert_eq!(got.path, m.path);
                assert_eq!(got.language, m.language);
                assert_eq!(got.parse_quality, m.parse_quality);
                assert_eq!(got.file_version, m.file_version);
                assert_eq!(got.content_hash, m.content_hash);
                assert_eq!(got.classes, m.classes);
                assert_eq!(got.functions, m.functions);
                assert_eq!(got.imports, m.imports);
                assert_eq!(got.constants, m.constants);
                assert_eq!(got.type_aliases, m.type_aliases);
                // Module-scope liveness survives a cold start.
                assert_eq!(got.resolved_uses, m.resolved_uses);
                assert_eq!(got.attr_reads, m.attr_reads);
            }
            other => panic!("expected Module, got {other:?}"),
        }
    }

    #[test]
    fn class_roundtrip() {
        let c = sample_class();
        let json = class_content(&c).to_string();
        match parse_v2_concept(&c.id, &json).unwrap() {
            V2Entity::Class(got) => {
                assert_eq!(got.name, c.name);
                assert_eq!(got.grammar_kind, c.grammar_kind);
                assert_eq!(got.parent_module, c.parent_module);
                assert_eq!(got.bases, c.bases);
                assert_eq!(got.fields, c.fields);
                assert_eq!(got.source, c.source);
                assert_eq!(got.decorators, c.decorators);
                assert_eq!(got.is_type_checking_only, c.is_type_checking_only);
                assert_eq!(got.line, c.line);
                assert_eq!(got.exit_line, c.exit_line);
                assert_eq!(got.docstring, c.docstring);
                assert_eq!(got.parse_quality, c.parse_quality);
                assert_eq!(got.content_hash, c.content_hash);
                assert_eq!(got.span, c.span);
                assert_eq!(got.name_span, c.name_span);
                assert_eq!(got.body_span, c.body_span);
                assert_eq!(got.decorators_span, c.decorators_span);
                // cascade-rebuilt state stays empty on load:
                assert!(got.resolved_bases.is_empty());
                assert!(got.mro.is_empty());
                assert!(got.methods.is_empty());
                assert!(matches!(got.effective, EffectiveClass::Plain));
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    #[test]
    fn function_roundtrip_preserves_resolved_calls() {
        let mut f = sample_function();
        f.resolved_refs = vec!["pkg/alpha.py::callback".into()];
        let json = function_content(&f).to_string();
        match parse_v2_concept(&f.id, &json).unwrap() {
            V2Entity::Function(got) => {
                assert_eq!(got.name, f.name);
                assert_eq!(got.parent_module, f.parent_module);
                assert_eq!(got.parameters, f.parameters);
                assert_eq!(got.return_type, f.return_type);
                assert_eq!(got.kind, f.kind);
                assert_eq!(got.resolved_calls, f.resolved_calls);
                assert_eq!(got.calls, f.calls, "raw call sites now persist (§2.3)");
                assert_eq!(got.resolved_refs, f.resolved_refs);
                assert_eq!(got.is_async, f.is_async);
                assert_eq!(got.is_generator, f.is_generator);
                assert_eq!(got.source, f.source);
                assert_eq!(got.signature_hash, f.signature_hash);
                assert_eq!(got.body_hash, f.body_hash);
                assert_eq!(got.metrics, f.metrics);
                assert_eq!(got.parse_quality, f.parse_quality);
                assert_eq!(got.content_hash, f.content_hash);
                assert_eq!(got.span, f.span);
                assert_eq!(got.params_span, f.params_span);
                assert_eq!(got.body_span, f.body_span);
                assert_eq!(got.decorators_span, f.decorators_span);
            }
            other => panic!("expected Function, got {other:?}"),
        }
    }

    /// §2.3 step 2: arg spans survive the ledger round-trip, and a
    /// pre-§2.3 concept without `call_sites` still parses to empty calls.
    #[test]
    fn call_sites_roundtrip_preserves_arg_spans() {
        let mut f = sample_function();
        f.calls = vec![UnresolvedRef {
            name: "greet".into(),
            path: vec![],
            line: 3,
            col: 11,
            name_span: ByteSpan { start: 20, end: 25 },
            args_span: Some(ByteSpan { start: 25, end: 33 }),
        }];
        let json = function_content(&f).to_string();
        assert!(json.contains("call_sites"), "non-empty calls persist");
        match parse_v2_concept(&f.id, &json).unwrap() {
            V2Entity::Function(got) => {
                assert_eq!(got.calls, f.calls);
                assert_eq!(
                    got.calls[0].args_span,
                    Some(ByteSpan { start: 25, end: 33 })
                );
            }
            other => panic!("expected Function, got {other:?}"),
        }
        // Missing key (pre-§2.3 store) → empty, never an error.
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("call_sites");
        match parse_v2_concept(&f.id, &v.to_string()).unwrap() {
            V2Entity::Function(got) => assert!(got.calls.is_empty()),
            other => panic!("expected Function, got {other:?}"),
        }
    }

    /// DR-11 slice 1: the dedup key rides the ledger; vectors do not.
    /// Missing key (pre-DR-11 store) reads back as empty.
    #[test]
    fn embedding_hash_roundtrips_without_vectors() {
        let mut f = sample_function();
        f.embedding.hash = "bge#pp1#abc123".to_string();
        f.embedding.vec = vec![0.1, 0.2];
        let json = function_content(&f).to_string();
        assert!(
            !json.contains("\"vec\""),
            "vectors must not ride the ledger"
        );
        match parse_v2_concept(&f.id, &json).unwrap() {
            V2Entity::Function(got) => {
                assert_eq!(got.embedding.hash, "bge#pp1#abc123");
                assert!(
                    got.embedding.vec.is_empty(),
                    "vectors rehydrate from the model table, not the ledger"
                );
            }
            other => panic!("expected Function, got {other:?}"),
        }
        // Untouched fixture: no key written, empty back.
        let plain = function_content(&sample_function()).to_string();
        assert!(!plain.contains("embedding_hash"));
        match parse_v2_concept(&sample_function().id, &plain).unwrap() {
            V2Entity::Function(got) => assert!(got.embedding.hash.is_empty()),
            other => panic!("expected Function, got {other:?}"),
        }
    }

    #[test]
    fn import_roundtrip_keeps_kind_structure() {
        let i = sample_import();
        let json = import_content(&i).to_string();
        match parse_v2_concept(&i.id, &json).unwrap() {
            V2Entity::Import(got) => {
                assert_eq!(got.raw, i.raw);
                assert_eq!(got.kind, i.kind);
                assert_eq!(got.line, i.line);
                assert_eq!(got.is_type_only, i.is_type_only);
                assert_eq!(got.name_span, i.name_span);
            }
            other => panic!("expected Import, got {other:?}"),
        }
    }

    #[test]
    fn constant_and_type_alias_roundtrip() {
        let k = Constant {
            id: "pkg/alpha.py::MAX".into(),
            name: "MAX".into(),
            annotation: Some("int".into()),
            source: SourceType::Impl,
            default_value: Some("100".into()),
            span: ByteSpan { start: 5, end: 15 },
            name_span: ByteSpan { start: 5, end: 8 },
            embedding: EmbeddingVec::default(),
        };
        let json = constant_content(&k).to_string();
        match parse_v2_concept(&k.id, &json).unwrap() {
            V2Entity::Constant(got) => {
                assert_eq!(got.name, k.name);
                assert_eq!(got.annotation, k.annotation);
                assert_eq!(got.source, k.source);
                assert_eq!(got.default_value, k.default_value);
                assert_eq!(got.span, k.span);
            }
            other => panic!("expected Constant, got {other:?}"),
        }

        let t = TypeAlias {
            id: "pkg/alpha.py::Alias".into(),
            name: "Alias".into(),
            target: "dict[str, int]".into(),
            source: SourceType::Stub,
            span: ByteSpan { start: 20, end: 40 },
            name_span: ByteSpan { start: 20, end: 25 },
            embedding: EmbeddingVec::default(),
        };
        let json = type_alias_content(&t).to_string();
        match parse_v2_concept(&t.id, &json).unwrap() {
            V2Entity::TypeAlias(got) => {
                assert_eq!(got.name, t.name);
                assert_eq!(got.target, t.target);
                assert_eq!(got.source, t.source);
            }
            other => panic!("expected TypeAlias, got {other:?}"),
        }
    }

    #[test]
    fn v1_concept_is_hard_rejected() {
        // v1 module JSON: kind present, no meta_version.
        let v1 = r#"{"kind": "module", "name": "alpha", "file_path": "pkg/alpha.py", "language": "python"}"#;
        let err = parse_v2_concept("pkg/alpha.py::module", v1).unwrap_err();
        assert!(err.contains("lacks meta_version: 2"), "err: {err}");
    }

    #[test]
    fn retire_v1_leftovers_clears_load_blockers_f6() {
        // A poisoned store: one v1 row (absolute-path id, no meta_version)
        // beside its v2 successor. Retirement must remove exactly the v1
        // row from the live state while keeping the v2 row loadable.
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("v1.db")).unwrap();
        let now = now_iso8601();
        let mk = |id: &str, content: &str| {
            ConceptUpsert::new(id, id)
                .content(content)
                .valid_from(now.clone())
                .valid_to(TS_OPEN.to_string())
                .retired(false)
        };
        store
            .upsert_concepts_bulk(&[
                mk(
                    r"D:\proj\a.py::module",
                    r#"{"kind": "module", "name": "a", "file_path": "D:\\proj\\a.py"}"#,
                ),
                mk(
                    "a.py::module",
                    r#"{"meta_version": 2, "kind": "module", "name": "a", "file_path": "a.py"}"#,
                ),
            ])
            .unwrap();
        let (retired, _) = store.retire_v1_leftovers().unwrap();
        assert_eq!(retired, 1);
        let live = store.live_concept_ids().unwrap();
        assert_eq!(live, vec!["a.py::module".to_string()]);
        // Second call is a no-op (idempotent).
        assert_eq!(store.retire_v1_leftovers().unwrap().0, 0);
    }

    #[test]
    fn classify_v2_concept_variants() {
        let m = sample_module();
        match classify_v2_concept(&module_content(&m).to_string()) {
            V2ConceptClass::Canonical(k) => assert_eq!(k, "module"),
            other => panic!("expected Canonical, got {other:?}"),
        }
        let v1 = r#"{"kind": "function", "name": "f", "file_path": "a.py"}"#;
        assert!(matches!(
            classify_v2_concept(v1),
            V2ConceptClass::V1Leftover
        ));
        let field = r#"{"kind": "field", "name": "x", "file_path": "a.py"}"#;
        assert!(matches!(
            classify_v2_concept(field),
            V2ConceptClass::StandaloneField
        ));
        assert!(matches!(
            classify_v2_concept("not json"),
            V2ConceptClass::Unreadable
        ));
    }
}

// ── Edge Properties Builder ─────────────────────────────────────────────────

/// Build a JSON properties string for edge assertions.
pub fn edge_properties_json(props: &[(&str, &str)]) -> String {
    use serde_json::Value;
    let mut map = serde_json::Map::new();
    for (k, v) in props {
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    Value::Object(map).to_string()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// DR-11 slice 3: persist flushes hashes into concept JSON, because
    /// compute never touches files and the ledger would otherwise keep
    /// the last flushed (hash-less) generation.
    #[test]
    fn persist_flushes_embedding_hashes_to_concepts() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("h.db")).unwrap();
        let graph = crate::graph::CodeGraph::new(crate::graph::GraphConfig::default());
        graph
            .index_file(
                "def add(a, b):\n    return a + b\n",
                "math.py",
                &crate::types::Language::Python,
            )
            .unwrap();
        graph
            .set_embeddings_bulk(vec![(
                "math.py::add".into(),
                vec![0.5; 4],
                "m#pp1#x".into(),
            )])
            .unwrap();
        // Concept row must exist first (FK), with hash-less content.
        let now = now_iso8601();
        store
            .upsert_concepts_bulk(&[ConceptUpsert::new("math.py::add", "math.py::add")
                .content("{\"kind\":\"function\",\"name\":\"add\",\"content_hash\":\"abc\"}")
                .valid_from(now)
                .valid_to(TS_OPEN.to_string())
                .retired(false)])
            .unwrap();
        let before = store
            .fetch_current_states(&["math.py::add".to_string()])
            .unwrap();
        assert!(!before["math.py::add"].1.contains("embedding_hash"));
        // Persist writes vectors AND flushes the hash into the concept.
        let snap = graph.snapshot();
        let report = store
            .persist_projection_embeddings(
                "BAAI/bge-small-en-v1.5",
                &snap,
                &[("math.py::add".to_string(), "m#pp1#x".to_string())],
            )
            .unwrap();
        assert_eq!(report.persisted, 1);
        let after = store
            .fetch_current_states(&["math.py::add".to_string()])
            .unwrap();
        let content = &after["math.py::add"].1;
        assert!(
            content.contains("\"embedding_hash\":\"m#pp1#x\""),
            "{content}"
        );
        // The gate annotation is untouched: the hash is derived state.
        assert!(content.contains("\"content_hash\":\"abc\""), "{content}");
    }

    // Regression: the "now" pitfall. macrame-db 0.12's
    // `timestamp::normalize` (src/util/timestamp.rs) accepts ONLY the
    // canonical `YYYY-MM-DDTHH:MM:SS.ffffffZ` form (or the legacy
    // second-precision form widened by appending `.000000`). It rejects
    // `"now"`, offsets like `+01:00`, and millisecond precision. Every
    // persistence path in this crate stamps `valid_from` with
    // [`now_iso8601`], so this function must stay in the exact canonical
    // form or `Connection::reconstruct` (which normalizes first) will abort
    // the whole cold start. This test pins that contract against the real
    // macrame normalizer rather than a local regex re-implementation.
    #[test]
    fn now_timestamp_is_macrame_canonical() {
        let ts = now_iso8601();
        // Shape: 10 (date) + 1 (T) + 8 (time) + 1 (.) + 6 (frac) + 1 (Z) = 27.
        assert_eq!(ts.len(), 27, "unexpected length in {ts:?}");
        // `is_canonical` checks the exact digit/separator layout; the
        // fractional digits must be real microseconds (see the function
        // docs — truncating them breaks same-second cold starts).
        assert!(
            macrame::util::timestamp::is_canonical(&ts),
            "macrame does not consider {ts:?} canonical"
        );
        assert!(
            ts[20..26].chars().all(|c| c.is_ascii_digit()),
            "frac: {ts:?}"
        );
        // And the normalizer accepts it unchanged (the call load_snapshot
        // must survive — `reconstruct(&now_iso8601())`).
        assert_eq!(macrame::util::timestamp::normalize(&ts).unwrap(), ts);
    }

    /// Regression: `now_iso8601()` once truncated to whole seconds (the
    /// format literal ended in `.000000`), while Macrame stamps
    /// `transaction_log.recorded_at` at microsecond precision. A
    /// `reconstruct(now)` inside the same second as the last write then sat
    /// *behind* every recorded_at it should see: `hot_log_reach` classified
    /// the ts as predating the recorded history and returned the empty state
    /// without error — a cold start that silently restored nothing. A
    /// microsecond-precision `now` is always >= the newest stamp.
    #[test]
    fn reconstruct_at_now_sees_a_just_written_concept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("regress.db");
        let store = CodeGraphStore::open(&path).unwrap();
        let concept = macrame::ConceptUpsert::new("a.py::module", "a")
            .content(r#"{"meta_version": 2, "kind": "module"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[concept]).unwrap(), 1);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let state = store.reconstruct(&now_iso8601()).unwrap();
        assert!(
            state.concepts.contains_key("a.py::module"),
            "reconstruct at now must see the concept written microseconds ago (predates_recorded_history = {})",
            state.predates_recorded_history
        );
        assert!(
            state.seq_anchor > 0,
            "empty fold: seq_anchor = {}",
            state.seq_anchor
        );
    }

    #[test]
    fn now_timestamp_rejects_the_now_pitfall() {
        // The value that would be *wrong*: the literal `"now"` (and a few
        // other common-but-invalid shapes) must be rejected by macrame, so
        // that a regression in [`now_iso8601`] toward any of them is a
        // loud failure rather than a silent broken ledger.
        for bad in [
            "now",
            "latest",
            "2024-01-01T00:00:00Z",
            "2024-01-01T00:00:00.123Z",
        ] {
            // `2024-01-01T00:00:00Z` is the legacy second-precision form and
            // IS accepted (widened); the others must not be. Keep the set to
            // the genuinely-invalid ones.
            if bad == "2024-01-01T00:00:00Z" {
                assert!(
                    macrame::util::timestamp::normalize(bad).is_ok(),
                    "legacy second-precision should widen"
                );
            } else {
                assert!(
                    macrame::util::timestamp::normalize(bad).is_err(),
                    "{bad:?} should be rejected"
                );
            }
        }
    }

    fn make_test_function() -> ExtractedUnit {
        ExtractedUnit::Function(ExtractedFunction {
            id: "test.py::foo".into(),
            name: "foo".into(),
            qualified_name: "foo".into(),
            parent_module: "test.py".into(),
            parent_class: None,
            parameters: vec![],
            return_type: Some("int".into()),
            calls: vec![],
            bindings: vec![],
            refs: vec![],
            decorators: vec!["@staticmethod".into()],
            docstring: Some("Does stuff.".into()),
            kind: FunctionKind::StaticMethod,
            is_async: true,
            is_generator: false,
            line: 42,
            exit_line: 58,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            content_hash: 0,
            signature_hash: 0,
            body_hash: 0xdead,
            metrics: crate::types::FunctionMetrics::default(),
            span: ByteSpan {
                start: 1000,
                end: 1500,
            },
            name_span: ByteSpan {
                start: 1004,
                end: 1007,
            },
            params_span: ByteSpan {
                start: 1008,
                end: 1020,
            },
            body_span: ByteSpan {
                start: 1030,
                end: 1490,
            },
            decorators_span: None,
        })
    }

    #[test]
    fn test_build_concept_function() {
        let f = make_test_function();
        let concept = build_concept(&f, "test.py", "python");
        assert_eq!(concept.id, "test.py::foo");
        assert_eq!(concept.title, "foo");
        assert!(!concept.content.is_empty());

        let meta: serde_json::Value = serde_json::from_str(&concept.content).unwrap();
        assert_eq!(meta["kind"], "function");
        assert_eq!(meta["language"], "python");
        assert_eq!(meta["line"], 42);
        assert_eq!(meta["is_async"], true);
        assert_eq!(meta["return_type"], "int");
        assert!(meta["decorators"]
            .as_str()
            .unwrap()
            .contains("@staticmethod"));
        let extra: serde_json::Value =
            serde_json::from_str(concept.extra.as_deref().unwrap()).unwrap();
        assert_eq!(extra["coderadar"]["format"], 1);
        assert_eq!(extra["coderadar"]["kind"], "function");
        assert_eq!(extra["coderadar"]["file_path"], "test.py");
    }

    #[test]
    fn test_build_concept_class() {
        let c = ExtractedUnit::Class(ExtractedClass {
            id: "test.py::MyClass".into(),
            name: "MyClass".into(),
            qualified_name: "MyClass".into(),
            grammar_kind: "class_definition".into(),
            parent_module: "test.py".into(),
            parent_class: None,
            bases: vec![],
            decorators: vec!["@dataclass".into()],
            docstring: None,
            fields: vec![],
            line: 10,
            exit_line: 30,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            content_hash: 0,
            span: ByteSpan {
                start: 200,
                end: 800,
            },
            name_span: ByteSpan {
                start: 206,
                end: 213,
            },
            body_span: ByteSpan {
                start: 220,
                end: 790,
            },
            decorators_span: None,
        });

        let concept = build_concept(&c, "test.py", "python");
        assert_eq!(concept.title, "MyClass");

        let meta: serde_json::Value = serde_json::from_str(&concept.content).unwrap();
        assert_eq!(meta["kind"], "class");
        assert!(meta["decorators"].as_str().unwrap().contains("@dataclass"));
    }

    #[test]
    fn test_edge_properties_json() {
        let json = edge_properties_json(&[
            ("confidence", "direct"),
            ("line", "42"),
            ("resolution_method", "stack_graph"),
        ]);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["confidence"], "direct");
        assert_eq!(parsed["line"], "42");
    }

    // ── Retirement (plan §1.1) ───────────────────────────────────────────
    //
    // Before this, `retired: true` was written nowhere: the ledger recorded
    // every entity that had ever existed as still existing.

    fn temp_store() -> (tempfile::TempDir, CodeGraphStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("t.db")).unwrap();
        (dir, store)
    }

    /// §1.10 (DR-34): the spike probes pinned at the store level — top-1
    /// for two natural queries, hostile-input safety, and retirement
    /// removing a hit. Triggers keep `concepts_fts` current on upsert, so
    /// no explicit rebuild is needed on this path.
    #[test]
    fn fts_search_symbols_pins_spike_probes() {
        let (dir, store) = temp_store();
        let now = now_iso8601();
        let mk = |id: &str, content: &str| {
            ConceptUpsert::new(id, id)
                .content(content)
                .valid_from(now.clone())
                .valid_to(TS_OPEN.to_string())
                .retired(false)
        };
        store
            .upsert_concepts_bulk(&[
                mk(
                    "watch.py::start_watcher",
                    r#"{"meta_version": 2, "kind": "function", "name": "start_watcher", "file_path": "watch.py", "docstring": "Debounce the file watcher before restarting the index."}"#,
                ),
                mk(
                    "graph.py::CodeGraph.rename",
                    r#"{"meta_version": 2, "kind": "function", "name": "rename", "file_path": "graph.py", "docstring": "Rename a function via MutationEngine.plan_rename."}"#,
                ),
                mk(
                    "svc.py::authenticate",
                    r#"{"meta_version": 2, "kind": "function", "name": "authenticate", "file_path": "svc.py", "docstring": "Check the service token."}"#,
                ),
            ])
            .unwrap();
        // Spike probe 1: both terms only co-occur on the watcher.
        let hits = store.search_symbols("watcher debounce", 10, false).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "watch.py::start_watcher");
        // Spike probe 2: "function" is in every v2 JSON (IDF self-penalty)
        // yet the entity also mentioning rename still ranks top-1.
        let hits = store.search_symbols("rename function", 10, false).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "graph.py::CodeGraph.rename");
        // Hostile input: safe empty, never an error or silent exclusion.
        assert!(store
            .search_symbols("cats not dogs", 10, false)
            .unwrap()
            .is_empty());
        assert!(store
            .search_symbols("say \"hi", 10, false)
            .unwrap()
            .is_empty());
        assert!(store.search_symbols("", 10, false).unwrap().is_empty());
        assert!(store.search_symbols("   ", 10, false).unwrap().is_empty());
        // Retirement removes the hit: live-only, no history search.
        store
            .retire_entities(&["watch.py::start_watcher".to_string()])
            .unwrap();
        let hits = store.search_symbols("watcher debounce", 10, false).unwrap();
        assert!(hits.iter().all(|(id, _)| id != "watch.py::start_watcher"));
        // A store without the FTS table names rebuild_fts, never empties.
        // The diagnostic conn is readonly, so drop via a throwaway local
        // connection against the live store (no reopen: `open` verifies the
        // schema and would refuse the tampered file first).
        let path = dir.path().join("t.db");
        runtime().block_on(async {
            let db = libsql::Builder::new_local(&path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute("DROP TABLE concepts_fts", libsql::params![])
                .await
                .unwrap();
        });
        let err = store.search_symbols("watcher", 10, false).unwrap_err();
        assert!(format!("{err:?}").contains("rebuild_fts"), "got: {err:?}");
    }

    /// One row per id, so the count is also an assertion that retirement
    /// replaces the current row rather than adding a second live one.
    fn live_concept_ids(store: &CodeGraphStore) -> Vec<String> {
        store.live_concept_ids().unwrap()
    }

    fn open_edge_count(store: &CodeGraphStore) -> i64 {
        let (_diag_guard, conn) = store.open_diagnostic_conn().unwrap();
        runtime().block_on(async {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM links_current WHERE valid_to > ?1",
                    libsql::params![now_iso8601()],
                )
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
        })
    }

    #[test]
    fn bulk_upsert_skips_byte_identical_concepts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gate.db");
        let store = CodeGraphStore::open(&path).unwrap();
        let now = now_iso8601();
        let mk = |hash: &str| {
            v2_upsert(
                "f.py::foo",
                "foo",
                serde_json::json!({"meta_version": 2, "content_hash": hash}),
                &now,
                None,
            )
        };
        // Fresh id writes once, identical re-upsert writes nothing.
        assert_eq!(store.upsert_concepts_bulk(&[mk("abc")]).unwrap(), 1);
        assert_eq!(store.upsert_concepts_bulk(&[mk("abc")]).unwrap(), 0);
        // Changed content writes once, then skips again.
        assert_eq!(store.upsert_concepts_bulk(&[mk("def")]).unwrap(), 1);
        assert_eq!(store.upsert_concepts_bulk(&[mk("def")]).unwrap(), 0);
        // Title drift writes even with identical content.
        let renamed = v2_upsert(
            "f.py::foo",
            "foo_renamed",
            serde_json::json!({"meta_version": 2, "content_hash": "def"}),
            &now,
            None,
        );
        assert_eq!(store.upsert_concepts_bulk(&[renamed]).unwrap(), 1);
        // Retired-flag flip writes even with identical content (separate id
        // so the live-state assertion below stays meaningful).
        let mk_qux = || {
            v2_upsert(
                "f.py::qux",
                "qux",
                serde_json::json!({"meta_version": 2, "content_hash": "def"}),
                &now,
                None,
            )
        };
        assert_eq!(store.upsert_concepts_bulk(&[mk_qux()]).unwrap(), 1);
        let tombstone = mk_qux().retired(true);
        assert_eq!(
            store
                .upsert_concepts_bulk(std::slice::from_ref(&tombstone))
                .unwrap(),
            1
        );
        assert_eq!(store.upsert_concepts_bulk(&[tombstone]).unwrap(), 0);
        // Unhashable on both sides: byte-identical absorbs (no fresh
        // version per run — the P3 spine must not carry a digest in its
        // content, so without the full-content fallback every maintain
        // would churn), any drift rewrites.
        let bare = v2_upsert(
            "f.py::bar",
            "bar",
            serde_json::json!({"meta_version": 2}),
            &now,
            None,
        );
        assert_eq!(
            store
                .upsert_concepts_bulk(std::slice::from_ref(&bare))
                .unwrap(),
            1
        );
        assert_eq!(store.upsert_concepts_bulk(&[bare]).unwrap(), 0);
        // A meta_version-less row with matching hash still upgrades (the
        // v1-store upgrade path); afterwards it skips like any v2 row.
        let legacy = ConceptUpsert::new("f.py::v1", "v1")
            .content(r#"{"kind":"function","content_hash":"abc"}"#)
            .valid_from(now.clone())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[legacy]).unwrap(), 1);
        let modern = v2_upsert(
            "f.py::v1",
            "v1",
            serde_json::json!({"meta_version": 2, "content_hash": "abc"}),
            &now,
            None,
        );
        assert_eq!(
            store
                .upsert_concepts_bulk(std::slice::from_ref(&modern))
                .unwrap(),
            1
        );
        assert_eq!(store.upsert_concepts_bulk(&[modern]).unwrap(), 0);

        // App metadata drift is a real concept change even when the content
        // hash agrees (e.g. a corrected canonical file path).
        let metadata = |file_path: &str| {
            v2_upsert(
                "src/a.py::f",
                "f",
                serde_json::json!({
                    "meta_version": 2,
                    "kind": "function",
                    "file_path": file_path,
                    "content_hash": "stable",
                }),
                &now,
                None,
            )
        };
        assert_eq!(
            store.upsert_concepts_bulk(&[metadata("src/a.py")]).unwrap(),
            1
        );
        assert_eq!(
            store
                .upsert_concepts_bulk(&[metadata("src/renamed.py")])
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .upsert_concepts_bulk(&[metadata("src/renamed.py")])
                .unwrap(),
            0
        );

        // Latest state still correct after all the skips: live concepts
        // present, the retired one folded out.
        let state = store.reconstruct(&now_iso8601()).unwrap();
        assert!(state.concepts.contains_key("f.py::foo"));
        assert!(state.concepts.contains_key("f.py::bar"));
        assert!(!state.concepts.contains_key("f.py::qux"));
    }

    #[test]
    fn exact_file_cleanup_queries_use_the_extra_and_id_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("extra.db")).unwrap();
        let now = now_iso8601();
        let make = |id: &str, file_path: &str| {
            v2_upsert(
                id,
                id.rsplit("::").next().unwrap(),
                serde_json::json!({
                    "meta_version": 2,
                    "kind": "function",
                    "file_path": file_path,
                    "content_hash": "same-hash",
                }),
                &now,
                None,
            )
        };
        store
            .upsert_concepts_bulk(&[
                make("src/a.py::one", "src/a.py"),
                make("src/a.pyx::sibling", "src/a.pyx"),
                make("src/b.py::two", "src/b.py"),
                ConceptUpsert::new("src/a.py::legacy", "legacy")
                    .content(r#"{"kind":"function"}"#)
                    .valid_from(now.clone())
                    .valid_to(TS_OPEN.to_string())
                    .retired(false),
            ])
            .unwrap();

        assert_eq!(
            store.live_concept_ids_for_file("src/a.py").unwrap(),
            vec!["src/a.py::legacy".to_string(), "src/a.py::one".to_string(),]
        );

        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        let extra_sql = format!("EXPLAIN QUERY PLAN {LIVE_IDS_BY_EXTRA_FILE_PATH_SQL}");
        let mut rows = runtime()
            .block_on(conn.query(&extra_sql, libsql::params!["src/a.py"]))
            .unwrap();
        let mut extra_plan = Vec::new();
        while let Some(row) = runtime().block_on(rows.next()).unwrap() {
            extra_plan.push(row.get::<String>(3).unwrap_or_default());
        }
        assert!(
            extra_plan
                .iter()
                .any(|detail| detail.contains("idx_concepts_extra_coderadar_file_path")),
            "metadata lookup should use the registered index; plan: {extra_plan:?}"
        );

        let id_sql = format!("EXPLAIN QUERY PLAN {LIVE_IDS_BY_ID_FILE_RANGE_SQL}");
        let mut rows = runtime()
            .block_on(conn.query(&id_sql, libsql::params!["src/a.py::", "src/a.py:;"]))
            .unwrap();
        let mut id_plan = Vec::new();
        while let Some(row) = runtime().block_on(rows.next()).unwrap() {
            id_plan.push(row.get::<String>(3).unwrap_or_default());
        }
        assert!(
            id_plan
                .iter()
                .any(|detail| detail.contains("id>?") && detail.contains("id<?")),
            "legacy-prefix lookup should use a primary-key range; plan: {id_plan:?}"
        );
    }

    #[test]
    fn stale_file_concepts_are_retired_only_when_absent_from_the_projection() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("stale.db")).unwrap();
        let now = now_iso8601();
        let make = |id: &str, kind: &str, file_path: &str| {
            v2_upsert(
                id,
                id.rsplit("::").next().unwrap(),
                serde_json::json!({
                    "meta_version": 2,
                    "kind": kind,
                    "name": id,
                    "file_path": file_path,
                    "content_hash": "same-hash",
                }),
                &now,
                None,
            )
        };
        store
            .upsert_concepts_bulk(&[
                make("src/a.py::module", "module", "src/a.py"),
                make("src/a.py::import@19", "import", "src/a.py"),
                ConceptUpsert::new("src/a.py::legacy_field", "legacy_field")
                    .content(r#"{"kind":"field","file_path":"src/a.py"}"#)
                    .valid_from(now.clone())
                    .valid_to(TS_OPEN.to_string())
                    .retired(false),
                ConceptUpsert::new("external::custom", "custom")
                    .content(r#"{"kind":"custom","file_path":"src/a.py"}"#)
                    .valid_from(now)
                    .valid_to(TS_OPEN.to_string())
                    .retired(false),
            ])
            .unwrap();

        let keep = HashSet::from(["src/a.py::module".to_string()]);
        assert_eq!(store.retire_stale_file_concepts(&keep).unwrap(), 2);
        assert_eq!(
            store.live_concept_ids().unwrap(),
            vec![
                "external::custom".to_string(),
                "src/a.py::module".to_string(),
            ]
        );
        // Reconciliation is idempotent.
        assert_eq!(store.retire_stale_file_concepts(&keep).unwrap(), 0);
    }

    fn seed(store: &CodeGraphStore) {
        let ts = now_iso8601();
        let concept = |id: &str| {
            ConceptUpsert::new(id.to_string(), id.rsplit("::").next().unwrap().to_string())
                .content("{}")
                .valid_from(ts.clone())
                .valid_to(TS_OPEN.to_string())
                .retired(false)
        };
        store
            .upsert_concepts_bulk(&[
                concept("a.py::caller"),
                concept("a.py::callee"),
                concept("b.py::bystander"),
            ])
            .unwrap();
        store
            .assert_edge("a.py::caller", "a.py::callee", "CALLS", "{}", &ts, 1.0)
            .unwrap();
        store
            .assert_edge("b.py::bystander", "a.py::callee", "CALLS", "{}", &ts, 1.0)
            .unwrap();
    }

    #[test]
    fn test_retire_entities_closes_the_concept() {
        let (_dir, store) = temp_store();
        seed(&store);
        assert_eq!(live_concept_ids(&store).len(), 3);

        let (concepts, _) = store
            .retire_entities(&["a.py::callee".to_string()])
            .unwrap();

        assert_eq!(concepts, 1);
        assert_eq!(
            live_concept_ids(&store),
            vec!["a.py::caller".to_string(), "b.py::bystander".to_string()]
        );
    }

    /// An edge is retired when *either* endpoint goes: a call into a deleted
    /// function is no more a fact about the code than a call out of one.
    #[test]
    fn test_retire_entities_closes_edges_on_both_sides() {
        let (_dir, store) = temp_store();
        seed(&store);
        assert_eq!(open_edge_count(&store), 2);

        let (_, edges) = store
            .retire_entities(&["a.py::callee".to_string()])
            .unwrap();

        assert_eq!(edges, 2, "both the incoming and the outgoing edge close");
        assert_eq!(open_edge_count(&store), 0);
    }

    #[test]
    fn test_retire_entities_leaves_untouched_entities_alone() {
        let (_dir, store) = temp_store();
        seed(&store);

        store
            .retire_entities(&["b.py::bystander".to_string()])
            .unwrap();

        assert!(live_concept_ids(&store).contains(&"a.py::caller".to_string()));
        assert_eq!(
            open_edge_count(&store),
            1,
            "only the bystander's edge closes; caller→callee is untouched"
        );
    }

    #[test]
    fn test_retire_entities_is_a_no_op_for_unknown_or_empty_ids() {
        let (_dir, store) = temp_store();
        seed(&store);

        assert_eq!(store.retire_entities(&[]).unwrap(), (0, 0));
        assert_eq!(
            store
                .retire_entities(&["nowhere.py::ghost".to_string()])
                .unwrap(),
            (0, 0)
        );
        assert_eq!(live_concept_ids(&store).len(), 3);
    }

    /// Retiring twice must not error or resurrect anything — the watcher can
    /// deliver the same deletion more than once.
    #[test]
    fn test_retire_entities_is_idempotent() {
        let (_dir, store) = temp_store();
        seed(&store);

        store
            .retire_entities(&["a.py::callee".to_string()])
            .unwrap();
        let (concepts, edges) = store
            .retire_entities(&["a.py::callee".to_string()])
            .unwrap();

        assert_eq!((concepts, edges), (0, 0), "nothing left open to close");
        assert_eq!(live_concept_ids(&store).len(), 2);
    }

    /// §0.1(a) (DR-9): `reconstruct(T)` folds on recorded time, not valid
    /// time. A retroactive correction — `valid_from` backdated, recorded now
    /// — is invisible at any T before its recording. The honest-Snapshot
    /// surface depends on this: `as_of(T)` answers "what was RECORDED by
    /// T", and a pre-history T reports `predates_recorded_history` instead
    /// of an error or a valid-time guess.
    #[test]
    fn reconstruct_is_recorded_time_not_valid_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitemporal.db");
        let store = CodeGraphStore::open(&path).unwrap();
        let concept = macrame::ConceptUpsert::new("retro.py::module", "retro")
            .content(r#"{"meta_version": 2, "kind": "module"}"#)
            .valid_from("2000-01-01T00:00:00.000000Z".to_string())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[concept]).unwrap(), 1);
        // The backdated write was recorded NOW, so a mid-past T sees
        // nothing — and knows it predates the recorded history.
        let mid = store.reconstruct("2020-06-15T12:00:00.000000Z").unwrap();
        assert!(
            !mid.concepts.contains_key("retro.py::module"),
            "recorded-now write must be invisible at a pre-recording T"
        );
        assert!(
            mid.predates_recorded_history,
            "a T before the first recorded write must say so distinctly"
        );
        // …while the present sees the backdated concept fine.
        std::thread::sleep(std::time::Duration::from_millis(1));
        let now = store.reconstruct(&now_iso8601()).unwrap();
        assert!(now.concepts.contains_key("retro.py::module"));
    }

    /// §1.9 (DR-25): digest validation at the persistence boundary.
    #[test]
    fn validate_digest_accepts_canonical_rejects_rest() {
        assert!(validate_digest(&"a".repeat(64)).is_ok());
        assert!(validate_digest(&"0123456789abcdef".repeat(4)).is_ok());
        // Short, long, uppercase, non-hex, empty.
        assert!(validate_digest(&"a".repeat(63)).is_err());
        assert!(validate_digest(&"a".repeat(65)).is_err());
        assert!(validate_digest(&"A".repeat(64)).is_err());
        assert!(validate_digest(&"g".repeat(64)).is_err());
        assert!(validate_digest("").is_err());
    }

    /// §1.9 (DR-25): the extra read boundary — legacy generations (no key)
    /// and malformed digests read as absent, never as errors.
    #[test]
    fn source_blob_of_extra_reads_new_and_legacy() {
        let digest = "b".repeat(64);
        let with = format!(r#"{{"coderadar":{{"format":1,"source_blob":"{digest}"}}}}"#);
        assert_eq!(
            source_blob_of_extra(&with).as_deref(),
            Some(digest.as_str())
        );
        // Legacy: no source_blob key.
        assert_eq!(source_blob_of_extra(r#"{"coderadar":{"format":1}}"#), None);
        // Malformed: uppercase digest reads as absent.
        let bad = r#"{"coderadar":{"format":1,"source_blob":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}}"#;
        assert_eq!(source_blob_of_extra(bad), None);
        // Not JSON at all.
        assert_eq!(source_blob_of_extra("not json"), None);
    }

    /// P0 (DR-36) fixture: a module with a blob digest plus one function
    /// concept, both asserted now. Returns the store and a pre-history
    /// timestamp (before the first recorded write) for the predates pin.
    fn source_ref_store(dir: &std::path::Path) -> (CodeGraphStore, String) {
        let store = CodeGraphStore::open(dir.join("refs.db")).unwrap();
        let digest = "c".repeat(64);
        let module_extra = format!(r#"{{"coderadar":{{"format":1,"source_blob":"{digest}"}}}}"#);
        let now = now_iso8601();
        let module = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
            .extra(module_extra)
            .valid_from(now.clone())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        let func = macrame::ConceptUpsert::new("m.py::f", "m.py::f")
            .content(
                r#"{"kind":"function","name":"f","file_path":"m.py","span":{"start":0,"end":10}}"#,
            )
            .valid_from(now.clone())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[module, func]).unwrap(), 2);
        // Stamp AFTER the write: `now` above predates the recording (the
        // upsert lands later), and a sweep must query at-or-after it.
        std::thread::sleep(std::time::Duration::from_millis(2));
        (store, now_iso8601())
    }

    /// P0 (DR-36) anti-divergence pin: the batched path returns exactly
    /// what N single calls return, for present, digest-less and absent ids.
    #[test]
    fn source_refs_at_matches_single_calls() {
        let dir = tempfile::tempdir().unwrap();
        let (store, now) = source_ref_store(dir.path());
        let ids = vec![
            "m.py::f".to_string(),
            "m.py::module".to_string(),
            "m.py::ghost".to_string(),
        ];
        let (batch, predates) = store.source_refs_at(&ids, &now, "main").unwrap();
        assert!(!predates);
        // Absent at T: absent from the map, never a None entry.
        assert_eq!(batch.len(), 2);
        assert!(!batch.contains_key("m.py::ghost"));
        for id in ["m.py::f", "m.py::module"] {
            let single = store
                .entity_source_ref_at(id, &now, "main")
                .unwrap()
                .unwrap();
            let batched = &batch[id];
            assert_eq!(batched.id, single.id);
            assert_eq!(batched.file_path, single.file_path);
            assert_eq!(batched.digest, single.digest);
            assert_eq!(batched.digest.as_deref(), Some("c".repeat(64).as_str()));
            assert_eq!(
                (batched.span_start, batched.span_end),
                (single.span_start, single.span_end)
            );
            assert_eq!(batched.has_span, single.has_span);
            assert_eq!(batched.full_file, single.full_file);
        }
        // The function carries a span; the module concept has none.
        assert!(batch["m.py::f"].has_span);
        assert!(!batch["m.py::module"].has_span);
        assert!(batch["m.py::module"].full_file);
    }

    /// P0 (DR-36): a pre-floor T reports `predates_recorded_history` with
    /// an empty map — the sweep's stop condition, matching `entities_at`.
    #[test]
    fn source_refs_at_predates_history_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _now) = source_ref_store(dir.path());
        let (batch, predates) = store
            .source_refs_at(
                &["m.py::f".to_string()],
                "2000-01-01T00:00:00.000000Z",
                "main",
            )
            .unwrap();
        assert!(predates);
        assert!(batch.is_empty());
    }

    /// P4 (DR-40): on an unforked database `reconstruct_on(main)` is
    /// `reconstruct` — same concepts, same edges, no predates flag.
    #[test]
    fn reconstruct_on_trunk_matches_reconstruct() {
        let dir = tempfile::tempdir().unwrap();
        let (store, now) = source_ref_store(dir.path());
        let plain = store.reconstruct(&now).unwrap();
        let on = store.reconstruct_on(&now, "main").unwrap();
        assert!(!on.predates_recorded_history);
        assert_eq!(on.concepts.len(), plain.concepts.len());
        assert_eq!(on.edges.len(), plain.edges.len());
    }

    /// P4 (DR-40): one lineage before the fork (the trunk, no parent),
    /// two after — the fork names its parent and cut instant.
    #[test]
    fn branches_lists_trunk_then_fork() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _now) = source_ref_store(dir.path());
        let before = store.branches().unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].name, "main");
        assert_eq!(before[0].parent, None);
        let cut = store.fork_branch("feat", "main").unwrap();
        assert_eq!(cut.name, "feat");
        assert_eq!(cut.parent.as_deref(), Some("main"));
        assert!(cut.forked_at.is_some());
        let after = store.branches().unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().any(|b| b.name == "feat"));
    }

    /// P4 (DR-40): forking from an unregistered lineage refuses (never
    /// trunk-fallback), and so does a blank name — a typo'd lineage must
    /// never silently read as another.
    #[test]
    fn fork_rejects_unknown_parent_and_bad_name() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _now) = source_ref_store(dir.path());
        let err = store.fork_branch("feat", "nope").unwrap_err();
        assert!(
            matches!(err, macrame::DbError::UnknownBranch(_)),
            "unknown parent must refuse, got {err:?}"
        );
        let err = store.fork_branch("", "main").unwrap_err();
        assert!(
            matches!(err, macrame::DbError::InvalidBranchId(_)),
            "blank name must refuse, got {err:?}"
        );
    }

    /// P4 (DR-40) core: divergent bytes on a branch. The branch reads its
    /// own blob (full file — spans belong to the trunk's lineage); the
    /// trunk keeps its module-concept fallback; the spine stays one
    /// shared content-addressed chain across lineages.
    #[test]
    fn branch_pointer_diverges_and_trunk_fallback_survives() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _now) = source_ref_store(dir.path());
        store.fork_branch("feat", "main").unwrap();
        let bytes_b = b"x = 1\ny = 2\n";
        let spine_id = store.write_branch_content("m.py", bytes_b, "feat").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let now = now_iso8601();
        // The branch reads its own bytes.
        let branched = store
            .entity_source_ref_at("m.py::module", &now, "feat")
            .unwrap()
            .unwrap();
        let sha_b = branched.digest.clone().expect("branch must name its blob");
        assert_eq!(spine_id, format!("m.py@{sha_b}"));
        assert!(branched.full_file);
        assert!(!branched.has_span);
        let got = store.blob_get(&sha_b).unwrap().unwrap();
        assert_eq!(got, bytes_b);
        // The trunk is unaffected: module-concept fallback, legacy digest.
        let trunk = store
            .entity_source_ref_at("m.py::module", &now, "main")
            .unwrap()
            .unwrap();
        assert_eq!(trunk.digest.as_deref(), Some("c".repeat(64).as_str()));
        // One shared chain: trunk content is not in it (no blob was ever
        // put for the fixture digest), branch content is.
        let chain = store.history_of("m.py").unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].digest, sha_b);
    }

    /// P4 (DR-40): re-pointing to the same bytes is a gate no-op
    /// (zero-growth ethos); re-pointing elsewhere turns over the same
    /// pointer id, so the archive reclaims the old bytes exactly like
    /// superseded module content — same-entity supersession, no writer
    /// close step, no terminal rows.
    #[test]
    fn branch_repoint_is_idempotent_and_turns_over() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _now) = source_ref_store(dir.path());
        store.fork_branch("feat", "main").unwrap();
        let v1 = b"x = 1\n";
        let v2 = b"x = 1\ny = 2\n";
        let id1 = store.write_branch_content("m.py", v1, "feat").unwrap();
        let id1_again = store.write_branch_content("m.py", v1, "feat").unwrap();
        assert_eq!(id1, id1_again);
        // Turnover, not accumulation: the stable pointer id names the
        // new bytes after the re-point, through the read path.
        let id2 = store.write_branch_content("m.py", v2, "feat").unwrap();
        assert_ne!(id1, id2);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let now = now_iso8601();
        let state = store.reconstruct_on(&now, "feat").unwrap();
        let pointer = state
            .concepts
            .get("branch:feat:content:m.py")
            .expect("pointer concept visible on its branch");
        let sha2 = id2.trim_start_matches("m.py@");
        assert_eq!(source_blob_of_extra(&pointer.extra).as_deref(), Some(sha2));
        // And the trunk cannot see it (ancestry-narrowed fold).
        let trunk_state = store.reconstruct_on(&now, "main").unwrap();
        assert!(!trunk_state
            .concepts
            .contains_key("branch:feat:content:m.py"));
        std::thread::sleep(std::time::Duration::from_millis(2));
        let now = now_iso8601();
        let r = store
            .entity_source_ref_at("m.py::module", &now, "feat")
            .unwrap()
            .unwrap();
        assert_eq!(r.digest.as_deref(), Some(id2.trim_start_matches("m.py@")));
    }

    /// P4 (DR-40): reading on an unregistered lineage refuses — the
    /// branch-unaware-reader hazard answered as an error, not trunk data.
    #[test]
    fn unknown_branch_read_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let (store, now) = source_ref_store(dir.path());
        let err = store
            .entity_source_ref_at("m.py::module", &now, "nope")
            .unwrap_err();
        assert!(
            matches!(err, macrame::DbError::UnknownBranch(_)),
            "unknown branch must refuse, got {err:?}"
        );
    }

    /// P4 (DR-40): pointer concepts are ledger infrastructure — invisible
    /// to every entity listing, while the ancestry union still hands a
    /// branch sweep the trunk's modules it inherits.
    #[test]
    fn branch_infrastructure_is_invisible_to_entity_listings() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _now) = source_ref_store(dir.path());
        store.fork_branch("feat", "main").unwrap();
        store
            .write_branch_content("m.py", b"x = 1\ny = 2\n", "feat")
            .unwrap();
        let live = store.live_concept_ids().unwrap();
        assert!(!live.iter().any(|id| id.starts_with("branch:")));
        assert!(live.contains(&"m.py::module".to_string()));
        let feat_ids = store.all_concept_ids("feat").unwrap();
        assert!(!feat_ids.iter().any(|id| id.starts_with("branch:")));
        // The strict listing is mint-lineage: nothing but the pointer was
        // minted on the branch, and the pointer is excluded.
        assert!(feat_ids.is_empty());
        // The visible union hands the branch its inherited universe.
        assert_eq!(
            store.visible_module_concept_ids("feat").unwrap(),
            store.module_concept_ids("main").unwrap()
        );
        assert!(store
            .visible_all_concept_ids("feat")
            .unwrap()
            .contains(&"m.py::f".to_string()));
        // And the classifier diverts pointer rows like spine rows (never
        // entities, never V1Leftovers).
        let pointer_content = r#"{"meta_version":2,"kind":"branch_content","branch":"feat"}"#;
        assert!(matches!(
            classify_v2_concept(pointer_content),
            V2ConceptClass::Spine
        ));
    }

    /// P1 (DR-36) fixture: two recorded acts. The first asserts module
    /// `m.py`; the second retires it and asserts `n.py`. Returns the store
    /// plus the valid-from stamp of the second act and a post-write stamp.
    fn generations_store(dir: &std::path::Path) -> (CodeGraphStore, String, String) {
        let store = CodeGraphStore::open(dir.join("gens.db")).unwrap();
        let digest = "d".repeat(64);
        let m_extra = format!(r#"{{"coderadar":{{"format":1,"source_blob":"{digest}"}}}}"#);
        let m_v1 = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
            .extra(m_extra)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[m_v1]).unwrap(), 1);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let s1 = now_iso8601();
        let m_v2 = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
            .valid_from(s1.clone())
            .valid_to(TS_OPEN.to_string())
            .retired(true);
        let n_v1 = macrame::ConceptUpsert::new("n.py::module", "n.py::module")
            .content(r#"{"kind":"module","name":"n.py","file_path":"n.py"}"#)
            .valid_from(s1.clone())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[m_v2, n_v1]).unwrap(), 2);
        std::thread::sleep(std::time::Duration::from_millis(2));
        (store, s1, now_iso8601())
    }

    /// P1 (DR-36): the listing is one stamp per recorded act, oldest
    /// first, and the window bounds narrow it.
    #[test]
    fn recorded_generations_lists_windowed_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _s1, _s2) = generations_store(dir.path());
        let all = store.recorded_generations(None, None, "main").unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0] < all[1]);
        let tail = store
            .recorded_generations(Some(&all[1]), None, "main")
            .unwrap();
        assert_eq!(tail, vec![all[1].clone()]);
    }

    /// P1 (DR-36): ever-ids, not live ones — the retired `m.py::module`
    /// is in the universe the sweep iterates.
    #[test]
    fn module_concept_ids_includes_retired() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _s1, _s2) = generations_store(dir.path());
        let mut ids = store.module_concept_ids("main").unwrap();
        ids.sort();
        assert_eq!(
            ids,
            vec!["m.py::module".to_string(), "n.py::module".to_string()]
        );
        // And the live-only view disagrees, which is why the sweep must
        // not use it.
        let live = store.live_concept_ids().unwrap();
        assert!(!live.contains(&"m.py::module".to_string()));
    }

    /// P2 (DR-37): the full ever-universe, not just modules — a
    /// function retired with its file stays diffable.
    #[test]
    fn all_concept_ids_lists_every_kind_and_retired() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _s1, _s2) = generations_store(dir.path());
        // The fixture store holds two module concepts (one retired).
        // Assert a function concept asserted alongside: the universe is
        // not the module list.
        let f_v1 = macrame::ConceptUpsert::new("m.py::f", "m.py::f")
            .content(r#"{"kind":"function","name":"f"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[f_v1]).unwrap(), 1);
        let mut ids = store.all_concept_ids("main").unwrap();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "m.py::f".to_string(),
                "m.py::module".to_string(),
                "n.py::module".to_string()
            ]
        );
        // Modules-only is the strict subset; the diff must use the full one.
        let modules = store.module_concept_ids("main").unwrap();
        assert!(modules.len() < ids.len());
        assert!(modules.iter().all(|m| ids.contains(m)));
    }

    /// DR-42 (Q-2, H-3): the framework hook's sweep is an inclusion list
    /// over the `synthetic:` namespace. Structural rows — even DECORATES
    /// and REFERENCES, which the old exclusion list swept — and
    /// non-namespaced direct-path rows (`DEPENDS_ON`) are not returned;
    /// a retired namespaced row leaves the sweep via `retire_synthetic_pairs`.
    #[test]
    fn open_synthetic_triples_returns_only_namespaced_framework_edges() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("syn.db")).unwrap();
        let now = now_iso8601();
        for id in ["r::route", "h.py::handler", "a.py::f", "b.py::g"] {
            let up = macrame::ConceptUpsert::new(id, id)
                .content(r#"{"kind":"function","name":"x","file_path":"f.py"}"#)
                .valid_from(now.clone())
                .valid_to(TS_OPEN.to_string())
                .retired(false);
            assert_eq!(store.upsert_concepts_bulk(&[up]).unwrap(), 1);
        }
        let triples = [
            ("r::route", "h.py::handler", "synthetic:flask:handles"),
            ("r::route", "h.py::handler", "synthetic:django:registers"),
            ("a.py::f", "b.py::g", "DECORATES"),
            ("a.py::f", "b.py::g", "REFERENCES"),
            ("a.py::f", "b.py::g", "DEPENDS_ON"),
        ];
        let batch: Vec<macrame::graph::EdgeAssertion> = triples
            .iter()
            .map(|&(s, t, k)| {
                macrame::graph::EdgeAssertion::new(s, t, k)
                    .valid_from(now.clone())
                    .weight(1.0)
            })
            .collect();
        assert_eq!(store.assert_edges_bulk(batch).unwrap(), 5);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mut got = store.open_synthetic_triples(&now_iso8601()).unwrap();
        got.sort();
        assert_eq!(
            got.iter().map(|(_, _, k, _)| k.clone()).collect::<Vec<_>>(),
            vec![
                "synthetic:django:registers".to_string(),
                "synthetic:flask:handles".to_string()
            ]
        );
        // Retired namespaced rows leave the sweep (the hook's step-4
        // close path converges: re-running finds nothing stale).
        std::thread::sleep(std::time::Duration::from_millis(2));
        let retired_at = now_iso8601();
        assert_eq!(store.retire_synthetic_pairs(&got, &retired_at).unwrap(), 2);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(store
            .open_synthetic_triples(&now_iso8601())
            .unwrap()
            .is_empty());
    }

    /// P3 (DR-38): the spine writer's contract — one timeless concept
    /// per actual content change (parents and intervals derived from
    /// chain order), the unchanged gate absorbing true no-ops. Also pins
    /// the retention coexistence: no SPINE log payload names a digest
    /// (title/content carry the path, `extra` numbers only; the digest
    /// lives in the unscanned id column), so a superseded blob stays
    /// archivable. And the universe hygiene: spine ids appear in
    /// neither `live_concept_ids*` nor `all_concept_ids`, so file sweeps
    /// and the P2 diff never see ledger infrastructure as entities.
    /// The v1-upgrade heal: a spine row stripped of `meta_version` is
    /// rewritten on the next maintain (same digest, fresh metadata) —
    /// the digest pre-skip that swallowed the upgrade is gone; the gate,
    /// not the caller, decides no-op.
    #[test]
    fn spine_maintain_links_one_chain_per_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("spine.db")).unwrap();
        let now = now_iso8601();
        let module = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
            .valid_from(now.clone())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[module]).unwrap(), 1);
        let blob_put = |name: &str, bytes: &[u8]| match store.put_source_blob(name, bytes).unwrap()
        {
            BlobOutcome::Stored { digest, .. } => digest,
            other => panic!("expected Stored, got {other:?}"),
        };
        let d1 = blob_put("m.py", b"x = 1\n");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t1 = now_iso8601();
        let r1 = store
            .maintain_content_spine(&[("m.py::module".to_string(), d1.clone())], &t1)
            .unwrap();
        assert_eq!((r1.files_seen, r1.concepts_upserted), (1, 1));
        // Unchanged content: the gate absorbs the re-upsert — the
        // report shows zero upserts, no new version, no new commit.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let r_same = store
            .maintain_content_spine(&[("m.py::module".to_string(), d1.clone())], &now_iso8601())
            .unwrap();
        assert_eq!((r_same.files_seen, r_same.concepts_upserted), (1, 0));
        // v1-upgrade heal: a metadata-stripped row for the SAME digest
        // is rewritten, not skipped. Write the stripped variant through
        // the bulk API (the digest pre-skip is gone, so the gate sees
        // it); the next maintain must count one healing upsert.
        let full_content = spine_concept_upsert("m.py", &d1, 6, 1, &now_iso8601())
            .content
            .clone();
        let mut stripped_value: serde_json::Value = serde_json::from_str(&full_content).unwrap();
        stripped_value
            .as_object_mut()
            .unwrap()
            .remove("meta_version");
        let mut stripped_upsert = spine_concept_upsert("m.py", &d1, 6, 1, &now_iso8601());
        stripped_upsert.content = stripped_value.to_string();
        assert_eq!(
            store
                .upsert_concepts_bulk(std::slice::from_ref(&stripped_upsert))
                .unwrap(),
            1,
            "the stripped variant must land (drift from the open row)"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t_heal = now_iso8601();
        let r_heal = store
            .maintain_content_spine(&[("m.py::module".to_string(), d1.clone())], &t_heal)
            .unwrap();
        assert_eq!(
            (r_heal.files_seen, r_heal.concepts_upserted),
            (1, 1),
            "a metadata-stripped row must heal, not skip"
        );
        // New content: one concept, parent-linked.
        let d2 = blob_put("m.py", b"x = 1\ny = 2\n");
        assert_ne!(d1, d2);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t2 = now_iso8601();
        let r2 = store
            .maintain_content_spine(&[("m.py::module".to_string(), d2.clone())], &t2)
            .unwrap();
        assert_eq!((r2.files_seen, r2.concepts_upserted), (1, 1));
        let chain = store.history_of("m.py").unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].digest, d2);
        assert!(chain[0].open);
        assert_eq!(chain[0].parent_digest.as_deref(), Some(d1.as_str()));
        assert_eq!(chain[0].size_bytes, 12);
        assert_eq!(chain[0].line_count, 2);
        assert_eq!(chain[1].digest, d1);
        assert!(!chain[1].open);
        assert_eq!(chain[1].parent_digest, None);
        assert_eq!(
            chain[1].valid_from, t_heal,
            "the heal rewrite restamps the same digest row"
        );
        assert_eq!(
            chain[1].valid_to, chain[0].valid_from,
            "intervals derive from chain order: contiguous by construction"
        );
        // Universe hygiene: no sweep and no diff universe lists spine rows.
        assert_eq!(
            store.live_concept_ids_for_file("m.py").unwrap(),
            vec!["m.py::module".to_string()]
        );
        assert!(
            !store
                .live_concept_ids()
                .unwrap()
                .iter()
                .any(|id| id.contains('@')),
            "spine ids are not live entities"
        );
        assert!(
            !store
                .all_concept_ids("main")
                .unwrap()
                .iter()
                .any(|id| id.contains('@')),
            "spine ids are not diff-universe entities"
        );
        // Retention coexistence: no SPINE log payload names a spine digest.
        // Concept payloads log (v, title, content, stamps) AND `extra`,
        // so the spine title/content/extra carry the path and numbers
        // only; the digest lives solely in the id — an unscanned
        // `entity_id` column, since the archive scan reads payloads. A
        // superseded blob therefore stays archivable. Scoped to spine
        // rows: module payloads name their live digest by design (and
        // age out via their successor row), which is exactly the
        // mechanism the pin must not disturb.
        let (_leak_guard, leak_conn) = store.open_diagnostic_conn().unwrap();
        let leaked: i64 = runtime()
            .block_on(async {
                let mut rows = leak_conn
                    .query(
                        "SELECT COUNT(*) FROM transaction_log                          WHERE payload LIKE ?1 AND entity_id LIKE 'm.py@%'",
                        libsql::params![format!("%{d1}%")],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                let row = rows
                    .next()
                    .await
                    .map_err(macrame::DbError::Engine)?
                    .unwrap();
                Ok::<_, macrame::DbError>(row.get(0).unwrap_or(0))
            })
            .unwrap();
        assert_eq!(leaked, 0, "spine rows must not pin blobs via hot payloads");
    }

    /// §1.9 (DR-25): put round-trip + idempotent retry (the
    /// failure-injection property without hooks — a put with no following
    /// assert is an orphan; retry converges on the same digest, one row).
    #[test]
    fn blob_put_retry_converges_on_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("retry.db")).unwrap();
        let bytes = b"def f():\n    return 1\n";
        // First put (imagine the crash before any digest assert).
        let first = store.put_source_blob("a.py", bytes).unwrap();
        let digest = match &first {
            BlobOutcome::Stored { digest, .. } => digest.clone(),
            other => panic!("expected Stored, got {other:?}"),
        };
        // Retry: same digest, and still exactly one blob row.
        let second = store.put_source_blob("a.py", bytes).unwrap();
        assert_eq!(second, first);
        let n: i64 = runtime().block_on(async {
            let mut rows = store
                .db
                .read_conn()
                .query("SELECT COUNT(*) FROM blobs", ())
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get(0).unwrap()
        });
        assert_eq!(n, 1, "content-addressed: one row for identical bytes");
        // And the bytes come back.
        let back: Vec<u8> = runtime()
            .block_on(store.db.blob_get(&digest))
            .unwrap()
            .unwrap();
        assert_eq!(back, bytes);
    }

    /// §1.9 (DR-25): oversize degrades to skip+count, never an index failure.
    #[test]
    fn blob_put_oversize_skips() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("big.db")).unwrap();
        let big = vec![0u8; BLOB_MAX_BYTES + 1];
        match store.put_source_blob("big.py", &big).unwrap() {
            BlobOutcome::SkippedOversize { bytes } => {
                assert_eq!(bytes, (BLOB_MAX_BYTES + 1) as u64)
            }
            other => panic!("expected SkippedOversize, got {other:?}"),
        }
    }

    /// §1.9 (DR-25): policy skips — kill-switch and secret defaults.
    #[test]
    fn blob_put_honors_kill_switch_and_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("policy.db")).unwrap();
        // Secret default: .env gets graph coverage, no blob.
        assert_eq!(
            store.put_source_blob(".env", b"KEY=1\n").unwrap(),
            BlobOutcome::SkippedExcluded
        );
        assert_eq!(
            store.put_source_blob("sub/creds.env", b"x\n").unwrap(),
            BlobOutcome::SkippedExcluded
        );
        // Ordinary file stores.
        assert!(matches!(
            store.put_source_blob("app.py", b"x\n").unwrap(),
            BlobOutcome::Stored { .. }
        ));
    }

    /// §1.9 (DR-25): backfill-once — a module extra missing `source_blob`
    /// counts as changed (re-persist asserts the digest); once present and
    /// equal, the concept filters unchanged (zero-growth).
    #[test]
    fn missing_digest_forces_repersist_then_filters() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("backfill.db")).unwrap();
        let now = now_iso8601();
        let content = serde_json::json!({"meta_version": 2, "kind": "module", "content_hash": "h"});
        // Legacy-shaped (no digest) vs current without digest: stable.
        let legacy = v2_upsert("m.py::module", "m", content.clone(), &now, None);
        assert_eq!(
            store
                .upsert_concepts_bulk(std::slice::from_ref(&legacy))
                .unwrap(),
            1
        );
        assert_eq!(store.upsert_concepts_bulk(&[legacy]).unwrap(), 0);
        // Same content WITH a digest: changed (backfill), then stable.
        let digest = "c".repeat(64);
        let filled = v2_upsert("m.py::module", "m", content, &now, Some(&digest));
        assert_eq!(
            store
                .upsert_concepts_bulk(std::slice::from_ref(&filled))
                .unwrap(),
            1
        );
        assert_eq!(store.upsert_concepts_bulk(&[filled]).unwrap(), 0);
    }

    /// §0.6 (DR-25): the 0.19 blob surface round-trips through a store
    /// opened by `CodeGraphStore` — and a missing digest reads as absent
    /// (None), not error, which is how pre-0.19 cold files must behave
    /// after the v21 -> v22 migration.
    #[test]
    fn blob_put_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blobs.db");
        let store = CodeGraphStore::open(&path).unwrap();
        let bytes = b"def f():\n    return 1\n";
        let digest = runtime().block_on(store.db.blob_put(bytes)).unwrap();
        assert_eq!(digest.len(), 64, "sha256 hex: {digest:?}");
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex: {digest:?}"
        );
        let back = runtime().block_on(store.db.blob_get(&digest)).unwrap();
        assert_eq!(back.as_deref(), Some(bytes.as_slice()));
        let missing = runtime()
            .block_on(store.db.blob_get(&"0".repeat(64)))
            .unwrap();
        assert_eq!(missing, None, "unknown digest reads as absent");
    }
}

#[cfg(test)]
mod stamped_write_tests {
    use super::*;

    #[test]
    fn nudge_passes_greater_stamps_unnudged() {
        let (stamp, nudged) = nudge_valid_from(
            Some("2026-10-29T10:00:00+00:00"),
            "2026-10-29T10:00:01+00:00",
        );
        assert!(!nudged);
        assert_eq!(stamp, "2026-10-29T10:00:01+00:00");
    }

    #[test]
    fn nudge_advances_ties_by_one_microsecond() {
        let (stamp, nudged) = nudge_valid_from(
            Some("2026-10-29T10:00:00+00:00"),
            "2026-10-29T10:00:00+00:00",
        );
        assert!(nudged, "a tie must be flagged for stamp_nudged");
        assert_eq!(stamp, "2026-10-29T10:00:00.000001Z");
    }

    #[test]
    fn nudge_compares_instants_not_offset_spellings() {
        // 10:00:01+01:00 is 09:00:01Z — before the 09:30Z bound despite
        // sorting after it lexicographically. String comparison would
        // pass this through; the nudge must catch it.
        let (stamp, nudged) =
            nudge_valid_from(Some("2026-10-29T09:30:00Z"), "2026-10-29T10:00:01+01:00");
        assert!(nudged);
        assert_eq!(stamp, "2026-10-29T09:30:00.000001Z");
    }

    #[test]
    fn nudge_rejects_regressing_stamps() {
        let (stamp, nudged) = nudge_valid_from(
            Some("2026-10-29T10:00:00+00:00"),
            "2026-10-28T10:00:00+00:00",
        );
        assert!(nudged, "a backwards stamp must never be written");
        assert_eq!(stamp, "2026-10-29T10:00:00.000001Z");
    }

    #[test]
    fn nudge_without_a_bound_passes_through() {
        let (stamp, nudged) = nudge_valid_from(None, "2026-10-29T10:00:00+00:00");
        assert!(!nudged);
        assert_eq!(stamp, "2026-10-29T10:00:00+00:00");
    }

    #[test]
    fn zulu_normalizer_folds_offsets_into_the_instant() {
        // Macrame's normalize_timestamp accepts only ...SS.ffffffZ, so
        // the write boundary speaks Zulu and the offset folds here.
        assert_eq!(
            zulu_valid_from("2026-10-29T10:00:01+01:00"),
            Some("2026-10-29T09:00:01.000000Z".to_string())
        );
        assert_eq!(
            zulu_valid_from("2026-10-29T10:00:00.000000Z"),
            Some("2026-10-29T10:00:00.000000Z".to_string())
        );
        assert_eq!(zulu_valid_from("not a stamp"), None);
    }

    fn one_module_projection() -> ProjectedGraph {
        use std::collections::HashMap;
        use std::sync::Arc;
        let mut modules = HashMap::new();
        modules.insert(
            "m.py::module".to_string(),
            Arc::new(crate::types::Module {
                id: "m.py::module".to_string(),
                name: "m".to_string(),
                path: std::path::PathBuf::from("m.py"),
                language: crate::types::Language::Python,
                package: None,
                exports: Vec::new(),
                star_exports: None,
                uses: Vec::new(),
                resolved_uses: Vec::new(),
                attr_reads: Vec::new(),
                classes: Vec::new(),
                functions: Vec::new(),
                imports: Vec::new(),
                constants: Vec::new(),
                type_aliases: Vec::new(),
                parse_quality: crate::types::ParseQuality::Clean,
                file_version: 1,
                content_hash: 0x9e37,
                embedding: crate::types::EmbeddingVec::default(),
            }),
        );
        ProjectedGraph {
            modules,
            classes: HashMap::new(),
            functions: HashMap::new(),
            imports: HashMap::new(),
            constants: HashMap::new(),
            type_aliases: HashMap::new(),
            routes: HashMap::new(),
            file_to_modules: HashMap::new(),
            module_by_dotted_name: HashMap::new(),
            module_path_index: HashMap::new(),
            importers: HashMap::new(),
            imports_by_importer: HashMap::new(),
            callers_by_callee: HashMap::new(),
            callees_by_caller: HashMap::new(),
            call_evidence: HashMap::new(),
            subclasses: HashMap::new(),
            implementers: HashMap::new(),
            instantiated_by: HashMap::new(),
            referenced_by: HashMap::new(),
            path_maps: crate::graph::path_maps::PathMaps::default(),
            decorated_by: HashMap::new(),
            overridden_by: HashMap::new(),
            overrides_base: HashMap::new(),
            ambiguous_bases: Vec::new(),
            synthetic_edges: Default::default(),
        }
    }

    /// P5a.2: a stamped flush lands the given `valid_from` — every
    /// concept the build mints carries the commit stamp, not the clock.
    #[test]
    fn stamped_concept_build_carries_the_given_stamp() {
        let projection = one_module_projection();
        let digests = SourceBlobDigests::new();
        let stamp = "2026-10-29T10:00:00+00:00";
        let built = build_v2_concepts_all_at(&projection, &digests, stamp);
        assert_eq!(built.len(), 1);
        assert!(built.iter().all(|c| c.valid_from == stamp));
        let filed = build_v2_concepts_for_file_at(&projection, "m.py", &digests, stamp);
        assert_eq!(filed.len(), 1);
        assert!(filed.iter().all(|c| c.valid_from == stamp));
        // And the default twin still stamps the clock.
        let clocked = build_v2_concepts_all(&projection, &digests);
        assert!(clocked.iter().all(|c| c.valid_from != stamp));
    }

    /// P5a.2: `history_of` over three commit-stamped generations returns
    /// commit order (newest first), each version carrying its commit stamp.
    #[test]
    fn stamped_spine_generations_read_back_in_commit_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("p5a2.db")).unwrap();
        let stamps = [
            "2026-10-29T10:00:00.000000Z",
            "2026-10-29T10:00:01.000000Z",
            "2026-10-29T10:00:02.000000Z",
        ];
        let mut digests = Vec::new();
        for (i, contents) in [b"x = 1\n".as_slice(), b"x = 2\n", b"x = 3\n"]
            .iter()
            .enumerate()
        {
            let digest = match store.put_source_blob("m.py", contents).unwrap() {
                BlobOutcome::Stored { digest, .. } => digest,
                outcome => panic!("blob {i} must store, got {outcome:?}"),
            };
            store
                .maintain_content_spine(&[("m.py::module".to_string(), digest.clone())], stamps[i])
                .unwrap();
            digests.push(digest);
        }
        let chain = store.history_of("m.py").unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].digest, digests[2]);
        assert_eq!(chain[0].valid_from, stamps[2]);
        assert_eq!(chain[1].digest, digests[1]);
        assert_eq!(chain[1].valid_from, stamps[1]);
        assert_eq!(chain[2].digest, digests[0]);
        assert_eq!(chain[2].valid_from, stamps[0]);
    }

    /// P5a.2 (H-14 pin): the chain follows the stamps, not the insertion
    /// order. An out-of-order ingest would silently reorder history with
    /// no error anywhere — which is exactly why INV-10 refuses it (P5a.3)
    /// instead of letting the reads sort it out.
    #[test]
    fn out_of_order_stamps_reorder_the_chain_silently() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("p5a2c.db")).unwrap();
        // Inserted d1, d2, d3 — but d2's stamp predates d1's.
        let stamps = [
            "2026-10-29T10:00:00.000000Z",
            "2026-10-29T09:00:01.000000Z",
            "2026-10-29T10:00:02.000000Z",
        ];
        let mut digests = Vec::new();
        for (i, contents) in [b"x = 1\n".as_slice(), b"x = 2\n", b"x = 3\n"]
            .iter()
            .enumerate()
        {
            let digest = match store.put_source_blob("m.py", contents).unwrap() {
                BlobOutcome::Stored { digest, .. } => digest,
                outcome => panic!("blob {i} must store, got {outcome:?}"),
            };
            store
                .maintain_content_spine(&[("m.py::module".to_string(), digest.clone())], stamps[i])
                .unwrap();
            digests.push(digest);
        }
        let chain = store.history_of("m.py").unwrap();
        assert_eq!(chain.len(), 3);
        // Stamp order, not insertion order: d1 sorts between d3 and d2.
        assert_eq!(chain[0].digest, digests[2]);
        assert_eq!(chain[1].digest, digests[0]);
        assert_eq!(chain[2].digest, digests[1]);
    }

    /// P5a.2: a stamped branch write lands the stamp on both rows it
    /// mints — the spine concept and the branch pointer concept.
    #[test]
    fn stamped_branch_write_lands_the_given_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("p5a2b.db")).unwrap();
        store.fork_branch("feat", "main").unwrap();
        let stamp = zulu_valid_from("2026-10-29T10:00:00+00:00").unwrap();
        let spine_id = store
            .write_branch_content_at("m.py", b"x = 1\n", "feat", &stamp)
            .unwrap();
        let pointer_id = branch_pointer_id("m.py", "feat");
        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        let stamps: std::collections::HashMap<String, String> = runtime()
            .block_on(async {
                let mut rows = conn
                    .query(
                        "SELECT id, valid_from FROM concepts WHERE id IN (?1, ?2)",
                        libsql::params![pointer_id.as_str(), spine_id.as_str()],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                let mut out = std::collections::HashMap::new();
                while let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                    out.insert(
                        row.get::<String>(0).unwrap_or_default(),
                        row.get::<String>(1).unwrap_or_default(),
                    );
                }
                Ok::<_, macrame::DbError>(out)
            })
            .unwrap();
        assert_eq!(
            stamps.get(&pointer_id).map(String::as_str),
            Some(stamp.as_str())
        );
        assert_eq!(
            stamps.get(&spine_id).map(String::as_str),
            Some(stamp.as_str())
        );
    }

    /// P5a.3: retiring at an explicit stamp closes the concept row at
    /// that stamp — a file deleted in a commit ends at the commit's
    /// time, not at ingest time.
    #[test]
    fn retire_entities_at_lands_the_given_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("ret.db")).unwrap();
        let v1 = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m"}"#)
            .valid_from("2026-01-01T10:00:00.000000Z")
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[v1]).unwrap(), 1);
        let (n, _) = store
            .retire_entities_at(&["m.py::module".to_string()], "2026-01-02T10:00:00.000000Z")
            .unwrap();
        assert_eq!(n, 1);
        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        let valid_to: String = runtime()
            .block_on(async {
                let mut rows = conn
                    .query(
                        "SELECT valid_to FROM concepts WHERE id = ?1 AND retired = 1",
                        libsql::params!["m.py::module"],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                let row = rows
                    .next()
                    .await
                    .map_err(macrame::DbError::Engine)?
                    .expect("retired row exists");
                Ok::<_, macrame::DbError>(row.get::<String>(0).unwrap_or_default())
            })
            .unwrap();
        assert_eq!(valid_to, "2026-01-02T10:00:00.000000Z");
    }

    /// P5a.3: the tree-scoped edge sweep closes a deleted call at the
    /// commit's stamp — `reconstruct` before the commit still sees it,
    /// after does not.
    #[test]
    fn retire_stale_edges_at_closes_at_the_given_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("rete.db")).unwrap();
        for id in ["a.py::f", "b.py::g"] {
            let v = macrame::ConceptUpsert::new(id, id)
                .content(r#"{"kind":"function","name":"x"}"#)
                .valid_from("2026-01-01T10:00:00.000000Z")
                .valid_to(TS_OPEN.to_string())
                .retired(false);
            store.upsert_concepts_bulk(&[v]).unwrap();
        }
        store
            .assert_edge(
                "a.py::f",
                "b.py::g",
                "CALLS",
                "{}",
                "2026-01-01T10:00:00.000000Z",
                1.0,
            )
            .unwrap();
        let retired = store
            .retire_stale_edges_at(
                &std::collections::HashSet::new(),
                "2026-01-02T10:00:00.000000Z",
            )
            .unwrap();
        assert_eq!(retired, 1);
        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        let valid_to: String = runtime()
            .block_on(async {
                let mut rows = conn
                    .query(
                        "SELECT valid_to FROM links WHERE source_id = ?1 \
                         AND target_id = ?2 AND edge_type = ?3 \
                         ORDER BY valid_from DESC LIMIT 1",
                        libsql::params!["a.py::f", "b.py::g", "CALLS"],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                let row = rows
                    .next()
                    .await
                    .map_err(macrame::DbError::Engine)?
                    .expect("edge row exists");
                Ok::<_, macrame::DbError>(row.get::<String>(0).unwrap_or_default())
            })
            .unwrap();
        assert_eq!(valid_to, "2026-01-02T10:00:00.000000Z");
    }

    /// P5a.3: the commit kind is ledger infrastructure — the cold-start
    /// classifier skips it exactly like the spine and the pointers, so a
    /// store with ingested history still loads.
    #[test]
    fn commit_kind_classifies_as_spine() {
        let content = r#"{"meta_version":2,"kind":"commit","sha":"abc"}"#;
        assert!(matches!(
            classify_v2_concept(content),
            V2ConceptClass::Spine
        ));
    }

    /// P5a.3 (INV-12): the stamping-mode flag round-trips through `kv` —
    /// the mechanism both the ingest claim and the analyze refusal read.
    #[test]
    fn stamping_mode_flag_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("kv.db")).unwrap();
        assert_eq!(
            store
                .kv_get_sync(crate::graph::ingest::STAMPING_MODE_KEY)
                .unwrap(),
            None
        );
        store
            .kv_put_sync(
                crate::graph::ingest::STAMPING_MODE_KEY,
                crate::graph::ingest::MODE_COMMIT_TIME,
            )
            .unwrap();
        assert_eq!(
            store
                .kv_get_sync(crate::graph::ingest::STAMPING_MODE_KEY)
                .unwrap()
                .as_deref(),
            Some(crate::graph::ingest::MODE_COMMIT_TIME)
        );
    }

    /// P5a.4 (DR-41): a tombstone reads as absent on its lineage and
    /// changes nothing on the trunk. The file is gone on the branch even
    /// though the trunk still holds it — absence, never trunk fallback,
    /// never empty bytes.
    #[test]
    fn tombstone_reads_as_absent_not_fallback() {
        // Local twin of `tests::source_ref_store`: a trunk module with a
        // digest plus one function (separate test modules, no shared
        // fixtures across them).
        fn trunk_module_store(dir: &std::path::Path) -> CodeGraphStore {
            let store = CodeGraphStore::open(dir.join("refs.db")).unwrap();
            let digest = "c".repeat(64);
            let module_extra =
                format!(r#"{{"coderadar":{{"format":1,"source_blob":"{digest}"}}}}"#);
            let module = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
                .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
                .extra(module_extra)
                .valid_from(now_iso8601())
                .valid_to(TS_OPEN.to_string())
                .retired(false);
            assert_eq!(store.upsert_concepts_bulk(&[module]).unwrap(), 1);
            std::thread::sleep(std::time::Duration::from_millis(2));
            store
        }
        let dir = tempfile::tempdir().unwrap();
        let store = trunk_module_store(dir.path());
        store.fork_branch("feat", "main").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let stamp = now_iso8601();
        store
            .write_branch_tombstone_at("m.py", "feat", &stamp)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let at = now_iso8601();
        let gone = store
            .entity_source_ref_at("m.py::module", &at, "feat")
            .unwrap();
        assert!(gone.is_none(), "tombstoned on feat: {gone:?}");
        let kept = store
            .entity_source_ref_at("m.py::module", &at, "main")
            .unwrap()
            .expect("trunk still holds m.py");
        assert_eq!(kept.digest.as_deref(), Some("c".repeat(64).as_str()));
    }

    /// P5a.4 (DR-41): restating a trunk-owned id on a branch refuses with
    /// `CrossLineage` — the D-225 rule the whole branch-write design
    /// stands on. Pointers and branch-minted ids exist because this
    /// refuses; the ingest routes around it instead of catching it.
    #[test]
    fn branch_restate_of_trunk_id_refuses_cross_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("restate.db")).unwrap();
        let module = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[module]).unwrap(), 1);
        store.fork_branch("feat", "main").unwrap();
        let restate = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py","changed":true}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false)
            .on_branch(macrame::BranchId::new("feat").unwrap());
        match store.upsert_concepts_bulk(&[restate]) {
            Err(macrame::DbError::CrossLineage { .. }) => {}
            other => {
                panic!("restating a trunk id on a branch must refuse CrossLineage, got {other:?}")
            }
        }
    }

    /// P5a.4 (DR-41): retiring a branch-minted row carries its mint
    /// lineage on the retire upsert — retiring through a trunk upsert
    /// would trip `trg_concepts_cross_lineage` on the row's own id.
    /// Trunk retires round-trip to the trunk default (existing tests
    /// cover that path unchanged).
    #[test]
    fn retire_carries_the_mint_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("retline.db")).unwrap();
        store.fork_branch("feat", "main").unwrap();
        let minted = macrame::ConceptUpsert::new("n.py::module", "n.py::module")
            .content(r#"{"meta_version":2,"kind":"module","name":"n.py","file_path":"n.py"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false)
            .on_branch(macrame::BranchId::new("feat").unwrap());
        assert_eq!(store.upsert_concepts_bulk(&[minted]).unwrap(), 1);
        let (n, _) = store
            .retire_entities_at(&["n.py::module".to_string()], &now_iso8601())
            .unwrap();
        assert_eq!(n, 1);
        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        let (retired, branch_id): (i64, String) = runtime()
            .block_on(async {
                let mut rows = conn
                    .query(
                        "SELECT retired, branch_id FROM concepts WHERE id = ?1",
                        libsql::params!["n.py::module"],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                let row = rows
                    .next()
                    .await
                    .map_err(macrame::DbError::Engine)?
                    .expect("retired row exists");
                Ok::<_, macrame::DbError>((
                    row.get::<i64>(0).unwrap_or(0),
                    row.get::<String>(1).unwrap_or_default(),
                ))
            })
            .unwrap();
        assert_eq!(retired, 1);
        assert_eq!(branch_id, "feat");
    }

    /// P5a.5 (DR-41): the embedding-hash flush restates rows it did not
    /// mint, so each upsert rides the row's mint lineage — a branch-minted
    /// entity flushed on the trunk tripped the cross-lineage trigger on
    /// its own id (fail-open `[diag]`, hashes never landed).
    #[test]
    fn embedding_hash_flush_carries_the_mint_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("embline.db")).unwrap();
        store.fork_branch("feat", "main").unwrap();
        let minted = macrame::ConceptUpsert::new("e.py::g", "e.py::g")
            .content(r#"{"meta_version":2,"kind":"function","name":"g","file_path":"e.py"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false)
            .on_branch(macrame::BranchId::new("feat").unwrap());
        assert_eq!(store.upsert_concepts_bulk(&[minted]).unwrap(), 1);
        let n = store
            .flush_embedding_hashes(&[("e.py::g".to_string(), "keyed-hash".to_string())])
            .unwrap();
        assert_eq!(n, 1);
        let (_guard, conn) = store.open_diagnostic_conn().unwrap();
        let (content, branch_id): (String, String) = runtime()
            .block_on(async {
                let mut rows = conn
                    .query(
                        "SELECT content, branch_id FROM concepts WHERE id = ?1",
                        libsql::params!["e.py::g"],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                let row = rows
                    .next()
                    .await
                    .map_err(macrame::DbError::Engine)?
                    .expect("flushed row exists");
                Ok::<_, macrame::DbError>((
                    row.get::<String>(0).unwrap_or_default(),
                    row.get::<String>(1).unwrap_or_default(),
                ))
            })
            .unwrap();
        assert_eq!(branch_id, "feat");
        assert!(content.contains("keyed-hash"), "hash landed: {content}");
    }

    /// P5a.4 (DR-41): sweep candidates are mint-lineage rows. A trunk
    /// sweep never retires a branch-minted module (it is not in the
    /// trunk's tree and never will be); a branch sweep never touches
    /// trunk rows (global ids — the candidate set is where the scoping
    /// lives).
    #[test]
    fn sweep_candidates_are_mint_lineage_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("sweep.db")).unwrap();
        let trunk_module = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[trunk_module]).unwrap(), 1);
        store.fork_branch("feat", "main").unwrap();
        let minted = macrame::ConceptUpsert::new("n.py::module", "n.py::module")
            .content(r#"{"meta_version":2,"kind":"module","name":"n.py","file_path":"n.py"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false)
            .on_branch(macrame::BranchId::new("feat").unwrap());
        assert_eq!(store.upsert_concepts_bulk(&[minted]).unwrap(), 1);
        let trunk_keep: std::collections::HashSet<String> =
            ["m.py::module".to_string()].into_iter().collect();
        // The branch-minted module is live, file-annotated, and absent
        // from the trunk keep set — an unscoped sweep would retire it.
        let retired = store
            .retire_stale_file_concepts_at(&trunk_keep, &now_iso8601(), "main")
            .unwrap();
        assert_eq!(retired, 0, "trunk sweep must spare branch-minted rows");
        let retired = store
            .retire_stale_file_concepts_at(
                &std::collections::HashSet::new(),
                &now_iso8601(),
                "feat",
            )
            .unwrap();
        assert_eq!(retired, 1, "branch sweep retires its own stale rows");
    }

    /// P5a.4 (DR-41): `mint_lineage` names the mint lineage — the query
    /// both ingest lineages consult before minting entity concepts.
    #[test]
    fn mint_lineage_names_the_mint_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeGraphStore::open(dir.path().join("mint.db")).unwrap();
        let trunk_module = macrame::ConceptUpsert::new("m.py::module", "m.py::module")
            .content(r#"{"kind":"module","name":"m.py","file_path":"m.py"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false);
        assert_eq!(store.upsert_concepts_bulk(&[trunk_module]).unwrap(), 1);
        assert!(
            matches!(
                store.mint_lineage("m.py::module").unwrap().as_deref(),
                None | Some("main")
            ),
            "trunk rows read as trunk (or default)"
        );
        assert_eq!(store.mint_lineage("nope.py::module").unwrap(), None);
        store.fork_branch("feat", "main").unwrap();
        let minted = macrame::ConceptUpsert::new("n.py::module", "n.py::module")
            .content(r#"{"meta_version":2,"kind":"module","name":"n.py","file_path":"n.py"}"#)
            .valid_from(now_iso8601())
            .valid_to(TS_OPEN.to_string())
            .retired(false)
            .on_branch(macrame::BranchId::new("feat").unwrap());
        assert_eq!(store.upsert_concepts_bulk(&[minted]).unwrap(), 1);
        assert_eq!(
            store.mint_lineage("n.py::module").unwrap().as_deref(),
            Some("feat")
        );
    }
}
