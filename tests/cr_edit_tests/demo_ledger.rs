//! Demo ledger module for CodeRadar self-review mutation tests.
//! Contains deliberate bug(s) targeted by the four mutation tools.

#[derive(Debug, Clone)]
pub struct Entry {
    pub label: String,
    pub amount_cents: i64,
}

#[derive(Debug, Default)]
pub struct Ledger {
    entries: Vec<Entry>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, label: &str, amount_cents: i64) {
        self.entries.push(Entry {
            label: label.to_string(),
            amount_cents,
        });
    }

    /// BUG: drops negative entries, so refunds disappear from the total.
    pub fn total_cents(&self) -> i64 {
        self.entries.iter().map(|e| e.amount_cents).sum()
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Target for rename: underscore-heavy legacy name.
    pub fn fmt_total_for_display(&self) -> String {
        let total = self.total_cents();
        format!("${}.{:02}", total / 100, (total % 100).abs())
    }
}

pub fn demo_ledger() -> Ledger {
    let mut ledger = Ledger::new();
    ledger.record("widget sale", 1999);
    ledger.record("refund", -500);
    ledger.record("gizmo sale", 450);
    ledger
}
