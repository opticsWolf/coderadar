// Row-shape tests for `core_indexer::api` (plan unit R0).
//
// `api::entity_json` and friends replace the PyO3 `*_to_dict` builders when
// the Rust binary answers a CLI/MCP read. They drifted from
// `ffi_convert.rs` — missing `name_span_*`, `has_embedding`,
// `embedding_hash` on classes, imports, constants and type aliases — which
// made the two front doors return different JSON for the same entity. These
// tests pin the shared field set by construction from one indexed file.

use super::*;
use crate::api;

fn graph_with(source: &str) -> CodeGraph {
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file(source, "rows.py", &Language::Python)
        .expect("index the fixture file");
    graph
}

const SOURCE: &str = r##"
from os import path
LIMIT: int = 3


class Widget:
    """A widget."""

    def bump(self, n: int) -> int:
        return n + 1


def top(item: Widget) -> int:
    return item.bump(LIMIT)
"""##;

fn keys(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn entity_rows_carry_the_binding_field_set() {
    let graph = graph_with(SOURCE);
    let snap = graph.snapshot();

    let class = api::entity_json(&snap, "rows.py::Widget").expect("class row");
    for key in [
        "id",
        "name",
        "grammar_kind",
        "kind",
        "parent_module",
        "file_path",
        "line",
        "end_line",
        "start_line",
        "decorators",
        "span_start",
        "span_end",
        "name_span_start",
        "name_span_end",
        "bases",
        "has_embedding",
        "embedding_hash",
    ] {
        assert!(
            keys(&class).iter().any(|k| k == key),
            "class row is missing {key}: {:?}",
            keys(&class)
        );
    }

    let function = api::entity_json(&snap, "rows.py::top").expect("function row");
    for key in [
        "signature",
        "references",
        "is_async",
        "is_generator",
        "name_span_start",
        "name_span_end",
        "has_embedding",
        "embedding_hash",
    ] {
        assert!(
            keys(&function).iter().any(|k| k == key),
            "function row is missing {key}: {:?}",
            keys(&function)
        );
    }

    let import = snap
        .imports
        .keys()
        .next()
        .cloned()
        .expect("the fixture imports os.path");
    let import_row = api::entity_json(&snap, &import).expect("import row");
    for key in [
        "id",
        "name",
        "kind",
        "line",
        "start_line",
        "name_span_start",
        "name_span_end",
        "has_embedding",
        "embedding_hash",
    ] {
        assert!(
            keys(&import_row).iter().any(|k| k == key),
            "import row is missing {key}: {:?}",
            keys(&import_row)
        );
    }

    let constant_id = snap.constants.keys().next().cloned().expect("one constant");
    let constant = api::entity_json(&snap, &constant_id).expect("constant row");
    for key in ["span_start", "span_end", "has_embedding", "embedding_hash"] {
        assert!(
            keys(&constant).iter().any(|k| k == key),
            "constant row is missing {key}: {:?}",
            keys(&constant)
        );
    }

    // Type aliases share the constant insert block; the full analyze
    // pipeline (not `index_file`) is what materialises them.
}

#[test]
fn module_children_uses_full_entity_rows_and_binding_import_shape() {
    let graph = graph_with(SOURCE);
    let snap = graph.snapshot();
    let children = api::module_children(&snap, "rows.py::module").expect("module row");

    let function = &children["functions"][0];
    assert!(
        function.get("signature").is_some(),
        "module children must carry full function rows: {:?}",
        keys(function)
    );
    let import = &children["imports"][0];
    assert_eq!(import["id"], snap.imports.keys().next().cloned().unwrap());
    assert!(import.get("raw").is_some(), "imports carry the raw text");
    assert!(
        import.get("kind").is_some(),
        "imports carry the debug kind, not a display name"
    );
    assert!(import.get("name").is_none());
    let constant = &children["constants"][0];
    assert_eq!(keys(constant), vec!["id".to_string(), "name".to_string()]);
}

#[test]
fn traverse_reports_its_start_at_depth_zero() {
    let graph = graph_with(SOURCE);
    let snap = graph.snapshot();
    let rows = api::traverse(&snap, "rows.py::top", "downstream", &[], 3);

    let first = rows.first().expect("the walk reports its start");
    assert_eq!(first["id"], "rows.py::top");
    assert_eq!(first["depth"], 0);
    assert_eq!(first["edge_type"], "");
    assert!(
        first.get("signature").is_some(),
        "the start row is a full entity row"
    );
    assert!(
        rows.iter()
            .any(|row| row["id"].as_str().is_some_and(|id| id.ends_with(".bump"))),
        "the callee is reachable: {:?}",
        rows.iter().map(|r| r["id"].clone()).collect::<Vec<_>>()
    );
}

#[test]
fn plan_query_routes_each_intent_family() {
    let cases = [
        ("who calls parse", "impact_analysis", "callers_of"),
        ("path from a to b", "call_chain", "traverse"),
        (
            "find similar functions",
            "similarity_search",
            "search_similar",
        ),
        ("module dependencies", "dependency_graph", "traverse"),
        ("what is foo", "definition_lookup", "find"),
        ("list everything", "scope_exploration", "list_by_kind"),
    ];
    for (text, intent, method) in cases {
        let plan = api::plan_query(text);
        assert_eq!(plan["intent"], intent, "intent for {text:?}");
        assert_eq!(plan["method"], method, "method for {text:?}");
        assert_eq!(plan["top_k"], 10);
    }
}

#[test]
fn graphrag_context_compresses_and_truncates_on_the_budget() {
    let rows = vec![serde_json::json!({
        "id": "a.py::f",
        "name": "f",
        "signature": "def f()",
        "body": "if x:\n  return 1\nelse:\n  pass",
    })];
    let built = api::graphrag_build_context(&rows, "definition_lookup", 8192, "structural");
    // chars/4 over the compressed row's values (skeleton included).
    let used = built["tokens_used"].as_u64().unwrap_or(0);
    assert!(
        (13..=20).contains(&used),
        "unexpected token estimate {used}"
    );
    assert_eq!(built["rows"][0]["body_skeleton"], "if x:\n  return 1");
    assert_eq!(built["rows"][0]["name"], "f");

    let tiny = api::graphrag_build_context(&rows, "definition_lookup", 1, "structural");
    assert_eq!(tiny["tokens_used"], 0);
    assert_eq!(tiny["rows"].as_array().map(Vec::len), Some(0));

    let signatures = api::graphrag_build_context(&rows, "call_chain", 8192, "signatures_only");
    let row = signatures["rows"][0].as_object().expect("one row");
    let mut keys: Vec<&str> = row.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["docstring", "name", "signature"]);
}
