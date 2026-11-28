//! E2 — the embedding drift contract.
//!
//! The reference vectors come from the Python fastembed stack the product
//! shipped before the port (`rust/golden/embedding_parity.json`, recorded by
//! `rust/embedding_parity.py`). The Rust side links a different ONNX Runtime
//! build, so bit-identical floats are not achievable; what must hold is:
//!
//! 1. the same model (dimension and near-identical components),
//! 2. per-component drift within the recorded tolerance,
//! 3. identical ranking — the property every caller actually depends on.
//!
//! This test loads the ONNX model, so it is `#[ignore]`d by default and run
//! explicitly (`cargo test -p core_indexer embedding_parity -- --ignored`).

use serde_json::Value;
use std::path::PathBuf;

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("rust")
        .join("golden")
        .join("embedding_parity.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("embedding golden is valid JSON")
}

fn vectors(value: &Value, key: &str) -> Vec<Vec<f64>> {
    value[key]
        .as_array()
        .expect("vectors array")
        .iter()
        .map(|row| {
            row.as_array()
                .expect("vector row")
                .iter()
                .map(|cell| cell.as_f64().expect("f64 component"))
                .collect()
        })
        .collect()
}

fn cosine(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|y| y * y).sum::<f64>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

fn top_k(queries: &[Vec<f64>], corpus: &[Vec<f64>], k: usize) -> Vec<Vec<usize>> {
    queries
        .iter()
        .map(|query| {
            let mut scored: Vec<(usize, f64)> = corpus
                .iter()
                .enumerate()
                .map(|(index, vector)| (index, cosine(query, vector)))
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.into_iter().take(k).map(|(index, _)| index).collect()
        })
        .collect()
}

#[test]
#[ignore = "loads the ONNX model; run with --ignored"]
fn embedding_drift_stays_within_the_recorded_contract() {
    let golden = golden();
    let model = golden["model"].as_str().expect("model name");
    let dimension = golden["dimension"].as_u64().expect("dimension") as usize;
    let tolerance = golden["tolerance"].as_f64().expect("tolerance");
    let texts: Vec<String> = golden["texts"]
        .as_array()
        .expect("texts")
        .iter()
        .map(|text| text.as_str().expect("text").to_string())
        .collect();
    let reference = vectors(&golden, "vectors");

    let ported = crate::api::embed_texts(model, &texts, 32).expect("rust embedding");

    assert_eq!(ported.len(), reference.len(), "one vector per text");
    for (index, vector) in ported.iter().enumerate() {
        assert_eq!(vector.len(), dimension, "text {index} width");
    }

    let drift = ported
        .iter()
        .zip(&reference)
        .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs()))
        .fold(0.0f64, f64::max);
    let mean = ported
        .iter()
        .zip(&reference)
        .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs()))
        .sum::<f64>()
        / (ported.len() * dimension) as f64;
    let reference_ranks = top_k(&reference, &reference, 3);
    let ported_ranks = top_k(&ported, &ported, 3);
    println!("max drift {drift:.3e}, mean drift {mean:.3e}, tolerance {tolerance:.3e}");
    println!("ranking identical: {}", ported_ranks == reference_ranks);
    assert!(
        drift <= tolerance,
        "max component drift {drift:.3e} exceeds the recorded contract {tolerance:.3e}; \
         a different model or preprocessing would land here, not kernel noise"
    );

    assert_eq!(
        ported_ranks, reference_ranks,
        "top-3 ranking must match the reference for every corpus text"
    );
}

#[test]
#[ignore = "loads the ONNX model; run with --ignored"]
fn cross_runtime_cosine_agrees() {
    let golden = golden();
    let model = golden["model"].as_str().expect("model name");
    let texts: Vec<String> = golden["texts"]
        .as_array()
        .expect("texts")
        .iter()
        .map(|text| text.as_str().expect("text").to_string())
        .collect();
    let reference = vectors(&golden, "vectors");
    let ported = crate::api::embed_texts(model, &texts, 32).expect("rust embedding");

    // Every pair's similarity must agree far more tightly than the distance
    // between neighbouring candidates: that is what ranking stability means.
    let mut worst = 0.0f64;
    for i in 0..texts.len() {
        for j in 0..texts.len() {
            let want = cosine(&reference[i], &reference[j]);
            let got = cosine(&ported[i], &ported[j]);
            worst = worst.max((want - got).abs());
        }
    }
    assert!(
        worst <= 2e-4,
        "cosine similarity drifted by {worst:.3e} across runtimes"
    );
}
