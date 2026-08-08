// CodeRadar v3.5 — Macrame Storage Interface (§10)
// Bridges CodeRadar's entity model to Macrame's concept+assertion model.
// Macrame is tokio-based; CodeRadar wraps it with block_on behind a sync API.

use macrame::prelude::*;
use macrame::graph::{Subgraph, EdgeAssertion, TraversalBuilder};
use macrame::temporal::{MaterializedState, SnapshotCadence};
use std::path::Path;
use std::sync::Arc;
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

/// Key lifespan timestamp — the Macrame open sentinel for "still true".
pub const TS_OPEN: &str = "9999-12-31T00:00:00.000000Z";

// ── CodeGraphStore — owns the Macrame Database + Runtime ────────────────────

pub struct CodeGraphStore {
    pub db: Database,
    /// Held for the lifetime of the Database. Tokio's block_on needs a runtime.
    #[allow(dead_code)]
    runtime: Runtime,
}

impl CodeGraphStore {
    /// Open or create a Macrame database at `path`.
    pub fn open(path: impl AsRef<Path>) -> macrame::Result<Self> {
        let runtime = Runtime::new().expect("tokio runtime for Macrame");
        let db = runtime.block_on(Database::open(path))?;
        Ok(Self { db, runtime })
    }

    /// Open with a snapshot cadence (production).
    pub fn open_with_cadence(
        path: impl AsRef<Path>,
        cadence: Option<SnapshotCadence>,
    ) -> macrame::Result<Self> {
        let runtime = Runtime::new().expect("tokio runtime");
        let db = runtime.block_on(Database::open_with_cadence(path, cadence))?;
        Ok(Self { db, runtime })
    }

    /// Synchronous wrapper around Macrame's async upsert.
    pub fn upsert_entity(&self, unit: &ExtractedUnit, file_path: &str, language: &str) -> macrame::Result<()> {
        let concept = build_concept(unit, file_path, language);
        self.runtime.block_on(self.db.upsert_concept(concept))
    }

    /// Synchronous wrapper — bulk upsert entities from one file.
    pub fn upsert_entities(
        &self,
        units: &[ExtractedUnit],
        file_path: &str,
        language: &str,
    ) -> macrame::Result<()> {
        for unit in units {
            let concept = build_concept(unit, file_path, language);
            self.runtime.block_on(self.db.upsert_concept(concept))?;
        }
        Ok(())
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
        self.runtime.block_on(self.db.assert_edge(edge))
    }

    /// Bulk assert edges (atomic — one transaction stamp per D-014).
    pub fn assert_edges_bulk(&self, edges: Vec<EdgeAssertion>) -> macrame::Result<usize> {
        self.runtime.block_on(self.db.write_bulk_atomic(edges))
    }

    /// Traverse the graph from a source entity.
    pub fn traverse(
        &self,
        start_id: &str,
        max_depth: usize,
        edge_types: &[&str],
    ) -> macrame::Result<Subgraph> {
        let mut traversal = TraversalBuilder::new(start_id)
            .max_depth(max_depth);
        if !edge_types.is_empty() {
            traversal = traversal.edge_types(edge_types.iter().map(|s| s.to_string()).collect());
        }
        self.runtime.block_on(self.db.load_subgraph_with(&traversal, "now", 10_000_000))
    }

    /// Reconstruct state as of a timestamp.
    pub fn reconstruct(&self, ts: &str) -> macrame::Result<MaterializedState> {
        self.runtime.block_on(self.db.reconstruct(ts))
    }
}

// ── Concept Builder ─────────────────────────────────────────────────────────

/// Build a Macrame ConceptUpsert from a CodeRadar ExtractedUnit.
/// Entity metadata is stored as JSON in the `content` field.
fn build_concept(unit: &ExtractedUnit, file_path: &str, language: &str) -> ConceptUpsert {
    let entity_id = unit.entity_id();
    let (title, kind, metadata) = entity_meta(unit, file_path, language);

    let content = serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".into());

    ConceptUpsert {
        id: entity_id,
        title: title.to_string(),
        content,
        embedding_model: None,
        valid_from: String::new(), // defaults to "now" on upsert
        valid_to: TS_OPEN.to_string(),
        retired: false,
    }
}

/// Build entity metadata JSON for a ConceptUpsert.content field.
fn entity_meta<'a>(unit: &'a ExtractedUnit, file_path: &'a str, language: &'a str) -> (&'a str, &'a str, serde_json::Value) {
    use serde_json::{json, Value};

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
    use serde_json::{json, Value};
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
            signature_hash: 0,
            body_hash: 0xdead,
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
}
