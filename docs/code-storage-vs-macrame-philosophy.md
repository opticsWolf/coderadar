# Code storage in CodeRadar vs Macrame's bitemporal + branching philosophy

> **Date:** 2026-10-06 · **Baseline:** CodeRadar v0.11.0 on `macrame-db 0.18`; plan targets **0.19**
> (blobs, schema v22, D-281/D-287/D-288). Macrame refs: `docs/architecture/s0-s3` (Doctrine I–VIII),
> `s4-schema` §§4.1/4.4/4.8–4.10, `s5-modules`, `s13` D-174/D-213/D-278/D-280–D-288; `src/blob.rs`, `src/connection.rs`.
> CodeRadar refs: `core_indexer/src/storage.rs`, `types.rs:133 ByteSpan`, `ops.py:485,792`, `__init__.py:980`.

## TL;DR

The planned design — full file bytes as SHA-256 content-addressed blobs, digest in versioned
`extra.coderadar.source_blob`, diffs computed on read, retention by the archive reference scan —
is **the Macrame-idiomatic shape**. It respects every doctrine item provided five CodeRadar-side
contracts are kept: (1) recorded-time reads, (2) digest hygiene, (3) put-before-assert ordering,
(4) hot+cold backup/restore, (5) an explicit security/retention decision. Two CodeRadar habits need
narrowing: valid-time stamping and the present-tense `Snapshot`. Branching needs no work today
(CodeRadar lives on the trunk; blobs are correctly branch-global) but constrains future branch use.

## Doctrine-by-doctrine check

- **I Boundary sacred.** Plan keeps `libsql` co-pinned with Macrame, one libsql in `cargo tree`,
  `block_on` isolated in `storage.rs`. Conformant. Keep Macrame as the temporal engine; do not re-implement
  folding, scanning, or hashing around it (compute digests with the same SHA-256/lowercase-hex rule so the
  archive scan sees them).
- **II Two clocks, never mixed.** Macrame: `valid_from/valid_to` = world time; `recorded_at` = learned time;
  `put_at` = blob age-guard input, not a third axis. CodeRadar stamps `valid_from = now_iso8601()` per run
  (`storage.rs:68-79,792`) for concepts **and** edges. Pragmatic (index time ≈ world time for a code index)
  but lossy: file mtime/commit time never enters valid time, and per-run fresh `valid_from` on unchanged
  entities would churn versions without the idempotence guard (`open_edge_triples`, skip-if-open).
  Keep: `valid_to` explicit-or-`TS_OPEN`, microsecond width (the `now_iso8601` comment is load-bearing for
  `recorded_at <= ts` folding). Code history must mean **recorded time** (`reconstruct(T)`), not valid time;
  the plan's A16 pin is not optional — `as_of_valid` vs `as_of_recorded`/`reconstruct` answer different
  questions (D-174), and retroactive corrections only make sense if the axis is stated.
- **III Assertions immutable.** Macrame links/concepts are superseded, never updated; blobs immutable
  (`sha256/size/bytes` frozen, only `put_at` moves on re-put). CodeRadar retires (`valid_to=now`,
  `retired=1`, carrying title/content/extra forward: `storage.rs:348-375`) rather than deleting, and
  `persist_edges` is assert-only with idempotent open-triple checks. Conformant. Blob re-put = "imminent
  reference" announcement; ordering **put bytes → assert digest** is the crash-safe direction (orphan bytes
  age out via the scan; the reverse strands live digests pointing at absent bytes).
- **IV Ledger is the table, not the log.** `transaction_log` is the sole transaction-time mechanism.
  CodeRadar cold start already folds it (`reconstruct(now)`), and §0.1 routes `Snapshot` through the same
  fold. Conformant — provided `Snapshot` stops answering from live tables. No WAL/CDC reading is proposed.
- **V No physical deletion in hot.** Archive session is the only deleter (marker-gated guards incl. blobs).
  CodeRadar's `retire_entities`/`retire_stale_edges` close intervals; nothing issues ad-hoc DELETEs.
  Conformant. Consequence: superseding a concept that named a blob does **not** release the blob while the
  superseded entry is hot — the refcount design Macrame rejected (D-281 gate 3) would get this wrong, and
  CodeRadar must not reintroduce it with its own GC.
- **VI Derivative disposable.** `links_current` rebuildable/auditable. Blobs are the interesting case: **not**
  in the ledger (no trigger, like `kv_store`) yet **archive participants** (unlike `kv_store`) because past
  ledger states name them. The plan's "one cutoff, same session" (§3.4) is the correct consequence: blob
  retention tied to ledger retention, enforced by the reference scan, not a second reclamation path. Snapshots
  (`MaterializedState`) carry digests, never bytes — CodeRadar's `Snapshot`-as-`MaterializedState`+`blob_get`
  reading follows directly.
- **VII Embeddings immutable per version, excluded from ledger.** Blobs are the byte analogue: bulk bytes
  outside payloads, addressed from `extra`/content/link properties, per-model/per-byte immutability by
  address. The plan's embedding note (Phase-1 keys become future blob addresses) and the "no stored diffs"
  policy both follow: key cached diffs by `(digest_a, digest_b)` if ever needed, never by mutable id.
- **VIII Fidelity is a parameter.** The current `Snapshot` violates it (yesterday's graph with today's text,
  silently). The consolidated plan repairs it: recorded-time reconstruction, explicit
  available/unavailable/unsupported/not-at-T outcomes, canonical-UTC-or-reject at the boundary, distinct
  `predates_recorded_history`. The `BLOB_WARN_HOLD`/cap behavior must also be explicit, never silent
  truncation — same doctrine, write path.

## Branching check (D-213/D-214/D-220/D-224/D-259, §15)

Macrame: a branch is a fork in the sequence the DB was *told* things; transaction time is total within a
lineage, partial across lineages (`(ancestry, recorded_at)`); reads resolve against ancestry with fork-point
cutoffs; `reconstruct` answers whole-ledger belief, `reconstruct_on`/lineage reads answer one lineage's view.
Blobs and `kv_store` carry **no `branch_id`** deliberately: bytes and operational state are branch-global;
what is versioned per lineage is the *reference* (the `extra` naming the digest), not the bytes.

CodeRadar today: no branch use (trunk only) — verified by absence of branch APIs in `storage.rs`/`lib.rs`/
facade. Verdict: **no conflict and no work**. Consequences to record so a future branch feature does not
regress history: (a) never add per-branch blob copies or branch-scoped GC; (b) historical-byte lookup on a
branched ledger must resolve the digest through the reader's lineage (`reconstruct_on`/ancestry-bounded fold),
then `blob_get` the branch-global bytes — the plan's `reconstruct(T)` must gain its `_on(branch)` form when
branches arrive; (c) `put_at` refresh is lineage-independent, which is correct because eligibility also
requires "no hot log entry names it" in the archiving lineage's scan.

## Reference-scan conformance (D-287) — the load-bearing detail

The archive finds references by scanning hot-log payloads for 64-char lowercase-hex windows, wherever they
appear (nested JSON included, longer hex runs included; uppercase/truncated/base64/prefixed = invisible).
Therefore: `extra.coderadar.source_blob` (nested string) **is** found; an uppercased or shortened digest is a
silent orphan at the next archive. The plan's `validate_digest` boundary check, lowercase-hex-only rule, and
serialization/ledger-reconstruction tests are not polish — they are the retention mechanism. Same reason the
cold file has no guards and pre-0.19 cold files read as absent: old rows must not fail new rules, and only the
archive writer creates `cold.blobs`.

## Remaining mismatches to fix in CodeRadar (not in Macrame)

1. `Snapshot` present-tense delegation (VIII violation) — fixed by §0.1+§3.0 split.
2. `read_source` disk-reading display function cannot prove byte equality — split raw-byte vs display paths.
3. `valid_from = index-now` conflates world time with learn time — acceptable only if documented; prefer file
   mtime/commit time if CodeRadar ever promises "code as committed" rather than "code as indexed".
4. Naive/offset timestamps accepted at the Python boundary but rejected by Macrame `normalize` — normalize or
   reject before the engine.
5. `UpdateReport` has no oversize/blob-outcome field; watcher (1 MiB) vs blob (8 MiB) caps unpaired — wire both.
6. No backup/restore drill for a coherent hot+cold pair — required, because bytes may live in either file.
7. No at-rest policy for full-source bytes (excludes lack `.env`/secrets; scanner redacts output, not stored
   bytes) — DR-30 must precede default writes.

## Judgment

Adopt the blob design as specified in the consolidated plan: it is the shape Macrame's schema, guards,
reference scan, and backup model were built to support. The risk is not philosophical — it is contractual:
keep the five contracts above green, and the ledger stays honest; drop any one (especially digest hygiene or
recorded-time discipline) and history silently degrades into present-tense answers with missing bytes.
