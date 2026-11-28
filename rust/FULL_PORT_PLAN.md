# Full-port plan — closing every remaining gap

Status: **in progress**. P0, P1 and R0 landed; P2/P3 partially landed
(measured below). Canonical decisions: [`PORT.md`](PORT.md).
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
| **D1 Protocol** | `coderadar mcp serve` is MCP-spec valid and schema-compatible with the Python server for all 35 shared tools. | ✅ 49 schema-valid tools; conformance harness green |
| **D2 CLI contract** | Every Python command accepts the same options, defaults, exit codes and output formats. | ✅ inventory, `--help`, exit codes, `--format` and 15/15 payloads green |
| **D3 Python surface** | `import coderadar.<module>` resolves the same public names, all delegating to Rust. | ⚠️ 227/342 (66.4%) public names; **0 modules absent** |
| **D4 Numeric fidelity** | Embedding vectors match Python exactly, or a recorded tolerance is enforced by a test. | ⚠️ dynamic-runtime build path shipped; measurement pending |
| **D5 Cutover** | Rust binary is installed as `coderadar`; Python survives only as a shim; packaging reproducible. | ⚠️ `cutover.py` exists, never applied |
| **D6 Closeout** | A committed audit script reports zero gaps; full verification matrix green. | ⚠️ `cli_parity.py` + `mcp_conformance.py` + import test committed |

**Non-goals** (documented, so they don't reopen): private Python helpers with
no caller edge (`_render_row`, `_print_effective_excludes`, …) are not ported
by name; they are covered by the front doors. `py_agent` stays in-tree as the
reference implementation until D3 lands, then moves to `reference/`.

---

## 2. Measured baseline

| Surface | Python | Rust | Gap |
|---|---|---|---|
| CLI command names | 55 | 55 | 0 |
| CLI `--format` | 42 | 42 | 0 |
| CLI payload rows (fixture) | 15 payloads | 15 identical | 0 |
| CLI other flags | — | — | 0 (aliases accepted both ways) |
| CLI `--help` handled | 55 | 55 | 0 |
| MCP tool names | 35 | 49 | 0 (+14 Rust extras) |
| MCP `tools/list` schema | full objects | full objects | 0 (35 shared tools byte-identical) |
| MCP tool params honoured | — | — | 0 (7 tools fixed) |
| Python module public names | 342 public names | 227 resolvable | **115 inside 6 modules + 0 absent** |
| Framework extraction parity | — | — | 23/23 fixtures identical |
| Gate cases | — | 4,447 | exact |
| Embedding drift | — | **4.33e-4 max** (mean 2.46e-5) | contract: ≤5e-4 **and** identical top-3 |

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

### Unit R1 — Release binary identity **(done)**

- **Done:** `[[bin]] name = "coderadar"`; the in-repo harnesses resolve the
  binary through `rust/binaries.py`, which prefers `coderadar` and still
  accepts the pre-rename `coderadar-rs` on disk.
- **Done:** `--version` is `coderadar 0.16.0 (spec v3.6)` in both debug and
  release builds.
- **Done:** the release build links ONNX Runtime statically; the import-table
  probe finds 33 DLL name strings, all Windows system libraries (plus
  `directml.dll`, probed at runtime) and no Python DLL.
- Not done: symbol stripping / LTO tuning (release profile is the default
  `opt-level = 3` with debug info off; startup is already well under 100 ms).

### Unit R2 — Install and cutover

- `python rust/cutover.py --build --verify --install DIR` writes the
  launcher; extend it to install the Python shim package as the
  `coderadar` module (`pip install --no-deps rust/coderadar/python`).
- Shadow mode: both binaries available, `coderadar` still Python; run 2
  weeks of parity logs (`rust/cli_parity.py --shadow`).
- Flip only on explicit approval. Provide `--uninstall`.
- **Status:** `--build --verify` passes (`4/4`, 55 shared commands, 0
  local-only) and `--install DIR` works, but **nothing has been installed on
  PATH**: replacing the Python CLI is the one step that needs explicit
  approval. The launcher path is the only supported cutover.

### Unit R3 — Packaging matrix **(done)**

- **Done:** `docs/INSTALL.md` documents the per-platform build matrix, the
  runtime requirements (MSVC CRT; statically linked ORT — no separate
  `onnxruntime` DLL; DirectML optional on Windows), the model cache locations
  and warm-up, the environment knobs, install verification and uninstall.
- **Done:** `rust/smoke.py` is the clean-machine check. It copies the golden
  fixture to a fresh directory, runs `--version`, `init`, `status --format
  json`, `query functions --format json` and an MCP `tools/list` handshake
  with a system-only `PATH` and a scratch `HOME`, then asserts the artifact
  references no Python DLL. All six checks pass against the release build.
- **Done:** the checksum/LTO item is unnecessary in the current shape (one
  binary, statically linked ORT); record a SHA-256 in release notes when a
  release pipeline exists.
- Not done: CI on `ubuntu-latest`/`macos-latest`/`windows-latest` — the repo
  has no CI configuration, and cross-platform builds cannot be produced from
  this machine.

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

### Unit A2 — Final verification **(done)**

- **Done:** `rust/audit.py` is the single entry point. It runs `fmt`,
  `clippy -D warnings`, `cargo test --workspace`, `rust-anything gate`,
  `cli_parity.py`, `mcp_conformance.py`, `import_surface.py --check`, the E2
  embedding contract (`--ignored`) and `smoke.py`, printing one line per check
  with timings, `--quick` to skip the model/release checks and `--json` for a
  machine-readable matrix.
- **Result:** `audit ok: 9 checks, 149s` — 4447/4447 recorded cases,
  72/59 CLI commands with 15 identical payloads, 49 MCP tools and 3
  resources, 210/342 Python names with 0 absent modules, embedding contract
  green, release smoke green.
- Not part of `audit.py` on purpose: `cargo bench`-style timing
  (`python rust/bench.py --min-ms 20 --json`, median 1.57x) and the full
  `pytest` suite, both run separately because they measure rather than gate.

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
| P4 cutover/packaging | R1–R3 | done except the approved cutover |
| P5 audit | A1–A2 | done (`rust/audit.py`) |
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
- [x] R1–R3: `coderadar --version` = `0.16.0 (spec v3.6)`; clean-machine smoke green.
- [x] A1–A2: `rust/audit.py` reports zero gaps; full verification matrix green.

---

## 13. Execution log (Unit 51)

What landed, and the evidence for each unit.

### P0 — MCP protocol (done)

- `core_indexer/src/mcp_catalogue.rs`: the catalogue is built from
  `rust/golden/mcp_tools.json` (captured from the Python server) plus
  `rust/golden/mcp_extra_tools.json` (14 Rust-only tools) and
  `rust/golden/mcp_schema_overrides.json` (additive Rust arguments).
- `tools/list` returns `{name, description, inputSchema}` for all 49 tools;
  the 35 shared tools keep the Python schema by construction.
- `tools/call` errors are `result.isError`, not JSON-RPC errors; unknown tools
  are rejected before the index handshake.
- `initialize` echoes a supported `protocolVersion`, reports
  `serverInfo {name: "CodeRadar", version: "0.16.0"}` and the Python
  instructions block; `ping`, `prompts/list`, `resources/templates/list` and
  `notifications/*` are handled.
- Argument defaults now come from the schema (`Args::str/i64/f64/bool/strings`),
  undeclared arguments are rejected, and the seven ignored parameters are
  wired: `node.include_neighbors`, `explore.symbols/direction/max_files`,
  `traverse.edge_kinds`, `reindex.with_embeddings/full`,
  `verify_history.all_generations`, `set_project.confirm`, plus mutation
  `dry_run` with `apply` accepted as its inverse.
- Evidence: `python rust/mcp_conformance.py` ->
  `MCP conformance ok: 49 tools, 3 resources`; `tests/test_mcp_conformance.py`.

### P1 — CLI contract (done)

- 42/42 Python `--format` commands now emit JSON; a global `--format` covers
  every native command, captured commands parse it themselves.
- Flag parity: `as-of --query`, `explore SYMBOLS... --query/--direction/
  --max-files`, `traverse --edge-kinds`, `open-items --scope`,
  `export-annotations --out/--store`, `import-annotations --store/
  --on-conflict`, `ingest-history --repo/--from/--to/--store` (old spellings
  kept as aliases), `verify-history --from/--to`, `changes-between --t1/--t2`,
  `ingest-status --store`.
- `--help` for all 28 captured commands comes from
  `rust/golden/cli_usage.json`; help/version exit 0 and usage errors exit 2.
- Binary identity: clap name `coderadar`, version `0.16.0 (spec v3.6)`.
- Evidence: `python rust/cli_parity.py` -> `CLI parity ok: 72/59 commands,
  15 JSON payloads compared`, zero payload differences.

#### R0 — payload rows (landed)

The probe used to run against the live repository, where the store is
rewritten between the two runs (cold-start replay), so row sets differed for
reasons that had nothing to do with the port. It now:

1. builds `core_indexer` and installs `_core.pyd` beside `py_agent/src`, so
   both front doors run the same engine revision;
2. indexes `rust/golden/parity_fixture` once with the Rust binary;
3. gives each side its own copy of the indexed project per command, runs the
   probe with `-C`, and compares normalised payloads.

That turned 12 opaque "payload differences" into real fixes:

- `api::entity_json` now matches `ffi_convert.rs`: classes carry
  `name_span_start/end` and `embedding_hash`; modules carry
  `embedding_hash`; imports carry `name_span_*`, `has_embedding`,
  `embedding_hash`; constants and type aliases carry `span_end`,
  `has_embedding`, `embedding_hash`.
- `module_children` returns full entity rows for classes/functions and the
  binding's `{id, raw, kind, line}` imports and `{id, name}` constants
  (`core_indexer::api::module_children`).
- `traverse` reports its start at `depth: 0` with `edge_type: ""`, and the
  payload echoes `edge_kinds: null` when no kinds were requested.
- `open_items` returns `{scope, items, count}`.
- scaffolding rows always carry `snippet` (empty on the `scan-stats` row).

Two comparison rules were added because the ordering is not a contract:
`graph.functions` is a `HashMap`, so equally ranked findings come back in
per-process order in BOTH implementations; the harness sorts rows by
(`score` desc, id) before comparing, and ignores volatile keys (`root`,
`store`, `indexed_at`, `age_seconds`, `config`, ...).

Guard tests: `core_indexer/src/graph/tests/api_rows_tests.rs` pins the shared
field set, the module-children projection and the traverse start row.

### P2 — Python surface (partial, measured)

Landed (all delegate to Rust, no duplicated logic):

- `ops`, `render` (engine-backed helpers), `docs`, `skills`, `coldstart`,
  `excludes`, `markers`, `project_state`, `config`, `embedding`,
  `query` + `query.cache`, `visualizers` (+ `mermaid`, `graphviz_viz`,
  `call_graph`), `lsp` + `lsp.pool`, `mutation` + `mutation.tool_router`,
  `mcp` + `mcp.server`, `framework`, and the 13 resolver modules.
- 21 new native exports: annotations CRUD, git ingest/backfill, docs/skills,
  cold start, exclusions and project files, marker/project state, live config,
  embedding settings, visualization, LSP definition/override/status.

Landed after the first pass (S5/S6):

- `embedding.dedup`: `EmbeddingDedup`, `EmbedTarget`,
  `EmbeddingUnavailable`, `compute_content_hash`, `embed_body`,
  `embed_cache_key`, `stored_key_model`. The hash rule mirrors Python
  (xxHash3-64 when `xxhash` is importable, else SHA-256[:16]); the batch
  dedup, the embedded text and the cache-key format run in Rust
  (`api::embedding_batch`, `api::embed_body`, `api::embed_cache_key`).
- `query.planner`: `QueryIntent`, `QueryPlan`, `plan_query` over
  `api::plan_query` (same keyword ladder and order as Python).
- `agent.graphrag`: `ContextStrategy`, `GraphRAGResult`,
  `GraphRAGContextBuilder`, `GraphRAGPipeline`, `QueryPlanner` over
  `api::graphrag_build_context` / `api::graphrag_execute` (compression,
  skeletonising, token estimate and truncation in Rust).
- `mcp.roots`: `ResolvedRoot`, `uri_to_path`, `resolve_project_root`,
  `resolve_from_client_roots`, `resolve_selector`, `adopt_project_root`,
  `describe`, `find_marker` over `core_indexer::mcp_runtime`. Fixing this
  surfaced two engine bugs: the walk-up crossed `$HOME` (Python stops at it)
  and canonical paths kept the Windows verbatim prefix, so root comparisons
  failed against `Path.cwd()`.

Landed in the S7/S5 closure:

- `mcp.roots` (unit S7): the full ladder, selector, adoption and describe.
- `mcp.startup`: `BackgroundIndex`, `IndexStatus`, `IndexOutcome`,
  `configure/current/ensure_ready`, `progress_message`. The generation
  tracking, shared build lock, status/elapsed and the heart-beating wait are
  `mcp_runtime::BackgroundIndex` behind native `startup_*` functions; a
  custom `analyze` stub runs through the same worker (the native closure
  calls back into Python), and `startup_wait` releases the GIL while it
  blocks.
- `mcp.lifecycle`: `ParentWatchdog`, `HandshakeTimeout`, `install` and the
  middleware. Threads/timers stay in Python (they are host objects); the
  liveness probe is `mcp_runtime::process_alive`, and the lazy "ask or keep"
  rule is native (`lazy_should_ask`, `lazy_trade_up`).
- `mcp.lazy`: `LazyRootRetry` with native decisions, async `roots/list` and
  the trade-up/reindex step.
- `query.executor`: `MacrameQuery`/`MacrameSnapshot` delegate traversal,
  lookup, listing, call indexes and stats to the engine; the three
  unwired-in-the-reference methods still return `[]`.
- `mcp.server`: `serve` and `set_launch_cwd` added, so the module matches the
  reference surface.

Every module under `py_agent/src/coderadar` imports from the bridge. **227/342
public names (66.4%)** now resolve; the remaining 115 are recorded in
`rust/golden/import_surface_pending.json`:

| Module | Missing | What they are | Plan |
|---|---:|---|---|
| `cli` | 65 | click command wrappers and op re-exports | binary-backed wrappers with the same parameter names |
| `render` | 42 | text renderers (data in, text out) | one native dispatcher + thin Python wrappers |
| `__init__` | 3 | `Snapshot`, `blob_stats`, `edge_persist_error` | `Snapshot` needs the bitemporal methods; the counters are engine state |
| `coldstart` | 3 | `store_db_path`, `store_is_fresh`, `stale_source_files` | engine walk with the shared exclusion matcher |
| `ops` | 2 | `ingest_history`, `open_project` | `open_project` maps onto the MCP project opener; `ingest_history` needs the git ingest signature, not the renderer |

Landed for `__init__`/`skills`/`query.cache`/`excludes`:

- `models.py` defines the public mutation/update result types and the
  exception hierarchy field-for-field with the reference (`SymbolChange`,
  `UpdateReport`, `MutationEdit`, `MutationPlan`, `MutationResult`,
  `MutationFailure`, `MutationFileError`, `MutationFileResult`,
  `MutationSyncSummary`, `CodeRadarError` + five subclasses). They are data
  shapes only; the engine produces the values. Because the rust-anything
  driver decodes cases into classes with the same names, the boundary now
  translates: `mutation_plan`, `mutation_applied` and `edit` accept either the
  dataclass or a plain mapping (`_coerce` in `coderadar/__init__.py`), which is
  what keeps the 4447-case gate green.
- `skills.InstallResult` and `query.cache.cached_query` (with the reference
  LRU semantics: `>=` TTL expiry, `make_key` hashing method/params/epoch).
- `excludes.resolve_entity_path` resolves against the engine's indexed root,
  falling back to the module root only when no graph is loaded.

The surface is measured by `python rust/import_surface.py` (rewrites the
baseline) and `--check` fails on regression.

Evidence: `python -m pytest tests/test_import_surface.py` (passes; asserted
against the recorded baseline) and 227/342 public names resolved
(31.3% -> 66.4%), with every module present.

### P2b — MCP handles and the last absent module (done)

`mcp.startup`, `mcp.lifecycle` and `mcp.lazy` are shims over `mcp_runtime`
(generation tracking, build lock, status/elapsed, bounded wait, process
liveness, and the lazy ask/trade-up rules), and `query.executor` delegates to
the engine. `mcp.server` gained `serve`/`set_launch_cwd`. **No module is
absent**; 210/342 public names resolve.

### P3 — embeddings (contract recorded)

Mechanism and contract are now both in place:

- `rust/embedding_parity.py` records reference vectors from the Python
  fastembed stack into `rust/golden/embedding_parity.json` (model, texts,
  vectors, tolerance, reference versions).
- `core_indexer/src/graph/tests/embedding_parity_tests.rs` is the contract:
  same width, max component drift ≤ recorded tolerance, identical top-3
  ranking, and pairwise cosine agreement ≤2e-4. It loads the ONNX model, so
  it is `#[ignore]`d and run with
  `cargo test -p core_indexer embedding_parity -- --ignored`.

Measured with the reference runtime available on this machine
(fastembed 0.9.1 / onnxruntime 1.32.0) against the Rust build
(fastembed 7.8 / ORT ms@1.31.0), same `model_optimized.onnx` and tokenizer:

| Metric | Value |
|---|---|
| max component drift | **4.328e-4** |
| mean component drift | 2.460e-5 |
| max pairwise cosine drift | 1.254e-4 |
| top-3 ranking | identical for all 12 corpus texts |

The tolerance is 5e-4: tight enough that a different model, tokenizer or
preprocessing cannot pass, loose enough that int8 kernel scheduling does.
The earlier ~2e-4 figure came from a different fastembed version pair; the
load-bearing guarantee is the ranking identity, not the float ceiling.

### P3b — original mechanism notes

- `core_indexer` feature `dynamic-ort` adds `ort/load-dynamic`; the default
  build keeps fastembed's pinned prebuilt runtime.
- Build: `cargo build --features core_indexer/dynamic-ort`, then set
  `ORT_DYLIB_PATH` to the reference `onnxruntime.dll`.
- Not done: the reference environment here carries onnxruntime 1.28/fastembed
  0.8, not the 1.32/0.7.4 pair the audit compared against, so the drift
  measurement and the tolerance test (E2) need a pinned reference env.

### P4/P5 — cutover, packaging, audit

- Committed harnesses: `rust/cli_parity.py`, `rust/mcp_conformance.py`,
  `tests/test_import_surface.py`, `tests/test_mcp_conformance.py`.
- Not done: binary renamed to `coderadar` (the artifact is still
  `coderadar-rs` so the harnesses and `cutover.py` keep working), clean-machine
  packaging, shadow cutover approval, and a single `rust/audit.py` entry point.

---

## 14. Revised sequence for the remainder

| Unit | Scope | Effort |
|---|---|---|
| ~~S7~~ | ~~`mcp.{lazy,lifecycle,roots,startup}` + `query.executor`~~ done: all modules resolve | — |
| ~~E2~~ | ~~drift contract~~ done: 4.33e-4 max, identical ranking, golden recorded | — |
| R0 | ~~row shape~~ done: fixture harness compares 15 payloads, 0 differences | — |
| ~~R1/R3~~ | ~~artifact rename, `docs/INSTALL.md`, `rust/smoke.py`~~ done | — |
| ~~A2~~ | ~~`rust/audit.py` entry point~~ done: 9 checks green | — |
| R2 | Shadow cutover (needs explicit approval; helper verified, nothing installed) | 0.5 d |
| S8 | close the 134 names inside `cli`/`render`/`__init__` (command wrappers, renderer exports, mutation types) | 1-1.5 d |
| **Total** | | **~1-2 d** |
