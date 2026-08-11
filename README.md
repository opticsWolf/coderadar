# CodeRadar v0.5.3

**Live semantic graph of your codebase — incremental, queryable, LLM-writable.**

CodeRadar maintains an incrementally updatable graph of your code's logical structure, enabling LLMs and developer tools to both **query** and **safely rewrite** code through a unified pipeline. Based on techniques from [CodeGraph](https://github.com/opticsWolf/codegraph) (MIT License).

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
| Synthetic (200 files) | 200 | Python | 1,096ms | 1,137ms | **0.96×** (parity) |
| CodeRadar self | 102 | Python+Rust | 855ms | 1,434ms | **0.59×** (faster) |
| codegraph-main | 546 | TypeScript | 19,328ms | 6,970ms | 2.77× |

CodeRadar wins on small-to-medium Python/Rust projects due to zero runtime boot overhead. On large TypeScript codebases, CodeGraph's hand-written per-language Rust walkers and flat-buffer emission are faster than the generic `.scm`-query engine. See [performance-roadmap.md](docs/performance-roadmap.md) for the optimization backlog.

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
| **Languages indexed** | 18 (12 Tier 1 full, 6 Tier 2) |
| **Tests** | 303 (163 Rust + 140 Python) |
| **Query surface** | Pest structural + Macrame agent traversals + vector search |
| **Frameworks** | Django, Flask, FastAPI, Go net/http, Rust Actix |
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
| **Tier 2** | Swift, Scala, Lua, Elixir, Zig, R | Import → Signature | replace_body, create_entity |
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

| Framework | Detection | Extracted Patterns |
|-----------|-----------|-------------------|
| **Django** | `manage.py` | `path()` routes, DRF `router.register()`, admin registrations, `.as_view()` handlers |
| **Flask** | `@app.route` | Route decorators, Flask-RESTful `add_resource()`, Blueprint registration |
| **FastAPI** | `APIRouter` | `@app.get()`/`@router.post()` routes, `Depends()` injection chains, `include_router()` |
| **Go net/http** | `http.HandleFunc` | `mux.Handle()` routes, `gin.GET()` detection, stdlib handler registration |
| **Rust Actix** | `actix_web` | `App::new().route()`, `web::resource()`, `#[get]`/`#[post]` attribute macros, `HttpServer::new` |

Framework edges are registered in the Rust graph — agents can trace from URL patterns to handler functions via `callers_of()` / `callees_of()`.

## v0.5.3 Feature Highlights

- **Parallel extraction pipeline** — 3-phase design: collect → parallel parse/tag/walk (fragment merge) → sequential projection commit. 0.96× CodeGraph on synthetic benchmarks.
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
    resolvers/             # Django, Flask, FastAPI, Go, Actix framework resolvers + __all__ exports
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
