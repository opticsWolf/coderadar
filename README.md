# CodeRadar 0.1.0 (spec v3.3)

**Live semantic graph of your codebase — incremental, queryable, LLM-writable.**

CodeRadar maintains an incrementally updatable graph of your code's logical
structure, enabling LLMs and developer tools to both **query** and **safely
rewrite** code through a unified pipeline.

## Architecture

```
Python Layer (CLI, Visualizers, GraphRAG, Embedding, Mutation Router)
        │
    PyO3 FFI
        │
Rust Core (CodeGraph, Tree-sitter Parsing, Incremental Updates,
           Semantic Resolution, Query Engine, Mutation Engine)
        │
    LadybugDB (ACID persistence, HNSW vector search, Cypher queries)
```

## Quick Start

```bash
pip install coderadar

# Initial analysis
coderadar init src/

# Query
coderadar query "functions where is_async == true"

# Watch for changes
coderadar watch src/
```

## Python API

```python
import coderadar

# Initial analysis
graph = coderadar.analyze("src/")

# Query
for cls in graph.query("classes where inherits_from contains 'BaseModel'"):
    print(cls.name, [m.name for m in cls.methods])

# Update after LLM writes a file
report = graph.update_file("src/core/engine.py")

# Mutation (LLM-driven)
plan = graph.plan_body_replacement(
    entity_id="src/auth.py::validate_user",
    new_body="    return bool(re.match(r'^[^@]+@[^@]+$', email))",
    dry_run=True
)
print(plan.diff_preview)
```

## Resolution Cascade

| Layer | Method            | Confidence  |
|-------|------------------|-------------|
| L1    | Stack Graphs     | 0.90 – 1.00 |
| L2    | Import + Scope   | 0.80 – 0.89 |
| L3    | Signature Match  | 0.40 – 0.79 |
| L4    | Embedding (Py)   | 0.20 – 0.39 |
| L5    | LSP Override     | 1.00        |

## Project Structure

```
core_indexer/          # Rust core
  src/
    extract/           # Tree-sitter tagging + walker
    update/            # Incremental diff + patch + WAL
    resolve/           # 5-layer semantic resolution
    query/             # Pest grammar + execution
    mutation/          # AST-aware refactoring engine
    fs/                # File watcher + git integration

py_agent/src/coderadar/  # Python layer
    embedding/         # Content-addressed dedup
    agent/             # GraphRAG query pipeline
    lsp/               # Persistent LSP warm pool
    mutation/          # Tool router for LLM
    query/             # Cypher templates + cache
    visualizers/       # Mermaid/Graphviz output
```

## License

MIT
