//! Upstream-report fixture: Macrame `load_subgraph_with` at a historical
//! instant drops edges whose endpoint retired AFTER the instant.
//!
//! Pure `macrame` APIs only — no `core_indexer` layers. Root cause, proven
//! by narrowing (loader SQL + params + connection all verified identical
//! to hand-executed SQL that finds the edge): the WALK finds the edge, but
//! `hydrate` reads node attributes from live `concepts WHERE retired = 0`
//! and `drop_dangling_adjacency` then enforces present-tense closure,
//! pruning the edge. Documented Macrame design for current-belief reads
//! (§4.1: retired = not visible); a real gap for historical instants, where
//! the endpoint was live. (Secondary: `execute_ids`/`build_sql` bakes the
//! same `WHERE c.retired = 0` into its projection openly.)
//!
//! CodeRadar reads temporal topology from the `reconstruct` state instead
//! (`bfs_over_state`); the ignored tests below are kept as the reproducer
//! for the upstream report — un-ignore when the loader honors instants in
//! node closure.
//!
//! `T_RETIRE` is a fixed future stamp so no clock formatting is needed;
//! `t1` (real `max(recorded_at)`) always lands inside `[T0, T_RETIRE)`.

use macrame::graph::{EdgeAssertion, Subgraph, TraversalBuilder};
use macrame::{ConceptUpsert, Database};

const TS_OPEN: &str = "9999-12-31T23:59:59.999999Z";
const T0: &str = "2026-01-01T00:00:00.000000Z";
const T_RETIRE: &str = "2027-01-01T00:00:00.000000Z";

const A: &str = "repro::caller";
const B: &str = "repro::old_name";

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn max_recorded(db: &Database) -> String {
    let mut rows = db
        .read_conn()
        .query("SELECT MAX(recorded_at) FROM transaction_log", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn edge_triples(sub: &Subgraph) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (node, _) in sub.nodes() {
        for e in sub.out_edges(node) {
            out.push((
                node.to_string(),
                e.node(sub).to_string(),
                e.edge_type(sub).to_string(),
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn setup(rt: &tokio::runtime::Runtime) -> (tempfile::TempDir, Database, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("repro.db");
    let db = rt.block_on(Database::open(&path)).unwrap();
    for id in [A, B] {
        rt.block_on(db.upsert_concept(
            ConceptUpsert::new(id, id)
                .content(r#"{"meta_version": 2, "kind": "function"}"#)
                .valid_from(T0.to_string())
                .valid_to(TS_OPEN.to_string())
                .retired(false),
        ))
        .unwrap();
    }
    rt.block_on(db.assert_edge(
        EdgeAssertion::new(A, B, "CALLS")
            .valid_from(T0.to_string())
            .weight(1.0)
            .properties("{}"),
    ))
    .unwrap();
    let t1 = rt.block_on(max_recorded(&db));
    assert!(
        T0 <= t1.as_str() && t1.as_str() < T_RETIRE,
        "t1 must land inside the edge's valid interval: {t1}"
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    rt.block_on(db.retire_edge(A, B, "CALLS", T0, T_RETIRE))
        .unwrap();
    (dir, db, t1)
}

async fn dump_ledger(db: &Database, t1: &str) {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT source_id, target_id, edge_type, valid_from, valid_to, recorded_at \
             FROM links ORDER BY recorded_at",
            (),
        )
        .await
        .unwrap();
    println!("--- links (t1={t1}) ---");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} -> {} {} [{}, {}) rec={}",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
            r.get::<String>(3).unwrap(),
            r.get::<String>(4).unwrap(),
            r.get::<String>(5).unwrap()
        );
    }
    let mut rows = db
        .read_conn()
        .query(
            "SELECT source_id, target_id, edge_type, valid_from, valid_to \
             FROM links_current",
            (),
        )
        .await
        .unwrap();
    println!("--- links_current ---");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} -> {} {} [{}, {})",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
            r.get::<String>(3).unwrap(),
            r.get::<String>(4).unwrap()
        );
    }
    let mut rows = db
        .read_conn()
        .query(
            "SELECT entity_id, operation, recorded_at FROM transaction_log \
             WHERE table_name = 'links' ORDER BY seq_id",
            (),
        )
        .await
        .unwrap();
    println!("--- transaction_log (links) ---");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} {} {}",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap()
        );
    }
}

// Ignored: documents the upstream gap (loader closure vs instant).
// Un-ignore when Macrame's loader honors historical instants in node
// closure; CodeRadar's own temporal path does not use the loader.
#[test]
#[ignore = "upstream: load_subgraph_with drops edges retired after the instant"]
fn repro_bare_walk_at_own_timestamp() {
    let rt = rt();
    let (_dir, db, t1) = setup(&rt);
    rt.block_on(dump_ledger(&db, &t1));
    let traversal = TraversalBuilder::new(A).max_depth(2);
    let sub = rt
        .block_on(db.load_subgraph_with(&traversal, &t1, 10_000_000))
        .unwrap();
    let found = edge_triples(&sub);
    println!("bare walk at t1={t1}: {found:?}");
    assert!(
        found.contains(&(A.to_string(), B.to_string(), "CALLS".to_string())),
        "BARE WALK MISSED the retired edge at its own timestamp"
    );
}

// Ignored: same upstream gap via the recorded-fold shape.
#[test]
#[ignore = "upstream: load_subgraph_with drops edges retired after the instant"]
fn repro_recorded_fold_walk_at_own_timestamp() {
    let rt = rt();
    let (_dir, db, t1) = setup(&rt);
    let traversal = TraversalBuilder::new(A)
        .max_depth(2)
        .as_of_recorded(t1.clone());
    let sub = rt
        .block_on(db.load_subgraph_with(&traversal, &t1, 10_000_000))
        .unwrap();
    let found = edge_triples(&sub);
    println!("recorded-fold walk at t1={t1}: {found:?}");
    assert!(
        found.contains(&(A.to_string(), B.to_string(), "CALLS".to_string())),
        "RECORDED-FOLD WALK MISSED the retired edge at its own timestamp"
    );
}

/// Against an EXTERNAL db (env `REPRO_DB`, `REPRO_T1`, `REPRO_START`): run
/// the loader walk, print the emitted SQL, hand-execute that exact SQL
/// with the loader's params, and dump the interval-matching rows — the
/// divergence, if any, is then visible in one screen.
#[test]
fn repro_against_external_db() {
    // Manual probe: set REPRO_DB/T1/START to run the loader against any
    // store. Skips (green) when unset so the suite stays clean.
    let (Some(db_path), Some(t1), Some(start)) = (
        std::env::var("REPRO_DB").ok(),
        std::env::var("REPRO_T1").ok(),
        std::env::var("REPRO_START").ok(),
    ) else {
        println!("REPRO_DB/T1/START unset: skipping external probe");
        return;
    };
    let rt = rt();
    let db = rt.block_on(Database::open(&db_path)).unwrap();

    let traversal = TraversalBuilder::new(start.clone()).max_depth(2);

    let sub = rt
        .block_on(db.load_subgraph_with(&traversal, &t1, 10_000_000))
        .unwrap();
    println!("loader walk at t1={t1}: {:?}", edge_triples(&sub));

    // Same walk CTE through the builder's own ids path (shares walk_cte;
    // no projection, no hydrate) — isolates walk vs projection/hydrate.
    let ids = rt
        .block_on(traversal.execute_ids(db.read_conn(), &t1))
        .unwrap();
    println!("execute_ids at t1={t1}: {ids:?}");

    // Hand-execute the FULL loader statement (walk CTE + edge projection,
    // transcribed from subgraph.rs) with the loader's params (?1 start,
    // ?2 depth, ?3 valid instant, ?4 min weight).
    let full_sql = "
WITH RECURSIVE walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
)
SELECT DISTINCT l.source_id, l.target_id, l.edge_type, l.weight, l.valid_from, l.valid_to
FROM walk w
JOIN links_current l ON l.source_id = w.node_id
WHERE l.valid_from <= ?3 AND ?3 < l.valid_to
  AND l.weight >= ?4
ORDER BY l.source_id, l.target_id, l.edge_type
";
    let params: Vec<libsql::Value> = vec![
        start.clone().into(),
        2i64.into(),
        t1.clone().into(),
        0.0f64.into(),
    ];
    let mut rows = rt
        .block_on(db.read_conn().query(full_sql, params))
        .unwrap();
    println!("--- hand-executed full loader SQL rows ---");
    while let Some(r) = rt.block_on(rows.next()).unwrap() {
        println!(
            "  {} -> {} {} w={} [{}, {})",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
            r.get::<f64>(3).unwrap(),
            r.get::<String>(4).unwrap(),
            r.get::<String>(5).unwrap()
        );
    }
    rt.block_on(dump_ledger(&db, &t1));
}

#[test]
fn repro_reconstruct_state_at_own_timestamp() {
    let rt = rt();
    let (_dir, db, t1) = setup(&rt);
    let state = rt.block_on(db.reconstruct(&t1)).unwrap();
    let open: Vec<_> = state
        .edges
        .iter()
        .filter(|e| e.valid_from.as_str() <= t1.as_str() && t1.as_str() < e.valid_to.as_str())
        .map(|e| (e.source_id.clone(), e.target_id.clone(), e.edge_type.clone()))
        .collect();
    println!("reconstruct(t1) valid-open edges: {open:?}");
    assert!(
        open.contains(&(A.to_string(), B.to_string(), "CALLS".to_string())),
        "RECONSTRUCT MISSED the retired edge at its own timestamp"
    );
}
