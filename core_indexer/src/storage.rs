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

/// Current UTC time as an RFC 3339 timestamp (no chrono dependency).
/// Used as `valid_from` for concepts AND edges so the bitemporal ledger can
/// distinguish *when* a fact was asserted (temporal traversal depends on it).
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days_since_epoch = secs / 86400;
    let secs_of_day = secs % 86400;
    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    let seconds = secs_of_day % 60;
    let (y, m, d) = civil_from_days(days_since_epoch as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000000Z", y, m, d, hours, minutes, seconds)
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
