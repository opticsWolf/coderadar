# CodeRadar — core-first port to Rust

Canonical copy of the decisions and state for moving the Python surface
(`py_agent/`, 20k LOC) onto the Rust engine so the whole tool links as Rust
crates. Started 2026-02 (repo at 0.16.0, `44c6cf9`).

## Decisions (user, step 0)

1. **Layout — mono crate.** One crate under `rust/` holds every ported unit:
   shared types, one dependency set. Existing `core_indexer` stays a workspace
   member and is linked as an rlib (`crate-type = ["cdylib", "rlib"]`); no
   function round-trips through Python.
2. **Target — pure Rust app, PyO3 wrappers stay.** The binary is the product.
   Thin PyO3 wrappers are kept only so Python can call the Rust API; they hold
   no logic. Plan: an optional `python` feature on the mono crate exposing the
   `#[pymodule]`. Fallback if `extension-module` link semantics bite the bin
   target: a zero-logic facade crate (`rust/coderadar-py`). This is the single
   permitted exception to the mono-crate rule.
3. **Shape — library first, then CLI, then MCP.** Long term one binary:
   `coderadar <command>` plus `coderadar mcp` stdio. In scope: CLI text and MCP
   JSON.
4. **Core scope —** `ops` → `render` → CLI → MCP → framework resolvers. Left in
   Python for later/never: `visualizers/`, `lsp/pool.py` (never constructed —
   see `.coderadar.toml`), `coldstart`, `mcp/startup` + `lifecycle` glue.
5. **Equivalence bar —** byte-for-byte on `ops` return values and MCP JSON;
   normalized on rendered CLI text (rich markup, key order, float text).
   Floats: exact. Genuine cosmetic drift needs a `known-diffs.jsonl` line and
   the user's approval.
6. **Checkpoints —** approval of the ranked unit list, then one report and
   go-ahead per unit.

## Layout (mono crate)

```
rust/                     crate `coderadar` (lib + bin + optional `python` feature)
  src/lib.rs              re-exports; owns the graph handle core_indexer exposes
  src/error.rs            OpError taxonomy (12 subclasses → one enum + kind)
  src/ids.rs              canonical_entity_id, friendly_entity_id, display_file
  src/ops/read.rs         search, search_symbols, node, affected, explore, traverse, query, resolve
  src/ops/notes.rs        annotations/comments/open_items/export/import
  src/render.rs           text surfaces (normalized-equivalence)
  src/cli.rs              clap surface, exit codes
  src/mcp.rs              JSON-RPC / stdio
  src/python.rs           #[pymodule] behind feature `python`
```

`core_indexer` stays the workspace member it is today and is linked as a
library; the ported functions call its Rust API directly.

## Ranked units (step 1)

Ranked by fan-in, blast radius, contract centrality, test density and I/O
surface — measured on the store built from this tree (10 353 call edges,
235 py_agent functions with caller_count ≥ 3).

1. **Read/query vertical** — `search`, `search_symbols`, `node`, `affected`,
   `explore`, `traverse`, `query`, `resolve`, `module_children`, with the
   foundation they all sit on (`OpError` taxonomy, id canonicalisation,
   `_entity_or_raise`, `_attach_notes`). Fan-in: `search` 14, `traverse` 13,
   `resolve` 13, `node` 8+, `affected` 5; ~600 LOC; read-only; tests
   `tests/test_mcp.py`, `tests/precision/`, `test_call_edge_accounting.py`,
   `test_framework_routes.py`, `test_entity_id_format.py`. **Recommended
   first:** it freezes the Rust result types that `render`, the CLI and MCP
   JSON all key off, and it has no writes to reason about.
2. **Annotations vertical** — `annotate`, `comment`, `revise_annotation`,
   `retire_annotation`, `annotations_on`, `comments_on`, annotation/comment
   history, `open_items`, `export_annotations`, `import_annotations`. Fan-in
   21/18/11; real domain rules (grammar, invariants, scope resolution);
   dense tests (`test_annotations.py`, `test_p6b_open_items.py`).
3. **Render vertical** — `render.py` (1666 LOC), pure formatting over the
   dicts from 1–2; golden-text equivalence; must follow the type freeze.
4. **CLI surface** — `cli.py` (1954 LOC, click), text-equivalence bar, exit
   codes; 51 callers of `_enter_project`.
5. **MCP surface** — `mcp/server.py` (2028 LOC, `create_server` 1142), JSON
   equality; needs 1–2 plus a protocol crate decision.

Deliberately deferred: the temporal/git family (`ingest_history` 113 LOC /
71 refs, `changes_between` 210, `as_of`, `verify_history`, `sync_git`,
`migrate_to_commit_time`) — densest tests, heaviest I/O, and mostly glue
over git ingest that is already Rust; `compute_embeddings` (model runtime);
`lsp/pool.py` (never constructed); `visualizers/` (optional extras).

## State (after units 1�5)

| unit | module | state |
|---|---|---|
| 0 | `rust/PORT.md` | decisions recorded; annotation on `cli.py::main` |
| 1 | `rust/coderadar/src/port.rs` | **gate passed** � 1204/1204 recorded cases, fmt/clippy/tests clean |
| 2 | `rust/coderadar/src/annotations_ops.rs` | implemented directly on `core_indexer::annotations`; CLI + MCP wired; not rust-anything-gated (see note) |
| 3 | `rust/coderadar/src/render.rs` | text renderers for the live CLI/MCP surfaces; normalized bar |
| 4 | `rust/coderadar/src/cli.rs`, `src/main.rs` | Rust binary `coderadar-rs`, read + note commands wired |
| 5 | `rust/coderadar/src/mcp.rs` | stdio JSON-RPC `initialize`/`tools/list`/`tools/call` over the same library |
| � | `core_indexer/src/api.rs` | new pure-Rust fa�ade (load, entity JSON, search, callers/callees, stats, query, traverse, analyze) |

### Unit 1 proof

- `rust-anything record rust/coderadar --count 20 --from-tests -- pytest`: 1204 cases (301 ok, 903 error paths), from the project's own pytest suite.
- `rust-anything diff`: **1204/1204**.
- `rust-anything gate`: **passed** (fmt, clippy -D warnings, cargo test, no unsafe, no `unimplemented!`, error cases covered, API complete).
- `rust-anything bench`: **unavailable** � the 3.3.0 bench driver imports `coderadar.ops` from the generated wrapper, which only contains the ported top-level functions. No verdict was inferred.

### Why units 2�5 are not rust-anything-gated

`rust-anything bridge` on `ops.py` does not expose the note-writing functions
(`annotate`, `comment`, `revise_annotation`, �): they were absent from the
generated export list even though they are public in `ops.py`. They are
implemented as direct Rust calls into `core_indexer::annotations` with their
own error mapping, and exercised through the Rust CLI and MCP server instead.
Unit 1's generated crate was kept as the single mono crate; `rust/PORT.md`
stays the canonical decision record.

### Link spike � resolved

`core_indexer`'s rlib links into both the cdylib and the new binary on this
toolchain. Wrappers pin pyo3 to core_indexer's 0.24 (Cargo forbids two crates
with `links = "python"`); `rust/after_bridge.py` re-applies the generated-file
patches needed after any re-bridge.

## Remaining

- Full-surface parity: the temporal/git family, embeddings, mutations and the
  framework resolvers stay on the Python reference until ported.
- MCP parity: the Rust server implements the core read tools; the remaining
  ~30 tools in `mcp/server.py` are not yet exposed.
- Cutover: `coderadar-rs` runs beside the Python CLI; no cutover script yet.
