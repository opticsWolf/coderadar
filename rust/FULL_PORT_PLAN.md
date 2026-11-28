# Full-port plan — closing every remaining gap

Status: **planned, not started**. Canonical decisions: [`PORT.md`](PORT.md).
Forward plan for the original gap set: [`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).
This document supersedes both as the *completion* plan: it lists every
measured gap between the Rust product and the Python reference, and the
units that close them.

Baseline evidence: CodeRadar store refreshed by `coderadar reindex --full`
(345 modules, 5,548 functions, 12,243 call edges, 0 unresolved structural
edges), direct CLI/MCP probes, AST diff of the Python module surface, and a
23-fixture differential run of the framework extractors.

---

## 1. What "fully done" means

The port is done when **all six** gates pass. Today gates D1–D3 fail; D4–D6
are partially done.

| Gate | Definition | Status |
|---|---|---|
| **D1 Protocol** | `coderadar mcp serve` is MCP-spec valid and schema-compatible with the Python server for all 35 shared tools. | ❌ `tools/list` returns bare strings |
| **D2 CLI contract** | Every Python command accepts the same options, defaults, exit codes and output formats. | ❌ 33 commands lack `--format`; 18 capture `--help` |
| **D3 Python surface** | `import coderadar.<module>` resolves the same public names, all delegating to Rust. | ❌ 169 public names missing across 27 modules |
| **D4 Numeric fidelity** | Embedding vectors match Python exactly, or a recorded tolerance is enforced by a test. | ⚠️ ~2e-4 drift, untested |
| **D5 Cutover** | Rust binary is installed as `coderadar`; Python survives only as a shim; packaging reproducible. | ⚠️ `cutover.py` exists, never applied |
| **D6 Closeout** | A committed audit script reports zero gaps; full verification matrix green. | ❌ audit is ad-hoc |

**Non-goals** (documented, so they don't reopen): private Python helpers with
no caller edge (`_render_row`, `_print_effective_excludes`, …) are not ported
by name; they are covered by the front doors. `py_agent` stays in-tree as the
reference implementation until D3 lands, then moves to `reference/`.

---

## 2. Measured baseline

| Surface | Python | Rust | Gap |
|---|---|---|---|
| CLI command names | 55 | 55 | 0 |
| CLI `--format` | 42 | 12 | **33** |
| CLI other flags | — | — | **12 commands** |
| CLI `--help` handled | 55 | 37 | **18 captured commands execute `--help`** |
| MCP tool names | 35 | 49 | 0 (+14 Rust extras) |
| MCP `tools/list` schema | full objects | **strings** | **blocking** |
| MCP tool params honoured | — | — | **7 tools ignore args** |
| Python module public names | 768 callables | 599 resolvable | **169 across 27 modules** |
| Framework extraction parity | — | — | 23/23 fixtures identical |
| Gate cases | — | 4,447 | exact |
| Embedding max drift | — | ~2e-4 | ORT 1.31 vs 1.32 |

---

## 3. Phase P0 — MCP protocol compliance (blocking, do first)

A Rust MCP server that returns `"tools": ["coderadar_status", …]` fails every
client SDK schema check. This phase is the difference between "has tools" and
"is an MCP server".

### Unit M1 — Tool spec table with real JSON Schema

**Goal:** `tools/list` returns MCP `Tool` objects for all 49 tools.

**Design:** one declarative table is the single source of truth for names,
descriptions, argument schema **and** dispatch defaults. This removes the
whole "silently ignored parameter" class of bug.

**Files:**
- `rust/coderadar/src/mcp.rs` — replace `tool_names()` with
  `tool_specs() -> Vec<ToolSpec>`; `dispatch()` reads arguments through
  `ToolSpec::read()`.
- New `rust/coderadar/src/mcp_tools.rs` — `ToolSpec`, `ArgSpec`
  (`name`, `json_type`, `required`, `default`, `description`) and the table.
- `core_indexer/src/mcp_runtime.rs` — no change expected.

**Steps:**
1. Capture the Python schema as a golden:
   `python rust/capture_mcp_schema.py > rust/golden/mcp_tools.json`
   (uses `create_server(GraphStub()).list_tools()`, 35 tools, full
   `input_schema`).
2. Encode each Python tool's docstring first line as `description` and each
   parameter's type/default as `ArgSpec`. For the 14 Rust-only tools, write
   descriptions in the same style.
3. Serialize `{name, description, inputSchema}`; `inputSchema` is
   `{"type":"object","properties":{…},"required":[…]}` with
   `additionalProperties: false` only where Python sets it.
4. Add a unit test asserting every `dispatch()` arm has a `ToolSpec` and
   vice versa (`spec_table_and_dispatch_agree`).
5. Add a golden test comparing the 35 shared tools' `name` + sorted
   `properties` keys + `required` against `rust/golden/mcp_tools.json`.

**Acceptance:** `tools/list` validates against the MCP `Tool` schema; the 35
shared tools match Python property-for-property; no arm reads an argument
that is absent from its spec.

**Effort:** 2 days. **Risk:** low–medium (mechanical, large table).

### Unit M2 — Result, error and handshake semantics

**Goal:** tool failures and lifecycle match MCP conventions.

**Files:** `rust/coderadar/src/mcp.rs` (`tool_result`, `dispatch`, `serve`).

**Steps:**
1. Errors become `result: {content:[{type:"text",text}], isError:true}`, not
   JSON-RPC `error` (clients abort the session on `-32000`).
2. Keep `structuredContent` only when the payload is an object; Python text
   content should diff cleanly after normalisation.
3. `initialize`: echo the client's `protocolVersion` when supported, else
   reply with the newest supported; set `serverInfo` to
   `{"name":"CodeRadar","version":"0.16.0 (spec v3.6)"}` for parity.
4. Handle `notifications/initialized`, `notifications/cancelled`,
   `ping`, and `resources/*` without erroring; add `prompts/list` →
   `{"prompts":[]}` if clients probe it.
5. `tools/call` with an unknown tool returns `isError:true` with the
   Python-style message.

**Acceptance:** Python `mcp` client `ClientSession.initialize()` +
`list_tools()` + one failing call round-trips without protocol errors.

**Effort:** 0.5 day.

### Unit M3 — Parameter wiring and `dry_run` compatibility

**Goal:** every documented argument changes behaviour.

**Files:** `rust/coderadar/src/mcp.rs`, `rust/coderadar/src/port.rs`.

**Fixes (measured):**

| Tool | Missing argument | Correct behaviour |
|---|---|---|
| `coderadar_node` | `include_neighbors` | pass through to `port::node` |
| `coderadar_explore` | `symbols`, `direction`, `max_files` | pass through |
| `coderadar_traverse` | `edge_kinds` | parse list, pass to `port::traverse` |
| `coderadar_reindex` | `with_embeddings`, `full` | forward to `port::reindex_op` |
| `coderadar_verify_history` | `all_generations` | accept both `all_generations` and `all` |
| `coderadar_set_project` | `confirm` | require `confirm=true` when switching away from the served root |
| mutations | `dry_run` vs `apply` | accept both; `dry_run` wins if both present; document `apply` as an alias |

**Acceptance:** a table-driven test calls each tool twice with the argument
flipped and asserts the two responses differ (or that the flag is accepted
and forwarded), plus `dry_run=false` writes while `apply=false` does not.

**Effort:** 0.5 day.

### Unit M4 — Conformance harness

**Goal:** the protocol can never silently regress.

**Files:** new `rust/mcp_conformance.py`, `tests/test_mcp_conformance.py`.

**Steps:** spawn `coderadar-rs mcp serve`; assert `initialize` result shape,
`tools/list` schema validity, `resources/list|read`, `isError` on failure,
and the roots/list one-shot still fires before the first tool call. Reuse
`tests/cr_edit_tests/mcp_stdio_probe.py` patterns.

**Effort:** 0.5 day. **Phase total: 3.5 days.**

---

## 4. Phase P1 — CLI contract parity

### Unit C1 — `--format` on every command that has it in Python

**Design:** add `#[command(flatten)] format: FormatArg` to each affected clap
variant, and route output through one helper:

```rust
fn emit(value: &Value, format: &str, text: impl Fn(&Value) -> String) -> Result<(), String>
```

`port::*` already returns the JSON-shaped value the gate compares, so
`--format json` is `serde_json::to_string_pretty` of that value.

**33 commands to add:** `affected`, `annotate`, `annotation-history`,
`annotations-on`, `as-of`, `changes-between`, `comment`, `comment-history`,
`comments-on`, `compute-embeddings`, `create-entity`, `explore`,
`export-annotations`, `history`, `import-annotations`, `ingest-history`,
`ingest-status`, `migrate-to-commit-time`, `module-children`, `node`,
`open-items`, `query`, `rename`, `replace-body`, `resolve`,
`retire-annotation`, `revise-annotation`, `search-hybrid`, `search-similar`,
`traverse`, `update-file`, `update-signature`, `verify-history`.

**Files:** `rust/coderadar/src/cli.rs` (variants + `run_external`),
`rust/coderadar/src/render*.rs` (text paths unchanged).

**Acceptance:** for each command, `--format json` parses and equals the
`port::` value byte-for-byte after volatile-field normalisation; the text
path is unchanged.

**Effort:** 1.5 days. **Risk:** clap stack pressure — flatten, don't add
top-level variants (that overflowed before).

### Unit C2 — Missing flags and renamed flags

**Files:** `rust/coderadar/src/cli.rs`.

| Command | Add / align |
|---|---|
| `as-of` | `--query` |
| `explore` | `--query`, `--direction`, `--max-files` |
| `traverse` | `--edge-kinds` |
| `open-items` | `--scope` |
| `export-annotations` | `--out`, `--store` |
| `import-annotations` | `--store`, `--on-conflict` |
| `ingest-history` | `--repo`, `--from`, `--to`, `--store` (keep `--from-sha`/`--to-sha` as aliases) |
| `verify-history` | `--from`, `--to` (keep `--from-ts`/`--to-ts` as aliases) |
| `changes-between` | `--t1`, `--t2` |
| `ingest-status` | `--store` (keep `--store-path` as alias) |
| `search-hybrid` | accept `--query` and `--query-text` |
| `replace-body`, `update-signature`, `rename`, `create-entity` | `--format` from C1; confirm `--expected-hash` / `--inject-defaults` / `--decorators` / `--anchor` match |

Prefer **accepting both spellings** over renaming, so existing scripts keep
working.

**Effort:** 0.5 day.

### Unit C3 — `--help`, usage text and exit codes

**Problem:** 18 of 21 probed captured commands treat `--help` as data:
`diagnose` runs, `history` prints `{"file":"--help"}`, `reindex --help`
starts a full reindex and hangs. `--help` and `--version` exit **1**.

**Steps:**
1. In `run_external`, intercept `-h`/`--help` before dispatch; print a static
   per-command usage block; exit 0.
2. Add a usage table (`&[(&str, &str)]`) covering the 18 captured commands;
   `exclude`/`git`/`skills` keep their existing usage output.
3. Fix exit codes in `main`: help/version → 0, usage error → 2, runtime
   error → 1. `--version` must print `coderadar 0.16.0 (spec v3.6)`.
4. Guard: a command that starts work must never see `--help` in its args
   (assert in `run_external`).

**Acceptance:** every `coderadar-rs <cmd> --help` exits 0, prints `Usage:`,
and performs no work; a scripted probe over all 55 commands proves it.

**Effort:** 0.5 day.

### Unit C4 — CLI parity harness

**Files:** new `rust/cli_parity.py` (committed), run in CI.

**Steps:**
1. Enumerate commands from both CLIs; diff option sets.
2. For read-only commands, run Python and Rust with `--format json` on the
   same store and compare after normalising `elapsed`, absolute paths and
   timestamps.
3. Assert exit codes for `--help`, `--version`, unknown flag, unknown command.
4. Record the report as `rust/golden/cli_parity.json`.

**Effort:** 0.5 day. **Phase total: 3 days.**

---

## 5. Phase P2 — Python import-surface parity

The product binary is done at this point; this phase makes the *Python
package* a thin shim, which is what "only thin wrappers remain" requires.

### Unit S1 — Mirror the import tree

**Files (new, under `rust/coderadar/python/coderadar/`):**

```
ops.py                 render.py            config.py
excludes.py            markers.py           coldstart.py
framework.py           project_state.py     skills.py
docs.py                cli.py               visualizers/__init__.py
mutation/__init__.py   query/__init__.py    embedding/__init__.py
lsp/__init__.py        agent/__init__.py    mcp/{__init__,server,roots,lazy,startup,lifecycle,tool_router}.py
```

Each file re-exports the native functions/classes and contains **no logic**
beyond argument marshalling. Public dataclasses/config objects come from
`core_indexer` through new native getters (see S2).

**Steps:**
1. Generate the target name list from the Python AST:
   `python rust/audit.py --emit-surface` → `rust/golden/module_surface.json`
   (module → public names).
2. For each module, add shims until every name resolves. Missing names by
   module today: `cli 30`, `ops 24`, `__init__ 17`, `config 15`,
   `mcp.roots 8`, `skills 8`, `embedding.dedup 7`, `mcp.startup 7`,
   `mcp.lifecycle 6`, `agent.graphrag 5`, `coldstart 4`, `docs 4`,
   `excludes 4`, `mcp.lazy 4`, `mcp.server 4`, `lsp.pool 3`,
   `mutation.tool_router 3`, `project_state 3`, `query.planner 3`,
   `query.cache 2`, `query.executor 2`, and 1 each for `embedding`,
   `markers`, `visualizers`, `visualizers.call_graph`.
3. Keep `coderadar.resolvers` as-is; extend with the remaining names.
4. `cli.py` shim: `main()` calls the Rust binary or the native CLI runner;
   the 30 `*_cmd` helpers wrap `port::` functions (they exist for tests).
5. `mcp/server.py` shim: `create_server(graph)` returns a Python MCP server
   whose tools call `_native`; reuse `mcp_runtime` semantics (roots, lazy,
   startup, lifecycle) via native calls.

### Unit S2 — Types and exceptions parity

**Files:** `core_indexer/src/api.rs`, `rust/coderadar/src/lib.rs`,
`rust/coderadar/python/coderadar/__init__.py`.

**Steps:**
1. Native constructors for the config dataclasses (`CodeRadarConfig`,
   `ProjectConfig`, `DatabaseConfig`, `EmbeddingConfig`, `LSPConfig`, …) so
   `load_config()` returns the same shape as Python (attribute access, not a
   dict).
2. Exception parity: expose `OpError`, `NoIndex`, `NotFound`,
   `ContentUnavailable`, `MutationFailed`, `MissingDependency`,
   `NoEmbeddings`, `TemporalUnsupported` from `coderadar.ops` and
   `coderadar` (already native — add the re-exports).
3. `skills`/`docs` result dataclasses (`InstallResult`, `SkillNotFound`,
   `DocNotFound`) as small Python `NamedTuple`/exception shims over native
   data.

### Unit S3 — Import-surface test

**Files:** new `tests/test_import_surface.py`.

Assert, for every module in `rust/golden/module_surface.json`, that
`import coderadar.<module>` succeeds and every public name resolves. Allow a
short, explicit allowlist of private-only helpers; the allowlist is the
only place "not ported" is recorded.

### Unit S4 — Retire the legacy package

**Steps:**
1. `pyproject.toml`: package source becomes `rust/coderadar/python`;
   drop `pythonpath = ["py_agent/src"]` and replace with the bridge path so
   tests exercise Rust by default.
2. Move `py_agent/` to `reference/py_agent/` (kept for differential tests and
   `rust-anything` re-recording, not shipped).
3. Update `tests/_rust_bridge.py` to load the bridge as the default package.
4. Re-run the full pytest matrix: tests that imported Python internals must
   either pass through the shim or move to `tests/reference/`.

**Effort:** S1 2 days, S2 0.5, S3 0.5, S4 0.5–1 day. **Phase total: 3.5–4 days.**

---

## 6. Phase P3 — numeric fidelity (embeddings)

### Unit E1 — Align the ONNX Runtime

**Evidence:** `fastembed 7.8` → `ort 2.0.0-rc.15` → `ort-sys` prebuilt
`ms@1.31.0`; Python wheel ships `onnxruntime 1.32.0`. Same model
(`BAAI/bge-small-en-v1.5`, quantized Qdrant ONNX, 384 dims), drift
`~2e-4`–`2.3e-4`.

**Steps:**
1. `core_indexer/Cargo.toml`: add
   `ort = { version = "=2.0.0-rc.15", features = ["load-dynamic"] }`
   (feature unification with fastembed's pinned `ort`).
2. Load the 1.32 runtime at startup: `ORT_DYLIB_PATH` when set, else
   `onnxruntime.dll` next to the binary, else the prebuilt 1.31 fallback.
3. Ship `onnxruntime.dll` 1.32.0 in release artifacts; record its version in
   `coderadar status --format json` (`embedding.runtime`).
4. Re-measure max abs drift over a 100-text corpus and record it in
   `rust/golden/embedding_parity.json`.

**Acceptance:** drift `<= 1e-6` on the corpus, or E2 applies.

### Unit E2 — Explicit tolerance contract (fallback)

If kernels still differ, make the tolerance a test rather than folklore:
assert `max_abs <= 2.5e-4`, and assert **identical top-k ordering** for 50
queries with fixed vectors. Document `CODERADAR_DETERMINISTIC_EMBEDDINGS=1`
and the `--recompute-embeddings` path. The gate must fail if drift grows.

### Unit E3 — Model distribution

Bundle the model or warm the cache in the installer; support offline
(`CODERADAR_MODEL_DIR`), and make `compute-embeddings` fail with the Python
error text when the model is unavailable.

**Effort:** 1–2 days if alignment works, 0.5 if tolerance is accepted.
**Phase total: 1–2.5 days.**

---

## 7. Phase P4 — cutover and packaging

### Unit R1 — Release binary identity

- Build `--release -p coderadar`; rename the artifact to `coderadar`
  (`[[bin]] name = "coderadar"`), keep `coderadar-rs` as an alias during
  shadow mode.
- Version from `py_agent/pyproject.toml` (0.16.0) so `--version` matches.
- Strip symbols, enable LTO; confirm startup `< 100 ms` and no Python DLL
  imports (the import-table probe already exists in the audit).

### Unit R2 — Install and cutover

- `python rust/cutover.py --build --verify --install DIR` writes the
  launcher; extend it to install the Python shim package as the
  `coderadar` module (`pip install --no-deps rust/coderadar/python`).
- Shadow mode: both binaries available, `coderadar` still Python; run 2
  weeks of parity logs (`rust/cli_parity.py --shadow`).
- Flip only on explicit approval. Provide `--uninstall`.

### Unit R3 — Packaging matrix

- Windows: MSVC runtime (`VCRUNTIME140*.dll`), ORT DLL, DirectML optional.
- Linux/macOS: static or vendored ORT; smoke in CI (`ubuntu-latest`,
  `macos-latest`, `windows-latest`).
- Model cache warm-up step; checksum file; `docs/INSTALL.md`.
- Clean-machine smoke: `coderadar init`, `status`, `query`, `mcp serve`,
  `search-similar` without Python or a Rust toolchain.

**Effort:** R1 0.5, R2 1, R3 1. **Phase total: 2.5 days.**

---

## 8. Phase P5 — audit and closeout

### Unit A1 — Committed audit script

New `rust/audit.py` (replaces ad-hoc probes) that prints a single JSON
report and exits non-zero on any gap:

- CLI: command set, option diff, `--help`/`--version` exit codes.
- MCP: `tools/list` schema validation, shared-tool diff vs golden, param
  forwarding.
- Python surface: module/name matrix vs `module_surface.json`.
- Framework: 23-fixture differential.
- Graph: CodeRadar store freshness and call edges.
- Numeric: embedding drift vs golden.

### Unit A2 — Final verification

Run and record: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -D warnings`, `cargo test --workspace`,
`rust-anything gate rust/coderadar`, `python rust/audit.py`,
`python rust/cli_parity.py`, `python rust/cutover.py --verify`,
`python rust/bench.py --min-ms 20 --json`, full `pytest`.

**Effort:** 1 day. **Phase total: 1 day.**

---

## 9. Sequencing and effort

```
P0 M1 ──► M3 ──► M4
   └────► M2 ──┘
P1 C1 ──► C2 ──► C4
   └────► C3 ──┘
P2 S1 ──► S2 ──► S3 ──► S4
P3 E1 ──► E2/E3
P4 R1 ──► R2 ──► R3        (after P0–P2)
P5 A1 ──► A2               (last)
```

| Phase | Units | Effort |
|---|---|---|
| P0 MCP protocol | M1–M4 | 3.5 d |
| P1 CLI contract | C1–C4 | 3 d |
| P2 Python surface | S1–S4 | 3.5–4 d |
| P3 embeddings | E1–E3 | 1–2.5 d |
| P4 cutover/packaging | R1–R3 | 2.5 d |
| P5 audit | A1–A2 | 1 d |
| **Total** | | **14.5–17 d** (~3–3.5 weeks solo, ~2 weeks if P0/P1 run in parallel with P2) |

P0 is independent of P1/P2 and should start immediately: it is the only
phase that blocks real MCP clients. P2 can run alongside P0/P1 with a second
worker.

---

## 10. Verification matrix

| Gate | Command | Proves |
|---|---|---|
| D1 | `python rust/mcp_conformance.py` | spec-valid `tools/list`, `isError`, handshake, roots |
| D2 | `python rust/cli_parity.py` | options, defaults, exit codes, JSON equality |
| D3 | `python -m pytest tests/test_import_surface.py` | every public module name resolves in Rust |
| D4 | `python rust/audit.py --embedding` | drift ≤ tolerance, top-k identical |
| D5 | `python rust/cutover.py --verify --install DIR` | launcher, shim install, uninstall |
| D6 | `python rust/audit.py` | zero gaps, single JSON report |
| Regression | `rust-anything gate rust/coderadar` | 4,447+ recorded cases still match |
| Perf | `python rust/bench.py --min-ms 20 --json` | median ≥ 1.0× vs Python |
| Rust health | `cargo fmt/clippy/test` | no warnings, no unsafe, tests green |

---

## 11. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| `tools/list` schema drift from dispatch | clients call wrong args | single `ToolSpec` table, unit test that table and dispatch agree |
| clap stack overflow when adding variants | build failure | flatten `FormatArg`; never add top-level variants |
| ORT 1.32 incompatible with `ort` rc.15 bindings | P3 blocked | keep prebuilt 1.31 fallback; ship E2 tolerance instead |
| Moving `py_agent` breaks recorded gate cases | gate regressions | keep `reference/` on the recording path; re-record only if source moves |
| Python shim grows logic | defeats the goal | S3 import test + review rule: shims may only marshal |
| Shadow cutover breaks scripts | user-visible outage | 2-week shadow log, dual binaries, `--uninstall` |

---

## 12. Definition-of-done checklist

- [ ] M1: `tools/list` returns 49 valid `Tool` objects; 35 shared tools match the Python golden.
- [ ] M2: client `initialize`/`list_tools`/failing call round-trips; errors are `isError`.
- [ ] M3: every documented tool argument changes behaviour; `dry_run`/`apply` both accepted.
- [ ] C1: 42/42 Python `--format` commands supported.
- [ ] C2: 12 flag/alias gaps closed.
- [ ] C3: 55/55 `--help` exit 0 and run no work; `--version` exits 0.
- [ ] C4: CLI parity harness green.
- [ ] S1–S3: 169 missing names resolved; `tests/test_import_surface.py` green.
- [ ] S4: `py_agent` moved to `reference/`; bridge is the installed `coderadar`.
- [ ] E1/E2: embedding drift ≤ 1e-6 or recorded tolerance enforced by a test.
- [ ] R1–R3: `coderadar --version` = `0.16.0 (spec v3.6)`; clean-machine install works.
- [ ] A1–A2: `rust/audit.py` reports zero gaps; full verification matrix green.
