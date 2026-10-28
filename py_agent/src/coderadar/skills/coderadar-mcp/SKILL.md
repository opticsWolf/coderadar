---
name: coderadar-mcp
description: Use CodeRadar's MCP tools (coderadar_*) for codebase intelligence on an indexed project: structural search, call graphs, blast radius, history-as-of, semantic search, smells, dead code, clones, and dry-run-first mutations. Use when the task needs graph/structural answers instead of grep loops, on any repo with a .coderadar/ store. Install the CLI/index first via the coderadar-cli skill when no store exists.
---

# CodeRadar MCP

CodeRadar is a local-first semantic code graph (tree-sitter + Macrame
ledger + FTS5 + embeddings). Twenty-eight `coderadar_*` tools answer
structural questions in one round-trip: what calls this, what breaks if
this changes, what did this look like at time T, where are the smells.

## When to trigger

- "How does X work", "what calls X", "what breaks if I change Y"
- Blast-radius / impact questions before editing
- "What did this look like on <date>" (history questions)
- Finding dead code, clones, smells, scaffolding
- Semantic ("find code like ...") questions
- Mutations (rename, signature change, body replace) — dry-run first

## Setup

1. The project needs a `.coderadar/store/coderadar.db`. Check with
   `coderadar_status` (or `coderadar_set_project` with a `project_path`
   to point at the repo — the server follows its working directory
   otherwise, so prefer passing `project_path` explicitly).
2. No store: use the `coderadar-cli` skill (`coderadar init`) first,
   then come back here.
3. The agent-facing docs ship in the wheel as MCP resources:
   `skills://coderadar-mcp`, `skills://coderadar-cli` and
   `docs://query-language`. On the shell the same text is
   `coderadar skills show <name>`, `coderadar skills install` and
   `coderadar docs query-language` (`--project`, `--dest` for other
   harnesses).

## Tools (all take an optional `project_path`)

| Tool | Ask it |
|---|---|
| `coderadar_explore` | "how does X work" — guided structural tour |
| `coderadar_search` / `coderadar_node` | symbol search / one node's facts |
| `coderadar_query` | Pest structural queries (reference: `docs://query-language`) |
| `coderadar_callers` / `coderadar_callees` | upstream / downstream edges |
| `coderadar_traverse` | bounded walk from an id |
| `coderadar_resolve` | name → bound entity (incl. `/route` paths) |
| `coderadar_affected` | blast radius of a change |
| `ops.search_symbols` (API-only) | keyword search over symbol text — MCP/CLI binding deferred, use the Python API |
| `coderadar_search_similar` | semantic search (needs embeddings first) |
| `coderadar_search_hybrid` | RRF-fused keyword + semantic (needs embeddings first) |
| `coderadar_compute_embeddings` | build vectors; `recompute=true` on model switch |
| `coderadar_diagnose` | unresolved refs, ambiguous edges |
| `coderadar_get_smells` / `coderadar_dead_code` | smells / entry-point-aware dead code |
| `coderadar_find_clones` / `coderadar_find_scaffolding` | clones / scaffolding |
| `coderadar_as_of` | names+entities at timestamp T |
| `coderadar_verify_history` | per-generation byte coverage (can the past be reconstructed) |
| `coderadar_module_children` | a module's contents |
| `coderadar_reindex` (`full`) / `coderadar_update_file` | refresh; full walk only on demand |
| `coderadar_status` / `coderadar_set_project` | freshness + root switching |
| `coderadar_replace_body` / `coderadar_update_signature` / `coderadar_rename` / `coderadar_create_entity` | mutations — `dry_run=true` default, review, then apply |

## Call signatures

Every tool takes `project_path` to name the repo (default: the project the
server serves); `coderadar_set_project` is the only one where it is
required. Defaults shown.

Entity ids: `path/to/file.py::Qualified.name`, methods as
`File::Class.member`, modules as `File::module`. Root-relative, forward
slashes, no `./`. `external::name` marks a callee outside the index.

### Read

```python
coderadar_explore(query="", symbols=[], direction="both", max_files=8)
coderadar_search(query, kind=None, top_k=10)      # kind: function|class|type_alias|constant|module|import
coderadar_node(entity_id, include_neighbors=False)
coderadar_query(query)                            # <entity> [select] [where] [group by] [order by] [limit]
coderadar_traverse(entity_id, direction="both", edge_kinds=None, max_depth=3)
coderadar_callers(entity_id)                      # 1 hop upstream
coderadar_callees(entity_id)                      # 1 hop downstream
coderadar_affected(entity_id, max_depth=5)        # transitive blast radius
coderadar_resolve(name, limit=5)                  # '/users/:id', '*Model', '*View'
coderadar_as_of(timestamp, query="", symbols=[])  # names at T, not bytes at T
coderadar_verify_history(from_ts=None, to_ts=None, generations=25)  # trust in history
coderadar_module_children(module_id)              # '{file_path}::module'
coderadar_diagnose(unresolved=True, low_confidence=True)
coderadar_status()
```

`traverse` directions are `downstream` / `upstream` / `both`; edge kinds
are `calls`, `imports`, `extends` (alias `inherits`), `implements`,
`decorates`, `instantiates`, `references`, `overrides` — all
when omitted. `max_depth` defaults to 3.

### Quality and search

```python
coderadar_get_smells(entity_id=None, rule_id=None, strictness="normal")
coderadar_dead_code(min_confidence=0.6, include_test_reachable=False, max_findings=100)
coderadar_find_clones(min_lines=10, min_similarity=0.8, max_groups=100)
coderadar_find_scaffolding(include_secrets=False, max_findings=100)
coderadar_search_similar(query, top_k=10)
coderadar_search_hybrid(query, top_k=10)
coderadar_compute_embeddings(model_name=None, recompute=False)
```

Smell rule ids: `god-class`, `long-method`, `long-parameter-list`,
`deep-nesting`, `data-class`, `high-cyclomatic-complexity`,
`brain-method`, `excessive-returns`, `too-many-fields`, `dead-code`,
`dead-branch`, `intra-dead-statements`. Strictness is `strict` |
`normal` | `loose`.

### Index and project

```python
coderadar_reindex(with_embeddings=False, full=False)
coderadar_update_file(file_path, content=None)    # content omitted -> read from disk
coderadar_set_project(project_path, confirm=False)  # confirm only for markerless dirs
```

### Mutations (`dry_run=True` default)

```python
coderadar_replace_body(entity_id, new_body, expected_hash=None, dry_run=True)
coderadar_update_signature(entity_id, new_signature, inject_defaults=False, dry_run=True)
coderadar_rename(entity_id, new_name, dry_run=True)
coderadar_create_entity(file_path, language, kind, name, body,
                        decorators=None, anchor="end", signature=None, dry_run=True)
```

`replace_body` takes the body only (no `def` line, no decorators) and
re-bases indentation; `expected_hash` refuses a stale edit. `update_signature`
edits the definition and returns call sites as `unverified_sites` — it
cannot rewrite argument spans. `create_entity` renders the header from
`signature` when given, otherwise from `name`; `anchor` is `end`, `top`,
or an entity id to insert after.

### Mutation outcomes

A mutation result is one public status, not a header plus an unrelated
`Applied`. `syntax_errors` carries syntax diagnostics only; operational
failures use `failure`, per-file outcomes, and `sync`.

- `Applied`: every edit is on disk, verified, and synchronized.
- `RolledBack`: no plan bytes remain; verification plus restored-state
synchronization completed.
- `RejectedStale`: disk changed since planning; re-plan from current bytes.
- `RejectedPolicy`: change the target/configuration; do not retry unchanged.
- `ApplyIncomplete`: valid source stayed on disk, but graph, persistence,
or derived-data synchronization did not complete. Repair the named sync
problem with `coderadar_update_file`/`coderadar_reindex`; do not blindly
re-apply the old plan.
- `RollbackIncomplete`: stop automatic retries, inspect per-file states and
retained `recovery_path` values, recover manually, then resync.

"Graph updated: yes" appears only when `sync.state` is `Synchronized`.

## Honesty contracts (do not work around these)

- `ContentUnavailable` ("content unavailable for T") ≠ "not in graph at
  T" (`None`) ≠ "unsupported" (`TemporalUnsupported`). Never present a
  present-tense answer for a historical question.
- Pre-blob generations have graph but no bytes — that is the honest
  answer, not a failure to retry.
- History questions: `coderadar_as_of` for names, then byte reads at T.
  There is no temporal query execution. `coderadar_verify_history` says
  whether those bytes exist at all — ask it before quoting a past version.
- Mutations are dry-run by default: review the diff, then apply with
  `dry_run=false`. `recompute=true` on `coderadar_compute_embeddings`
  clears and regenerates (required for a model switch).

## Names

Tools are `coderadar_*` on this surface only. The CLI spells the same
operations differently (`reindex`, `update-file`, `status`, `git blame`); the
pre-0.13 CLI aliases are gone. See the `coderadar-cli` skill for terminal use.
