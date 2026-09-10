# cr_edit_tests — dogfood playground

Demo files created **only** to exercise CodeRadar's mutating tools
(`coderadar_replace_body`, `coderadar_update_signature`, `coderadar_rename`,
`coderadar_create_entity`) during the self-review. Everything in here is
throwaway and safe to delete after the review.

- `demo_billing.py` — Python; deliberate tax/discount-order bug in `calculate_total`
- `demo_router.ts`   — TypeScript; deliberate last-match-wins bug in `Router::match`
- `demo_ledger.rs`   — Rust; deliberate refund-dropping bug in `total_cents`

Each file contains one bug intended to be fixed via `replace_body`, one
signature worth extending via `update_signature`, one name worth renaming,
and a small call chain so `callers`/`callees`/`affected` have edges to walk.

Harnesses (kept for reproduction of every finding in
[docs/dogfood-review-2026-09.md](../../docs/dogfood-review-2026-09.md)):

- `battery_readonly.py` — drives all 15 read-only MCP tools + dry-run mutations;
  writes `battery_output.txt`
- `battery_mutation.py` — applies all four mutation tools with `dry_run=False`
  **only to the demo files above**, verifies on disk and in the graph, tests
  the stale-hash guard and the policy gate, then tries to restore (note: git
  checkout is a no-op for untracked files — the `.coderadar-bak` backups are
  the real undo path); writes `battery_mutation_output.txt`
- `mcp_stdio_probe.py` — real protocol test of `coderadar mcp serve`
  (initialize → tools/list → tools/call)

Re-run after resetting the demo files:

    .venv/Scripts/python tests/cr_edit_tests/battery_readonly.py
    .venv/Scripts/python tests/cr_edit_tests/battery_mutation.py
