# macrame-db 0.12 → 0.15 upgrade notes (`dev_macrame15`)

## What changed
- `core_indexer/Cargo.toml`: `macrame-db = "0.15"` (schema v10 → **v15**, auto-migrates on open; old snapshot payloads get a named refusal and fall through to `analyze` per the existing cold-start contract).
- `ConceptUpsert` struct literals → `new(id, title)` builder chain (now `#[non_exhaustive]`; `embedding_model: None` is the default).
- `MaterializedState::edges` is `Vec<EdgeBelief>`: field access in `restore_synthetic_edges`, `EdgeBelief::new(...)` in tests.
- `write_concepts` returns `Result<usize, BulkInterrupted>`: propagated via `From` (committed prefix stays committed; upserts are idempotent; CodeRadar never passes a cancel token).
- Adopted `Database::analyze()` after the bulk flush (planner stats, upstream W2/D-149): best-effort, ~50ms worst case measured, never fails the index.
- `libsql` stays a single v0.9.30 in the tree — the "must track macrame's libsql" invariant holds.

## Perf investigation (the 3.5× scare)
A/B on identical tests: ~24s on `dev` (0.12) vs ~58s here (0.15). Bisection:
`Database::open` 8ms, `analyze()` 52ms, extract→commit phases 73ms total —
yet one `analyze` took 1.78s. The remainder was the P2 `LEDGER_REVISION`
stamp, which ran a **full `reconstruct(now)` fold to read one integer**.
Fix: `current_seq_anchor()` reads `MAX(seq_id)` via the diagnostic
connection (single btree probe; empty log → 0 like the empty fold).
e2e-fixture analyze 1.7s → 0.08s; full suite 146s → **44s** (0.12 baseline: 94s).

## Upstream note (macrame, with numbers)
`reconstruct` on the same 313MB / 76k-page WAL-mode log: **~0.1–0.2s on
0.12 vs ~1.7s on 0.15** — roughly 10×. Suspected cause: the
lineage-partitioned replay folds (four folds since schema v12, none
projecting `branch_id` until v15's `EdgeBelief`). Our exposure is
eliminated (stamp no longer folds), but any caller reconstructing large
logs pays this. Worth reporting upstream with these numbers; the fixture
db that demonstrated it is gone (deleted, gitignored cruft).

## Store regrowth: measured, and what to do about it
Every `analyze` on a warm store appends rows even when nothing changed —
measured **+26 log rows per no-op analyze** on the 7-file cold-start
fixture (13 concepts + 13 edges re-versioned). Mechanism: concepts are
bulk-upserted with a fresh per-run `valid_from`, so each run mints a new
version of every concept; edges are already idempotent (open interval not
re-asserted). At Süvea scale that is ~10k rows per analyze, growing
without bound — which is exactly how fixture dirs reached 300MB and made
the fold cost above bite. (Production dbs grow the same way, not just
fixtures.)

Recommendation, in order:
1. **Keep fixture dirs store-free** (done; enforced by
   `tests/test_fixture_hygiene.py`, which fails loudly if `.coderadar`
   reappears under `tests/fixtures`). Nothing in the suite needs warm
   fixture stores — verified green without them, and nothing re-plants
   them.
2. **Content-hash-gated concept upsert** (done): `filter_unchanged`
   drops concepts the open row already holds byte-identically (title +
   `content_hash` + retired flag; `valid_from`/`valid_to` are write
   mechanics and not compared). Per-file v1 flush is now insert-if-
   absent (the v2 flush owns known ids; v1 rows exist only as
   crash-partial progress). Imports/constants/aliases had no stable
   hash, so their v2 writers now stamp xxh3-over-canonical-JSON.
   Measured: +26 rows per no-op analyze → **+0**; edits still version
   normally (+6 for a new function) and the revision anchor advances.
   Subtlety found by the v1-fallback test: the gate must also require
   `meta_version == 2`, or downgraded rows that agree on hash/title/
   retired never upgrade (a future meta_version bump rewrites every row
   exactly once for free).
3. If growth ever matters beyond this, macrame has `archive()` /
   retention for moving closed intervals out of the hot log — a stopgap,
   not a fix.
