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
Fix: a shared ensure-config step — `analyze()` (and server startup)
loads `.coderadar.toml` excludes before walking; or `build_graph`
activates project config like `_activate` does. Battery anchors:
`cli/exclude-takes-effect`, `cli/exclude-cli-side-honored`.

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

### R2-4 — `remove_file` leaves search ghosts
`remove_file("pkg/mod.py")` reports 4 removals (module + 3 fns — correct)
but `search_entities("starred_alpha")` still returns the removed function.
Concept rows go; the search backend is never invalidated. Stale search →
stale plans → mutations against deleted code.
Fix: purge per-file entries from the search index on remove (and audit the
`update_file` rename path, which round-1 showed working — keep it green).
Battery anchor: `surface/remove-file-clears-search`.

### R2-6 — git revision errors swallowed (`.ok()` × 2)
`git_changed_files` binding: `Oid::from_str(s).ok()`; `changed_files_between`:
`repo.find_commit(oid).ok()`. A garbage OID degrades to `None` and prints
"No changed files" — indistinguishable from a genuinely empty diff, and
`--old HEAD~1` (rev syntax, not hex) silently diffs the wrong thing
(harness `git-diff-two-commits` only passes with full hex from
`git rev-parse`). Fix: surface unknown-revision as an error; optionally
resolve rev syntax via `revparse_single`. Battery anchor:
`cli/git-diff-bad-oid-errors`.

### R2-15 — CLI `query` table truncates every column to ~4 chars
`coderadar query functions` renders `com�`, `hel�`, `['.�` — names, ids,
and call lists chopped to 4–5 chars (non-tty width detection collapse).
The flagship query command's default output is unreadable when piped.
Fix: sane `max_width`/no-wrap defaults for piped output (or `--full`
flag); never truncate the `name`/`id` columns below usefulness; consider
a `--format json|tsv` machine mode (agents are the primary readers).
Battery anchor: `cli/query-basic`.

## 3. P3 papercuts (UX — batch into one polish pass)

- **R2-9** `traverse --edges bogus_kind` → silent "No results", rc 0.
  Validate edge kinds (calls/imports/extends/overrides + aliases) and error
  like query-syntax does. Anchor: `cli/traverse-bad-edge-kind-errors`.
- **R2-10** `codegraph_as_of` with garbage timestamp echoes it into a
  snapshot template with no validation (empty string IS validated — only
  garbage slips). Validate ISO-8601 at the boundary. Anchor:
  `mcp/as-of-bad-ts-signals`.
- **R2-11** shell `traverse`/`callers` print nothing on empty results —
  looks hung. Print the same honest "No …" lines the CLI uses.
- **R2-12** `diagnose --unresolved` shows counts per function but not WHICH
  targets (`run: 1` — which one?). List the target names.
- **R2-13** `[debug] macrame.traverse …` log lines pollute CLI/shell stdout
  (breaks piping/parsing). Logs → stderr, or gate debug behind a flag.
- **R2-14** `visualize --format bogus` silently ignored (renders as if
  valid). Error listing supported formats. Anchor:
  `cli/visualize-bad-format-errors` (now with correct VIZ_TYPE).

## 4. Carryover (still open from round 1)

- **Issue 9** (re-export resolution): `from app import combine` via
  `__init__` re-export still resolves `external::` on all legs. Follow
  `FromImport` chains transitively with a cycle guard (R1§9 Issue 9).
- **R1§6-13** (partial): exclusion-s
...[truncated 3010 chars]