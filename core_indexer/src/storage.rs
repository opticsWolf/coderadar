// CodeRadar v3.6 — Macrame Storage Interface (§10)
// Bridges CodeRadar's entity model to Macrame's concept+assertion model.
// Macrame is tokio-based; CodeRadar wraps it with block_on behind a sync API.

use macrame::prelude::*;
use macrame::graph::{Subgraph, EdgeAssertion, TraversalBuilder};
use macrame::temporal::{MaterializedState, SnapshotCadence};
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
    pub const GRAMMAR_KIND: &str = "grammar_kind";  // v3.6: tree-sitter node kind
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
}

/// Serialises every libSQL open in this process — `Database::open` and
/// `diagnostic_conn` alike (see [`CodeGraphStore::open_diagnostic_conn`]).
static DIAGNOSTIC_OPEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Key lifespan timestamp — the Macrame open sentinel for "still true".
pub const TS_OPEN: &str = "9999-12-31T00:00:00.000000Z";

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
fn runtime() -> &'static Runtime {
    static RUNTIME: std::sync::LazyLock<Runtime> = std::sync::LazyLock::new(|| {
        Runtime::new().expect("tokio runtime for Macrame")
    });
    &RUNTIME
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
        Ok(Self { db })
    }

    /// Synchronous wrapper around Macrame's async upsert.
    pub fn upsert_entity(&self, unit: &ExtractedUnit, file_path: &str, language: &str) -> macrame::Result<()> {
        let concept = build_concept(unit, file_path, language);
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
        if units.is_empty() {
            return Ok(());
        }
        let concepts: Vec<ConceptUpsert> = units
            .iter()
            .map(|unit| build_concept(unit, file_path, language))
            .collect();
        runtime().block_on(self.db.write_concepts(concepts))?;
        Ok(())
    }

    /// Bulk-persist pre-built concepts via Macrame's `write_concepts`.
    /// Chunked internally at 70/chunk, one transaction per chunk.
    /// For batch index: 2,191 concepts ≈ 32 chunks ≈ 75ms.
    pub fn upsert_concepts_bulk(&self, concepts: &[ConceptUpsert]) -> macrame::Result<usize> {
        if concepts.is_empty() {
            return Ok(0);
        }
        runtime().block_on(self.db.write_concepts(concepts.to_vec()))
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
    fn open_diagnostic_conn(
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
    pub fn retire_entities(&self, entity_ids: &[String]) -> macrame::Result<(usize, usize)> {
        if entity_ids.is_empty() {
            return Ok((0, 0));
        }
        let ts = now_iso8601();
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {

            // Title and content are carried over: the upsert overwrites every
            // column, so reading them back is what keeps the retired row a
            // record of the entity rather than a blank tombstone.
            let mut concepts: Vec<ConceptUpsert> = Vec::new();
            for id in entity_ids {
                let mut rows = conn
                    .query(
                        "SELECT title, content, valid_from FROM concepts \
                         WHERE id = ?1 AND retired = 0",
                        libsql::params![id.as_str()],
                    )
                    .await
                    .map_err(macrame::DbError::Engine)?;
                if let Some(row) = rows.next().await.map_err(macrame::DbError::Engine)? {
                    concepts.push(ConceptUpsert {
                        id: id.clone(),
                        title: row.get::<String>(0).unwrap_or_default(),
                        content: row.get::<String>(1).unwrap_or_default(),
                        embedding_model: None,
                        valid_from: row.get::<String>(2).unwrap_or_else(|_| ts.clone()),
                        valid_to: ts.clone(),
                        retired: true,
                    });
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
                    .retire_edge(source.as_str(), target.as_str(), etype.as_str(),
                                 valid_from.as_str(), ts.as_str())
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

    /// Ids of every concept the ledger still holds to be true.
    ///
    /// The counterpart to [`Self::retire_entities`]: retirement is only
    /// meaningful if something can tell the difference, and until now nothing
    /// read the `retired` column at all.
    pub fn live_concept_ids(&self) -> macrame::Result<Vec<String>> {
        let (_diag_guard, conn) = self.open_diagnostic_conn()?;
        runtime().block_on(async {
            let mut rows = conn
                .query("SELECT id FROM concepts WHERE retired = 0 ORDER BY id", ())
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
    pub fn open_edge_triples(
        &self,
        now: &str,
    ) -> macrame::Result<Vec<(String, String, String)>> {
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

    /// Traverse the graph as it existed at `ts` (temporal read).
    pub fn traverse_at(
        &self,
        start_id: &str,
        max_depth: usize,
        edge_types: &[String],
        ts: &str,
    ) -> macrame::Result<Subgraph> {
        let mut traversal = TraversalBuilder::new(start_id)
            .max_depth(max_depth);
        if !edge_types.is_empty() {
            traversal = traversal.edge_types(edge_types.to_vec());
        }
        runtime().block_on(self.db.load_subgraph_with(&traversal, ts, 10_000_000))
    }

    /// Reconstruct state as of a timestamp.
    pub fn reconstruct(&self, ts: &str) -> macrame::Result<MaterializedState> {
        runtime().block_on(self.db.reconstruct(ts))
    }
}

// ── Concept Builder ─────────────────────────────────────────────────────────

/// Build a Macrame ConceptUpsert from a CodeRadar ExtractedUnit.
/// Entity metadata is stored as JSON in the `content` field.
pub fn build_concept(unit: &ExtractedUnit, file_path: &str, language: &str) -> ConceptUpsert {
    let entity_id = unit.entity_id();
    let (title, _kind, metadata) = entity_meta(unit, file_path, language);

    let content = serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".into());

    let valid_from = now_iso8601();

    ConceptUpsert {
        id: entity_id,
        title: title.to_string(),
        content,
        embedding_model: None,
        valid_from,
        valid_to: TS_OPEN.to_string(),
        retired: false,
    }
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
fn entity_meta<'a>(unit: &'a ExtractedUnit, file_path: &'a str, language: &'a str) -> (&'a str, &'a str, serde_json::Value) {
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
pub const V2_CANONICAL_KINDS: &[&str] =
    &["module", "class", "function", "import", "constant", "type_alias"];

fn file_path_of(id: &str) -> &str {
    // Entity ids are `{file_path}::{rest}`; file paths never contain "::".
    match id.split_once("::") {
        Some((file, _)) => file,
        None => id,
    }
}

fn v2_common(kind: &str, name: &str, file_path: &str, line: u64, exit_line: u64, docstring: &Option<String>, decorators: &[String]) -> serde_json::Value {
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

fn v2_upsert(id: &str, title: &str, content: serde_json::Value, now: &str) -> ConceptUpsert {
    ConceptUpsert {
        id: id.to_string(),
        title: title.to_string(),
        content: content.to_string(),
        embedding_model: None,
        valid_from: now.to_string(),
        valid_to: TS_OPEN.to_string(),
        retired: false,
    }
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
    v
}

fn class_content(c: &Class) -> serde_json::Value {
    let file_path = file_path_of(&c.id);
    let mut v = v2_common("class", &c.name, file_path, c.line as u64, c.exit_line as u64, &c.docstring, &c.decorators);
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
    v
}

fn function_content(f: &Function) -> serde_json::Value {
    let file_path = file_path_of(&f.id);
    let mut v = v2_common("function", &f.name, file_path, f.line as u64, f.exit_line as u64, &f.docstring, &f.decorators);
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
    v
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
    v
}

fn type_alias_content(t: &TypeAlias) -> serde_json::Value {
    let file_path = file_path_of(&t.id);
    let mut v = v2_common("type_alias", &t.name, file_path, 0, 0, &None, &[]);
    v["target"] = serde_json::json!(&t.target);
    v["source"] = serde_json::json!(&t.source);
    v["span"] = serde_json::json!(&t.span);
    v["name_span"] = serde_json::json!(&t.name_span);
    v
}

/// Build concept JSON v2 [`ConceptUpsert`]s for the WHOLE projection.
///
/// Call this on the FINAL projection (post-cascade), BEFORE
/// `persist_edges`, because edges reference concept ids and the concept
/// upsert must land first.
pub fn build_v2_concepts_all(projection: &ProjectedGraph) -> Vec<ConceptUpsert> {
    let now = now_iso8601();
    let mut out: Vec<ConceptUpsert> = Vec::new();
    for m in projection.modules.values() {
        out.push(v2_upsert(&m.id, &m.name, module_content(m), &now));
    }
    for c in projection.classes.values() {
        out.push(v2_upsert(&c.id, &c.name, class_content(c), &now));
    }
    for f in projection.functions.values() {
        out.push(v2_upsert(&f.id, &f.name, function_content(f), &now));
    }
    for i in projection.imports.values() {
        out.push(v2_upsert(&i.id, &i.raw, import_content(i), &now));
    }
    for k in projection.constants.values() {
        out.push(v2_upsert(&k.id, &k.name, constant_content(k), &now));
    }
    for t in projection.type_aliases.values() {
        out.push(v2_upsert(&t.id, &t.name, type_alias_content(t), &now));
    }
    out
}

/// Build concept JSON v2 upserts for the entities of ONE file (id prefix
/// `{file_path}::`). Used by `update_file` after its scoped cascade.
pub fn build_v2_concepts_for_file(projection: &ProjectedGraph, file_path: &str) -> Vec<ConceptUpsert> {
    let prefix = format!("{file_path}::");
    let now = now_iso8601();
    let mut out: Vec<ConceptUpsert> = Vec::new();
    for m in projection.modules.values() {
        if m.id.starts_with(&prefix) {
            out.push(v2_upsert(&m.id, &m.name, module_content(m), &now));
        }
    }
    for c in projection.classes.values() {
        if c.id.starts_with(&prefix) {
            out.push(v2_upsert(&c.id, &c.name, class_content(c), &now));
        }
    }
    for f in projection.functions.values() {
        if f.id.starts_with(&prefix) {
            out.push(v2_upsert(&f.id, &f.name, function_content(f), &now));
        }
    }
    for i in projection.imports.values() {
        if i.id.starts_with(&prefix) {
            out.push(v2_upsert(&i.id, &i.raw, import_content(i), &now));
        }
    }
    for k in projection.constants.values() {
        if k.id.starts_with(&prefix) {
            out.push(v2_upsert(&k.id, &k.name, constant_content(k), &now));
        }
    }
    for t in projection.type_aliases.values() {
        if t.id.starts_with(&prefix) {
            out.push(v2_upsert(&t.id, &t.name, type_alias_content(t), &now));
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
    u64::from_str_radix(s, 16)
        .map_err(|e| v2_err(id, &format!("'{key}' is not hex: {e}")))
}

fn str_list(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
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
    v.get(key).and_then(|x| serde_json::from_value::<T>(x.clone()).ok())
}

fn deser_or_default<T: serde::de::DeserializeOwned + Default>(v: &serde_json::Value, key: &str) -> T {
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
        classes: str_list(v, "classes"),
        functions: str_list(v, "functions"),
        imports: str_list(v, "imports"),
        constants: str_list(v, "constants"),
        type_aliases: str_list(v, "type_aliases"),
        parse_quality: ParseQuality::from_json(&parse_quality).map_err(|e| v2_err(id, &e))?,
        file_version: v.get("file_version").and_then(|x| x.as_u64()).unwrap_or(1),
        content_hash: hex_u64(v, "content_hash", id)?,
        embedding: EmbeddingVec::default(),
    })
}

fn parse_v2_class(id: &str, v: &serde_json::Value) -> std::result::Result<Class, String> {
    let file_path = req_str(v, "file_path", id)?;
    let parse_quality = req_str(v, "parse_quality", id)?;
    Ok(Class {
        id: id.to_string(),
        name: req_str(v, "name", id)?,
        grammar_kind: v.get("grammar_kind").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        parent_module: format!("{file_path}::module"),
        parent_class: opt_str(v, "parent_class"),
        bases: deser_or_default(v, "bases"),
        // Cascade-rebuilt, never persisted:
        resolved_bases: vec![],
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
        embedding: EmbeddingVec::default(),
    })
}

fn parse_v2_function(id: &str, v: &serde_json::Value) -> std::result::Result<Function, String> {
    let file_path = req_str(v, "file_path", id)?;
    let parse_quality = req_str(v, "parse_quality", id)?;
    let kind = deser::<FunctionKind>(v, "fn_kind")
        .ok_or_else(|| v2_err(id, "missing 'fn_kind'"))?;
    Ok(Function {
        id: id.to_string(),
        name: req_str(v, "name", id)?,
        parent_module: format!("{file_path}::module"),
        parent_class: opt_str(v, "parent_class"),
        parameters: deser_or_default(v, "parameters"),
        return_type: opt_str(v, "return_type"),
        // Raw per-call-site refs are deliberately NOT persisted (design);
        // cold start skips resolve_all_calls.
        calls: vec![],
        resolved_calls: deser_or_default(v, "resolved_calls"),
        decorators: str_list(v, "decorators"),
        setter_of: None,
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
        embedding: EmbeddingVec::default(),
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
        embedding: EmbeddingVec::default(),
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
        embedding: EmbeddingVec::default(),
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
        embedding: EmbeddingVec::default(),
    })
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
            }],
            resolved_bases: vec![],
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
            span: ByteSpan { start: 30, end: 200 },
            name_span: ByteSpan { start: 36, end: 41 },
            body_span: ByteSpan { start: 42, end: 199 },
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
                    },
                },
            ],
            decorators: vec![],
            setter_of: None,
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
            span: ByteSpan { start: 100, end: 250 },
            name_span: ByteSpan { start: 112, end: 118 },
            params_span: ByteSpan { start: 119, end: 123 },
            body_span: ByteSpan { start: 124, end: 249 },
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
    fn module_roundtrip() {
        let m = sample_module();
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
        let f = sample_function();
        let json = function_content(&f).to_string();
        match parse_v2_concept(&f.id, &json).unwrap() {
            V2Entity::Function(got) => {
                assert_eq!(got.name, f.name);
                assert_eq!(got.parent_module, f.parent_module);
                assert_eq!(got.parameters, f.parameters);
                assert_eq!(got.return_type, f.return_type);
                assert_eq!(got.kind, f.kind);
                assert_eq!(got.resolved_calls, f.resolved_calls);
                assert!(got.calls.is_empty(), "raw call sites are not persisted");
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
    fn classify_v2_concept_variants() {
        let m = sample_module();
        match classify_v2_concept(&module_content(&m).to_string()) {
            V2ConceptClass::Canonical(k) => assert_eq!(k, "module"),
            other => panic!("expected Canonical, got {other:?}"),
        }
        let v1 = r#"{"kind": "function", "name": "f", "file_path": "a.py"}"#;
        assert!(matches!(classify_v2_concept(v1), V2ConceptClass::V1Leftover));
        let field = r#"{"kind": "field", "name": "x", "file_path": "a.py"}"#;
        assert!(matches!(classify_v2_concept(field), V2ConceptClass::StandaloneField));
        assert!(matches!(classify_v2_concept("not json"), V2ConceptClass::Unreadable));
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
        assert!(ts[20..26].chars().all(|c| c.is_ascii_digit()), "frac: {ts:?}");
        // And the normalizer accepts it unchanged (the call load_snapshot
        // must survive — `reconstruct(&now_iso8601())`).
        assert_eq!(
            macrame::util::timestamp::normalize(&ts).unwrap(),
            ts
        );
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
        let concept = macrame::ConceptUpsert {
            id: "a.py::module".into(),
            title: "a".into(),
            content: r#"{"meta_version": 2, "kind": "module"}"#.into(),
            embedding_model: None,
            valid_from: now_iso8601(),
            valid_to: TS_OPEN.to_string(),
            retired: false,
        };
        assert_eq!(store.upsert_concepts_bulk(&[concept]).unwrap(), 1);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let state = store.reconstruct(&now_iso8601()).unwrap();
        assert!(
            state.concepts.contains_key("a.py::module"),
            "reconstruct at now must see the concept written microseconds ago (predates_recorded_history = {})",
            state.predates_recorded_history
        );
        assert!(state.seq_anchor > 0, "empty fold: seq_anchor = {}", state.seq_anchor);
    }

    #[test]
    fn now_timestamp_rejects_the_now_pitfall() {
        // The value that would be *wrong*: the literal `"now"` (and a few
        // other common-but-invalid shapes) must be rejected by macrame, so
        // that a regression in [`now_iso8601`] toward any of them is a
        // loud failure rather than a silent broken ledger.
        for bad in ["now", "latest", "2024-01-01T00:00:00Z", "2024-01-01T00:00:00.123Z"] {
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
            span: ByteSpan { start: 1000, end: 1500 },
            name_span: ByteSpan { start: 1004, end: 1007 },
            params_span: ByteSpan { start: 1008, end: 1020 },
            body_span: ByteSpan { start: 1030, end: 1490 },
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
        assert!(meta["decorators"].as_str().unwrap().contains("@staticmethod"));
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
            span: ByteSpan { start: 200, end: 800 },
            name_span: ByteSpan { start: 206, end: 213 },
            body_span: ByteSpan { start: 220, end: 790 },
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

    fn seed(store: &CodeGraphStore) {
        let ts = now_iso8601();
        let concept = |id: &str| ConceptUpsert {
            id: id.to_string(),
            title: id.rsplit("::").next().unwrap().to_string(),
            content: "{}".to_string(),
            embedding_model: None,
            valid_from: ts.clone(),
            valid_to: TS_OPEN.to_string(),
            retired: false,
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

        let (concepts, _) = store.retire_entities(&["a.py::callee".to_string()]).unwrap();

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

        let (_, edges) = store.retire_entities(&["a.py::callee".to_string()]).unwrap();

        assert_eq!(edges, 2, "both the incoming and the outgoing edge close");
        assert_eq!(open_edge_count(&store), 0);
    }

    #[test]
    fn test_retire_entities_leaves_untouched_entities_alone() {
        let (_dir, store) = temp_store();
        seed(&store);

        store.retire_entities(&["b.py::bystander".to_string()]).unwrap();

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
            store.retire_entities(&["nowhere.py::ghost".to_string()]).unwrap(),
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

        store.retire_entities(&["a.py::callee".to_string()]).unwrap();
        let (concepts, edges) = store.retire_entities(&["a.py::callee".to_string()]).unwrap();

        assert_eq!((concepts, edges), (0, 0), "nothing left open to close");
        assert_eq!(live_concept_ids(&store).len(), 2);
    }
}
