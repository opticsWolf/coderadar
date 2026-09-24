# CodeRadar integration with macrame-db 0.18

CodeRadar's Rust extension now depends on `macrame-db = "0.18"` and is locked to
0.18.0. The installed Python wheel is not the dependency used by CodeRadar's
native indexer; `Cargo.toml` and the rebuilt `_core` extension determine the
Macrame version in use.

## Adopted

### Application metadata in `concepts.extra`

The full, reconstructible CodeRadar entity remains in `concepts.content`. Each
CodeRadar write also carries a small, namespaced object in `concepts.extra`:

```json
{
  "coderadar": {
    "format": 1,
    "kind": "function",
    "file_path": "src/app.py",
    "content_hash": "…"
  }
}
```

The `file_path` expression index is asserted on every store open with
`register_extra_index("$.coderadar.file_path")`. This is a derived index, not
an app-managed schema migration. `remove_file` first removes entities present
in the live projection, then unions two indexed lookups: the exact metadata
index and a primary-key ID range for legacy rows. This retires any still-live
concepts left in the ledger by an interrupted or older indexing pass without
forcing a full concepts-table scan. Concept builders explicitly replace this
namespace; maintenance upserts that omit `extra` do not clear it. The existing
`content` metadata and cold-start format remain authoritative.

### Edge kinds

The synthetic-edge persistence path no longer removes underscores from a kind.
Framework extraction emits a stable lowercase namespace of the form
`synthetic:<resolver>:<relation>`, for example `synthetic:django:handles` and
`synthetic:fastapi:depends_on`. Structural kinds (`CALLS`, `IMPORTS`,
`EXTENDS`, `OVERRIDES`) are unchanged. Direct Rust callers retain control of
their kind string; Macrame validates it on write. Existing unnamespaced
synthetic ledger rows are not rewritten; cold start deduplicates them with the
new typed edge by endpoint pair, and normal entity retirement closes their
intervals when an endpoint is removed.

## Forward compatibility

Opening a database with macrame-db 0.18 advances its schema to v21. A 0.17
binary cannot open that schema, and it cannot fold a concept log entry written
with the 0.18 concept payload format. Do not roll back the native extension to
0.17 after the 0.18 binary has written concepts. A 0.18 open of an existing
0.17 store is automatic. A successful full analyze rewrites current projection
concepts with CodeRadar's `extra` metadata and retires stale canonical
file-backed concepts absent from that projection (for example, imports whose
line-based ids changed). This reconciliation is skipped if the project walk,
source reads, extraction, parse quality, or concept flush is incomplete; such a
run is not safe evidence that absent rows are stale. Historical retired rows
remain in the ledger as usual.

## Deliberately not adopted yet: `kv_store`

The 0.18 KV table is appropriate for operational state, not graph facts. The
current CodeRadar candidates did not justify adding it in this change: the
embedding vectors are presently held in the in-memory projection rather than
persisted in Macrame, so a KV “embedding model” marker could imply durable
embedding data that is not there; per-file fingerprints would need a batched
write design before they should replace the current mtime-based cold-start
heuristic. No ledger data or user state has been moved to KV. Revisit when a
specific operational-state owner and consistency lifecycle are defined.
