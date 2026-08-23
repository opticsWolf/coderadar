# CodeRadar v0.6.45

[![CI](https://github.com/opticsWolf/coderadar/actions/workflows/ci.yml/badge.svg)](https://github.com/opticsWolf/coderadar/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/coderadar-rs?label=pypi)](https://pypi.org/project/coderadar-rs/)
[![Python](https://img.shields.io/pypi/pyversions/coderadar-rs)](https://pypi.org/project/coderadar-rs/)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Languages](https://img.shields.io/badge/languages-41-brightgreen)]()

**Live semantic graph of your codebase — incremental, queryable, LLM-writable.**

CodeRadar maintains an incrementally updatable graph of your code's logical structure, enabling LLMs and developer tools to both **query** and **safely rewrite** code through a unified pipeline.

## Why CodeRadar

CodeGraph pioneered the semantic code graph for agents — CodeRadar builds on that foundation with capabilities CodeGraph doesn't have:

| Capability | CodeGraph | CodeRadar |
|-----------|-----------|-----------|
| **Mutate code** | ❌ Read-only | ✅ AST-aware body replacement, indent preservation, WriteGuard safety |
| **Temporal queries** | ❌ | ✅ Macrame bitemporal DB — query the graph as it existed at any point in time |
| **Rewrite safety** | ❌ | ✅ Dry-run mutation plans, stale-write rejection, automatic rollback on tainted updates |
| **Semantic fallback resolution** | ❌ | ✅ L4 embedding-based resolution when structural resolution fails |
| **Python-native embedding** | ❌ | ✅ Native Python integration for embeddings, GraphRAG, and ML pipelines |
| **Zero runtime boot** | 1.4s Node.js startup | ✅ <50ms — Python process is already warm |
| **LLM-driven refactoring** | ❌ | ✅ `plan_body_replacement()` — LLM proposes, CodeRadar validates, applies, and rolls back on error |

**The key insight:** CodeGraph answers "what is this codebase?" — CodeRadar answers that **and** "what was it yesterday?" **and** "what would it look like if I changed X?" **and** "apply that change safely."

## Performance

Head-to-head benchmarks (N=5 median, lower is better):

| Codebase | Files | Lang | CodeRadar | CodeGraph 1.5.0 | Ratio |
|----------|-------|------|-----------|-----------------|-------|
| CodeRadar self | 84 | Python+Rust | 554ms | 1,434ms | **0.39×** (faster) |
| codegraph-main | 558 | TypeScript | 12,232ms | 6,970ms | 1.75× |

CodeRadar wins on small-to-medium Python/Rust projects due to zero runtime boot overhead. On large TypeScript codebases, CodeGraph's hand-written per-language Rust walkers and flat-buffer emission are still faster than the generic `.scm`-query engine, but the gap narrowed from 2.77× to 1.75×. See [performance-roadmap.md](docs/performance-roadmap.md) for the optimization backlog.

## Architecture

```
Python Layer (CLI, Visualizers, Framework Resolvers, GraphRAG, MCP Server)
        │
    PyO3 FFI  +  register_synthetic_edge() bridge
        │
Rust Core (ProjectedGraph, Tree-sitter 41-lang, Parallel Extraction,
           Resolution Cascade L1-L3, Query Engine, Mutation Engine, Smell Engine)
        │
    Macrame DB (bitemporal persistence with valid_from/valid_to timestamps)
```

| Metric | Value |
|--------|-------|
| **Languages indexed** | 41 (12 Tier 1, 29 Tier 2, 330+ Tier 3) |
| **Tests** | 824 (250 Rust + 574 Python) |
| **MCP Tools** | 18 (explore, search, node, affected, resolve, query, search_similar, module_children, as_of, traverse, get_smells, replace_body, update_signature, rename, create_entity, compute_embeddings, reindex, update_file) |
| **Query surface** | Pest structural + Macrame agent traversals + vector search |
| **Frameworks** | Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, Rails, NestJS, Vue Router, React Router |
| **Agents** | MCP server over stdio — finds the project root, indexes in the background, and exits with its client |

## Quick Start

```bash
pip install coderadar-rs

# Write .coderadar.toml, create the store, run the first analysis
coderadar init

# Query
coderadar query "functions where is_async == true"

# Trace call flows
coderadar callers "src/services.py::UserService.create"
coderadar callees "src/services.py::UserService.create"

# Watch for changes
coderadar watch src/ --debounce 50

# Visualize (hierarchy, dependencies, call-graph)
coderadar visualize call-graph --format graphviz -o calls.dot

# Serve the graph to an MCP client (Claude Code, Cursor, ...)
coderadar mcp serve
```

## Python API

```python
import coderadar

# Index into memory. `coderadar init` is what creates the persistent store —
# analyze only writes to one that already exists, so a wrong path cannot
# leave a `.coderadar/` behind for the next root lookup to find.
graph = coderadar.analyze("src/")

# Query
for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
    print(cls.name, [m.name for m in cls.methods])

# Call-graph walk — rows of {entity_id, edge_kind, direction, depth}
flow = graph.explore("src/services.py::UserService.create",
                     direction="out", max_depth=2)

# Callers (includes framework edges: route → handler)
callers = graph.callers_of("views.py::user_detail")

# Update after file change
report = graph.update_file("src/core/engine.py")

# Mutation (LLM-driven)
plan = graph.plan_body_replacement(
    entity_id="src/auth.py::validate_user",
    new_body="    return bool(re.match(r'^[^@]+@[^@]+$', email))",
    dry_run=True
)

# Module children resolution
from coderadar._core import module_children
children = module_children("src/auth.py::module")
for cls in children["classes"]:
    print(cls["name"], cls["grammar_kind"])

# Temporal queries (Macrame bitemporal)
past = graph.as_of("2026-08-01T00:00:00Z")

# Graph walk across calls / imports / extends / overrides — full entity rows,
# with the start node at depth 0. edge_types=None walks all four kinds.
neighbors = graph.traverse("src/auth.py::validate_user", max_depth=3, direction="both")

# Code smells (native Rust engine, 9 rules)
from coderadar._core import get_smells
for finding in get_smells(rule_id="god-class"):
    print(finding["entity_name"], finding["severity"], finding["message"])
```

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
- **Same directory as the index.** The process moves onto the resolved root
  before indexing, because entity ids carry the path the walk started from
  while every read helper resolves against the cwd.
- **Fast handshake.** Indexing runs on a background thread; a tool call that
  arrives early waits, then reports elapsed seconds rather than answering from
  a half-built graph.
- **Saying where it looked.** A "no index" reply names the directory being
  served and how that directory was chosen, so an agent pointed at the wrong
  project can say so.
- **One project per server.** Every tool takes an optional `project_path`; a
  path that is not the served root is refused with the reason, not quietly
  answered from the wrong codebase.
- **Not outliving the client.** Handshake timeout, parent-process watchdog, and
  teardown when stdin closes.

## Language Support

| Tier | Languages | Resolution | Mutation |
|------|-----------|------------|----------|
| **Tier 1** | Python, TypeScript, JavaScript, Rust, Go, Java, C, C++, Ruby, PHP, C#, Kotlin | Import → Signature → Framework | Full tool suite |
| **Tier 2** | Swift, Scala, Lua, Elixir, Zig, R, Bash, Dart, Protobuf, Dockerfile, SQL, HCL, CMake, GraphQL, Erlang, Haskell, **Nix, Shell, Groovy, Perl, SystemVerilog, OCaml, Clojure, F#, Verilog, Julia, PowerShell, Emacs Lisp, Objective-C** | Import → Signature | replace_body, create_entity |
| **Tier 3** | Shell, SQL, HTML, CSS, YAML, TOML, JSON, Markdown + 280 more | Signature Match only | replace_body, create_entity |

## Resolution Cascade

| Layer | Method | Confidence | Languages |
|-------|--------|------------|-----------|
| L1 | Import + Scope | 0.80–0.89 | All |
| L2 | Signature Match | 0.40–0.79 | All |
| L3 | Framework Resolvers | 0.80–1.00 | Python, Go, Rust |
| L4 | Embedding (Python) | 0.20–0.39 | Python |
| L5 | LSP Override | 1.00 | Optional, disabled by default |

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

Framework edges are registered in the Rust graph — agents can trace from URL patterns to handler functions via `callers_of()` / `callees_of()`.

## v0.6.45 Feature Highlights

The v0.7 improvement plan, start to finish — write-path correctness, temporal
truth, scaling, dead-code retirement, configuration, and the MCP layer.

- **Write path.** `update_signature` had never worked: it wrote a whole
  `def f(a, b):` line into a span covering only `(a)`, so `apply` caught the
  syntax error and rolled back every time. Rename now verifies its byte spans
  before emitting edits, class rename is reachable, `apply_diff_update` stops
  dropping parameters, and mutation policy is enforced at the FFI boundary
  rather than trusting a plan that arrives as JSON.
- **Real diffs.** Mutation previews are unified diffs that apply cleanly with
  `patch`, replacing a positional line-by-line comparison that reported every
  line after an insertion as changed.
- **Temporal truth.** Removed entities and edges are retired in the ledger,
  `persist_edges` is scoped to the changed file, deletions reach the graph, and
  `graph_stats()` exposes `indexed_at` — the staleness banners read a key that
  nothing had ever set, so every one of them was unreachable.
- **Scaling.** Bulk write APIs remove whole-projection clones, resolution and
  smell lookups are indexed, and query rows are built lazily.
- **The GIL.** `analyze` and `update_file` release it. Held end to end, an
  `asyncio.to_thread(analyze, ...)` froze the event loop for the entire index.
- **Honest silence.** `analyze` reports extraction failures and panicked
  workers instead of returning a count that cannot distinguish "nothing to do"
  from "nothing worked".
- **MCP.** Root resolution, background init, lazy `roots/list`, optional
  `project_path`, lifecycle hygiene — see the MCP Server section above.
- **Visualizers drew fiction.** Every renderer answered an empty or
  unreadable graph with a hardcoded example — `BaseModel <|-- UserService`,
  `auth.login --> db.query` — and returned it as a normal result with exit 0.
  Both DOT renderers reached it *always*: they enumerated entities through a
  `CodeGraph.search_entities` that does not exist, swallowed the
  `AttributeError`, and fell through, so every DOT diagram ever produced was
  demo data. The Mermaid side text-searched for the word "class", matching
  `from dataclasses import dataclass`. Inheritance edges were read from
  `callees_of` (call edges, not inheritance) and dependency edges pointed at
  import-*statement* entities. All of it now reads the real indexes, and an
  empty graph is an error naming what to do about it.
- **Commands that answered nothing.** `rebuild` printed "Rebuilding..." and
  returned without indexing. `status` printed "CodeRadar is running"
  unconditionally — a health check that could not fail. `diagnose` printed
  two headers and no rows, which reads as a clean bill of health rather than
  a report that was never written. All three now report real numbers.
  `mutations` was removed: it documented an "audit trail from MutationLog"
  for a MutationLog that exists nowhere in the codebase.
- **Two commands named `watch`.** Click registers by function name, so the
  second definition silently replaced the first — and the losing one carried
  the config activation, leaving the survivor running without ever reading
  `.coderadar.toml`. The dead one is gone and the live one activates config
  and indexes before watching.
- **Exit codes that lied.** `coderadar update` printed "Fully applied:
  False" and exited 0, so a script driving updates could not tell a failure
  from a success. `git-clean` defaulted to reporting a clean worktree when
  the check itself failed — the answer a caller is most likely to act on.
- **Dead entity fields.** `is_async`, `is_generator`, and `decorators` were
  hardcoded `false`/empty at the single site that builds every function
  entity, for every language — so `functions where is_async == true`, a
  documented query, could never match, and `derive_function_kind` classified
  every `@property` and `@staticmethod` as a plain method.
- **A cold CLI.** The graph lives in the process that built it, so every
  read-only command after `coderadar init` started empty and answered "No
  graph loaded — run coderadar init first", which the user had just done.
  They now index on demand until cold start from the ledger lands.
- **Both graph walks.** `CodeGraph.explore()` read `target`/`source` keys off
  rows that carry neither, so it raised `KeyError` for any entity that had
  edges and looked correct only for entities that had none; it also
  advertised `max_depth` while taking exactly one hop. `traverse()` treated
  `edge_types=None` — documented as "all kinds" — as an empty kind list, and
  the BFS loops over the kinds it is given, so the default walk returned the
  start node and stopped.
- **Configuration.** `.coderadar.toml` is read by something, key by key, and
  `coderadar analyze` names any key it could not use. ~100 inert knobs were
  removed rather than left looking load-bearing.
- **~4,300 lines of dead code retired**, including the Stack Graphs
  placeholder.
- **824 tests, 0 failures** — 250 Rust + 574 Python, including an end-to-end
  mutation suite (plan → apply → reindex → read the file back) and a
  parametrised no-index suite that replaced fourteen assertions which could
  not fail.

## v0.6.6 Feature Highlights

- **Base-resolution heuristics** — language-family filtering (TypeScript/JavaScript treated as one inheritance family), import-aware base resolution, and `@/`/`~/` → `src/` path-alias normalization. TypeScript `import { X, type T } from '...'` now parses correctly (previously misclassified as an empty module); ambiguous base candidates are surfaced via `index_edge_stats` (real-world: 4 → 0)
- **Traversal honesty** — `traverse_unresolved` + an MCP warning reveal targets the walk couldn't follow instead of silently truncating; all four mutation renderers emit a loud ⚠️ `unverified_sites` warning; `traverse(as_of=<ts>)` now reads the Macrame bitemporal ledger (downstream)
- **Correctness fixes** — edges were being asserted with the 9999 open sentinel as `valid_from` (breaking temporal reads); inline date math double-added the epoch offset (every timestamp was ~year 5910); `Class.methods` is now derived denormalization (query `method_count` returns real values); `get_smells` and `as_of` release the graph read lock before long-running work
- **Smell golden tests** — exact-signal snapshots for deep-nesting, brain-method, excessive-returns, and a positive god-class fixture
- **574 tests, 0 failures** — 207 Rust + 367 Python

## v0.6.5 Feature Highlights

- **Native Rust code-smell engine** — 9 structural smells (god-class, long-method, long-parameter-list, deep-nesting, data-class, high-cyclomatic-complexity, brain-method, excessive-returns, too-many-fields) with severity tiers, exposed via the `codegraph_get_smells` MCP tool (filter by `entity_id` and/or `rule_id`)
- **AST metrics pass** — cyclomatic complexity, nesting depth, and return count computed during single-pass extraction (`Function.metrics`), so the engine needs no source re-parse; class-level roll-ups (WMC, max-method cyclomatic, CBO) derived from the resolved graph
- **Class-field extraction** — class-level `@field` captures now populate `Class.fields` (previously always empty), unblocking the class-scope rules
- **Generalized `traverse` binding** — native-Rust BFS across all 4 edge kinds (calls, imports, extends, overrides) with `py.allow_threads`, replacing the pure-Python fallback
- **Resolve back-fill** — `subclasses`, `importers`, and `overrides` reverse indexes populated (previously silently empty); cross-file MRO; TS/JS `extends`/`implements` base capture; Module concepts emitted so IMPORTS edges persist to Macrame
- **556 tests, 0 failures** — 200 Rust + 356 Python

## v0.6.4 Feature Highlights

- **Query engine fixed** — Pest `WHERE` clauses now match (atomic `path` rule yielded `Path([])`, non-silent `operand`/`value` wrappers fell through to a string-literal arm; `name == "x"` / `name contains "x"` / `caller_count > 0` all returned 0 rows). Fixed path parsing, operand/value recursion, and Int/Float mixed comparison arms.
- **`and`/`or` chains fixed** — boolean folds panicked (`parts.remove(1)` assumed the keyword was a pest pair, but string literals are silent); rewritten as left-associative folds.
- **`imports` query fixed** — `target_kind` is now derived from `ImportResolution` (function/class/module/import/external/wildcard/dynamic/unresolved) so `imports where target_kind == "external"` works.
- **`traverse` edge filter fixed** — `codegraph_traverse` returned "No neighbors" because the fallback filtered entity `kind` ("function") against edge types ("calls"); now matches the edge type case-insensitively.
- **Anonymous functions skipped** — anon callbacks no longer collapse to one empty-name `"file::"` entity; named functions stay accurate (calls still attributed to enclosing fn via stack frames).
- **Query UX** — single-quoted strings now parse; empty-query prompt shows even without a loaded graph.
- **531 tests, 0 failures** — 180 Rust + 351 Python (extended E2E + TestQueryTool with real-row assertions)

## v0.6.3 Feature Highlights

- **Mutation safety hardened** — stale-write rejection (every edit carries an xxh3_64 content hash of its span, verified before any write → `RejectedStale` on mismatch) and automatic rollback on tainted updates (backup → atomic write → tree-sitter post-parse → restore on introduced syntax errors)
- **WriteGuard wired up** — mutation writes are suppressed in a shared process-wide guard so the file watcher doesn't re-index the engine's own writes
- **create_entity fixed** — language-aware code rendering (Python/Rust/Go/JS/TS/Java/C#/PHP/Ruby), real byte spans for top/end anchors, project-relative path canonicalization
- **Honest error reporting** — `update_file` surfaces `fully_applied=False` instead of swallowing failures; `search_similar` caches the embedding model
- **524 tests, 0 failures** — 176 Rust + 348 Python

## v0.6.0 Feature Highlights

- **17 MCP tools** — full query surface: explore, search, node, affected, resolve, query (Pest), search_similar (embeddings), module_children, as_of (temporal), traverse (graph walk), replace_body, update_signature, rename, create_entity, compute_embeddings, reindex, update_file
- **Embeddings pipeline** — compute + store + search_similar across ALL entity types (functions, classes, modules, imports, constants, type aliases); fastembed/BGE-small, xxHash dedup, auto-trigger on first search_similar call
- **Mutation pipeline** — plan-review-apply via dry_run toggle; rename cascades to all references; create_entity with language-aware placement
- **13 framework resolvers** — Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, Rails, NestJS, Vue Router, React Router
- **41 languages** — 12 Tier 1, 29 Tier 2, 330+ Tier 3 via tree-sitter-language-pack 1.14
- **509 tests, 0 failures** — 168 Rust + 341 Python, full E2E and MCP coverage

## v0.5.7 Feature Highlights

- **13 framework resolvers** — Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, **Rails**, **NestJS**, **Vue Router**, **React Router** — detect route registrations, model associations, controller callbacks, and navigation links across 7 languages
- **10 new languages** — Bash, Dart, Protobuf, Dockerfile, SQL, HCL, CMake, GraphQL, Erlang, Haskell (28 languages total across 3 tiers)
- **QueryPlanner** — natural-language intent classifier routing to MacrameQuery primitives
- **476 tests, 0 failures** — 163 Rust + 313 Python, full E2E coverage

## v0.5.6 Feature Highlights

- **9 framework resolvers** — Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET — detect route registrations and synthesize handler edges across 6 languages
- **10 new languages** — Bash, Dart, Protobuf, Dockerfile, SQL, HCL, CMake, GraphQL, Erlang, Haskell (28 languages total across 3 tiers)
- **QueryPlanner** — natural-language intent classifier routing to MacrameQuery primitives
- **451 tests, 0 failures** — 163 Rust + 288 Python, full E2E coverage

## v0.5.4 Feature Highlights

- **Single-pass cursor-driven extraction** — QueryCursor directly drives entity emission, eliminating the two-pass tag→walk pipeline. Inline fn-ref subtree scanning during function emission. **37% faster** on real-world TypeScript codebases.
- **Parallel extraction pipeline** — 3-phase design: collect → parallel parse/tag/walk (fragment merge) → sequential projection commit.
- **18-language query files** — per-language `.scm` queries with automated compile validation. C/C++ and TypeScript/JavaScript query files split to eliminate grammar mismatches.
- **Query compilation caching** — `CompiledQuery` wraps pre-compiled queries + pre-indexed capture tags; compiles once per language, not per file.
- **`grammar_kind` field** — raw tree-sitter node kind on every Class entity (e.g. `class_declaration/struct` for Swift)
- **Function-as-value capture** — detects `self.on_click = handler`, callback assignments, return values, kwargs
- **Cross-file fn-ref** — resolves imported names across module boundaries
- **Noise filtering** — builtin type filter (70+ types), literal receiver filter, name stoplist (12 names)
- **Docstring extraction** — preceding comment runs for all languages, not just `@docstring` captures
- **Elixir `def`/`defp`** — precise extraction via predicate queries
- **`__all__` detection** — `=`, `+=`, `.extend()`, `.append()` patterns
- **`module.children()`** — resolves child entity IDs to full dicts
- **Parameter annotations** — type annotations extracted and filtered for builtins
- **Live file watcher** — `notify`-based debounced watcher with incremental re-indexing
- **Graphviz visualizer** — call graph rendering with SCC cycle highlighting
- **Scoped call resolution** — per-file resolution with caller/callee tracking
- **Benchmark pipeline** — balanced (50 modules × 1000 calls) and heavy (100 modules × 4000 calls) correctness tests

## Project Structure

```
core_indexer/              # Rust core
  queries/                 # 18 .scm query files (one per language)
  src/
    extract/               # Tree-sitter: tagger (query cursor) + walker (hierarchy) + docstring + decorators
    update/                # Incremental diff + patch + WAL
    resolve/               # Resolution cascade (import_graph, orchestrator, signature, cache)
    query/                 # Pest grammar + execution engine
    mutation/              # AST-aware refactoring (rope, indent, unified diffs, WriteGuard)
    fs/                    # File watcher (notify) + git integration
    graph/                 # In-memory ProjectedGraph, parallel extraction, reverse indexes,
                           #   call/import/inheritance resolution, traversal, persistence
    smells/                # Native code-smell engine (metrics pass, 9 rules, engine, registry)
    storage.rs             # Macrame concept/edge persistence
    lib.rs                 # PyO3 FFI bindings

py_agent/src/coderadar/    # Python layer
    resolvers/             # 13 framework resolvers: Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, Rails, NestJS, Vue Router, React Router
    embedding/             # Content-addressed dedup
    agent/                 # GraphRAG query pipeline
    lsp/                   # Persistent LSP warm pool
    mutation/              # Tool router for LLM
    mcp/                   # MCP server
      server.py            #   18 tools + guidance
      roots.py             #   project-root ladder and marker walk-up
      startup.py           #   background index, ensure_ready()
      lazy.py              #   roots/list retry on the first tool call
      lifecycle.py         #   handshake timeout, parent watchdog, teardown
    query/                 # Query planner + templates + cache
    visualizers/           # Mermaid + Graphviz (SCC cycle highlighting)

docs/                      # Specifications + code review + performance roadmap
tests/                     # 574 Python tests (E2E, mutation E2E, MCP, smells,
                           #   framework resolvers, ingest parity, benchmarks)
  mcp/                     # Root resolution, background init, lifecycle, project_path
```

## Configuration

`.coderadar.toml` at the project root is the only configuration file; `coderadar init` writes a starter one. Every key in it is read by something, and `coderadar analyze` prints a line naming any key it could not use, so a stale or misspelled setting says so instead of sitting silent.

```toml
# .coderadar.toml
[project]
# Omitted, the whole project root is walked. Set it and the walk is confined
# to these subdirectories — an empty index is the usual sign of a typo here.
# roots = ["src/", "tests/"]
exclude = ["**/__pycache__/**", "**/.venv/**"]

[database]
path = ".coderadar/store/coderadar.db"

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
