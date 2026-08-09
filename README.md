# CodeRadar v0.4.0

**Live semantic graph of your codebase — incremental, queryable, LLM-writable.**

CodeRadar maintains an incrementally updatable graph of your code's logical structure, enabling LLMs and developer tools to both **query** and **safely rewrite** code through a unified pipeline. Based on techniques from [CodeGraph](https://github.com/colbymchenry/codegraph) (MIT License).

## Architecture

```
Python Layer (CLI, Visualizers, Framework Resolvers, GraphRAG, MCP Server)
        │
    PyO3 FFI  +  register_synthetic_edge() bridge
        │
Rust Core (CodeGraph, Tree-sitter 18-lang, Incremental Updates,
           Semantic Resolution L1-L3, Query Engine, Mutation Engine)
        │
    Macrame DB (bitemporal persistence, DiskANN vectors, FTS5 search)
```

| Metric | Value |
|--------|-------|
| **Languages indexed** | 18 (12 Tier 1 full, 6 Tier 2) |
| **Tests** | 184 (155 Rust + 29 Python) |
| **Query surface** | Pest structural + Macrame agent traversals + vector search |
| **Frameworks** | Django, Flask, FastAPI (route→handler edges) |
| **Agents** | MCP server with explore, node, search, affected tools |

## Quick Start

```bash
pip install coderadar

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

# Framework-aware exploration (Django/Flask/FastAPI routes included)
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

# Module children resolution (v0.4.1)
children = graph.module_children("src/auth.py::auth")
for cls in children["classes"]:
    print(cls["name"], cls["grammar_kind"])

# Temporal queries (Macrame)
past = graph.as_of("2026-08-01T00:00:00Z")
```

## Language Support

| Tier | Languages | Resolution | Mutation |
|------|-----------|------------|----------|
| **Tier 1** | Python, TypeScript, JavaScript, Rust, Go, Java, C, C++, Ruby, PHP, C#, Kotlin | Stack Graphs → Import → Signature | Full tool suite |
| **Tier 2** | Swift, Scala, Lua, Elixir, Zig, R | Import → Signature | replace_body, create_entity |
| **Tier 3** | Erlang, Haskell, OCaml, Nim, Dart, Julia, Perl + 341 more | Signature Match | replace_body, create_entity |

## Resolution Cascade

| Layer | Method | Confidence | Languages |
|-------|--------|------------|-----------|
| L1 | Stack Graphs | 0.90–1.00 | 12 Tier 1 |
| L2 | Import + Scope | 0.80–0.89 | All |
| L3 | Signature Match | 0.40–0.79 | All |
| L4 | Embedding (Python) | 0.20–0.39 | Python |
| L5 | LSP Override | 1.00 | Optional |

## Framework Resolvers

CodeRadar detects and extracts framework-specific patterns that tree-sitter can't see:

| Framework | Detection | Extracted Patterns |
|-----------|-----------|-------------------|
| **Django** | `manage.py` | `path()` routes, DRF `router.register()`, admin registrations, `.as_view()` handlers |
| **Flask** | `@app.route` | Route decorators, Flask-RESTful `add_resource()`, Blueprint registration |
| **FastAPI** | `APIRouter` | `@app.get()`/`@router.post()` routes, `Depends()` injection chains, `include_router()` |

Framework edges are registered in the Rust graph — agents can trace from URL patterns to handler functions via `callers_of()` / `callees_of()`.

## v3.6 Feature Highlights

- **`grammar_kind` field** — raw tree-sitter node kind on every Class entity (e.g. `class_declaration/struct` for Swift)
- **Function-as-value capture** — detects `self.on_click = handler`, callback assignments, return values, kwargs
- **Cross-file fn-ref** — resolves imported names across module boundaries
- **Noise filtering** — builtin type filter (70+ types), literal receiver filter, name stoplist (12 names)
- **Docstring extraction** — preceding comment runs for all languages, not just `@docstring` captures
- **Elixir `def`/`defp`** — precise extraction via predicate queries (v0.4.1)
- **`__all__` detection** — `=`, `+=`, `.extend()`, `.append()` patterns (v0.4.1)
- **`module.children()`** — resolves child entity IDs to full dicts (v0.4.1)
- **Parameter annotations** — type annotations extracted and filtered for builtins (v0.4.1)

## Project Structure

```
core_indexer/              # Rust core
  queries/                 # 16 .scm query files for 18 languages
  src/
    extract/               # Tree-sitter two-pass (tagger + walker) + docstring + decorators
    update/                # Incremental diff + patch
    resolve/               # 5-layer cascade (stack_graph, import, signature, cache, orchestrator)
    query/                 # Pest grammar + execution engine
    mutation/              # AST-aware refactoring (rope, indent, WriteGuard)
    fs/                    # File watcher (notify) + git integration
    graph.rs               # In-memory ProjectedGraph + reverse indexes
    storage.rs             # Macrame concept/edge persistence
    lib.rs                 # PyO3 FFI bindings

py_agent/src/coderadar/    # Python layer
    resolvers/             # Django, Flask, FastAPI framework resolvers + __all__ exports
    embedding/             # Content-addressed dedup
    agent/                 # GraphRAG query pipeline
    lsp/                   # Persistent LSP warm pool
    mutation/              # Tool router for LLM
    mcp/                   # MCP server (explore, node, search, affected)
    query/                 # Query planner + templates + cache
    visualizers/           # Mermaid + Graphviz (SCC cycle highlighting)

docs/                      # Specifications + code review
tests/                     # 29 Python tests (framework resolvers, __all__)
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

MIT
