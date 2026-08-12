# CodeRadar v0.6.2

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
Rust Core (ProjectedGraph, Tree-sitter 18-lang, Parallel Extraction,
           Resolution Cascade L1-L3, Query Engine, Mutation Engine)
        │
    Macrame DB (bitemporal persistence with valid_from/valid_to timestamps)
```

| Metric | Value |
|--------|-------|
| **Languages indexed** | 41 (12 Tier 1, 29 Tier 2, 330+ Tier 3) |
| **Tests** | 509 (168 Rust + 341 Python) |
| **MCP Tools** | 17 (explore, search, node, affected, resolve, query, search_similar, module_children, as_of, traverse, replace_body, update_signature, rename, create_entity, compute_embeddings, reindex, update_file) |
| **Query surface** | Pest structural + Macrame agent traversals + vector search |
| **Frameworks** | Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, Rails, NestJS, Vue Router, React Router |
| **Agents** | MCP server with explore, node, search, affected tools |

## Quick Start

```bash
pip install coderadar-rs

# Initial analysis (includes framework detection)
coderadar init src/

# Query
coderadar query "functions where is_async == true"

# Explore call flows
coderadar explore UserService.create --direction downstream

# Watch for changes
coderadar watch src/ --debounce 50

# Visualize
coderadar visualize module_graph --format graphviz src/
```

## Python API

```python
import coderadar

# Initial analysis
graph = coderadar.analyze("src/")

# Query
for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
    print(cls.name, [m.name for m in cls.methods])

# Framework-aware exploration (Django/Flask/FastAPI/Go/Actix routes included)
flow = graph.explore(["UserService.create"], direction="downstream")

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
children = graph.module_children("src/auth.py::auth")
for cls in children["classes"]:
    print(cls["name"], cls["grammar_kind"])

# Temporal queries (Macrame bitemporal)
past = graph.as_of("2026-08-01T00:00:00Z")
```

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

> **Stack Graphs (L1 in v3.3 spec) was deferred to post-v1.** CodeGraph ships 30+ languages at production scale with zero Stack Graphs dependency — compiler-grade scope disambiguation is not required for MCP agent use cases.

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
    resolve/               # Resolution cascade (import_graph, orchestrator, signature, cache, stack_graph stub)
    query/                 # Pest grammar + execution engine
    mutation/              # AST-aware refactoring (rope, indent, WriteGuard)
    fs/                    # File watcher (notify) + git integration
    graph.rs               # In-memory ProjectedGraph + parallel extraction + reverse indexes
    storage.rs             # Macrame concept/edge persistence
    lib.rs                 # PyO3 FFI bindings

py_agent/src/coderadar/    # Python layer
    resolvers/             # 13 framework resolvers: Django, Flask, FastAPI, Go, Actix, Express, Spring Boot, Laravel, ASP.NET, Rails, NestJS, Vue Router, React Router
    embedding/             # Content-addressed dedup
    agent/                 # GraphRAG query pipeline
    lsp/                   # Persistent LSP warm pool
    mutation/              # Tool router for LLM
    mcp/                   # MCP server (explore, node, search, affected)
    query/                 # Query planner + templates + cache
    visualizers/           # Mermaid + Graphviz (SCC cycle highlighting)

docs/                      # Specifications + code review + performance roadmap
tests/                     # 140 Python tests (E2E, MCP, framework resolvers, benchmarks)
```

## Configuration

```toml
# .coderadar.toml
[project]
languages = ["python"]
roots = ["src/", "tests/"]

[resolution]
min_confidence = 0.3

[macrame]
db_path = ".coderadar/store/coderadar.db"

[mutation]
enabled = true
default_dry_run = true

[performance]
worker_threads = 4
debounce_ms = 50
```

## License

MIT — incorporates techniques from [CodeGraph](https://github.com/opticsWolf/codegraph) (MIT License).
