# CodeRadar Quick Reference

> v0.12.0 · Full design: `ARCHITECTURE.md` · Agent skills:
> `../skills/coderadar-{mcp,cli}/SKILL.md` (repo truth) · Query language:
> `query-language.md` (generated — read it, don't guess fields).

## Setup

```powershell
uv sync --extra gpu          # full dev env (CPU-only: plain `uv sync`)
uv run maturin develop       # rebuild coderadar._core after Rust changes
coderadar init               # write starter .coderadar.toml
coderadar reindex            # index / refresh (incremental by default)
coderadar status             # graph + store health
```

Multi-project: **one server + `coderadar_set_project`** (or `coderadar -C DIR …`).
Never run two servers on one store — the second gets `DB_LOCKED`.

## Everyday operations (CLI · MCP · Python)

| Task | CLI | MCP | Python |
|---|---|---|---|
| Find symbol | `coderadar search foo` | `coderadar_search` | `g.search("foo")` |
| Keyword search | — (API-only) | — | `g.search_symbols("token", top_k=10)` |
| Who calls X | `coderadar callers pkg.mod.fn` | `coderadar_callers` | `g.callers("…")` |
| Callees of X | `coderadar callees …` | `coderadar_callees` | `g.callees("…")` |
| Walk graph | `coderadar traverse ID --max-depth 3` | `coderadar_traverse` | `g.traverse(…)` |
| Impact of change | `coderadar affected FILE` | `coderadar_affected` | `g.affected("…")` |
| Resolve import/route | `coderadar resolve "a.b.c"` | `coderadar_resolve` | `g.resolve("…")` |
| Structured query | `coderadar query "functions where …"` | `coderadar_query` | `g.query("…")` |
| Smells / dead code | `coderadar get-smells`, `dead-code` | `coderadar_get_smells`, `…_dead_code` | `g.get_smells()` |
| History read | — (via Python) | `coderadar_as_of` | `snap = g.as_of(t); snap.read_bytes(id)` |
| Edit code | `coderadar replace-body … [--apply]` | `coderadar_replace_body` | plan → apply |
| Archive old blobs | — (API-only) | — | `g.archive() # cutoff or archive_after_days` |
| Refresh one file | `coderadar update-file F` | `coderadar_update_file` | `g.update_file("…")` |

Mutations are **dry-run unless applied**; the written file must parse or the
run fails loudly. Old spellings (`analyze`, `explore(start_id=…)`) warn and
die in 0.13.

## Query language (essentials)

```
functions where name contains "embed" select name, file_path order by name limit 20
classes select name, count(functions) as n group by file_path order by n desc
```

Entities: `modules classes functions methods constants entities`.
Ops: `== != < <= > >= contains matches starts_with ends_with in`, `and/or/not`.
Unknown fields are rejected — the reference lists every field.

## Config (`.coderadar.toml`)

```toml
[database]
path = ".coderadar/store/coderadar.db"
store_source_blobs = true     # kill-switch: false (DR-30)
# blob_exclude = ["**/secrets/**"]

[retention]
# archive_after_days = 30     # unset = keep hot; archive() only, never auto

[embedding]
model = "BAAI/bge-small-en-v1.5"   # whole process must agree; dimension = 384

[mutation]
default_dry_run = true
```

`reindex` names any key it could not use — a silent setting is a bug, report it.

## Troubleshooting

| Symptom | Cause → fix |
|---|---|
| `DB_LOCKED` | two holders on one store → kill the other `okf-mcp`/`coderadar` process |
| Stale results after config edit | excludes changed → `reindex` forces a full walk (by design, DR-31) |
| `ContentUnavailable` naming a digest | fact kept, blob file lost → restore from backup (hot+cold) |
| `… concepts_fts … rebuild_fts` | FTS table missing/tampered → `store-repair`, then reindex |
| `TemporalUnsupported` on callers-at-T | decided NO-GO (§3.3) — restructure the question to live graph + `as_of` names/bytes |
| `InvalidRequest: stored graph` | keyword search needs a served (stored) graph → `reindex` first |
| DeprecationWarning on old names | rename to the canonical op; removals land in 0.13 |

## Version notes

+0.0.1 per feature (`pyproject` + workspace `Cargo.toml`, one bump commit).
Specs, tests, dep patches: no bump. Current: **0.12.0**, `macrame-db` 0.19.1,
store schema v22. `../CHANGELOG.md` is the per-version record.
