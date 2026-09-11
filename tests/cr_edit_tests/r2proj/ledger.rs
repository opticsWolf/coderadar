// Small Rust surface for round-2 multi-language checks.
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

pub fn total_cents(amounts: &[i64]) -> i64 {
    amounts.iter().sum()
}
