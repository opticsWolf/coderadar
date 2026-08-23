# Open Items

> **Date:** 2026-08-23 · **Baseline:** v0.6.47 (`93b82f9`, `dev`)
> **Purpose:** one reconciled list of everything the four planning artifacts
> left open, plus everything the v0.7 release smoke-check found and did not fix.
> This is the input to [`v0.8-improvement-plan.md`](v0.8-improvement-plan.md).

Every line below was checked against the code on `dev` at `93b82f9`, not
against the plan text that proposed it. Items the plans list as deferred but
which have since been closed are recorded in §5 so they are not re-planned.

Sources reconciled:

| Document | Status |
|---|---|
| [`v0.7-improvement-plan.md`](v0.7-improvement-plan.md) | **Complete** — 26/26 tasks, Phases 0–6. Its own "out of scope" list survives into §2. |
| [`v1-readiness-plan.md`](v1-readiness-plan.md) | **Complete** — Phases 1–4 all ✅. Its "Explicitly Deferred" block survives into §2. |
| [`graph-split-plan.md`](graph-split-plan.md) | **Complete** — `graph.rs` is now 13 files + `tests/`, 3,037 lines. Nothing open. |
| [`traversal-matrix.md`](traversal-matrix.md) | **Complete** — steps A–F landed; §5's "open decision for the maintainer" (step D, resolve backfill) was decided *for* and shipped. Two residues survive into §2. |

---

## 1. Found by the v0.7 release smoke-check, not yet fixed

These are the items the four smoke passes (`b140cbf`, `acadb64`, `d269768`,
`93b82f9`) surfaced and deliberately did **not** act on, because each is a
scope decision rather than a bug fix.

### 1.1 `explore()` can only follow call edges — **the honesty is new, the gap is not**

[`__init__.py`](../py_agent/src/coderadar/__init__.py) ·
`EDGE_KIND_CALLS = "calls"`

`CodeGraph.explore(start_id, direction, max_depth, edge_kinds)` BFSes over
`_get_incoming`/`_get_outgoing`, which are `callers_of`/`callees_of` — one
edge kind. Asking for `imports`, `extends`, or `overrides` now returns `[]`
rather than silently returning call edges labelled as imports, which is
correct but not useful.

Meanwhile the Rust `traverse` binding handles all four kinds, backed by real
indexes since the `traversal-matrix.md` step-D backfill. The facade simply
does not call it.

**Shape of the fix:** route `explore` through `_core.traverse` and delete the
Python BFS, the same move `traversal-matrix.md` step B made for `executor.py`.
`explore` is one of the four primary MCP tools, so this is agent-visible.

### 1.2 Every CLI invocation re-indexes the project

[`cli.py`](../py_agent/src/coderadar/cli.py) · `_ensure_graph`

The graph lives only in the process that built it. `_ensure_graph` was added
in `b140cbf` so that `coderadar stats` after `coderadar init` answers instead
of saying "No graph loaded — run coderadar init first", advice the user had
just taken. It answers by **re-analyzing from source on every invocation**.

On the measured real-repo baseline (`v1-readiness-plan.md` §1.4:
`codegraph-main`, 605 files) a cold `analyze()` is **~46 s**. So
`coderadar query` on a large project now costs 46 seconds instead of failing
in 50 ms. That is the right trade for correctness and the wrong one for use.

The real fix is cold-start from the Macrame ledger — see §2.1. `_ensure_graph`
is the interim state and should be documented as such, not left to look like
the design.

### 1.3 C and C# are the two Tier-1 languages with no signature test

[`tests/test_language_signatures.py`](../tests/test_language_signatures.py)

The README's Tier-1 table promises 12 languages. `CASES` covers 10: Python,
PHP, JavaScript, TypeScript, Kotlin, Go, Rust, Ruby, Java, C++. **C and C#
are absent.** C matters most: `93b82f9`'s finding was precisely that C and C++
hang the parameter list off the declarator chain rather than the function
node, so C is the sibling of the case that broke, tested only by proxy.

### 1.4 Three packages are reachable only from tests

| Path | Lines | Only importer |
|---|---|---|
| `py_agent/src/coderadar/lsp/` | ~400 | `tests/test_python_layer.py:482–533` |
| `py_agent/src/coderadar/agent/` | ~300 | `tests/test_python_layer.py:124` |
| `py_agent/src/coderadar/mutation/tool_router.py` | 215 | `mutation/__init__.py` re-export + `tests/test_python_layer.py:341–437` |

No production path reaches any of them. `tool_router.py` was already named in
`v0.7-improvement-plan.md` §4 ("either wire it to the real facade or delete it
and the tests") and was the one §4 item not resolved. `lsp/` is documented in
the README's resolution cascade as **L5, "optional, disabled by default"** —
so its status is at least honest; `agent/` and `tool_router.py` have no such
cover.

This is the same call §3 and §4 of the v0.7 plan made twice already (~100 inert
config knobs, ~4,300 lines of dead code, the `mutations` CLI command): **wire
it or cut it.** Tests that exercise only a package's `graph=None` path are what
kept all three alive.

### 1.5 No CHANGELOG

`git log` and the README's highlights sections are doing that job. The project
publishes to PyPI as `coderadar-rs`; the four smoke-pass commits between
0.6.43 and 0.6.47 changed user-visible CLI behaviour (`mutations` removed,
`export`/`update` exit codes, visualizers now failing where they used to draw)
and a consumer has no single place to read that.

---

## 2. Deferred by the plans, still deferred

Carried forward verbatim in intent; re-verified as still open.

### 2.1 Cold-start from the Macrame ledger — Phase 3B

*(`v0.7-improvement-plan.md` "out of scope"; `v1-readiness-plan.md`
"Explicitly Deferred"; `traversal-matrix.md` §3)*

[`lib.rs:1772`](../core_indexer/src/lib.rs#L1772) `export_snapshot` and
[`lib.rs:1781`](../core_indexer/src/lib.rs#L1781) `load_snapshot` both raise
`PyNotImplementedError`; [`__init__.py:767`](../py_agent/src/coderadar/__init__.py#L767)
`load()` does the same. The honesty pass (matrix step A) is done — they fail
loudly and the CLI exits 1. The feature is not.

**This is now the highest-value open item**, because §1.2 made it load-bearing:
every session re-analyzes from source, and every CLI command pays for it.

### 2.2 `as_of` upstream and bidirectional traversal

*(`v1-readiness-plan.md` §2.5)*

[`lib.rs:1397`](../core_indexer/src/lib.rs#L1397) raises
`PyNotImplementedError` for `direction` in `{"in", "both"}` with an `as_of`
timestamp — Macrame's `TraversalBuilder` is out-edge-only. Downstream `as_of`
works and is round-trip tested. The limitation is honest and covered by
`test_as_of_upstream_and_both_rejected`; it is a real hole in the temporal
claim nonetheless.

### 2.3 Arg-span indexing for the `update_signature` call-site cascade

*(`v1-readiness-plan.md` "Explicitly Deferred")*

Mitigated for v1 by §2.4's `unverified_sites` warning, and further by v0.7
§0.1's byte-span verification. Still the reason `update_signature` cannot
rewrite its own call sites.

### 2.4 Smell-metric approximations

*(`v1-readiness-plan.md` "Explicitly Deferred")*

Substring cyclomatic counting and base/call-derived CBO stay as-is;
field-type-aware CBO deferred. Now covered by golden snapshots
(`v1-readiness-plan.md` §3.2), so the approximations are pinned rather than
drifting.

### 2.5 Full `tsconfig.json` / `pyproject.toml` path-map parsing

*(`v1-readiness-plan.md` §2.2, "Defer: post-v1")*

`find_module_by_dotted_name` normalizes `@/...` and `~/...` → `src/...` by
convention. Projects with a non-conventional path map resolve by suffix match
or not at all.

### 2.6 Four edge kinds are asserted; nine exist

*(`traversal-matrix.md` §1.1, §2)*

[`storage.rs:40`](../core_indexer/src/storage.rs#L40) declares `CONTAINS`,
`CALLS`, `IMPORTS`, `EXTENDS`, `IMPLEMENTS`, `REFERENCES`, `DECORATES`,
`INSTANTIATES`, `OVERRIDES`. `ALL_EDGE_KINDS`
([`lib.rs:1349`](../core_indexer/src/lib.rs#L1349)) is `["calls", "extends",
"imports", "overrides"]` — the four the resolve backfill populates.
`IMPLEMENTS` is the notable absence: interface implementation is folded into
`extends`, so Java/Go/TypeScript interface relationships are indistinguishable
from class inheritance in a traversal.

### 2.7 Stack Graphs / L1 resolution

*(`v0.7-improvement-plan.md` "out of scope")*

Correctly deferred; the placeholder module was deleted in Phase 4 and the
README documents the omission. Nothing to do — listed so it is not
rediscovered as a gap.

### 2.8 TypeScript throughput

*(`v0.7-improvement-plan.md` "out of scope" → `performance-roadmap.md`)*

Phase 2 removed the algorithmic cliffs (the O(F²) scans, the whole-projection
clone). Closing the remaining throughput gap to CodeGraph is a separate
programme.

### 2.9 SQLite PRAGMAs, string interning, columnar extraction

*(`v1-readiness-plan.md` "Explicitly Deferred" — "high-risk, explicitly
post-v1")*

---

## 3. Engineering hygiene

### 3.1 The lint backlog, and CI's `continue-on-error`

Measured on `dev` @ `93b82f9`:

| Check | Count |
|---|---|
| `cargo clippy --package core_indexer --all-targets` | **100 warnings** |
| `uvx ruff check py_agent/src tests` | **785 errors** (633 auto-fixable) |
| `cargo fmt --all --check` | **444 diffs** |

[`ci.yml:75`](../.github/workflows/ci.yml#L75) marks the whole lint job
`continue-on-error: true`. That was deliberate — v0.7 §5.1 shipped the lint leg
as advisory so it could land without a 1,300-item cleanup blocking it — but
§5.2's intent was to flip it to blocking once the backlog was cleared, and the
backlog was never cleared.

An advisory lint job that has never once been green is a job nobody reads.

### 3.2 The test-debt pattern behind all four smoke passes

`v0.7-improvement-plan.md` §"Cross-cutting test debt" named three gaps. All
three are closed. The pattern they were instances of is not:

> **Green tests were asserting broken behaviour.**

- 12 visualizer tests asserted the fabricated demo diagrams. One was named
  `test_graphviz_fallback_without_graph` and checked for `BaseModel`.
- `coderadar.cli` was imported by **nothing** in the suite before `acadb64`.
- MCP guidance strings — the text an agent trusts most — were never checked
  against the tool registry before `d269768`.
- `extract_parameters` was tested only against Python, so eight languages'
  parameters were dropped under a green suite.

Each pass found the same shape: a surface that produced *plausible output* and
exited 0. None of them crashed. The remaining question is which surfaces still
have that property and no test — see the v0.8 plan §4.

---

## 4. Not gaps

Recorded so they are not re-raised:

- **`graph.rs` split** — complete, all 16 steps.
- **`.coderadar.toml`** — wired (v0.7 §3); the store path contradiction, the
  dead `load_harness_config`, and the three-way embedding-model disagreement
  are all resolved to `BAAI/bge-small-en-v1.5` in one place.
- **Zero-vector embedding poisoning** — `dedup.py:_model_embed` now raises when
  `fastembed` is missing instead of storing zeros.
- **`search_entities` kind coverage** — now scans functions, classes,
  constants, modules, and imports with per-kind rank weights.
- **Dead code** — `flatbuffer.py`, `buffers.rs`, the five parallel `mcp/`
  tool modules, `update/diff.rs`, `update/patch.rs`, `executor.py`,
  `pipeline.py`, `walk_and_extract`, `stack_graph.rs`, `SmellRegistry`,
  `resolve_symbol`, and the duplicate `Watcher`/`as_of`/`callers_of`/
  `callees_of` definitions are all gone.
- **`mutations` CLI command** — removed rather than wired; `MutationLog` never
  existed.

---

## 5. Summary

| Bucket | Items | Blocking a 0.7.0 tag? |
|---|---|---|
| §1 smoke-check residue | 5 | No — all are scope decisions, none is a defect |
| §2 planned deferrals | 9 | No — deferred deliberately, honestly failing where user-visible |
| §3 hygiene | 2 | No |

Nothing in this list is a correctness defect on a documented path. The v0.7
plan's 26 tasks are closed, the v1-readiness plan's four phases are closed, the
graph split is complete, and the traversal matrix's step D shipped. The four
smoke passes converted ~10 latent defects into fixes and 46 new tests.

The list is what v0.8 is for.
