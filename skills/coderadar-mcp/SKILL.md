---
name: coderadar-mcp
description: Use CodeRadar's MCP tools (coderadar_*) for codebase intelligence on an indexed project: structural search, call graphs, blast radius, history-as-of, semantic search, smells, dead code, clones, and dry-run-first mutations. Use when the task needs graph/structural answers instead of grep loops, on any repo with a .coderadar/ store. Install the CLI/index first via the coderadar-cli skill when no store exists.
---

# CodeRadar MCP

CodeRadar is a local-first semantic code graph (tree-sitter + Macrame
ledger + FTS5 + embeddings). Twenty-six `coderadar_*` tools answer
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
   otherwise, so prefer passing `projectPath` explicitly).
2. No store: use the `coderadar-cli` skill (`coderadar init`) first,
   then come back here.

## Tools (all take an optional `projectPath`)

| Tool | Ask it |
|---|---|
| `coderadar_explore` | "how does X work" — guided structural tour |
| `coderadar_search` / `coderadar_node` | symbol search / one node's facts |
| `coderadar_query` | Pest structural queries |
| `coderadar_callers` / `coderadar_callees` | upstream / downstream edges |
| `coderadar_traverse` | bounded walk from an id |
| `coderadar_resolve` | name → bound entity (incl. `/route` paths) |
| `coderadar_affected` | blast radius of a change |
| `ops.search_symbols` (API-only) | keyword search over symbol text — MCP/CLI binding deferred, use the Python API |
| `coderadar_search_similar` | semantic search (needs embeddings first) |
| `coderadar_compute_embeddings` | build vectors; `recompute=true` on model switch |
| `coderadar_diagnose` | unresolved refs, ambiguous edges |
| `coderadar_get_smells` / `coderadar_dead_code` | smells / entry-point-aware dead code |
| `coderadar_find_clones` / `coderadar_find_scaffolding` | clones / scaffolding |
| `coderadar_as_of` | names+entities at timestamp T |
| `coderadar_module_children` | a module's contents |
| `coderadar_reindex` (`full`) / `coderadar_update_file` | refresh; full walk only on demand |
| `coderadar_status` / `coderadar_set_project` | freshness + root switching |
| `coderadar_replace_body` / `coderadar_update_signature` / `coderadar_rename` / `coderadar_create_entity` | mutations — `dry_run=true` default, review, then apply |

## Honesty contracts (do not work around these)

- `ContentUnavailable` ("content unavailable for T") ≠ "not in graph at
  T" (`None`) ≠ "unsupported" (`TemporalUnsupported`). Never present a
  present-tense answer for a historical question.
- Pre-blob generations have graph but no bytes — that is the honest
  answer, not a failure to retry.
- History questions: `coderadar_as_of` for names, then byte reads at T.
  There is no temporal query execution.
- Mutations are dry-run by default: review the diff, then apply with
  `dry_run=false`. `recompute=true` on `coderadar_compute_embeddings`
  clears and regenerates (required for a model switch).

## Names

Tools are `coderadar_*` on this surface only. The CLI spells the same
operations differently (`reindex`, `update-file`, `status`, `git blame`
— never the old `analyze`/`rebuild`/`update`/`stats`, which print a
0.13-removal notice). See the `coderadar-cli` skill for terminal use.
