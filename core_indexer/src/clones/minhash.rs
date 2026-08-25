// CodeRadar Stage 2.2 — MinHash signatures over token shingles.
//
// Fossil reference: `src/clones/minhash.rs`. 128 rows, seeded with xxh3 of
// the row index (deterministic constants — no RNG dependency).

/// 128-row MinHash signature.
#[derive(Clone, Debug)]
pub struct MinHash {
    pub rows: [u64; 128],
}

const N_ROWS: usize = 128;

/// Deterministic per-row seed keys.
fn seeds() -> &'static [u64; N_ROWS] {
    static SEEDS: std::sync::OnceLock<[u64; N_ROWS]> = std::sync::OnceLock::new();
    SEEDS.get_or_init(|| {
        let mut s = [0u64; N_ROWS];
        for (i, v) in s.iter_mut().enumerate() {
            let key = (i as u64) | 0x9e37_79b9_7f4a_7c15; // golden-ratio salt
            *v = xxhash_rust::xxh3::xxh3_64(&key.to_le_bytes());
        }
        s
    })
}

impl MinHash {
    /// Signature over a shingle stream: min over hash_i(shingle).
    pub fn of(shingles: impl Iterator<Item = u64>) -> Self {
        let seeds = seeds();
        let mut rows = [u64::MAX; N_ROWS];
        for sh in shingles {
            for (i, r) in rows.iter_mut().enumerate() {
                let h = xxhash_rust::xxh3::xxh3_64(&sh.to_le_bytes()) ^ seeds[i];
                *r = (*r).min(h);
            }
        }
        Self { rows }
    }

    /// Estimated Jaccard similarity against another signature.
    pub fn estimate_jaccard(&self, other: &MinHash) -> f64 {
        let matching = self.rows.iter().zip(other.rows.iter()).filter(|(a, b)| a == b).count();
        matching as f64 / N_ROWS as f64
    }

    pub fn empty() -> Self {
        Self { rows: [u64::MAX; N_ROWS] }
    }
}
