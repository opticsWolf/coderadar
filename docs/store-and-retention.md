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

## One engine per store path, per process

Opening a store joins the engine this process already has for that path;
there is never a second one. Two measured reasons (probed on 0.14.0 /
macrame-db 0.22.0, see `storage::StoreInner`):

- libSQL refuses a second `Database` on a live path on POSIX
  (`SqliteFailure(5, "database is locked")`) and tolerates it on Windows —
  so "open it again" is a platform-dependent bug, not a style choice.
- Closing one of two engines is not a shutdown: the survivor keeps
  answering current reads but loses the rows the close folded into its
  snapshot (and a store opened *afterwards*, even on another path, sees the
  same loss). An engine's shutdown is a process-final act.

Consequences worth knowing:

- **The file stays open for the process's life.** Nothing can move or
  delete a store this process has opened (`ops.rebuild` therefore moves the
  store first, refuses a loaded graph, and calls `_core.forget_store` so the
  ingest after the move mints an engine for the new file rather than joining
  the one aimed at the backup).
- **An external writer is invisible.** A `sqlite3` connection in the same
  process that deletes rows behind a live engine changes the file, not the
  engine's view; the next op through the engine still sees the old rows.
  Tests that need that shape must cross a process boundary — child ingests,
  child exits, parent (or another child) does the mutation and the repair.
  `tests/test_p5b3a_horizon.py::_fresh_process` is the worked example.
- **Repeated ops are fine now**: a second `ingest_history` on one store, a
  backfill beside a loaded graph, `open_items(store_path=…)` after `load` —
  all join the same engine. The lookup, the open and the registration happen
  under one lock: releasing it in between let two threads each open and each
  keep its own engine (a silent second engine on Windows, `database is
  locked` on POSIX) — the race the MCP daemon can reach because it drops the
  interpreter lock around ops.
- **One file, one identity.** The registry key is the path the filesystem
  reports for an existing file (`canonicalize`), so letter case and symlinks
  fold exactly where the platform folds them — Windows and default macOS are
  case-insensitive, Linux is not, and folding case by hand would merge two
  genuine stores on a case-sensitive volume. A path that does not exist yet
  falls back to its lexical spelling, which is why `forget_store` and
  `holds_store` consult both.
- **The cadence belongs to the engine.** A later `open_with_cadence` joins
  the engine the first opener configured and its own cadence argument is
  ignored, silently — production is the only caller that passes one.

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

## Stores written before macrame 0.22 (the legacy open end)

An open interval is spelled `9999-12-31T23:59:59.999999Z` (`TS_OPEN`) since the
`macrame-db` 0.22.0 pin. Stores written earlier hold `9999-12-31T00:00:00.000000Z`
(`TS_OPEN_LEGACY`) on their open rows. Reads admit both spellings (`is_open_end`), so
queries keep answering; **writes do not**, because the engine's single-open/supersede
law keys on its own sentinel. The first `analyze` over such a store refuses the whole
concept flush:

```
[diag] v2 concept flush failed: OverlappingConcept { ... id: "core_indexer/src/fs/git.rs::module",
       valid_from: "…", valid_to: "9999-12-31T23:59:59.999999Z",
       existing_from: "2026-10-03T…", existing_to: "9999-12-31T00:00:00.000000Z", provenance: AlreadyRecorded }
[coderadar] WARNING edge persistence failed part way through: NotFound(...)
```

The new span is open, the incumbent is open *in the old spelling*, so the engine sees
an overlap rather than a supersession: its supersede path looks for its own sentinel,
finds nothing open, and an ordinary upsert then fails the interval guard. This has been
the behavior since the 0.22.0 pin (M6-g slice 1, `622bde4`) — it is not new in 0.16.0, and
a store whose rows are all new-spelling never sees it.

The guards around it hold: the stale-concept sweep is skipped when the flush failed, so
concepts keep their old rows instead of being retired against a batch that never landed.
(The edge retraction pass still runs, so *edges* can drift in the same run; that is the
`WARNING edge persistence failed` line above, not a concept loss.) Re-running does not
help: every attempt meets the same incumbent, and the one path that would replace it —
the stale-file retire sweep — is skipped precisely because the flush failed.

The cure is a store that writes one sentinel, not an in-place patch: `coderadar
rebuild-store` (git projects), `migrate-to-commit-time`, or a fresh `analyze` into a
new store file. Detecting the shape is one query —
`SELECT COUNT(*) FROM concepts WHERE valid_to = '9999-12-31T00:00:00.000000Z'` — and it
belongs in a health report rather than in prose; until then, this section is the check.

## Restoring

Copy the pair back, `coderadar.load(store, root)`, and every retained
digest resolves to its recorded bytes — the restore test proves this for
both generations plus the current one, off the copied pair.
