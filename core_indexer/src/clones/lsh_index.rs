// CodeRadar Stage 2.3 — banded LSH over MinHash signatures.
//
// Fossil reference: `src/clones/lsh_index.rs`. 16 bands × 8 rows: candidate
// pairs collide when true Jaccard ≳ 0.72. The (b, r) tradeoff is exposed as
// constructor parameters like fossil's `select_lsh_params`.

use std::collections::HashMap;

use super::minhash::MinHash;

pub struct LshIndex {
    bands: Vec<HashMap<u64, Vec<u32>>>,
    sigs: Vec<MinHash>,
    rows_per_band: usize,
}

impl LshIndex {
    /// `bands × rows_per_band` must equal the signature width (128 default).
    pub fn new(bands: usize, rows_per_band: usize) -> Self {
        assert_eq!(bands * rows_per_band, 128, "band layout must cover the signature");
        Self {
            bands: vec![HashMap::new(); bands],
            sigs: Vec::new(),
            rows_per_band,
        }
    }

    pub fn insert(&mut self, id: u32, sig: MinHash) {
        for (b, chunk) in sig.rows.chunks(self.rows_per_band).enumerate() {
            let key = band_key(chunk);
            self.bands[b].entry(key).or_default().push(id);
        }
        self.sigs.push(sig);
    }

    pub fn signature(&self, id: u32) -> &MinHash {
        &self.sigs[id as usize]
    }

    /// Candidate pairs only — never compare all pairs. Deduplicated.
    pub fn candidate_pairs(&self) -> Vec<(u32, u32)> {
        let mut seen = std::collections::HashSet::new();
        let mut pairs = Vec::new();
        for band in &self.bands {
            for bucket in band.values() {
                for i in 0..bucket.len() {
                    for j in i + 1..bucket.len() {
                        let (a, b) = if bucket[i] < bucket[j] {
                            (bucket[i], bucket[j])
                        } else {
                            (bucket[j], bucket[i])
                        };
                        if seen.insert((a, b)) {
                            pairs.push((a, b));
                        }
                    }
                }
            }
        }
        pairs
    }
}

fn band_key(chunk: &[u64]) -> u64 {
    // Byte-view via a stack buffer — no unsafe, fixed width ≤ 8 rows.
    let mut buf = [0u8; 64];
    for (i, v) in chunk.iter().enumerate() {
        buf[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    xxhash_rust::xxh3::xxh3_64(&buf[..chunk.len() * 8])
}
