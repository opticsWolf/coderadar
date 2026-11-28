# Implementation plan — closing the remaining CodeRadar port gaps

Status: **A, B, D, E and F complete (Units 39–40, 42–44); C partial (Unit 41). Post-plan Units 45–49 closed LSP hardening, lifecycle, resolver classes, live call-edge confidence and route refresh. A fresh audit supersedes the numeric baseline below.**
Prepared: after commit `cab7ff1` (`dev_rustify`)
Canonical decisions/state: [`PORT.md`](PORT.md)  
Completion plan: [`FULL_PORT_PLAN.md`](FULL_PORT_PLAN.md)  
Graph evidence: refreshed by `coderadar reindex --full` in Unit 49; see the fresh audit at the end of `PORT.md`.

## 0. Post-plan residuals (Unit 50 audit)

The Unit 49/50 audit changed the residual list. Names are covered; the
**contract** is not:

| Gap | Measured | Effort |
|---|---|---|
| CLI `--format` | 42/55 Python commands vs 12/55 Rust | 1.5-2 days |
| CLI flags on shared commands | 25 commands missing at least one Python flag | included above |
| Captured `--help` | 18 of 21 captured commands execute `--help` as an argument; `reindex --help` hangs | 0.5 day |
| MCP parameters | 7 tools ignore documented args (`node.include_neighbors`, `explore.symbols/direction/max_files`, `traverse.edge_kinds`, `reindex.with_embeddings/full`, `verify_history.all_generations`, `set_project.confirm`); mutations use `apply` not `dry_run` | 0.5 day |

Framework extraction parity is verified: 23/23 fixtures produce identical
`routes`/`handlers`/`frameworks` in Rust and Python. The port tests for it
now load the Rust bridge explicitly (`tests/_rust_bridge.py`) instead of
resolving to `py_agent`.

Still open from Unit 49: exact embedding float parity (1-3 days) and shadow
cutover approval.

## 1. Scope and ground rules

This plan covers the gap between the current Rust port and the Python package as measured by CodeRadar. It does **not** change the decisions already recorded in `PORT.md`:

- One `rust/coderadar` mono crate plus the `core_indexer` rlib; not multi-crate.
- Pure-Rust binary is the target; PyO3 wrappers remain a thin facade for Python callers.
- CLI text parity is normalized; `ops`/MCP JSON is byte-shaped; floats exact.
- No function round-trips through Python: engine logic lives in `core_indexer`.
- Gate is the merge bar: `fmt`, `clippy -D warnings`, `cargo test`, `diff` N/N, size ≤ 3×, no unjustified unsafe, no recorded case mismatches.
- Shadow `enable` stays approval-gated; use `rust/cutover.py --install DIR`.

Rust-guideline rules that shape this plan:

- **M-FFI-TRANSLATES** — business logic belongs in `core_indexer`; PyO3/CLI layers only translate. This is why Unit 37's coldstart engine logic was moved out of `port.rs`, and why every unit below starts in the engine.
- **M-PANIC-IS-STOP** / **M-PANIC-CONTINUATION** — fallible config, model and lifecycle paths return `Result`; never `unwrap`/`expect`/`panic!` where Python returns an error or a best-effort result.
- **M-DOC-FIRST-SENTENCE** / **M-DOC-INLINE** — every new public Rust API gets a first-line summary plus `# Errors`.
- **M-INITIAL-CAPACITY** / **M-HOTPATH** where relevant to watcher buffers and embedding batches.

## 2. CodeRadar evidence

Store at plan preparation: **334 modules, 696 classes, 5,309 functions, 11,813 call edges, 0 unresolved structural edges**. The Unit 49 refresh is **345 modules, 735 classes, 5,548 functions, 12,243 call edges, 0 unresolved structural edges**; `diagnose --unresolved` lists only builtins/external calls, so there is no missing in-repo edge left to chase.

Python surface: **768 callables** (292 are methods).

Current parity:

| Surface | Current state |
|---|---|
| CLI | 55/55 shared commands, 0 local-only |
| MCP | Python 35/35 tools present; Rust has 49; resources 3/3 |
| Public `CodeGraph` | 55/55 methods delegate to Rust (100%) |
| Gate | 4447/4447 recorded cases |
| Coldstart | Ported into `core_indexer::api::coldstart_build_graph` |

High-fan-in evidence for the remaining units:

| Candidate | Direct callers | Blast radius (depth 2) | Why it matters |
|---|---:|---:|---|
| `config.py::activate_config` | 4 | 4 + 23 | Startup config activation; gates roots, mutation policy, blob policy |
| `config.py::load_config` | 11 | — | Feeder for activation, watcher, excludes, CLI edits |
| `mcp/lazy.py::configure` | 13 | 13 + 21 | Lazy root ladder and background startup |
| `mcp/startup.py::BackgroundIndex.wait` | 14 | — | MCP readiness/heartbeat behavior |
| `mcp/lifecycle.py::HandshakeTimeout.arm` | 7 | — | Handshake timeout semantics |
| `mcp/roots.py::resolve_project_root` | 15 | — | Root ladder; Rust MCP currently fixed-root |
| `mcp/roots.py::resolve_selector` | 11 | — | Tool `project_path` interpretation |
| `project_state.py::record_project` | 10 | — | Persisted project across sessions |
| `embedding/__init__.py::embedding_settings` | 8 | 8 + 25 | Model/dimension/batch source |
| `ops.py::compute_embeddings` | 22 | — | High-fan-in embedding op |
| `__init__.py::CodeGraph.watch` | 3 | 3 + 1 | Facade process helper |
| `CodeGraph.watch_git`, `CodeGraph.batch` | 1 each | — | Low-value facade helpers |
| `resolvers/resolution.py::resolve_reference` | 4 | 4 + 13 | Callable-taking API; Rust has data-shaped equivalent |
| `lsp/pool.py` | max 4 | — | Optional, no CLI/MCP front door |
| `agent/graphrag.py` | max 1 | — | Optional, no CLI/MCP front door |

Completion estimate from this evidence:

- **User-visible parity: ~96%.**
- **Callable-weighted internal coverage: ~81%.**
- **6–8 working days** to close visible parity; **14–20 working days** to near-source parity including LSP/GraphRAG and model integration.

## 3. Ranked implementation units

### Unit A — Full config activation in the Rust binary — DONE

**Rank: 1. Value: highest. Estimate: 1.5–2 days. Implemented as Unit 39.**

Problem: Python calls `activate_config()` and `set_config()` at CLI/MCP startup. The engine has the mapping in `core_indexer/src/ffi_config.rs`, but it is PyO3-only, so `coderadar-rs` runs with `GraphConfig::default()`. Unit 37 parses only `[project] exclude` and `[database] path` for the coldstart walk.

Currently ignored by the Rust binary:

- `project.roots` — Rust walks the whole root.
- `mutation.allow/deny`, limits, `require_clean_git`, `post_verify` — mutation policy uses defaults.
- `database.store_source_blobs`, `database.blob_exclude` — blobs always stored with built-in excludes.
- `resolution.*`, `analysis.use_cfg_metrics`, `query.*` — defaults only.
- `embedding.model/dimension/batch_size` — `port::embedding_config()` reads `coderadar.toml`, not `.coderadar.toml`.
- `watch.debounce_ms`, `watch.max_file_size_bytes` — CLI defaults used.
- `retention.archive_after_days` — explicit cutoff only.

Implementation steps:

1. Add `core_indexer/src/config_activation.rs`:
   - `ActivationReport { applied: Vec<String>, ignored: Vec<String>, config: GraphConfig }` or a JSON equivalent.
   - `apply_config(value: &serde_json::Value) -> Result<ActivationReport, String>` performing the exact type validation currently in `ffi_config::set_config`.
   - Mirror the existing “merge, never replace” rule for `database.blob_exclude`.
   - Preserve the existing ignored-key reporting contract.
2. Refactor `ffi_config::set_config` to translate `PyDict → serde_json::Value` and call `apply_config`; keep Python-facing error text stable (M-FFI-TRANSLATES).
3. Add `core_indexer::api::activate_config(root: &str) -> Result<Value, String>`:
   - read `root/.coderadar.toml`;
   - parse with `toml = "0.8"` (already a `core_indexer` dependency);
   - convert to `serde_json::Value`;
   - call `apply_config`;
   - write `ACTIVE_CONFIG`;
   - return `{applied, ignored, config_present}`.
4. Wire activation at every Rust entry point before graph work:
   - `cli::run` after root resolution;
   - `mcp::serve` startup and `coderadar_set_project`;
   - `coldstart_build_graph` before analyze/stale walk;
   - facade `analyze`/`reindex` native wrappers.
5. Replace `port::embedding_config()`'s `coderadar.toml` read with the activated config; wire `watch` flags and `retention` default.
6. Add Rust engine tests for applied/ignored keys, `project.roots`, mutation deny, blob kill switch, and broken TOML best-effort behavior.
7. Add a focused Python test asserting a configured project behaves identically through CLI/MCP/façade; record it with `rust-anything record rust/coderadar --from-tests -- python -m pytest ...`.

Acceptance criteria:

- A fresh `coderadar-rs` process honors a project TOML without any Python call.
- `exclude list`, full analyze, mutation planning, blob stats and watch defaults match Python for configured projects.
- Broken TOML never fails startup; it reports the error and keeps defaults, as Python does.
- `ffi_config::set_config` and `api::activate_config` share one mapping; no divergence.
- Gate stays N/N or higher; cutover 4/4.

Risks: type-validation drift between PyDict and TOML; avoiding a new clap stack overflow by not adding top-level variants.

---

### Unit B — Façade process helpers: `batch`, `watch`, `watch_git` — DONE

**Rank: 2. Value: low–medium. Estimate: 0.5–1 day. Implemented as Unit 40.**

Problem: `rust/coderadar/python/coderadar/codegraph.py` covers 52/55 public `CodeGraph` methods. Missing:

- `CodeGraph.batch()` — context manager; 1 test caller.
- `CodeGraph.watch()` — returns the watcher object; 3 callers, one is CLI `watch`.
- `CodeGraph.watch_git()` — 1 caller.

CLI `watch` already uses Rust polling, so this unit is about Python API identity, not CLI parity.

Implementation steps:

1. Add engine operations:
   - `api::batch_update(paths: Vec<String>, contents: Vec<Option<String>>)` → folded `UpdateReport`; reuse the existing two-pass convergence behavior from the facade's `_synchronize_mutation_files`.
   - `api::watch_paths(paths, debounce_ms, max_bytes)` and `api::watch_git(repo, poll_ms, store_path)` returning opaque watcher handles with `poll_once`, `reports` and `last_error`.
2. Keep Python wrappers thin:
   - `BatchContext.__exit__` calls the native batch op.
   - `CodeGraph.watch`/`watch_git` return the existing Python `Watcher`/`GitWatcher` objects, but their work loops delegate to the native handles.
3. Preserve private helper dataclasses; do not shadow gate decoder classes.
4. Record and gate with `tests/test_python_layer.py` watcher/batch tests.

Acceptance criteria:

- The three façade tests that currently exercise these methods pass unchanged.
- No polling loop dies because of one bad file; errors land in `last_error`.
- CLI watch behavior is unchanged.

Risks: Python object identity for watcher classes; do not fake a different API.

---

### Unit C — Exact fastembed-backed natural-language vectors — PARTIAL

**Rank: 3. Value: medium. Estimate: 1.5–2 days plus model download. Implemented as Unit 41; exact-float residual remains.**

Problem: Python uses `BAAI/bge-small-en-v1.5` (dimension 384) via fastembed. Rust uses a deterministic local hashing embedder. Exact parity is proven only for vector-input searches, not for natural-language embedding values.

Implementation steps:

1. Confirm a pure-Rust model runtime (for example the `fastembed` crate) can load `BAAI/bge-small-en-v1.5` offline in CI; pin model name and revision.
2. Add `core_indexer::api::embed_texts(model, texts) -> Result<Vec<Vec<f64>>, String>`; keep the deterministic embedder as an explicit fallback with a visible notice.
3. Replace `port::embed_text` with the model-backed call for `compute_embeddings`, `search_similar` and `search_hybrid` text queries.
4. Persist model name + dimension in the store metadata; enforce the existing one-model-per-store rule.
5. Record exact vectors from Python tests; floats must match exactly or within the crate-configured tolerance.
6. Bench the embedding path; if model inference dominates, keep the Rust boundary but consider batching rather than per-item calls (M-HOTPATH).

Acceptance criteria:

- A Python-built store and a Rust-built store produce the same vectors for the same text and model.
- Search ranking over natural-language queries matches Python, byte-for-byte where required.
- Offline mode fails with an explicit error, never silent zeros.

Risks: dependency availability/licensing, model download in CI, changed vector bytes invalidating existing recorded cases.

---

### Unit D — MCP lifecycle parity — DONE

**Rank: 4. Value: medium for server embedding, low for CLI. Estimate: 2–3 days. Implemented as Unit 42; `roots/list` request and parent watchdog added in Unit 45.**

Problem: Rust MCP has all tools/resources, but the process model is synchronous and single-root. Python has a lazy root ladder, roots/list handshake, background indexing, build lock, heartbeat and persisted project.

CodeRadar evidence: `lazy.configure` 13 callers / 21 at depth 2, `BackgroundIndex.wait` 14, `resolve_project_root` 15, `resolve_selector` 11, `record_project` 10, `HandshakeTimeout.arm` 7.

Implementation steps:

1. Add `rust/coderadar/src/mcp/roots.rs`:
   - `ResolvedRoot { path, source, marker, confirmed }`.
   - Two-pass ladder: first marker-confirmed lower rung, then unconfirmed higher rung.
   - Reuse the existing marker walk and project guard; do not reintroduce the verbatim Windows prefix bug.
2. Add `mcp/startup.rs`:
   - `BackgroundIndex` with `std::thread`, shared state and a build lock; `start` idempotent, `wait` reports progress on stderr.
   - Use `Result` everywhere; no panic in a request handler (M-PANIC-CONTINUATION).
3. Add `mcp/lifecycle.rs`: handshake timeout with a watchdog thread and explicit stderr heartbeat.
4. Persist the adopted project using the same state file as Python `project_state.py`.
5. Wire `serve()` to the ladder before loading a graph; keep `RUST_ANYTHING_MODE=old`-style escape behavior unnecessary because this is the Rust binary.
6. Add JSON-RPC integration tests mirroring `tests/mcp/test_root_resolution.py`, `test_lazy_roots.py` and `test_background_init.py`.

Acceptance criteria:

- Same root ladder outcomes as Python for flag/cwd/client-root combinations.
- Background index is idempotent and one thread per generation.
- Heartbeats stop once indexing completes.
- Existing 45 tools and 3 resources remain unchanged.

Risks: stdio handshake deadlock; keep server→client `roots/list` strictly timeout-bounded. Resolved in Unit 45: one negative-id `roots/list` request per session, inbound replies parsed off the dispatcher thread, bounded by `CODERADAR_ROOTS_TIMEOUT_MS` (default 1000ms). A best-effort parent watchdog polls the parent pid once a second and exits when it disappears; `CODERADAR_PARENT_WATCHDOG=0` disables it.

---

### Unit E — LSP pool and agent/GraphRAG — DONE

**Rank: 5. Value: low without a new front door. Estimate: 4–6 days combined. Implemented as Unit 43.**

Problem: 15 LSP callables and 9 GraphRAG callables remain Python-only. Both have low fan-in and no CLI/MCP command.

Implementation steps (only if a front door is requested):

1. Define CLI/MCP surfaces first:
   - `coderadar lsp status|definition FILE LINE COL`
   - `coderadar context QUERY [--budget TOKENS]`
2. For LSP, port `ManagedServer`/`LSPPool` semantics with process supervision, TTL and override threshold; respect `[resolution.lsp]`.
3. For GraphRAG, port query planning, context building, skeletonization and token budgeting; reuse Rust query/search ops rather than reimplementing graph access.
4. Record from Python tests before writing Rust; add focused tests if no Python test reaches a behavior.

Acceptance criteria:

- New front doors appear in `cutover.py` verification.
- Existing gate count does not regress.
- Optional features are disabled by default exactly as Python config specifies.

Risks: external language servers may be absent in CI; treat as best-effort with explicit status. Local verification installs Pyright 1.1.414 and rust-analyzer 1.100.0, enables them in `.coderadar.toml`, and proves CLI and MCP definitions against real source positions. The four live-use caveats are now closed: `timeout_ms` is enforced with a bounded retry budget, `override_threshold` drives low-confidence graph-edge overrides through the orchestrator plus CLI/MCP edge-list entry points, configured commands are shell-expanded so npm shims and quoted paths work, and live `ResolvedCall` edges carry per-edge confidence so `resolve_calls_scoped` can re-check already-resolved edges too.

---

### Unit F — Callable-taking resolver APIs — DONE

**Rank: 6. Value: low. Estimate: 0.5–1 day. Implemented as Unit 44.**

Problem: `resolve_reference` and `resolve_route` accept Python callables. rust-anything cannot bridge that shape. Rust already exposes data-shaped equivalents (`framework_resolve_reference`, `framework_resolve_route`).

Plan: do **not** fake these APIs. If a Python caller must keep the callable form, add a PyO3 wrapper that accepts encoded resolver descriptors (framework name, pattern, path) and delegates to the data-shaped Rust functions. Otherwise leave Python-side and record the decision.

Acceptance criteria: no new unit opened without a CodeRadar caller edge that cannot be served by the data-shaped API. Source-level closure was added in Unit 46: the facade now also ships `coderadar.resolvers` with a `resolution.py` shim and all 13 resolver classes delegating to `core_indexer::frameworks`.

## 4. Per-unit execution protocol

For every unit above:

1. **Graph step**
   ```bash
   coderadar status --format json
   coderadar callers <entity-id>
   coderadar affected <entity-id> --max-depth 2 --format json
   coderadar diagnose --unresolved
   ```
   Continue only if a caller edge is not already served by CLI/MCP/façade.

2. **Record step** (mandatory)
   ```bash
   rust-anything record rust/coderadar --from-tests -- python -m pytest <focused-tests>
   ```
   Keep the existing cases; never hand-edit `cases.jsonl` to force a diff. Volatile temp paths must be re-recorded with stable arguments, consistent with the framework-case policy in `PORT.md`.

3. **Implement**
   - Engine logic in `core_indexer`; `rust/coderadar/src/port.rs` stays op-shaped.
   - No unsafe; no `unwrap`/`expect`/`panic!` where the original returns an error.
   - Use `#[expect(lint, reason = "...")]`, not `#[allow]`.
   - Add Rust unit tests for reasoned cases.

4. **Prove**
   ```bash
   rust-anything build rust/coderadar
   rust-anything diff rust/coderadar
   python rust/bench.py --min-ms 20 --json
   rust-anything gate rust/coderadar
   python rust/cutover.py --verify
   ```
   Stop on a failing gate, `trend.stalled`, or a slower bench verdict.

5. **Report**
   - Changed functions/files, gate and diff counts, bench numbers, behavior differences, remaining ranked units.
   - Update `PORT.md` with the unit proof and keep this plan's estimates current.

## 5. Suggested sequence and calendar

| Order | Unit | Estimate | Cumulative |
|---:|---|---:|---:|
| 1 | A — config activation | 1.5–2 d | 2 d |
| 2 | C — exact embeddings | 1.5–2 d | 4 d |
| 3 | B — façade helpers | 0.5–1 d | 5 d |
| 4 | D — MCP lifecycle | 2–3 d | 8 d |
| 5 | E — LSP/GraphRAG (optional) | 4–6 d | 14 d |
| 6 | F — callable resolvers (only if demanded) | 0.5–1 d | 15 d |

Add review, re-recording and model/CI integration overhead: **6–8 working days for visible parity (A–D)** and **14–20 days for near-source parity (A–F)**.

## 6. Definition of done for the whole plan

- `rust-anything gate rust/coderadar` passes with all recorded cases.
- `python rust/cutover.py --verify` reports 4/4 and no local-only commands.
- `python rust/bench.py` reports no regression verdict on touched functions.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass.
- Python façade tests pass with the generated native extension.
- `PORT.md` records every unit proof; this plan is updated or closed.
- Shadow `enable` remains unapplied until explicit approval.
