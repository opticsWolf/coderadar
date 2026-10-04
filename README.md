# CodeRadar v0.11.0

[![CI](https://github.com/opticsWolf/coderadar/actions/workflows/ci.yml/badge.svg)](https://github.com/opticsWolf/coderadar/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/coderadar-rs?label=pypi)](https://pypi.org/project/coderadar-rs/)
[![Python](https://img.shields.io/pypi/pyversions/coderadar-rs)](https://pypi.org/project/coderadar-rs/)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Languages](https://img.shields.io/badge/languages-41-brightgreen)]()

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
| **Tests** | 1359 passing (413 Rust + 946 Python; 1 Python skipped) |
| **Operations** | 23, one name each: MCP tool `coderadar_<op>`, CLI command `coderadar <op>` (hyphenated), `CodeGraph.<op>()` — explore, node, search, affected, resolve, query, search_similar, compute_embeddings, module_children, as_of, traverse, get_smells, dead_code, find_clones, find_scaffolding, replace_body, update_signature, rename, create_entity, reindex, update_file, status, set_project (CLI: `-C`) |
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

## v0.11.0 Highlights — one surface

One name per operation on all three surfaces: the MCP tool
`coderadar_<op>`, the CLI command `coderadar <op>` (underscores become
hyphens) and the `CodeGraph` method `<op>`. All three call the same
`coderadar.ops` function, and the tools and commands print the same
`coderadar.render` text; `tests/test_surface_parity.py` keeps it that way.

**Breaking — MCP tool names.** The 17 `codegraph_*` tools are now
`coderadar_*` (`codegraph_explore` → `coderadar_explore`, …), so every tool
shares one prefix. `coderadar_node` and `coderadar_affected` take
`entity_id` instead of `id`. Update client allow-lists and scripts that call
tools by name.

**New**

- `coderadar_status` (MCP), `coderadar status`, `CodeGraph.status()`: the
  served project, its config and store, and how fresh the index is.
- CLI commands for every operation: `explore`, `node`, `search`,
  `affected`, `resolve`, `search-similar`, `compute-embeddings`,
  `module-children`, `as-of`, `get-smells`, `dead-code`, `find-clones`,
  `find-scaffolding`, `replace-body`, `update-signature`, `rename`,
  `create-entity`, `reindex`, `update-file`. Each takes `--format json`;
  edits are dry runs unless `--apply`. Errors go to stderr, exit 2 for bad
  input and 1 otherwise.
- `-C/--project`, and the CLI works from any directory inside a project:
  it walks up to the nearest `.coderadar` marker, as the MCP server does.
- `CodeGraph` methods for the operations it lacked: `node`, `callers`,
  `callees`, `replace_body`, `update_signature`, `rename`, `create_entity`,
  `reindex`, `status`; `explore` now returns what `coderadar_explore` shows.
- `update_file` on a file deleted from disk drops it from the graph.
- `coderadar traverse` follows every edge kind by default, like the tool.
- Piped CLI output is UTF-8 on every platform.

**Renamed** (old names still work, with a `DeprecationWarning` or a note on
stderr, and are hidden from `--help`)

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

## v0.10.0 Highlights — precision

The 0.10 release is one theme: **findings you can act on**. Every phase was
measured against a corpus before and after, and the numbers are guarded by
CI ([docs/precision-benchmark.md](docs/precision-benchmark.md)).

| | before | after |
|---|---|---|
| Dead-code findings on this repo (449 functions) | 269 (60 %), 93 High | 121 (27 %), 2 High |
| Extraction recall (call sites inside functions) | ~85 % | 99.97 % |
| In-repo resolution rate (dogfood corpus) | 21.1 % | 23.0 % |
| Resolution precision / recall | 100 % / 100 % | 100 % / 100 % |

**Extraction & resolution (§1).** Attribute calls on `self`, typed locals and
constructor results resolve to real edges instead of `external`; weak
evidence (fixture-typed receivers, ambiguous chains) is weighted ×0.8 and
never reported as "live for sure". Relative imports, aliases
(`from x import a as b`), function-local imports and module-level
initializers all produce edges now — each was a real missing-edge bug found
by dogfooding.

**Dead code (§2).** Overrides of external bases (Qt, Django, `unittest`,
`abc`) and `Protocol` members are entry points; framework packs (pytest, Qt,
click) are table-driven and opt-in; `pyproject.toml` scripts and `__all__`
name callable API. Findings carry `evidence` and `nearest_root_distance`, so
a 0.9 is checkable. `rta-dead` stays Speculative-only — it never claims High.

**Library surface.** A package façade's public functions and public-class
methods, a module a package re-exports (`import coderadar.lsp`), and any
definition used as a decorator are roots. Findings that could go either way
(value references, untyped receivers) are reported at the weakest tier with
the reason, instead of claiming 0.9 or being silently dropped.

**Query language (§3).** Keyword-prefixed identifiers parse; unknown fields
and values fail with a human-readable message and a "did you mean";
`methods`, `constants`, `entities`, `starts_with`/`ends_with` and row
identity are queryable; `docs/query-language.md` is generated from the
schema and drift-checked.

**Mutation follow-through (§4).** Renaming a method rewrites attribute call
sites and cascades base/override renames; unresolved sites are reported as
`unverified_sites` instead of being silently skipped.

**API ergonomics (§5).** Dotted qualified names (`pkg.mod.Class.method`),
canonical forward-slash ids on every platform, line/column companions next
to byte spans, a stale-handle guard that refuses to answer about a project
the handle was not created for, and a store migration path for pre-0.10 id
spellings (`coderadar store-repair`, cold start re-analyzes automatically).

**Noise (§6).** Secret detection requires a value that looks like a
credential (entropy or character-class mixing) and redacts the value, not
the keyword; nested functions are not clone pairs; dict-literal tables are
flagged `literal-table` instead of silently dropped; `long-parameter-list`
counts positional-or-keyword parameters only, so Qt-style keyword-only
constructors stop firing.

> **Upgrading from 0.9.x:** stores written by 0.9.x use the old id spelling
> (`./pkg/mod.py::x`). `load` refuses them with the migration hint;
> `coderadar analyze` (or a cold start, which does it automatically)
> re-keys the store. See [§5.1](docs/v0.10-precision-plan.md).

## v0.8 Feature Highlights — ledger-backed cold start + agent UX

v0.8 makes the MCP server restart-proof and the mutation plans honest, driven by two field
reports from live agent sessions (see [`docs/v0.8-p2-agent-ux-guide.md`](docs/v0.8-p2-agent-ux-guide.md);
cold-start design in [`docs/v0.8-p1-cold-start-design.md`](docs/v0.8-p1-cold-start-design.md)):

- **Ledger-backed cold start (P1).** `load_snapshot` replays the Macrame ledger instead of
  re-indexing from scratch (`_ensure_graph` retired) — sub-second store load vs 10–15 s full
  analyze on the benchmark repo. `LEDGER_REVISION` is stamped on both the load and analyze
  paths, so `graph_stats()["revision"]` is a valid Stage 0.3 cache key either way.
- **Incremental startup + reindex (P2-4).** `coderadar.coldstart.build_graph` loads the
  store when present and `update_file`s only stale sources, falling back to full analyze
  when there is no (or an unloadable) store. The MCP background index and `codegraph_reindex`
  both use it — project switches stop paying full re-indexes.
- **Project persistence across sessions (P2-3).** The last project per launch directory is
  recorded in `~/.coderadar/mcp/last_projects.json`; a restarted `mcp serve` resumes it
  without `--path` (explicit `--path` still wins).
- **Multi-token search (P2-1).** `search_entities` scores per whitespace-split token with OR
  semantics (name exact/prefix/contains tiers plus signature and docstring signals). Empty-query
  kind enumeration (the visualizers' contract) is preserved; Python function *and* class
  docstrings backfill onto their owners during extraction.
- **Empty-result wording (P2-6).** Misses state which tokens were tried with OR semantics
  and point at `codegraph_search_similar` / `codegraph_explore` instead of "try broader terms".
- **`create_entity` signatures (P2-2).** Optional complete header
  (`fn sync_status_text(store: &Store) -> String`) rendered verbatim with language-appropriate
  body delimiters.
- **Textual call-site backstop (P2-5).** Rename / signature-update / class-rename plans append
  word-boundary `name(` occurrences the call graph couldn't resolve (module-level calls, macro
  bodies, comments, strings) as unverified sites instead of silently missing them — the Dioxus
  `rsx!` gap from the field reports is reported, not parsed (explicit non-goal).
- **Shell-friendly entity IDs (E7).** Stored IDs keep OS-native separators (FK stability);
  display renders forward slashes with the redundant `./` dropped, and every pasted variant
  resolves back to the stored key.
- **macrame-db 0.18 (schema v21, auto-rungs v15→v21 on open).** CodeRadar stores
  compact namespaced metadata (`kind`, `file_path`, `content_hash`) in
  `concepts.extra`, indexes the file path for precise stale-row cleanup, and
  gives framework edges stable namespaced kinds such as
  `synthetic:django:handles` instead of flattening their types. This is a
  forward-only database upgrade: a 0.17 binary cannot open v21 or fold a
  concept written by 0.18. Existing 0.17 concept content remains canonical and
  is rewritten with `extra` on the next analyze. Existing no-op-write filtering
  and the `MAX(seq_id)` ledger-revision stamp remain in place; see the
  [integration notes](docs/macrame-0.18-integration.md) for the adopted fields
  and the KV deferral.

## v0.8.1–v0.8.14 Highlights — the dogfood batch

CodeRadar indexed and mutated itself (207 files, every CLI command, all 22
MCP tools); the review found four P0 defects in the mutation engine plus
ten more findings, and each was fixed against a live repro on the repo
(see [`docs/dogfood-review-2026-09.md`](docs/dogfood-review-2026-09.md) for
the full record, including the two diagnoses the first pass got wrong):

- **Mutation safety (P0s).** Clone-detection panic fixed at its true root
  (slot-space mapping, not the suspected off-by-one) with `catch_unwind` on
  analysis entries; the allow-list is anchored (`src/` no longer matches
  `.venv/…/src/…`) and gated at dry-run, not just apply; brace-language
  body splices and signature-span rebasing apply cleanly with stale plans
  healing (warning) instead of mangling; friendly errors replace raw Rust
  debug structs.
- **One canonical id form.** Entity ids are project-relative dot-prefix at
  every write path — no more absolute/walk/update triple spellings breaking
  store keys, call edges, and module lookups; analyze retires legacy rows.
- **First-class exclusion.** One shared matcher across walker, star exports,
  resolvers, watcher, and staleness; `exclude list|add|remove`,
  `--exclude` one-shots, retraction on next analyze, effective stack in
  `stats`. The star-export pass also stopped rglobbing `.venv` (17.6 s →
  0.16 s), restoring the benchmark claims.
- **Dead-code cross-language awareness.** `#[pyfunction]` bridge functions
  are production roots; Rust `pub`-export detection is source-backed;
  findings carry file+line.
- **Store repair + single version source.** `coderadar store-repair`,
  `init --force` ledger rebuild, v1-orphan auto-retirement; `--version`
  agrees everywhere; first-call index heartbeats on stderr.
- **Slop scan made loud.** Skip accounting with a stats footer, per-kind
  caps, noise markers dropped — 15/15 across all graph provenances.

## v0.8.15–v0.9.0 Highlights — the round-2 arc

A second dogfood round swept all 22 CLI commands and all 22 MCP tools
(104/118 green at v0.8.16 → **121/121 at v0.9.0**); every red became a
tracked finding with a battery anchor (see
[`docs/road_to_v0.9.0.md`](docs/road_to_v0.9.0.md), [`docs/v0.9.0-release-notes.md`](docs/v0.9.0-release-notes.md)):

- **Index accuracy.** External callees are visible instead of silently
  dropped; re-export chains resolve transitively (`from app import combine`
  → `helpers.py::combine`, cycle-guarded); rename heals the whole chain —
  import bindings rewrite link-by-link, scoped updates refresh imports,
  rename-back restores byte-identical; synthetic edges never persist as
  CALLS and stale edges retract; `new Store()`-style constructor calls
  extract across TS/JS/Java/C#/C++.
- **Strict, scriptable surfaces.** Unknown `--edges`/`--format` values and
  garbage `as_of` timestamps error instead of rendering plausibly; CLI ids
  canonicalize (`Unknown entity` vs empty); `query --format json`; all
  logging goes to stderr (WARNING default, `CODERADAR_DEBUG=1` for DEBUG),
  so stdout stays machine-readable; `diagnose --unresolved` names names;
  env knobs (`CODERADAR_INDEX_WAIT/HEARTBEAT`) validate instead of crashing.
- **Platform.** macrame-db 0.15 → 0.17 (schema v15→v19, libsql stays
  0.9.30); `[project] exclude` honored on library paths with `exclude
  list` layers + effect totals; `git-clean` agrees with `git status`;
  watcher exclusions proven; Rust bridge attributes actually captured
  (`#[pyfunction]` never reached the graph before) with `#[pymodule]`
  added as a production root.

## v0.9.1–v0.9.2 Highlights — CI reds closed + docs refresh

- **Temporal traversal on aliased roots (v0.9.1).** Windows CI failed
  `test_as_of_temporal_traversal` 3/3 runs (`got []`) while local +
  Ubuntu passed: GitHub runners set `TEMP` with an 8.3 short component
  (`C:\Users\RUNNER~1\…`), the walk spells paths as passed while
  `INDEXED_ROOT` is filesystem-canonicalized, the lexical strip missed,
  ids fell back to absolute form, and analyze retired all 3 fixture
  concepts as orphans in the same run. `canonical_file_form` now
  FS-canonicalizes once on a strip miss and retries (mismatch path only),
  with verbatim-alias (Windows) and symlink-alias (unix) regression tests.
- **Lint fully green.** The 951-error ruff backlog is cleaned to zero
  against pinned `ruff@0.16.5` (fixture excludes, mechanical fixes,
  per-site `noqa` where deliberate) — `cargo fmt` + clippy + ruff all pass.
- **This refresh (v0.9.2).** Test counts 1119 → 1128 (369 Rust + 759
  Python), query files 18 → 41 (one per Tier-1/2 language), Shell/SQL
  de-duplicated out of the Tier-3 row (both ship `.scm` files in Tier 2).

## v0.7.18 Feature Highlights — the fossil-mcp port

v0.7.3 through v0.7.18 ported the best of fossil-mcp's detector suite onto CodeRadar's graph
substrate — re-derived against CodeRadar types, not copied
(see [`docs/fossil-mcp-improvement-plan.md`](docs/fossil-mcp-improvement-plan.md)). Every stage
shipped as an independently demoable increment with golden tests written the same day:

- **Integrity hotfixes (Track H, v0.7.3–7).** `replace_body` now validates the file it *wrote*,
  auto-rolls-back on parse failure, and preserves leading docstrings; mutation status is one
  truth-sourced enum; build directories (`target/`, `node_modules/`, `dist/`) are default-excluded;
  smell findings dedupe per entity version.
- **Confidence scoring + strictness profiles (Stage 0, v0.7.8–10).** A single scoring module
  (tiers Certain→Speculative, `combine()`, tier bands) backs every detector; closed 3-level
  strictness profiles multiply baselines inside the smell engine.
- **Dead-code detection (Stage 1, v0.7.11).** Entry-point ladder (mains, framework decorators,
  dunder protocol methods, public API of unimported modules) → forward reachability → classifier.
  Findings carry kind (`unreachable` | `transitively-dead` | `test-only`), confidence tier,
  and removable line counts via `codegraph_dead_code` / `find_dead_code()`.
- **Token-level clone detection (Stages 2+6.1, v0.7.12/16).** Normalized token streams → MinHash
  + banded LSH → Type-1/2/3 funnel with union-find; candidate pairs verified by exact
  Zhang–Shasha tree-edit distance (~150 LOC where fossil's APTED path decomposition is 1,200).
  Rename-blind trees mean TED refines scores rather than rejecting them.
- **Scaffolding & secrets scanner (Stage 3, v0.7.13).** Declarative detector tables for AI-generated
  boilerplate plus a secrets scanner — pure Rust, no graph dependency.
- **Structured CFG metrics (Stages 4+6.2, v0.7.14/17).** Synthesis CFG with typed edges; cyclomatic
  = E−N+2 over the reachable subgraph; unreachable-block collection; a tri-state literal evaluator
  powers a `dead-branch` rule. All behind `analysis.use_cfg_metrics` (default off) with honest
  degradation when signal is absent.
- **Harmonic centrality (Stage 5, v0.7.15).** Revision-keyed cached centrality over upstream BFS;
  god-class/brain-method findings gain a normalized centrality signal; `affected()` ranks each
  depth group by centrality and star-marks the top three.
- **RTA-lite dispatch sharpening (Stage 6.3, v0.7.18).** Overrides whose ONLY liveness is virtual
  dispatch on a class never constructed in the indexed root are re-flagged as `rta-dead` — the
  weakest evidence tier, never demoting anything the base detector calls live. Python-exact scope
  until real constructor resolution lands.

The smell engine grew from 9 to 12 rules; Rust lib tests from ~250 to 303; Python tests past 519.

## v0.7.2 Feature Highlights

- **Runtime project switching** — the new `codegraph_set_project` MCP tool
  (19 total) re-runs startup against a new root from inside a tool call:
  that project's `.coderadar.toml` config and mutation policy take effect,
  indexing restarts in the background, and an explicit switch outranks the
  client's declared workspace for the rest of the connection. Unmarked roots
  require `confirm=true`; switching to the current root is a no-op that says
  so.
- **`project_path` reads like agents mean it** — a file, subdirectory, or
  root inside the served project is accepted via nearest-marker walk-up
  (`resolve_selector`), replacing a byte-exact root comparison that refused
  editor-tab paths and explored directories. Windows drive-letter casing can
  no longer split one directory in two.
- **Refusals name the way out** — "wrong project" replies point at
  `codegraph_set_project` instead of telling the agent to edit mcp.json and
  restart the server.
- **One writable project at a time, unchanged** — mutation confinement
  follows the switched root automatically: `analyze(root)` re-tightens
  `INDEXED_ROOT`, so policy, stale-write hashes and rollback need no
  redesign. E2E tests prove an escape-path mutation into the previous
  project never touches disk.

## v0.7.0 Feature Highlights

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
- **Guidance that named tools which don't exist.** `codegraph_as_of` told
  the agent to use "`codegraph_query` with timestamp" and `search_entities`.
  `codegraph_query` has no timestamp parameter and `search_entities` is not
  a tool — an agent following that advice failed twice with no way to tell
  the advice was wrong. A test now checks every tool name any message
  mentions against the registered set.
- **Two commands named `watch`.** Click registers by function name, so the
  second definition silently replaced the first — and the losing one carried
  the config activation, leaving the survivor running without ever reading
  `.coderadar.toml`. The dead one is gone and the live one activates config
  and indexes before watching.
- **Exit codes that lied.** `coderadar update` printed "Fully applied:
  False" and exited 0, so a script driving updates could not tell a failure
  from a success. `git-clean` defaulted to reporting a clean worktree when
  the check itself failed — the answer a caller is most likely to act on.
- **Tier 1 was Python-shaped.** `extract_parameters` only knew the Python
  grammar's node kinds, so every parameter of every PHP, Kotlin, C, C++, Go,
  Java, Rust, Ruby and TypeScript function was dropped — a PHP method taking
  `$name` was indexed as `hello()`. C and C++ hang the parameter list off the
  declarator chain rather than the function node, so they found nothing even
  by kind. The rendered keyword was hardcoded `def` for all of them, and
  TypeScript's return type came back as `-> : string`. Signatures are what an
  agent reads before calling `update_signature`.
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
- **857 tests, 0 failures** — 250 Rust + 607 Python, including an end-to-end
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
      server.py            #   22 tools + guidance
      roots.py             #   project-root ladder and marker walk-up
      startup.py           #   background index, ensure_ready()
      lazy.py              #   roots/list retry on the first tool call
      lifecycle.py         #   handshake timeout, parent watchdog, teardown
    query/                 # Query planner + templates + cache
    visualizers/           # Mermaid + Graphviz (SCC cycle highlighting)

docs/                      # Specifications + code review + performance roadmap
tests/                     # 759 Python tests (E2E incl. dead-code/clones/scaffold/CFG/
                           #   centrality/dead-branch/RTA goldens, mutation E2E, MCP,
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
