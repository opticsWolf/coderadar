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
//! (`bfs_over_state`), and production stays there. The tests below were the
//! upstream-report reproducer (opticsWolf/Macrame#3, fixed in 0.19.1 via
//! D-289/D-290 `hydrate_historical` + `AttributeMode`); re-proven green on
//! 0.19.1 including the rename fixture, they now pin loader correctness as
//! ordinary regression tests.
//!
//! `T_RETIRE` is a fixed future stamp so no clock formatting is needed;
//! `t1` (real `max(recorded_at)`) always lands inside `[T0, T_RETIRE)`.
//!
//! 0.19.1 note (D-289/D-290): setting either builder instant without
//! `attribute_mode` is now a hard `DbError::AttributeModeUnstated` (not a
//! silent live-text mix), and `AttributeMode::AtTime` hydrates node
//! attributes from the transaction log as believed at the instant via
//! `hydrate_historical`. The recorded-fold test below states AtTime; if it
//! goes green, the upstream #3 loader closure gap is fixed through the new
//! API (un-ignore then). CodeRadar production still reads temporal
//! topology from the `reconstruct` state (`bfs_over_state`).

use macrame::graph::{AttributeMode, EdgeAssertion, Subgraph, TraversalBuilder};
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
        rt.block_on(
            db.upsert_concept(
                ConceptUpsert::new(id, id)
                    .content(r#"{"meta_version": 2, "kind": "function"}"#)
                    .valid_from(T0.to_string())
                    .valid_to(TS_OPEN.to_string())
                    .retired(false),
            ),
        )
        .unwrap();
    }
    rt.block_on(
        db.assert_edge(
            EdgeAssertion::new(A, B, "CALLS")
                .valid_from(T0.to_string())
                .weight(1.0)
                .properties("{}"),
        ),
    )
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

// Was the upstream gap (loader closure vs instant); green since 0.19.1.
// CodeRadar's own temporal path does not use the loader.
#[test]
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

// Same shape via the recorded-fold + AtTime composition.
#[test]
fn repro_recorded_fold_walk_at_own_timestamp() {
    let rt = rt();
    let (_dir, db, t1) = setup(&rt);
    let traversal = TraversalBuilder::new(A)
        .max_depth(2)
        .as_of_recorded(t1.clone())
        .attribute_mode(AttributeMode::AtTime);
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

const T_MID: &str = "2026-06-01T00:00:00.000000Z";
const C: &str = "repro::new_name";

// Re-prove gate for the 0.19.1 loader: a rename (retire-old + assert-new)
// walked at a pre-rename valid instant must reach the OLD name, and at a
// post-rename instant the NEW name — the exact shape of the DR-9 failure
// (renames wore current names through the loader). Green here un-ignores
// the whole file: the loader is re-proven on the rename fixture, though
// CodeRadar production stays on `bfs_over_state` regardless.
#[test]
fn repro_rename_walk_wears_period_names() {
    let rt = rt();
    let (_dir, db, t1) = setup(&rt);
    // Rename B -> C: retire the old concept + its edge, assert the new pair.
    rt.block_on(
        db.upsert_concept(
            ConceptUpsert::new(B, B)
                .content(r#"{"meta_version": 2, "kind": "function"}"#)
                .valid_from(T0.to_string())
                .valid_to(T_MID.to_string())
                .retired(true),
        ),
    )
    .unwrap();
    rt.block_on(
        db.upsert_concept(
            ConceptUpsert::new(C, C)
                .content(r#"{"meta_version": 2, "kind": "function"}"#)
                .valid_from(T_MID.to_string())
                .valid_to(TS_OPEN.to_string())
                .retired(false),
        ),
    )
    .unwrap();
    rt.block_on(db.retire_edge(A, B, "CALLS", T0, T_MID))
        .unwrap();
    rt.block_on(
        db.assert_edge(
            EdgeAssertion::new(A, C, "CALLS")
                .valid_from(T_MID.to_string())
                .weight(1.0)
                .properties("{}"),
        ),
    )
    .unwrap();
    // Bitemporal composition (the BCDM cell): believed-at-r about true-at-v.
    // r = t1 (recorded before the rename) keeps B live in belief; v selects
    // the valid axis. as_of_valid alone cannot carry history: AtTime with no
    // recorded instant hydrates against current belief, where B is retired.
    let before = TraversalBuilder::new(A)
        .max_depth(2)
        .as_of_recorded(t1.clone())
        .as_of_valid("2026-03-01T00:00:00.000000Z")
        .attribute_mode(AttributeMode::AtTime);
    let found_before = edge_triples(
        &rt.block_on(db.load_subgraph_with(&before, "2026-09-01T00:00:00.000000Z", 10_000_000))
            .unwrap(),
    );
    println!("rename walk at 2026-03: {found_before:?}");
    assert!(
        found_before.contains(&(A.to_string(), B.to_string(), "CALLS".to_string())),
        "PRE-RENAME WALK MISSED the old name"
    );
    assert!(
        !found_before.contains(&(A.to_string(), C.to_string(), "CALLS".to_string())),
        "PRE-RENAME WALK LEAKED the new name"
    );
    let t2 = rt.block_on(max_recorded(&db));
    let after = TraversalBuilder::new(A)
        .max_depth(2)
        .as_of_recorded(t2)
        .as_of_valid("2026-09-01T00:00:00.000000Z")
        .attribute_mode(AttributeMode::AtTime);
    let found_after = edge_triples(
        &rt.block_on(db.load_subgraph_with(&after, "2026-09-01T00:00:00.000000Z", 10_000_000))
            .unwrap(),
    );
    println!("rename walk at 2026-09: {found_after:?}");
    assert!(
        found_after.contains(&(A.to_string(), C.to_string(), "CALLS".to_string())),
        "POST-RENAME WALK MISSED the new name"
    );
    assert!(
        !found_after.contains(&(A.to_string(), B.to_string(), "CALLS".to_string())),
        "POST-RENAME WALK LEAKED the old name"
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
    let mut rows = rt.block_on(db.read_conn().query(full_sql, params)).unwrap();
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
        .map(|e| {
            (
                e.source_id.clone(),
                e.target_id.clone(),
                e.edge_type.clone(),
            )
        })
        .collect();
    println!("reconstruct(t1) valid-open edges: {open:?}");
    assert!(
        open.contains(&(A.to_string(), B.to_string(), "CALLS".to_string())),
        "RECONSTRUCT MISSED the retired edge at its own timestamp"
    );
}
