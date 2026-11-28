//! Usage text for the captured parity commands.
//!
//! Captured commands bypass clap (adding top-level variants overflowed the
//! derive stack), so `--help` is answered from this table instead of being
//! passed through as an argument. The text lives in
//! `rust/golden/cli_usage.json` so the Rust sources stay code.

use std::sync::OnceLock;

use serde_json::Value;

const USAGE: &str = include_str!("../../golden/cli_usage.json");

fn table() -> &'static Value {
    static TABLE: OnceLock<Value> = OnceLock::new();
    TABLE.get_or_init(|| serde_json::from_str(USAGE).unwrap_or(Value::Null))
}

/// The usage block for one captured command.
pub fn for_command(name: &str) -> Option<&'static str> {
    table().get("commands")?.get(name)?.as_str()
}
