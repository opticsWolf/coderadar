---
name: coderadar-cli
description: Use the CodeRadar CLI in a terminal for codebase intelligence: init, status, reindex, query, traverse, callers/callees, and mutations on an indexed project. Use when MCP is unavailable, for scripting, or to create/refresh the .coderadar/ store the coderadar-mcp skill queries. Teaches current command names (old analyze/rebuild/update/stats spellings print a 0.13-removal notice) and installs the bundled MCP + CLI skill packs (`coderadar skills install`).
---

# CodeRadar CLI

Same graph as the MCP surface, terminal spelling. One name per
operation: the commands below are current — the old spellings
(`analyze`, `rebuild`, `update`, `stats`) still run but print a
removal notice (gone in 0.13).

## When to trigger

- No MCP access to the project, or scripting/batch work
- Creating the store (`init`) or refreshing it (`reindex`, `update-file`)
- Quick structural answers without leaving the terminal

## Setup

```bash
cd <repo>
coderadar init            # one-shot: walks, indexes, creates .coderadar/
coderadar status          # what is indexed, how stale
```

`status` reports `store_fresh`: `false` means reindex (cheap path loads
the store and updates changed files only).

## Skills

The CLI ships its own agent skill packs. `coderadar skills list` names them,
`coderadar skills show <name>` prints one, and `coderadar skills install`
copies both into `~/.claude/skills` and `~/.config/opencode/skills`
(`--project` for repo-local, `--dest` for another harness).

## Commands

```bash
coderadar -C <path> <command>      # run against another root
coderadar reindex                  # cheap refresh (changed files only)
coderadar reindex --full           # full walk, on demand only
coderadar update-file <path>       # sync one file
coderadar query '<pest>'           # structural query
coderadar traverse <id>            # bounded walk
coderadar callers <id>             # upstream
coderadar callees <id>             # downstream
coderadar resolve '<name>'         # bind a name (routes: "/users/:id")
# keyword search over symbol text is API-only (ops.search_symbols) —
# the CLI/MCP binding is deferred; use search-similar here.
coderadar search-similar '<text>'  # semantic (needs embeddings)
coderadar compute-embeddings       # build vectors; --recompute to switch models
coderadar diagnose                 # unresolved / ambiguous edges
coderadar status                   # freshness + counts
coderadar exclude list             # effective excludes (baseline+config counted)

# Mutations are dry-run unless --apply; review the diff first.
coderadar replace-body <id> '<body>'             # '-' reads stdin
coderadar update-signature <id> '<signature>'    # --inject-defaults for call sites
coderadar rename <id> <new_name>
coderadar create-entity <file> <language> <kind> <name> '<body>'
```

`coderadar --help` lists the full surface, including `explore`, `search`,
`node`, `affected`, `as-of`, `module-children`, `get-smells`, `dead-code`,
`find-clones`, `find-scaffolding`, and local-process commands (`git`,
`shell`, `visualize`, `watch`, `load-snapshot`, `store-repair`, `mcp`).

`coderadar docs query-language` prints the full query language and every
selectable field (also the MCP resource `docs://query-language`).

Mutations are dry-run by default; pass `--apply` (or the tool's
`dry_run=false` equivalent) only after reviewing the diff.

## Renamed (do not teach the old spellings)

`analyze` → `reindex --full`, `rebuild` → `reindex --full`,
`update` → `update-file`, `stats` → `status`. Old invocations print
`` `<old>` is now `<new>` (the old spelling is removed in 0.13). ``
and still run. `git blame` / `is-clean` / `diff` live under the `git`
group now.

## Notes

- Entity ids are root-relative (`a.py::alpha`); the walk root sets the prefix.
- MCP `coderadar_set_project` has no separate CLI command: use `-C/--project`
  (`coderadar -C ../other status`). One process serves one root.
- Keep using normal file tools for raw reads; use the graph for structure.
- History (as-of/bytes-at-T) and retention (`archive`) are API/MCP-first;
  the CLI covers the live graph.
