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

## Command reference

Form: `coderadar [-C DIR] <command> [ARGS] [OPTIONS]`. `-C/--project`
picks the root (nearest `.coderadar` marker otherwise). Most graph
commands and all mutations take `--format json` for scripts; `query` also
takes `table`. `callers`, `callees` and `diagnose` print text only.

Entity ids are root-relative: `path/to/file.py::Class.method`,
`path/to/file.py::module`. Absolute paths, backslashes and a leading `./`
are accepted too.

### Read the graph

```bash
coderadar explore [SYMBOL]... [-q TEXT] [--direction both|upstream|downstream] [--max-files 8]
coderadar search QUERY [--kind function|class|type_alias|constant|module|import] [--top-k 10]
coderadar node ENTITY_ID [--neighbors]
coderadar query 'functions where caller_count == 0' [--format text|table|json]
coderadar traverse ENTITY_ID [--direction both|upstream|downstream|in|out] \
                    [--edge-kinds calls,imports,extends,overrides] [--max-depth 3]
coderadar callers ENTITY_ID                  # one hop up; transitive = affected
coderadar callees ENTITY_ID                  # one hop down
coderadar affected ENTITY_ID [--max-depth 5] # transitive callers, the blast radius
coderadar resolve '/users/:id' [--limit 5]   # routes, *Model / *View names
coderadar as-of TIMESTAMP [SYMBOL]... [-q TEXT]   # names at T, not bytes at T
coderadar module-children MODULE_ID          # e.g. 'pkg/app.py::module'
coderadar diagnose [--unresolved] [--low-confidence]
```

`traverse` caps `--max-depth` at 10; `--direction` aliases are `out`
(downstream) and `in` (upstream); `--edge-kinds` is comma-separated and
accepts `inherits` for `extends`.

### Quality and search

```bash
coderadar get-smells [--entity-id ID] [--rule-id RULE] [--strictness strict|normal|loose]
coderadar dead-code [--min-confidence 0.6] [--include-test-reachable] [--max-findings 100]
coderadar find-clones [--min-lines 10] [--min-similarity 0.8] [--max-groups 100]
coderadar find-scaffolding [--include-secrets] [--max-findings 100]
coderadar search-similar 'error handling' [--top-k 10]   # needs embeddings
coderadar compute-embeddings [--model NAME] [--batch-size 32] [--recompute]
coderadar status [--format json]
```

Rule ids for `--rule-id`: `god-class`, `long-method`,
`long-parameter-list`, `deep-nesting`, `data-class`,
`high-cyclomatic-complexity`, `brain-method`, `excessive-returns`,
`too-many-fields`, `dead-code`, `dead-branch`, `intra-dead-statements`.
`--recompute` is required when switching embedding models. Keyword search
over symbol text is API-only (`ops.search_symbols`); this surface has
`search` (name/description) and `search-similar` (semantic).

### Index and store

```bash
coderadar init [PATH] [--force]
coderadar reindex [--full] [--exclude PATTERN]... [--with-embeddings]
coderadar update-file FILE [--content TEXT|-]     # deleted file: dropped from graph
coderadar exclude add|remove PATTERN              # writes [project] exclude
coderadar exclude list                            # user patterns + baseline
coderadar load-snapshot SNAPSHOT [--root PATH]
coderadar store-repair [--db PATH] [--delete]
coderadar watch [PATH]... [--debounce 100]
```

`--exclude` is one-shot and implies `--full`. `--with-embeddings` also
builds vectors for `search-similar`. `init` is the only command that
creates the store; everything else assumes it exists.

### Mutations

```bash
coderadar replace-body ID BODY|'-' [--expected-hash H] [--apply]
coderadar update-signature ID 'def f(x, y=1)' [--inject-defaults] [--apply]
coderadar rename ID NEW_NAME [--apply]
coderadar create-entity FILE LANGUAGE KIND NAME BODY|'-' [--decorator D]... \
                         [--anchor end|top|ENTITY_ID] [--signature 'def f(x)'] [--apply]
```

Without `--apply` each mutation prints the diff and writes nothing;
`--expected-hash` refuses the edit if the entity changed since you read
it. `replace-body` replaces the body only (no `def` line, no decorators)
and re-bases indentation. `update-signature` edits the definition and
lists call sites as unverified — it cannot rewrite argument spans.
`create-entity` renders the header from `--signature` when given,
otherwise from `name`.

### Local process

```bash
coderadar shell
coderadar visualize hierarchy|dependencies|call-graph [ARGS]... [-o FILE] \
                    [--format mermaid|graphviz|dot]   # default: mermaid
coderadar mcp serve [--path DIR]
coderadar git blame FILE [--repo DIR]
coderadar git is-clean [REPO]
coderadar git diff [REPO] [--old OID] [--new OID]
coderadar docs [NAME]
coderadar skills list|show NAME|install [--dest DIR]... [--project] [--skill NAME]... [--force]
```

`coderadar --help` lists the full surface; `coderadar <command> --help`
gives the exact flags for one command. `coderadar docs query-language`
prints the full query language and every selectable field (also the MCP
resource `docs://query-language`).

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
