# Store & Retention

> **Cutoff:** `[retention] archive_after_days` in `.coderadar.toml`
> (days-hot; `None` = no file default). Enforced by `ops.archive()` /
> `CodeGraph.archive()` — analyze never auto-archives.

## What lives where

- **Hot file** (`.coderadar/store/coderadar.db`, WAL-mode): the bitemporal
  ledger plus the `blobs` table (content-addressed file bytes, 8 MiB cap).
  Every indexed file generation puts its bytes; the module concept's
  `extra.coderadar.source_blob` names the digest.
- **Cold file** (`<store-stem>_archive.db` sibling, e.g.
  `coderadar_archive.db`): what one archive session moved — blobs last-put
  before the cutoff and named by no hot log entry, plus superseded ledger
  rows. Created on demand by the first session; absent until then.
- **Snapshots** (`.coderadar_snapshots/`): digests, never bytes.

Reads never break across the move: `blob_get` falls back hot→cold, and
`reconstruct(T)` unions hot+cold history. A hot entry naming a cold-only
blob copies it back (`blobs_restored` in the report).

## Backup = hot + cold, quiesced

1. Quiesce writers (no analyze/update/watch running — the suite holds none).
2. Checkpoint the WAL: `PRAGMA wal_checkpoint(TRUNCATE)` on the hot file,
   so the main file alone is consistent (a main-file copy without its `-wal`
   misses un-checkpointed rows — the test suite pins this, see below).
3. Copy the hot file **and** the cold file (if present) together.

A hot-only copy is pointers: recorded digests with no bytes. It degrades
honestly, never silently — archived blob addresses resolve to `None`,
current-generation reads still work, and reconstructing an archived T
raises naming the missing archive file (`ReplayCorrupt ... does not
exist`). Restore the pair and everything resolves.

## Retention behavior, pinned by tests

(`tests/test_retention.py`; `tests/test_blob_reads.py` for the read path.)

- `archive(cutoff)` reports `{links_archived, concepts_archived,
  log_entries_archived, horizon, blobs_archived, blobs_restored,
  blob_scan_bytes, cutoff}`. Future cutoffs archive everything superseded;
  live generations stay hot.
- Archived bytes stay readable through both `ops.blob_get` (cold fallback)
  and `Snapshot.read_bytes` at either generation.
- Lost cold bytes (ledger intact, `cold.blobs` row gone) =
  `ContentUnavailable` naming the digest — distinct from "not in the graph
  at T" (`None`) and from a broken ledger (`EngineError`).
- A pre-0.19 cold file (no `blobs` table) reads as absent, never an error;
  hot bytes still resolve.
- No TOML cutoff + no explicit cutoff = `InvalidRequest` (retention is an
  explicit action, never a default sweep).
- Entity spans are tree-sitter node extents: a trailing line break belongs
  to no entity. Whole-file exactness is the module's read
  (`{file}::module` returns the full blob, CRLF included).

## Restoring

Copy the pair back, `coderadar.load(store, root)`, and every retained
digest resolves to its recorded bytes — the restore test proves this for
both generations plus the current one, off the copied pair.
