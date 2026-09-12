# Road to v0.9.0 — Round-2 dogfood findings + improvement plan

Second self-review of CodeRadar, this time at v0.8.16 (commit `3f8b1a0`,
post fmt-clean). Round 1 (see `docs/dogfood-review-2026-09.md`) went deep
on a few subsystems; round 2 went **wide**: every CLI command (22) and
every MCP tool (22) — happy path, flags, and error paths — driven by a new
harness against a purpose-built fixture.

- Harness: `tests/cr_edit_tests/battery_round2.py` → `battery_round2_output.txt`
- Fixture: `tests/cr_edit_tests/r2proj/` (multi-language: Python re-export
  chain, star-export package, clone pair, smelly function, TS class, Rust
  fns, excludable `ignored/`)
- Result: **104/118 green**. The 14 reds are the findings below (each is a
  real product behavior, reproduced; harness bugs were fixed during the
  run, not hidden).

Prior open items referenced as `R1§6-N` (round-1 report §6) and `R1§9-N`
(round-1 report §9, issues 1–9). Issue 8 is fixed (v0.8.16); **Issue 9
(re-export resolution) re-confirmed open** by `surface/issue9-reexport-resolves`.

## 0. What round 2 verified as healthy

All 22 CLI commands and all 22 MCP tools invoke cleanly end-to-end
(`mcp serve` stdio probe lists exactly 22 tools; `tools/call` works).
Error paths are good far more often than not: bad ids, missing files,
bad queries, bad timestamps (empty), unknown modules, policy violations
all signal clearly in ~90% of cases. Specifically healthy: `init`/`--force`,
`analyze`, `status`, `stats`, `rebuild` (+`--full`, one-shot `--exclude`),
`update` (+missing-file error), `store-repair` (+bad-db error),
`load-snapshot`, `blame` (+missing-file error), `git-diff` happy path,
`exclude list/add/remove`, `shell` REPL, `watch` (reacts to edits),
`diagnose` flags, MCP mutations incl. dry-runs, stale-hash guard, policy
gate, synthetic-edge survival across `update_file` (v0.8.16 fix holds),
star exports, canonical ids on the mutation boundary, one-shot
`analyze(exclude=[...])`, multi-language indexing (Python/TS/Rust).

## 1. P1 findings (correctness — fix before v0.9.0)

### R2-1 — External callees invisible through index APIs
`run()` calls `combine()`, which resolves to `external::combine`
(re-export chain, cf. Issue 9). The edge **exists** (`g.query` shows
`resolved_call_targets: ['external::combine']`), but `callees_of(run)`,
`CodeGraph.callees_of`, and `MacrameQuery.callees_of` all return `[]`.
Root cause: `entity_ref_to_dict` (`core_indexer/src/lib.rs:443`) returns
`None` for ids with no concept row (`external::*`, builtins), and every
index API silently drops `None`s. The traverse docstring even documents
this as incidental ("naturally filtered").
Victims: CLI `callees` prints "No callees" (a lie — there is one),
`visualize call-graph` refuses to render, MCP `explore`/`node`
neighbor lists under-report. An agent asking "what does run call?"
gets told "nothing".
Fix: materialize minimal dicts for non-concept targets
(`{id, name, kind: "external"|"builtin"}`) instead of dropping them —
in `callees_of`/`callers_of`/`traverse` bindings. Battery anchor:
`surface/external-callee-visible`.

### R2-2 — Synthetic edges re-persisted as structural CALLS; rebuild never retracts
v0.8.16 tracks `synthetic_edges` apart in memory, but
`persist_edges_scoped` (`core_indexer/src/graph/persistence.rs:79`)
iterates the **whole** `callees_by_caller` map — synthetic pairs included —
and asserts every FK-safe pair as kind `"CALLS"`. A registered synthetic
pair therefore gains a second ledger life as a structural edge, and the
v0.8.16 kind separation ends at the process boundary.
Verified end-to-end in-battery: register synthetic `run→combine`, no-op
`update_file`, then fresh-process `analyze` reports 4 call edges while
fresh-process ledger `load` reports **5** (`surface/load-analyze-edge-parity`).
Second half: a full CLI `rebuild` of a 3-edge tree reported 4 edges —
vanished edges are never retracted from the ledger, so pollution
accumulates across persist/load cycles and `call_edges` inflates.
Fix: skip pairs present in `projection.synthetic_edges` in
`persist_edges_scoped`/`persist_edges` (the set exists now — this is the
follow-up the v0.8.16 commit message anticipated); the register path's own
synthetic-kind rows stay the sole record. Add retraction of ledger edges
absent from the fresh projection on full rebuild, or document append-only.

### R2-7 — `[project] exclude` is CLI-only; library `analyze()` ignores the toml (and can resurrect exclusions into the store)
`coderadar.analyze(path)` passes only the explicit `exclude=` param to Rust
(`__init__.py:757`) — it never reads `.coderadar.toml`, despite the
docstring promising a merge with `[project] exclude`. Fresh-process probe:
toml excludes `ignored/`, bare `analyze()` still finds the excluded symbol
(`cli/exclude-takes-effect`, `EXCLUDED_HITS=1`); one-shot
`analyze(exclude=[...])` works (0 hits), CLI honors the toml (rebuild
retires concepts, query → "No results"). Every library/MCP-server path
that calls `analyze`/`build_graph` without `exclude=` (i.e. the served
graph itself) silently over-indexes user-excluded paths.
Worse, the paths interact: `analyze` auto-attaches an existing store
("the re-index persists"), so a library `analyze()` on an initialized
project writes the excluded concepts **back into the ledger**, undoing
`exclude add` + `rebuild` for later readers. In-battery sequence
`exclude add → rebuild → library-analyze-probe → query` finds the excluded
symbol, while the probe-free manual sequence does not.
Fix: `analyze()` prepends the toml's `[project] exclude` to the explicit
`exclude=` list before walking (toml-first, mirroring
`exclusion_gitignore`'s config → extra → baseline order, so one-shot `!`
negations keep working). Deliberately NOT a full `activate_config`: that
would also enforce `[mutation] allow` (and roots/embedding keys) on
library flows — bare-library renames outside src/lib/tests/scripts
started failing where the CLI/server had always gated them (verified:
`app/helpers.py` rename rejected under activation). Full activation stays
with CLI `_activate` / MCP `_set_project`; `load()` needs nothing (it
walks nothing, and post-load passes only attach to graph entities).
Battery anchors: `cli/exclude-takes-effect`,
`cli/exclude-cli-side-honored`.

### R2-8 — `git-clean` reports dirty for ignored artifacts (always dirty on real projects)
`is_worktree_clean` (`core_indexer/src/fs/git.rs:85`) uses
`repo.statuses(None)` with default options, which counts **ignored** files.
Minimal repro: fresh repo → clean; `init` (writes `.gitignore` ignoring
`.coderadar/`) → commit → `git-clean` says "Worktree has uncommitted
changes" while `git status` is clean; `/tmp` repro shows the trigger is an
ignored dir with files in it. Since every initialized project has an
ignored `.coderadar/store/*.db`, `git-clean` is unusable exactly where it
matters. Sibling contradiction: `git_changed_files(repo,None,None)` on the
same repo returns `[]`.
Fix: pass explicit `StatusOptions` matching `git status` semantics
(no ignored), i.e. the `git status --porcelain` contract. Battery anchor:
`cli/git-clean-when-clean`.

### R2-16 — CLI id arguments skip F14 canonicalization; unknown vs empty indistinguishable
`visualize call-graph ./main.py::main` (slash form) → "No call edges…",
while `.\main.py::main` renders `main --> run`. Same for
`callers`/`callees`/`traverse`/`query` id args: the F14 canonical form
(`canonicalize_target`, `canonical_entity_id`) is applied at the mutation
boundary and some readers but not to CLI-supplied ids. Anyone pasting a
POSIX-style id (or any non-canonical spelling) gets silent emptiness —
and the output is identical for "entity unknown" vs "entity has no edges",
so there is no way to tell a typo from a true negative.
Fix: canonicalize CLI id args through the F14 helpers; print
"unknown entity" vs "no edges" distinctly. Battery anchors:
`cli/visualize-slash-id-works`; extend to callers/callees/traverse.

## 2. P2 findings (extraction, search, git, display)

### R2-3 — `new X()` constructor calls extracted in no language
`makeStore()` (`return new Store()`) has **zero** call targets — not even
`external::`/`unresolved`. No `.scm` query in any language captures
`new_expression`, and `emit_call_for_node` only handles
call/method-invocation shapes. Affects TS/JS/Java/C#/C++/all OO languages:
constructor edges (the backbone of "who instantiates this class") are
missing graph-wide. (Round-1's RTA-lite ctor tests cover Python
`Derived(...)` — same gap likely applies to other construction syntaxes;
re-check Python `super().__init__` chains while here.)
Fix: add `(new_expression constructor: … @call.name) @call` patterns per
language + constructor-field mapping in `emit_call_for_node`; resolve
`new Store()` to the class constructor like Python `Constructor` targets.
Battery anchor: `surface/ts-new-expression-captured`.
DONE v0.8.20 (extraction half): `new_expression` (TS/JS, `constructor`
field) + `object_creation_expression` (Java/C#, `type` field) +
`new_expression` (C++, `type` field) patterns, `constructor`/`type`
fields and `type_identifier` names in `emit_call_for_node`, per-language
regression test (5 legs). Unresolved class names fall back to
`external::Store` -- visible via R2-1, honest until constructor-target
resolution exists (deferred: even Python doesn't resolve `Derived(...)`
to `Constructor` today; needs cross-language class matching). Scoped
`new pkg.Store()` (Java `scoped_type_identifier`) stays uncovered.

### R2-4 — `remove_file` leaves search ghosts (CORRECTED v0.8.20: battery misreading)
`remove_file("pkg/mod.py")` reports 4 removals and the function IS gone
from every map -- search scans the live projection, there is no separate
index to invalidate. The remaining `hits=1` is `pkg/__init__.py`'s
`from .mod import starred_alpha` IMPORT entity, whose raw statement text
still names it: a legitimate reference hit, not a ghost. Battery anchor
corrected to assert zero `function`-kind hits.
Real hardening underneath: the `file_to_modules`-miss fallback collected
only functions+classes by a bare normalized prefix that can never match
canonical ids -- constants, aliases, imports and the module itself would
have survived had the branch ever run. It now tries the canonical prefix
too and collects across all six entity maps (regression test:
`remove_file_fallback_clears_every_kind_map`).

### R2-6 — git revision errors swallowed (`.ok()` × 2)
`git_changed_files` binding: `Oid::from_str(s).ok()`; `changed_files_between`:
`repo.find_commit(oid).ok()`. A garbage OID degrades to `None` and prints
"No changed files" — indistinguishable from a genuinely empty diff, and
`--old HEAD~1` (rev syntax, not hex) silently diffs the wrong thing
(harness `git-diff-two-commits` only passes with full hex from
`git rev-parse`). Fix: surface unknown-revision as an error; optionally
resolve rev syntax via `revparse_single`. Battery anchor:
`cli/git-diff-bad-oid-errors`.
DONE v0.8.20: `changed_files_between` takes revision strings and resolves
via `revparse_single` (hex, HEAD, HEAD~1, branches, tags) with a new
`GitError::UnknownRevision`; the CLI prints the error and exits 1.
Side effect, also fixed: `--new`'s documented HEAD default was
unimplemented (None always diffed empty) -- now honored.

### R2-15 — CLI `query` table truncates every column to ~4 chars
`coderadar query functions` renders `com�`, `hel�`, `['.�` — names, ids,
and call lists chopped to 4–5 chars (non-tty width detection collapse).
The flagship query command's default output is unreadable when piped.
Fix: sane `max_width`/no-wrap defaults for piped output (or `--full`
flag); never truncate the `name`/`id` columns below usefulness; consider
a `--format json|tsv` machine mode (agents are the primary readers).
Battery anchor: `cli/query-basic`.
DONE v0.8.20: identity columns `no_wrap`, everything else folds, pipes
DONE v0.8.20: identity columns `no_wrap`, everything else folds, pipes
render at width 250 (a pipe has no width; tty keeps auto-detect), plus
`query --format json` (soft-wrapped, parse-verified). Enabling it
required a slice of R2-13: `_ensure_graph` diagnostics (cold-start note,
fallback note) moved to stderr so stdout stays machine-readable; the
Rust `[debug]` lines remain for the v0.8.21 P3 batch.

## 3. P3 papercuts (UX — batch into one polish pass)

- **R2-9** `traverse --edges bogus_kind` → silent "No results", rc 0.
  Validate edge kinds (calls/imports/extends/overrides + aliases) and error
  like query-syntax does. Anchor: `cli/traverse-bad-edge-kind-errors`.
  DONE v0.8.21: pure `validate_edge_kinds` in the core binding (both
  `traverse` legs) + CLI `Traversal error: …` exit 1.
- **R2-10** `codegraph_as_of` with garbage timestamp echoes it into a
  snapshot template with no validation (empty string IS validated — only
  garbage slips). Validate ISO-8601 at the boundary. Anchor:
  `mcp/as-of-bad-ts-signals`.
  DONE v0.8.21: `datetime.fromisoformat` gate in `_as_of` returns
  `Invalid timestamp … expected ISO 8601`.
- **R2-11** shell `traverse`/`callers` print nothing on empty results —
  looks hung. Print the same honest "No …" lines the CLI uses.
  DONE v0.8.21: shell prints `No results` (query/traverse) and
  `No callers found for <id>` (callers).
- **R2-12** `diagnose --unresolved` shows counts per function but not WHICH
  targets (`run: 1` — which one?). List the target names.
  DONE v0.8.21: new `unresolved_targets` core binding (same
  Unresolved+External-not-Builtin population, dotted member spellings)
  and `diagnose --unresolved` lists names per function — attributing each
  function's OWN targets (the old neighborhood-count listed callers for
  their callees' gaps).
- **R2-13** `[debug] macrame.traverse …` log lines pollute CLI/shell stdout
  (breaks piping/parsing). Logs → stderr, or gate debug behind a flag.
  DONE v0.8.21: structlog configured once in `coderadar/__init__.py` —
  stderr sink, WARNING default, `CODERADAR_DEBUG=1` restores DEBUG.
  Every entry path (CLI, MCP, library, background) inherits it.
- **R2-14** `visualize --format bogus` silently ignored (renders as if
  valid). Error listing supported formats. Anchor:
  `cli/visualize-bad-format-errors` (now with correct VIZ_TYPE).
  DONE v0.8.21: CLI rejects anything but mermaid/graphviz/dot with
  `Unknown format: … (supported: …)` exit 1.

## 4. Carryover (still open from round 1)

- **Issue 9** (re-export resolution): `from app import combine` via
  `__init__` re-export still resolves `external::` on all legs. Follow
  `FromImport` chains transitively with a cycle guard (R1§9 Issue 9).
  DONE v0.8.21: `find_symbol_in_module` follows `FromImport`/`Relative`/
  star re-export chains transitively (direct definitions win, later
  imports shadow earlier, `visited` set stops A↔B cycles, module-bound
  names stop without resurrecting shadows). `run → helpers::combine` on
  every leg; two regression tests (chain resolves, cycle terminates).
- **R2-17** (new, v0.9.0 carryover — rename through re-export chains):
  found while closing Issue 9. `rename helpers::combine → combine_r2`
  rewrites the definition and the call site but NOT the import bindings
  (`from app import combine` in main.py, `from .helpers import combine`
  in `__init__`), leaving a broken tree; rename-back then cannot restore
  the call site (it no longer resolves to the entity). Full fix = rewrite
  import bindings along the re-export chain on rename. Repro: battery
  `mcp/rename-real` + `mcp/rename-back` round-trip on the r2proj chain;
  the harness now snapshots/restores the fixture and re-analyzes, with a
  `mcp/fixture-restored-after-rename` guard.
  DONE v0.8.23 (three layers): (1) plan — `collect_import_binding_edits`
  rewrites every confirmed `from X import <old>` binding (tree-sitter span
  location: first identifier after `import`/`export` not preceded by `as`;
  star imports need nothing; `__all__` strings stay for review), hooked
  into function + class rename; (2) scoped-update import refresh —
  `apply_diff_update` re-inserted imports by ID presence only, and import
  ids are line-stable, so same-line binding edits never landed (plus a
  missing dedupe on the per-unit membership push accumulated duplicate
  import entries, N-identical-edits-at-one-span corrupted files on apply);
  (3) `CodeGraph.apply` runs a second `update_file` pass over multi-file
  plans so importers re-resolve against settled sources. Full round-trip
  verified live: rename → mem `helpers::combine_r2`, rename-back → mem
  `helpers::combine`, disk byte-identical. 4 rename tests + 2 projection
  tests.
- **R1§6-13** (partial): exclusion-system follow-ups not yet covered by
  R2-7 — e.g. `exclude stats` stack output, watcher respect for excludes.
  DONE v0.8.24: `exclude list` shows all three layers (baseline + config +
  `.gitignore` patterns, comments skipped) plus engine effect totals
  (`N file(s), E excluded, N-E indexed` on the live tree); watcher
  exclusion proven by direct bridge test (synthetic debouncer events:
  excluded path dropped, normal path arrives) + a defaults test pinning
  every `DEFAULT_EXCLUDES` entry in the watch config.
- **R1§6-14/15/17**: placeholder-scan stats follow-ups, heartbeat tuning
  (`CODERADAR_INDEX_HEARTBEAT` default), bridge-decorator production-root
  list maintenance.
  DONE v0.8.25: slop footer anchored in battery (`scaffolding-shows-scan-stats`
  — stats survive even on clean trees); env knobs hardened (`_env_seconds`:
  garbage falls back with a note instead of crashing import, sub-minimum
  clamps, 4 tests); bridge maintenance with teeth — sibling `attribute_item`
  capture fixed (outer attributes are siblings of `function_item`, so
  `#[pyfunction]` never reached decorators and the bridge rule never fired)
  + `pymodule` added to `BRIDGE_DECORATORS` (with the `pyclass` non-listing
  documented). Live: bridge fns carry decorators and survive dead-code at
  0.0 confidence while plain privates still report.
- **R1§9 Issues 1–7**: closed in round 1; their regression tests stay green
  (verified in the v0.8.16 CI triage).

## 5. Sequencing (v0.8.17 → v0.9.0)

One fix = +0.0.1, each verified by its battery anchor flipping red→green
plus the standard gates (`cargo test`, `maturin develop`, `--version`,
relevant pytest files, full battery before merge):

| Release | Content | Battery anchors |
|---|---|---|
| v0.8.17 | R2-1 external-callee visibility | `surface/external-callee-visible`, `surface/run-callees-include-combine-or-external` |
| v0.8.18 | R2-2 synthetic persist skip + rebuild retraction | `surface/load-analyze-edge-parity` |
| v0.8.19 | R2-7 toml excludes on library path + R2-8 git-clean + R2-16 CLI canonicalization | `cli/exclude-takes-effect`, `cli/git-clean-when-clean`, `cli/visualize-slash-id-works` |
| v0.8.20 | R2-3 `new`-expression extraction + R2-4 remove_file search purge + R2-6 git OID errors + R2-15 table widths | `surface/ts-new-expression-captured`, `surface/remove-file-clears-search`, `cli/git-diff-bad-oid-errors`, `cli/query-basic` |
| v0.8.21 | P3 batch (R2-9…R2-14) + Issue 9 re-export chains | `cli/traverse-bad-edge-kind-errors`, `mcp/as-of-bad-ts-signals`, `surface/issue9-reexport-resolves` |
| v0.8.23 | R2-17 rename-chain rewrite (plan + scoped-update refresh + resolve fixpoint) | `mcp/rename-real-applied`, `mcp/fixture-restored-after-rename`, `surface/issue9-reexport-resolves` |
| v0.8.24 | R1§6-13 exclusion follow-ups (list layers + effect totals, watcher proof) | `cli/exclude-list-effect-totals` |
| v0.8.25 | R1§6-14/15/17 hygiene (slop footer anchor, knob hardening, bridge list) | `mcp/scaffolding-shows-scan-stats` |
| v0.9.0 | Carryover (§4) + full battery green + release notes | everything → 121/121 DONE |

Order inside Phase 1 is dependency-driven: R2-1 first (presentation-only,
no schema/ledger risk), R2-2 second (ledger semantics — do before R2-7's
resurrection interaction complicates testing), then R2-7/R2-8/R2-16
(independent, batchable).

## 6. Acceptance (v0.9.0 exits when…)

> **Status at v0.9.0: all met.** `battery_round2.py` **121/121** (118 +
> `fixture-restored-after-rename`, `exclude-list-effect-totals`,
> `scaffolding-shows-scan-stats`); round-1 batteries green; Rust 368 +
> Python 751 green; clippy advisory clean of new warnings (pre-existing
> toolchain drift untouched); §§1–4 DONE-marked (§7 log carries the
> release tags); the five invariants hold (verified live in §7 entries).

- `battery_round2.py` fully green (118/118) on a clean checkout.
- Round-1 batteries (`battery_readonly/mutation/slop`) still green.
- Full Rust suite + Python suite green, clippy advisory clean.
- `docs/road_to_v0.9.0.md` §§1–4 all marked DONE with release tags.
- No `external::` target silently dropped by any index API; no synthetic
  pair persisted as CALLS; excludes honored on every entry path;
  `git-clean` agrees with `git status`; CLI ids canonicalized.

## 7. Progress log

- **v0.8.17 — R2-1 DONE.** `unresolved_ref_to_dict` fallback in
  `callees_of`/`callers_of`/`traverse` (+as_of leg); builtin vs external
  split via shared `is_python_builtin`; CLI callers/callees print
  `(external)` for file-less targets. Anchors
  `surface/external-callee-visible` +
  `surface/run-callees-include-combine-or-external` flipped PASS in the
  v0.8.17 battery run (104/118 → 105/118). Side effect: R2-14
  (`cli/visualize-bad-format-errors`) newly detected — it was spuriously
  green via the empty-render nonzero-exit path and now correctly red.
- **v0.8.18 — R2-2 DONE.** `persist_edges_scoped` skips `synthetic_edges`
  pairs (their ledger life stays under the synthetic kind); new
  `retire_stale_edges` + `stale_edge_keep_set` close open structural edges
  absent from a fresh full index (bitemporal close, `as_of` intact),
  hooked into `analyze_inner` under the panicked-workers guard with a
  `[coderadar] retired N stale edge(s)` report. The battery anchor was
  corrected along the way: load == analyze + session synthetics (the +1
  is durable-by-design restore, not pollution) — strict equality could
  never hold once a synthetic is registered.
- **v0.8.19 — R2-7 + R2-8 + R2-16 DONE.** `analyze()` prepends the toml's
  `[project] exclude` to the walk (narrow fix -- a first-cut full
  activation leaked `[mutation] allow` enforcement into library flows and
  broke bare-library renames; reverted to excludes-only), so
  library/MCP/background paths honor excludes and previously-persisted
  exclusions heal via the existing retraction; `is_worktree_clean` spells out `git status`
  semantics (untracked counts, ignored never does — libgit2 defaults
  report IGNORED entries); read-path ids go through `canonical_lookup_id`
  (F14 heads converge, `external::*`/names pass through) in
  `callers_of`/`callees_of`/`traverse`/`lookup_entity`, and CLI
  callers/callees/traverse/visualize print `Unknown entity` distinctly
  from true empties.
- **v0.8.20 — R2-3 + R2-4 + R2-6 + R2-15 DONE.** `new`-expression
  extraction in TS/JS/Java/C#/C++ (unresolved names fall back to
  `external::`; constructor-target resolution deferred); `remove_file`
  fallback hardened (canonical prefix, all six maps) with the battery
  anchor corrected to the real contract (the import-reference hit is
  legitimate); git revisions resolve strictly via revparse (`HEAD~1`
  works, `--new` HEAD default honored, unknown revs error); query tables
  never crop piped output (wide pipe console, no-wrap ids) +
  `--format json`, with CLI diagnostics moved to stderr.
- **v0.8.21 — R2-9…R2-14 + Issue 9 DONE.** Edge-kind validation
  (`validate_edge_kinds`, both traverse legs, CLI exit 1); as-of
  ISO-8601 gate; shell empty-result lines; `unresolved_targets` binding +
  per-function target names in `diagnose --unresolved` (own-target
  attribution, not neighborhood counts); structlog→stderr /
  WARNING-by-default (`CODERADAR_DEBUG=1` restores DEBUG);
  visualize-format rejection; transitive `FromImport` re-export chains
  with cycle guard (`run → helpers::combine` everywhere, 2 regression
  tests). Battery 114/118 → **119/119**: the three red anchors flipped,
  but closing Issue 9 exposed two harness truths, both fixed in-harness —
  (a) the rename round-trip is lossy through re-export chains (filed as
  R2-17: import bindings are not rewritten, so rename-back cannot restore
  the call site; the battery now snapshots/restores the fixture +
  re-analyzes with a `fixture-restored-after-rename` guard), and
  (b) the C4 synthetic pair (RUN, COMB) union-collided with the newly-real
  CALL of the same pair, costing the parity probe its +1 — C4 now
  registers the novel (COMB, RUN) pair; the R2-1 `external-callee-visible`
  anchor repointed from `run` (no external callees left, correctly) to
  `makeStore → external::Store`. 359 Rust + 746 Python green.
- **v0.8.23 — R2-17 DONE.** Import-binding rewrites in the rename planner
  (function + class paths, alias-aware, shadow-safe via
  `find_symbol_in_module`); scoped-update import refresh (line-stable ids
  always replace) + membership dedupe; second `update_file` pass over
  multi-file plans in `CodeGraph.apply`. Live round-trip: rename → mem
  `helpers::combine_r2`, rename-back → mem `helpers::combine`, disk
  byte-identical. 365 Rust + 746 Python green; battery_round2 119/119.
- **v0.8.24 — R1§6-13 DONE.** `exclude list` gains the `.gitignore`
  pattern layer and engine effect totals on the live tree; watcher
  exclusion covered by a direct EventBridge test + a defaults-vs-baseline
  pinning test. 367 Rust + 747 Python green; battery_round2 120/120
  (new `cli/exclude-list-effect-totals` anchor).
- **v0.8.25 — R1§6-14/15/17 DONE.** Sibling-attribute capture in the
  extractor + `pymodule` in the bridge list (1 extractor test, F7 entry
  test extended); `_env_seconds` knob hardening (4 tests, defaults
  unchanged: wait 25 s, heartbeat 5 s); scaffolding footer battery anchor.
  368 Rust + 751 Python green; battery_round2 121/121.
- **v0.9.0 — RELEASE + Issue 5 fold-in.** `docs/v0.9.0-release-notes.md` (changelog v0.8.16→,
  behavior changes, verification totals, deferred list); version cut
  0.8.25 → 0.9.0 across `__init__.py` + `pyproject.toml` + `Cargo.toml` +
  README. All §6 acceptance met: round2 121/121, round-1 green, 368 Rust
  + 751 Python, §§1–4 DONE. Post-cut: Issue 5 folded in with no version
  change — ID-grammar block in README + shared grammar line on the eight
  id-taking tool descriptions (+ kind/strictness value lists); `kind`
  validated at `search_entities` + MCP `_search` (unknown kinds error);
  `tests/test_entity_id_grammar.py` (6 tests: spelling agreement,
  external passthrough, refusal, mirror).
- **v0.8.22 — macrame-db 0.15 → 0.17 DONE** (off-plan dep bump). The 0.16
  cycle's one caller-visible break was the W15.3 `#[non_exhaustive]` wave:
  four literal sites in `cold_start.rs` moved to constructors
  (`NodeAttributes::new`, `MaterializedState::empty` + pub field
  assignment; `EdgeBelief::new`/`ConceptUpsert` builder already
  constructor-based, `DbError` never matched exhaustively). libsql stays
  0.9.30 — 0.17.0 still pins it. Schema v15→v19 rungs climb on open;
  ledger round-trip counts identical (5 edges/16 fns on r2proj). 359 Rust
  + 746 Python green; battery_round2 119/119.