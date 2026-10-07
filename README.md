# CodeRadar v0.12.0

[![CI](https://github.com/opticsWolf/coderadar/actions/workflows/ci.yml/badge.svg)](https://github.com/opticsWolf/coderadar/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/coderadar-rs?label=pypi)](https://pypi.org/project/coderadar-rs/)
[![Python](https://img.shields.io/pypi/pyversions/coderadar-rs)](https://pypi.org/project/coderadar-rs/)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Languages](https://img.shields.io/badge/languages-41-brightgreen)]()
[![Website](https://img.shields.io/badge/website-coderadar-blue)](https://opticswolf.github.io/coderadar/)

[Homepage](https://github.com/opticsWolf/coderadar) · [Repository](https://github.com/opticsWolf/coderadar)

**Live semantic graph of your codebase — incremental, queryable, LLM-writable.**

CodeRadar maintains an incrementally updatable graph of your code's logical structure, enabling LLMs and developer tools to both **query** and **safely rewrite** code through a unified pipeline.

## Why CodeRadar

CodeGraph pioneered the semantic code graph for agents — CodeRadar builds on that foundation with capabilities CodeGraph doesn't have:

| Capability | CodeGraph | CodeRadar |
|-----------|-----------|-----------|
| **Mutate code** | ❌ Read-only | ✅ AST-aware body replacement, indent preservation, WriteGuard safety |
| **Static analysis suite** | ❌ | ✅ Dead-code detection with confidence tiers, Type-1–3 clone detection with TED verification, scaffolding/secrets scan, CFG metrics, harmonic centrality |
| **Temporal queries** | ❌ | ✅ Macrame bitemporal DB — query the graph as it existed at any point in time |
| **Rewrite safety** | ❌ | ✅ Dry-run mutation plans, stale-write rejection, automatic rollback on tainted updates |
| **Semantic fallback resolution** | ❌ | ✅ L4 embedding-based resolution when structural resolution fails |
| **Python-native embedding** | ❌ | ✅ Native Python integration for embeddings, GraphRAG, and ML pipelines |
| **Zero runtime boot** | 1.4s Node.js startup | ✅ <50ms — Python process is already warm |
| **LLM-driven refactoring** | ❌ | ✅ `replace_body()` — LLM proposes, CodeRadar validates, applies, and rolls back on error |

**The key insight:** CodeGraph answers "what is this codebase?" — CodeRadar answers that **and** "what was it yesterday?" **and** "what would it look like if I changed X?" **and** "apply that change safely."

## Performance

Head-to-head benchmarks (N=5 median, lower is better):

| Codebase | Files | Lang | CodeRadar | CodeGraph 1.5.0 | Ratio |
|----------|-------|------|-----------|-----------------|-------|
| CodeRadar self | 84 | Python+Rust | 554ms | 1,434ms | **0.39×** (faster) |
| codegraph-main | 558 | TypeScript | 12,232ms | 6,970ms | 1.75× |

CodeRadar wins on small-to-medium Python/Rust projects due to zero runtime boot overhead. On large TypeScript codebases, CodeGraph's hand-written per-language Rust walkers and flat-buffer emission are still faster than the generic `.scm`-query engine, but the gap narrowed from 2.77× to 1.75×. The optimization backlog lives in `docs/open-items.md` §2.8 (the old `performance-roadmap.md` was retired into the knowledge graph).

## Architecture

> Full system design: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) ·
> Operator cheat sheet: [`docs/QUICKREF.md`](docs/QUICKREF.md).

```
Surfaces (MCP 26 tools, CLI, Python CodeGraph — one name per op)
        │
    ops.py (shared validated op layer)  +  synthetic-edge bridge
        │
Rust _core (single-pass extraction, 41 grammars, resolve cascade L1-L4,
             query engine, mutation plans, smells, clones, snapshots)
        │
    Macrame 0.19 ledger (bitemporal facts) + SQLite hot store
    + sha256 blobs + archive sibling + FTS5 + Phase-1 vector keys
```

| Metric | Value |
|--------|-------|
| **Languages indexed** | 41 (12 Tier 1, 29 Tier 2, 330+ Tier 3) |
| **Tests** | 1512 passing (434 Rust + 1078 Python) |
| **Operations** | One name per graph operation on every surface that serves it: MCP tool `coderadar_<op>`, CLI command `coderadar <op>` (hyphenated), `CodeGraph.<op>()` — 26 ops (25 graph operations + `set_project` project switching, CLI: `-C`): explore, node, search, affected, resolve, query, search_similar, compute_embeddings, module_children, callers, callees, diagnose, as_of, traverse, get_smells, dead_code, find_clones, find_scaffolding, replace_body, update_signature, rename, create_entity, reindex, update_file, status, set_project. `search_symbols` (keyword search) and `archive` (retention) are Python-API-only by design; `visualize`, `shell`, `git`, `exclude`, `watch`, `init`, `load-snapshot`, `store-repair` stay CLI-side (local-process concerns) — see the step-4 surface verdict in the v0.12 deviations log |
| **Query surface** | Pest structural + Macrame agent traversals + vector search |
| **Frameworks** | Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, Rails, NestJS, Vue Router, React Router |
| **Agents** | MCP server over stdio — finds the project root, indexes in the background, and exits with its client |

## Quick Start

```bash
pip install coderadar-rs

# Write .coderadar.toml, create the store, run the first analysis
coderadar init

# Every MCP tool `coderadar_<op>` is the command `coderadar <op>`
# (underscores become hyphens), with the same arguments and the same text.
# Run from anywhere inside the project, or name it with -C.
coderadar explore UserService.create          # source + call paths
coderadar resolve "/users/:id"                # route → handler (any framework spelling)
coderadar query "functions where is_async == true"
coderadar affected "src/services.py::UserService.create"
coderadar traverse "src/auth.py::validate_user" --direction upstream
coderadar get-smells --rule-id god-class
coderadar -C ../other-project status

# Edits print a diff; --apply writes it
coderadar rename "src/auth.py::validate_user" is_valid_user
coderadar rename "src/auth.py::validate_user" is_valid_user --apply

# Scripts: --format json prints the data instead of the text
coderadar dead-code --min-confidence 0.8 --format json

# Keep the index current
coderadar update-file src/auth.py     # one file (a deleted file is dropped)
coderadar reindex                     # changed files; --full for everything
coderadar watch src/ --debounce 50

# Visualize (hierarchy, dependencies, call-graph)
coderadar visualize call-graph --format graphviz -o calls.dot

# Serve the graph to an MCP client (Claude Code, Cursor, ...)
coderadar mcp serve
```

> **0.13 removal notice.** The old CLI spellings (`analyze`, `rebuild`,
> `update`, `stats`, `blame`, `git-clean`, `git-diff`) still work but are
> hidden; they are removed in 0.13. Use `reindex`, `update-file`, `status`,
> and `git blame` / `git is-clean` / `git diff`.

## Python API

```python
import coderadar

# Index into memory. `coderadar init` is what creates the persistent store —
# analyze only writes to one that already exists, so a wrong path cannot
# leave a `.coderadar/` behind for the next root lookup to find.
graph = coderadar.analyze("src/")

# The same operations as methods. They return data (dicts, lists,
# dataclasses); the MCP tools and CLI commands render the same data as text.
# Failures raise coderadar.ops.OpError subclasses (NotFound, InvalidRequest, …).

# Source plus call paths for named symbols
result = graph.explore(symbols=["UserService.create"])

# Query
for row in graph.query("functions where caller_count == 0"):
    print(row["id"])

# One entity, its direct neighbours, the blast radius
entity = graph.node("src/services.py::UserService.create", include_neighbors=True)
callers = graph.callers("views.py::user_detail")  # includes framework edges
impact = graph.affected("src/auth.py::validate_user", max_depth=3)

# Walk calls / imports / extends / overrides (edge_kinds=None walks all four);
# full entity rows, the start node at depth 0
rows = graph.traverse("src/auth.py::validate_user", direction="upstream", max_depth=3)

# Edits are dry runs unless dry_run=False: {"plan", "result", "note"}
outcome = graph.replace_body(
    "src/auth.py::validate_user",
    "    return bool(re.match(r'^[^@]+@[^@]+$', email))",
)
print(outcome["plan"].diff_preview)
graph.rename("src/auth.py::validate_user", "is_valid_user", dry_run=False)

# Sync a changed (or deleted) file
report = graph.update_file("src/core/engine.py")

# Analyses and housekeeping
smells = graph.get_smells(rule_id="god-class")
dead = graph.dead_code(min_confidence=0.8)
print(graph.status())

# Temporal queries (Macrame bitemporal)
past = graph.as_of("2026-08-01T00:00:00Z")
```

### Entity IDs

Stored ids are canonical root-relative form: `<relative-path>::<Qualified.name>`
with forward slashes and no `./` prefix
(`src/auth.py::validate_user`) — one spelling on every platform, so ids
saved in a snapshot, baseline or CI artefact on one OS resolve on another.
The form is minted at write time by every path (analyze, `update_file`, the
store migration), so `analyze(".")` and `analyze(<abs-root>)` produce
identical keys. Pasted variants (absolute paths, backslashes, a leading
`./`) still resolve at every tool boundary, but prefer the canonical form in
scripts. A store written before 0.10 spells ids the old way
(`.\src\auth.py::validate_user`); `load` refuses it rather than serving a
half-broken graph, and the next analyze re-keys it (`coderadar store-repair`
reports the count).

## MCP Server

```bash
coderadar mcp serve            # walks up from the cwd looking for the project
coderadar mcp serve --path .   # or say where it is
```

```json
{
  "mcpServers": {
    "coderadar": {
      "command": "uv",
      "args": ["run", "coderadar", "mcp", "serve"]
    }
  }
}
```

The server key is yours: `"cr"` works identically and keeps client
configs short. Tool names keep the full `coderadar_` prefix either way —
the key names the server, the prefix names the tools.

MCP clients launch servers from wherever they happen to be, so the server does
not assume the cwd is the project:

- **Finding the project.** `rootUri` and `workspaceFolders` are LSP concepts
  and do not exist in MCP, so the ladder is what MCP actually offers — the
  client's `roots/list`, then `--path`, then the cwd. Each candidate is walked
  *up* looking for a `.coderadar/` or `.coderadar.toml` marker, stopping before
  the home directory. A confirmed root on a lower rung beats an unconfirmed one
  higher up: a marker on disk says where the project is, while a client root
  says where the client is.
- **Asking the client.** `roots/list` is a server-to-client request and
  awaiting one during `initialize` deadlocks, so it is asked lazily on the
  first tool call — once, and only if nothing on disk confirmed the root.
- **Entity-id grammar.** Every tool speaks one id shape:
  `<relative-path>::<Qualified.name>` — project-root-relative, forward
  slashes, no dot prefix (`app/helpers.py::combine`; methods as
  `File::Class.member`, modules as `File::module`). Read paths also accept
  absolute paths, backslashes, and a leading `./`, and a dotted qualified
  name (`app.helpers.combine`) resolves when a module prefix matches;
  `external::name` marks a callee outside the index (it resolves nothing
  further). Search `kind` is one of `function | class | type_alias |
  constant | module | import`; smell `strictness` is `strict | normal |
  loose` — anything else errors instead of returning empty.
- **Same directory as the index.** The process moves onto the resolved root
  before indexing, because entity ids are canonical root-relative form
  (`src/auth.py::validate_user`) and every read helper — Rust or Python
  — resolves them against the recorded indexed root, never the cwd.
- **Fast handshake.** Indexing runs on a background thread; a tool call that
  arrives early waits, then reports elapsed seconds rather than answering from
  a half-built graph. While waiting, heartbeats (`[coderadar] indexing … Ns
  elapsed`) go to stderr every few seconds (`CODERADAR_INDEX_HEARTBEAT`) —
  never silent, never an answer from a half-built graph.
- **Saying where it looked.** A "no index" reply names the directory being
  served and how that directory was chosen, so an agent pointed at the wrong
  project can say so.
- **One project at a time.** Every tool takes an optional `project_path`; a path
  inside the served project (a file, a subdirectory, or the root itself) is
  accepted via nearest-marker resolution; another project is refused with the
  reason, not quietly answered from the wrong codebase.
- **Switching projects without restarting.** `coderadar_set_project` re-runs
  startup against a new root from inside a tool call: that project's config
  and mutation policy take effect, indexing restarts in the background, and an
  explicit switch outranks the client's declared workspace for the rest of the
  connection.
- **Not outliving the client.** Handshake timeout, parent-process watchdog, and
  teardown when stdin closes.
- **Tool names are prefixed by the client.** Clients usually namespace tools
  as `<server>_<tool>`, so a server registered as `coderadar` shows them as
  `coderadar_coderadar_explore` and so on (or as `coderadar_explore` under a
  gateway that merges servers). Call tools by the name the client
  advertises, not the bare names used in this README
  (CODERADAR_BUGS_QUIRKS.md #10).
- **Default excludes.** The file walk skips a 15-directory build-output
  baseline (`.venv/`, `node_modules/`, `target/`, `dist/`, `build/`,
  `__pycache__/`, `.git/`, …) on top of `.gitignore` and `[project] exclude`,
  and one shared matcher enforces it in every pass — walker, star exports,
  framework extraction, watcher, and staleness — so generated artifacts never
  pollute counts or smells. `coderadar exclude list|add|remove` edits the
  config; `--exclude` narrows a single `reindex`; `coderadar status`
  prints the effective stack.

## Language Support

| Tier | Languages | Resolution | Mutation |
|------|-----------|------------|----------|
| **Tier 1** | Python, TypeScript, JavaScript, Rust, Go, Java, C, C++, Ruby, PHP, C#, Kotlin | Import → Signature → Framework | Full tool suite |
| **Tier 2** | Swift, Scala, Lua, Elixir, Zig, R, Bash, Dart, Protobuf, Dockerfile, SQL, HCL, CMake, GraphQL, Erlang, Haskell, **Nix, Shell, Groovy, Perl, SystemVerilog, OCaml, Clojure, F#, Verilog, Julia, PowerShell, Emacs Lisp, Objective-C** | Import → Signature | replace_body, create_entity |
| **Tier 3** | HTML, CSS, YAML, TOML, JSON, Markdown + 280 more | Signature Match only | replace_body, create_entity |

## Resolution Cascade

| Layer | Method | Confidence | Languages |
|-------|--------|------------|-----------|
| L1 | Import + Scope | 0.80–0.89 | All |
| L2 | Signature Match | 0.40–0.79 | All |
| L3 | Framework Resolvers | 0.80–1.00 | Python, Go, Rust |
| L4 | Embedding (Python) | 0.20–0.39 | Python |
| L5 | LSP Override | — | **Planned** — not wired into the cascade |

> **L5 (LSP Override) is planned, not shipped.** `py_agent/src/coderadar/lsp/` holds a server pool and an override type, but no production path reaches them — the resolution cascade runs L1–L4 only. The row is kept here because the layer numbering is referenced throughout the specs; it will move to shipped when the override is wired behind its config flag.

> **Stack Graphs (L1 in the v3.3 spec) was deferred to post-v1 and its placeholder module has been removed.** CodeGraph ships 30+ languages at production scale with zero Stack Graphs dependency — compiler-grade scope disambiguation is not required for MCP agent use cases.

## Framework Resolvers

CodeRadar detects and extracts framework-specific patterns that tree-sitter can't see:

| Framework | Language | Detection | Extracted Patterns |
|-----------|----------|-----------|-------------------|
| **Django** | Python | `manage.py` | `path()` routes, DRF `router.register()`, admin registrations, `.as_view()` handlers |
| **Flask** | Python | `@app.route` | Route decorators, Flask-RESTful `add_resource()`, Blueprint registration |
| **FastAPI** | Python | `APIRouter` | `@app.get()`/`@router.post()` routes, `Depends()` injection chains, `include_router()` |
| **Go** | Go | `go.mod` | `gin.GET()`, `mux.HandleFunc()`, Chi/Echo/Fiber route patterns |
| **Actix** | Rust | `Cargo.toml` | `App::new().route()`, `web::resource()`, `#[get]`/`#[post]` attribute macros |
| **Express** | JS/TS | `package.json` | `app.get()`, `router.post()`, chained `.route()` builder, `app.use()` middleware |
| **Spring Boot** | Java | `pom.xml`/`build.gradle` | `@GetMapping`, `@PostMapping`, `@RequestMapping(method=...)`, class-level `@RequestMapping` prefix, `[controller]` token replacement |
| **Laravel** | PHP | `composer.json` | `Route::get()`, `Route::resource()`, `Route::group()` prefix propagation, `[Controller::class, 'method']` array + `'Controller@method'` string syntax |
| **ASP.NET** | C# | `.csproj`/`.sln` | `[HttpGet]`, `[HttpPost]`, `[Route("api/[controller]")]` token replacement, Minimal API `app.MapGet()` |
| **Rails** | Ruby | `Gemfile` | `has_many`/`belongs_to`/`has_one` associations, `before_action`/`after_action` callbacks |
| **NestJS** | TS | `package.json` | `@Controller` route prefix, `@Get`/`@Post` routes, `@Module` dependency edges |
| **Vue Router** | JS/TS | `package.json` | `createRouter` route objects, lazy `import()` component resolution, `addRoute` dynamic routes |
| **React Router** | JSX/TSX | `package.json` | JSX `<Route>` declarations, v6 data router objects, `<Link>`/`<NavLink>` navigation tracking |

Framework edges are registered in the Rust graph — agents can trace from URL patterns to handler functions via `callers()` / `callees()`.

## v0.12.0 Highlights — history becomes real

Source history, keyword search, framework routes, Rust resolution, and
agent enablement — the full v0.12 plan per its release gate (see
`CHANGELOG.md`).

- **Source history in blobs (DR-25).** Every indexed generation puts its
  bytes as SHA-256 content-addressed blobs (`macrame-db` 0.19); the digest
  rides `extra.coderadar.source_blob`, diffs compute on read.
  `Snapshot.read_bytes` serves exact bytes-at-T (CRLF included), `read_source`
  the display form; missing blobs are `ContentUnavailable`, distinct from
  absent-at-T. `[retention] archive_after_days` moves cold bytes to a
  sibling archive file (hot+cold backup + restore tested); analyze never
  auto-archives. Default-on-with-notice (DR-30): blob counts on every
  report surface are the notice; kill-switch
  `[database] store_source_blobs=false`.
- **FTS5 keyword search (DR-34).** `search_symbols()` over
  trigger-maintained `concepts_fts` — escaped-by-default, live-only,
  single-digit milliseconds; Python-API-only by design.
- **Framework routes (DR-10).** Routes are canonical `route` concepts with
  persisted route→handler edges; `resolve /users/:id` follows them, with
  zero tree walks at steady state.
- **Rust cross-module resolution (DR-13).** `use` parsing (groups, globs,
  aliases, `crate`/`super`/`self` roots) + `::` call chains; `Self::assoc`
  and `Enum::Variant` resolve; self-corpus asserted edges 999 → 1320,
  dead-code findings 100 → 57.
- **Embeddings Phase 1 (DR-11).** Model id + preprocessing in the dedup key,
  write/query dimension gates, `recompute` flag on every surface;
  persistence stays 0.13.
- **Agent enablement (P2).** `skills/coderadar-mcp` + `skills/coderadar-cli`
  (repo source of truth, registry drift-tested), release-gate history proof
  (`as_of` → old/new names *and* bytes), read-path discipline pinned.
- **Upstream fix.** `macrame-db` 0.19.1 repairs the historical loader
  (opticsWolf/Macrame#3) via the bitemporal composition; the rename-fixture
  reproducer is green and un-ignored, production stays on the state fold by
  decision.

## Feature highlights — v0.5 → v0.11

Everything below shipped before v0.12 and still holds. Per-version detail
lives in `CHANGELOG.md`; the numbers are CI-guarded (1512 tests and
counting, clippy/ruff/fmt green and blocking, 100/100 resolution
precision-recall on the benchmark corpus).

**Index & serve.** Single-pass tree-sitter extraction (41 grammars: 12
Tier-1 with signature tests, 29 Tier-2), parallel pipeline, per-file
incremental `update_file` plus a debounced file watcher. The MCP server and
CLI cold-start from the Macrame ledger instead of re-indexing every session
(`build_graph`: load + refresh changed files only); the last project per
launch directory persists across restarts. Canonical forward-slash ids on
every path, one shared exclude matcher, `store-repair` + auto-retirement for
legacy rows, `-C/--project` with marker walk-up from any subdirectory.

**Resolution.** `self` attribute calls, typed locals and constructor results
resolve to real edges; relative imports, aliases, function-local imports,
module initializers, and transitive re-export chains all produce edges.
Thirteen framework resolvers (Django, Flask, FastAPI, Express, Rails,
NestJS, Vue/React Router, …) register route → handler edges in the graph.
Cross-file MRO with populated subclasses/importers/overrides indexes.
Rename rewrites attribute call sites and cascades base/override renames;
what the graph cannot resolve is reported as `unverified_sites`, never
silently skipped.

**Read.** A real query language (`functions where …`, generated reference,
"did you mean" on unknown fields) next to multi-token `search_entities`,
embedding `search_similar`, whole-graph `traverse` over all four asserted
edge kinds with honest `unresolved` targets, temporal `as_of`, blast-radius
`affected` (centrality-ranked), and `diagnose`. Misses say which tokens
were tried and where to look next.

**Analysis.** Twelve smell rules with confidence tiers and strictness
profiles; deterministic token→MinHash→LSH→TED clone detection
(byte-identical across processes); reachability dead code from an
entry-point ladder (framework decorators, protocol members, public API,
`pyproject` scripts, `__all__`) with evidence and distances — self-corpus
findings fell 269 → 121 as resolution improved while recall held 99.97 %.
CFG metrics, harmonic centrality, and a scaffolding/secrets scanner
round it out.

**Mutate.** Plan-then-apply with byte-span verification: every edit carries
a content hash (stale writes rejected), the written file must parse
(failures diagnose, never silently apply), docstrings survive body replace,
and method renames cascade. Dry-run unless applied; the allow-list is
root-anchored; the watcher's write guard skips the engine's own writes.

**Surfaces.** 26 operations, one name each — `coderadar_<op>` (MCP),
`coderadar <op>` (CLI), `CodeGraph.<op>()` — all calling one shared `ops`
function, pinned by parity tests. CLI takes `--format json`, logs to
stderr (exit 2 bad input, 1 otherwise), pipes UTF-8 everywhere; `status`
reports project, config, store, and freshness.


### Renamed in v0.11 (removed in 0.13)

Old names still work with a `DeprecationWarning` or a stderr note, hidden from `--help`.
Update client allow-lists and scripts that call tools by name: the 17 `codegraph_*` tools are now `coderadar_*`, and `node`/`affected` take `entity_id`.

| Old | New |
|-----|-----|
| `coderadar analyze PATH` / `rebuild PATH` | `coderadar -C PATH reindex --full` |
| `coderadar update FILE` | `coderadar update-file FILE` |
| `coderadar stats` | `coderadar status` |
| `coderadar blame` / `git-clean` / `git-diff` | `coderadar git blame` / `git is-clean` / `git diff` |
| `coderadar traverse --depth/--edges` | `--max-depth/--edge-kinds` |
| `CodeGraph.find` | `CodeGraph.node` |
| `CodeGraph.callers_of` / `callees_of` | `callers` / `callees` |
| `CodeGraph.plan_body_replacement` / `plan_signature_update` | `plan_replace_body` / `plan_update_signature` |
| `CodeGraph.explore(start_id=…, max_depth=…)` (call-graph walk) | `CodeGraph.traverse` |
| `CodeGraph.traverse(edge_types=…)` | `edge_kinds=…` |

> **Upgrading from 0.9.x:** old-spelling stores are refused with the migration hint;
> `reindex` (or a cold start, which does it automatically) re-keys them.

## Project Structure

```
core_indexer/              # Rust core
  queries/                 # 41 .scm query files (one per Tier-1/2 language)
  src/
    extract/               # Tree-sitter: tagger (query cursor) + walker (hierarchy) + docstring + decorators
    update/                # Incremental diff + patch + WAL
    resolve/               # Resolution cascade (import_graph, orchestrator, signature, cache)
    query/                 # Pest grammar + execution engine
    mutation/              # AST-aware refactoring (rope, indent, unified diffs, WriteGuard)
    fs/                    # File watcher (notify) + git integration
    graph/                 # In-memory ProjectedGraph, parallel extraction, reverse indexes,
                           #   call/import/inheritance resolution, traversal, persistence,
                           #   dead-code reachability + RTA-lite (deadcode/, rta_lite.rs),
                           #   structured CFG (cfg.rs), harmonic centrality (centrality.rs)
    clones/                # Token-level clone detection: normalized tokens, MinHash + banded
                           #   LSH, Type-1/2/3 funnel, Zhang–Shasha TED verification (apted.rs)
    scaffold/              # AI-scaffolding detector tables + secrets scanner
    smells/                # Native code-smell engine (metrics pass, 12 rules incl. dead-code /
                           #   dead-branch / intra-dead-statements, engine, registry,
                           #   strictness profiles, tri-state const evaluator)
    storage.rs             # Macrame concept/edge persistence
    lib.rs                 # PyO3 FFI bindings

py_agent/src/coderadar/    # Python layer
    resolvers/             # 13 framework resolvers: Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, Rails, NestJS, Vue Router, React Router
    embedding/             # Content-addressed dedup
    agent/                 # GraphRAG query pipeline
    lsp/                   # Persistent LSP warm pool
    mutation/              # Tool router for LLM
    mcp/                   # MCP server
      server.py            #   26 tools + guidance
      roots.py             #   project-root ladder and marker walk-up
      startup.py           #   background index, ensure_ready()
      lazy.py              #   roots/list retry on the first tool call
      lifecycle.py         #   handshake timeout, parent watchdog, teardown
    query/                 # Query planner + templates + cache
    visualizers/           # Mermaid + Graphviz (SCC cycle highlighting)

docs/                      # Live doc set: ARCHITECTURE.md (system design),
                           #   QUICKREF.md (operator cheat sheet), v0.12 trio +
                           #   deviations, store-and-retention, query-language
                           #   (generated), smell reference, open-items,
                           #   BUGS_QUIRKS. Retired docs live in OKFgraph.
tests/                     # 1078 Python tests (E2E incl. dead-code/clones/scaffold/CFG/
                           #   centrality/dead-branch/RTA goldens, mutation E2E, MCP,
                           #   framework resolvers, ingest parity, benchmarks)
  mcp/                     # Root resolution, background init, lifecycle, project_path
```

## Configuration

`.coderadar.toml` at the project root is the only configuration file; `coderadar init` writes a starter one. Every key in it is read by something, and `coderadar reindex` prints a line naming any key it could not use, so a stale or misspelled setting says so instead of sitting silent.

```toml
# .coderadar.toml
[project]
# Omitted, the whole project root is walked. Set it and the walk is confined
# to these subdirectories — an empty index is the usual sign of a typo here.
# roots = ["src/", "tests/"]
exclude = ["**/__pycache__/**", "**/.venv/**"]

[database]
path = ".coderadar/store/coderadar.db"
# Source-blob write path (§1.9, default-on-with-notice per DR-30):
# blob counts on every report surface are the notice; `false` is the
# kill-switch. `blob_exclude` unions with the secret belt (*.env et al).
store_source_blobs = true
# blob_exclude = ["**/secrets/**"]

[retention]
# Days-hot cutoff (§3.4): archive blobs older than this on explicit
# `CodeGraph.archive()` / `ops.archive()` only — analyze never
# auto-archives. Unset (default) means no file default.
# archive_after_days = 30

[embedding]
# Indexing and search must name the same model: a dimension mismatch produces
# confident nonsense rather than an error.
model = "BAAI/bge-small-en-v1.5"
dimension = 384

[watch]
debounce_ms = 100
max_file_size_bytes = 1048576

[mutation]
enabled = true
default_dry_run = true
allow = ["src/", "lib/", "tests/", "scripts/"]
deny = [".git/", ".coderadar/", "/migrations/", "/*.lock", "/generated/"]

[resolution]
min_confidence = 0.3

[resolution.import_graph]
max_import_depth = 3
```

`[resolution.signature]` and `[query]` are accepted and stored but not yet read on any live path — they wait on the code that would consume them. `[resolution.lsp]` is accepted by the schema and deliberately absent from the starter file: the pool it configures is never constructed, so a value there would only be noise.

## License

MIT — incorporates techniques from [CodeGraph](https://github.com/opticsWolf/codegraph) (MIT License).
