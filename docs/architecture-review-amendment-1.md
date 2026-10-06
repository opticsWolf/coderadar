# Amendment 1 — coderadar-architecture-review + decision-register

> **Date:** 2026-10-06 · **Applies to:** `coderadar-architecture-review` and
> `coderadar-decision-register` (okfgraph, topic `coderadar-arch-review`,
> baseline `dev_0.12` @ `0b18a36`).
> **Status:** the core documents are frozen; this amendment is authoritative
> where it contradicts them. Read together, review first, amendment second.
> **Method:** re-verification pass — every sampled factual claim in the review
> re-checked against the working tree, plus new observations.

## 1. Corrections (review statements that are inaccurate)

- **E1 — FFI export count.** Review §3 says `lib.rs` has "48 py fns".
  Actual: **47 `#[pyfunction]` exports**, plus **16 further `fn`s inside the
  `#[pymethods]` block** (module-class methods, not pyfunctions). The line
  count (4,197) was exact. God-module verdict unchanged.
- **E2 — test counts.** Review §5 claims "1,359: 413 Rust + 946 Python".
  Actual: pytest collects **947** tests; `#[test]` count is **415** across
  `core_indexer/src` + `core_indexer/tests`. Total **1,362**. Direction of the
  claim (surface pinned by parity/drift tests) unchanged.
- **E3 — stale header location.** Review cites `resolve/mod.rs:3`; the stale
  text spans **lines 2–3** (line 2 "CodeRadar v3.6 — Resolution Module" is
  fine; line 3 names the five-layer cascade with Stack Graphs and LSP). The
  one-liner fix covers both lines.
- **E4 — WAL tombstone path.** The file is `core_indexer/src/update/wal.rs`
  (under `src/` directly, not under `mutation/`). Review's "update/wal.rs" was
  correct but ambiguous; "verified not compiled" re-confirmed
  (`mod update` count in lib.rs: 0).

## 2. Re-verified — sampled claims that hold exactly

| Claim | Check |
|-------|-------|
| `Snapshot` at `__init__.py:980`; `CodeGraph.as_of` at `:464`; `ops.as_of` at `ops.py:792` | exact |
| `set_project` absent from ops, comment at `ops.py:74–78` | verbatim |
| D13 HashMap sites: `clones/lsh_index.rs:12,26` (`HashMap<u64,Vec<u32>>` bands), `clones/mod.rs:161–163,178–190,248` (sources/languages/paths/memo/by_raw) | exact |
| D11: CLI `callers` bypasses ops via `MacrameQuery(graph).callers_of(...)` at `cli.py:1113`, no `--format json` on that path | confirmed |
| D18: own `.coderadar.toml` `allow = ["src/", "lib/", "tests/", "scripts/"]`; no `src/`, `lib/`, or `scripts/` exists in the repo | verbatim |
| 41 `.scm` query files; 12 smell rule files (+`mod.rs`) in `smells/rules/` | exact |
| stale `py_agent/src/coderadar/__pycache__/flatbuffer.cpython-313.pyc` on disk | confirmed |
| D2: config still accepts `[resolution.lsp]`; pool unreachable from production | confirmed |

## 3. New observations (found during re-verification)

- **N1 — self-hosting is dry-run.** The repo's own config also sets
  `default_dry_run = true`. Even with a fixed allow-list, CodeRadar would not
  actually mutate itself by default. The D18 fix (DR-27) should state whether
  self-mutation keeps dry-run (recommendation: keep `default_dry_run = true`
  until D18 lands, then decide explicitly — self-hosting with real writes is
  its own dogfood milestone).
- **N2 — deny-over-allow precedence.** The deny list (`/migrations/`,
  `/*.lock`, `/generated/`) only matters because allow and deny can name
  overlapping trees once D18 *derives* the allow list. Wherever the template
  docs explain D18, they must state the precedence rule (deny wins) in one
  sentence.
- **N3 — macrame-db 0.19.0 is published on crates.io** (`cargo search`
  confirms; the local Macrame checkout is the same version). The plan's §0.6
  upgrade needs no git/path dependency — registry bump. (Actioned in the
  plan amendment.)

## 4. Decision register — proposed new rows

The register carries no DR numbers for the review's own proposals or for the
work decided since. Proposed rows (to be **added by the implementer**, per the
process rule — do not edit the register doc directly until then):

| Proposed ID | Decision | Status |
|-------------|----------|--------|
| **DR-25** | Code bodies content-addressed in macrame-0.19 blobs; digest in file-concept `extra["source_blob"]`; diffs computed on read, never stored; retention via the archive reference scan | **open** (v0.12 §0.6/§1.9/§3.0/§3.4) |
| **DR-26** | Version policy: +0.0.1 per implemented feature (`pyproject.toml` + workspace `Cargo.toml` together, one bump commit, named in the feature's DR row); patch line accumulates to 0.12.0 | **open** (process, active now) |
| **DR-27** | (was proposal P-A) Self-hosting allow-list: derive or fix `[mutation] allow` so CodeRadar can mutate its own engine dirs | **open** (P4 / D18) |
| **DR-28** | (was proposal P-B) Split `lib.rs` per App. E: bindings + converters + git/watcher/config/synthetics out of the 4,197-line god module | **open** (P4 / D19) |
| **DR-29** | (was proposal P-C) One-line residue: `resolve/mod.rs:2–3` header; delete stale flatbuffer `.pyc` | **open** (P4, first in sequencing) |

## 5. What this amendment does *not* change

Every other review claim and register row stands as written; the era history,
D1–D20 deviations, and DR-1..DR-24 statuses were not contradicted by any
sample checked. The v0.12 P0 set (DR-9, DR-12, DR-13, DR-14) remains exactly
as prioritized.
