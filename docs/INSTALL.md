# Installing CodeRadar

CodeRadar ships as a single native binary. There is no Python runtime
requirement, no virtualenv, and no interpreter on the `PATH` for any command —
including the MCP server.

## Quick start

```bash
cargo install --path rust/coderadar          # debug-free release build
coderadar --version                          # coderadar 0.16.0 (spec v3.6)
cd /path/to/project
coderadar init
coderadar status --format json
```

`coderadar init` writes `.coderadar/store/coderadar.db`. Everything after that
(`query`, `node`, `traverse`, `smells`, `mutation`, `as-of`, …) reads that
store, and `coderadar mcp serve` serves it over stdio.

## Build matrix

| Target | Build | Runtime requirements |
|---|---|---|
| Windows x86_64 (MSVC) | `cargo build --release -p coderadar` | MSVC C runtime (`vcruntime140.dll` — present on any machine with the VC++ redistributable), DirectML/D3D12/DXGI for GPU inference if available |
| Linux x86_64 (glibc ≥ 2.28) | same | `libgcc`, `libm`, `libpthread`, `libdl` (standard) |
| macOS 12+ (arm64/x64) | same | CoreFoundation/Accelerate (system) |

Notes:

- **ONNX Runtime is linked statically.** `fastembed 7.8` pulls the
  `ms@1.31.0` prebuilt runtime through `ort-sys`; there is no system
  `onnxruntime.dll`/`.so` to install. Windows builds additionally probe
  DirectML and fall back to CPU execution when it is unavailable.
- **No Python.** The release binary contains no `python3xx.dll` reference; the
  Python package remains available as a facade for existing callers, but it is
  not part of this install path.
- The artifact is `target/release/coderadar` (`coderadar.exe` on Windows).
  Older builds used `coderadar-rs`; both names are accepted by the in-repo
  harnesses during the transition.

## First run and embeddings

`init` indexes source code and, if `.coderadar.toml` enables it, computes
embeddings. The default model is `BAAI/bge-small-en-v1.5`
(`qdrant/bge-small-en-v1.5-onnx-q`, 384 dimensions, ~67 MB).

- The model is downloaded once and cached in `%TEMP%/fastembed_cache`
  (Windows) or `~/.cache/fastembed` (Unix). Offline installs: copy that cache
  directory to the target machine.
- No network and no cache? Either set a deterministic fallback with
  `CODERADAR_DETERMINISTIC_EMBEDDINGS=1` (vectors are then *not*
  Python-compatible — opt in deliberately), or index without embeddings.
- Embedding drift vs. the reference runtime is a recorded contract:
  ≤5e-4 max component drift with identical ranking. See
  `rust/golden/embedding_parity.json`.

## Configuration

`.coderadar.toml` in the project root (or any parent) is the single config
file. All sections are honoured by the binary: `[project]`, `[database]`,
`[languages.*]`, `[resolution]`, `[resolution.lsp]`, `[embedding]`,
`[smells]`, `[excludes]`, `[retention]`, `[watch]`.

Environment knobs that matter during install:

| Variable | Default | Purpose |
|---|---|---|
| `CODERADAR_DETERMINISTIC_EMBEDDINGS` | unset | `1` swaps the model for deterministic hashing vectors |
| `CODERADAR_INDEX_WAIT` | `25` | seconds a tool waits for a running index |
| `CODERADAR_INDEX_HEARTBEAT` | `5` | seconds between "still indexing" notices |
| `CODERADAR_PARENT_WATCHDOG` | `1` | `0` disables the MCP parent watchdog |
| `CODERADAR_ROOTS_TIMEOUT_MS` | `1000` | budget for the MCP `roots/list` probe |
| `CODERADAR_HANDSHAKE_TIMEOUT` | `60` | seconds before an idle MCP client is dropped |
| `ORT_DYLIB_PATH` | unset | only for `--features core_indexer/dynamic-ort` builds |

## Verifying an install

```bash
python rust/smoke.py                 # release binary, clean dir, MCP handshake
python rust/cli_parity.py            # CLI contract vs. the Python reference
python rust/mcp_conformance.py       # 49 tools, 3 resources
python rust/import_surface.py --check
```

`rust/smoke.py` is the clean-machine check: it copies the golden fixture into
a fresh directory, runs `init`, `status`, `query` and an MCP `tools/list`
handshake using only the system `PATH`, and asserts the binary references no
Python DLL.

## Replacing an existing install

`rust/cutover.py --install DIR` writes a `coderadar` launcher into `DIR` that
execs the Rust binary. The Python package is never modified. Verify first:

```bash
python rust/cutover.py --build --verify     # 55 shared commands, 0 local-only
python rust/cutover.py --install ~/.local/bin
```

## Uninstalling

Delete the binary (and, if you used `cargo install`, `cargo uninstall
coderadar`). Project data lives only in each repository's `.coderadar/`
directory; the model cache lives in the directories listed above.
