# CodeRadar — P5: Commit-Driven Ingestion (DR-41)

> **Plan, not a landing.** P5 is the phase §5 of
> `temporal-code-architecture-implementation.md` sketched and deferred (Q-1). This document is the
> "own plan" that sketch said it needed. Nothing here is built.
>
> **Status:** drafted 2026-10-28, against `dev` at `9b9f698` (v0.14.0 released). DR-41 stays
> `deferred` until the maintainer answers §10.

---

## 0. Verification header (code truth at `9b9f698`)

Every claim below that constrains the design was read in the code, not inferred from the earlier
plan. The ones that changed the design are marked **→**.

| Claim | Where | Consequence |
|---|---|---|
| `ConceptUpsert.valid_from` is caller-supplied | Macrame `src/connection.rs:594` | **→** writing commit time needs **no** Macrame change |
| `recorded_at` is engine-stamped, floored at `MAX(recorded_at)`, guarded by `trg_concepts_monotonic_ra` | Macrame `src/connection.rs:1590-1593, 1754` | the learn-time axis cannot be forged, which is what makes INV-8 enforceable rather than aspirational |
| `reconstruct` / `reconstruct_on` fold on **recorded** time only | `storage.rs:1853-1856` ("the state fold stays the recorded-time primitive"), Macrame `src/connection.rs:3545,3570` | **→** §2: commit-time `valid_from` alone changes nothing about what today's reads answer |
| Macrame's valid-time surface is `AsOf{valid,recorded}` over edges + attribute hydration, **not** a state fold | Macrame `src/temporal/as_of.rs:18-48,156,290` | **→** a valid-axis `MaterializedState` does not exist to call; building one in CodeRadar is F-6/INV-6 |
| Extraction takes a `&str`, not a path | `extract/single_pass.rs:787` | **→** the walker feeds git blob bytes directly; no per-commit checkout |
| The analyze pipeline's unit is `FileTask { path, source, language }`, built from a filesystem walk | `lib.rs:794-836` | the seam P5 substitutes is one function, not the pipeline |
| Put-then-assert is a loop over `tasks` after projection, before the v2 flush | `lib.rs:983-1000` | INV-1 holds per commit with the same ordering, unchanged |
| `valid_from` for concepts is `now_iso8601()` at the flush | `graph/persistence.rs:107,499,556`, `lib.rs:1066` | the four call sites P5 must thread a stamp through |
| The stale sweep is gated on `reconciliation_safe && failures.is_empty() && concept_flush_succeeded` and takes the whole current id set | `lib.rs:1135-1143` | **→** per-commit ingest must sweep against the commit's **full tree**, while parsing only its delta |
| `git2` 0.21 is already a dependency; `fs/git.rs` has `changed_files_between`, `blame_file`, `is_worktree_clean` | `core_indexer/Cargo.toml:27`, `fs/git.rs:16-131` | the walker extends an existing module |
| `kv_put` / `kv_get` / `kv_scan` exist and are branch-global | Macrame `src/connection.rs:2673,2695,2785` | the per-store mode flag and the ingest high-water mark have a home outside the ledger |
| `fork_branch` exists and names P5 as its reason | `storage.rs:1982-1991` | the lineage half of P5 is already shipped |
| Blob exclusion is path-pattern based (`SECRET_BLOB_EXCLUDE_DEFAULTS`) | `graph/config.rs:39-54` | **→** H-16: a secret in an ordinary source file is excluded by nothing |
| Embeddings are a separate op, not part of analyze | `lib.rs:2535` (`compute_embeddings` asks), MCP `coderadar_compute_embeddings` | **→** ingest must not embed per commit; see §5 P5a.5 |

Measured on this repository the same day (`git rev-list`, `git log`):

| Quantity | Value |
|---|---|
| Commits, all refs / on `main` | 559 / 556 |
| Distinct indexable blob contents in all history | 2,134 |
| Add-or-modify `(commit, indexable file)` pairs on `main` | ~2,130 |
| Largest indexable blob | 236 KB (under both the 1 MiB watcher and 8 MiB blob caps) |
| Commit pairs sharing one committer **second** on `main` | 4 |
| Non-monotonic committer steps along first-parent | 0 |
| Historical text blobs holding secret-shaped content | 8 of 2,874 — **all false positives** (the scaffolding detector's own literals and one fixture; §10 Q-9) |
| Paths in all history matching `SECRET_BLOB_EXCLUDE_DEFAULTS` | 1 (`core_indexer/src/scaffold/secrets.rs` — a source file, not a secret) |

---

## 1. What P5 is, and the one thing it changes

Today `valid_from` is the moment CodeRadar ran, for every fact in the store. The code's own history
is not in the ledger; what is in the ledger is the history of CodeRadar's observations of it. That is
honest (INV-8, mismatch #3 of the philosophy note) and it is not what a caller asking "what did this
file look like at that commit" means.

P5 makes `valid_from` the commit's time and leaves `recorded_at` as the ingest stamp. That one change
is the whole semantic content of the phase. Everything else in this document exists because of
what that change implies for reads, for ordering, and for what a store then is.

**It is also the only route to the measured gap.** The 6,441 byte-less generations on the self-host
store describe content that exists in no blob, no payload and no working tree; `verify_history` reads
3.19% store-wide against 99.6% on a freshly written generation. A re-analyze fixes the present and
never the past. Git still has the bytes, so a commit-driven ingest can *reconstruct* that history
rather than merely stop extending the gap.

---

## 2. The decision the sketch did not contain

The §5 sketch says P5 makes `as_of_valid` and `as_of_recorded`/`reconstruct` "stop being the same
question asked twice", and that "backfilling an old branch becomes a legitimate retroactive
correction rather than a lie told in valid time". Both sentences are right about the model and
together they describe something that cannot be read back.

Here is why. Every temporal read CodeRadar ships — `as_of`, `source_refs_at`, `entities_at`,
`verify_history`, `changes_between`, `history_of`'s chain order, `reconstruct_on` — resolves through
`reconstruct`, which folds the transaction log on **`recorded_at`**. Macrame's valid-time axis is
reachable (`AsOf::valid_at`, `AsOf::bitemporal`) for edge queries and attribute hydration, but there
is no valid-axis state fold to call, and CodeRadar must not write one: F-6 forbids a second fold
implementation, INV-6 puts folds in the engine. So after P5 writes commit time into `valid_from`:

- **Reads keep answering on the recorded axis.** They answer "as CodeRadar learned it", in ingest
  order, exactly as before.
- If the ingest walks history forward, recorded order and commit order **coincide**, and every read
  is correct about commit time by accident rather than by construction.
- The moment anything is ingested out of order — backfilling an old branch, re-ingesting a range,
  adding a second remote's history — the two orders diverge and every existing read silently answers
  the wrong question. Not an error: a plausible wrong answer, which is the failure class this whole
  document set exists to refuse.

So the sketch's two sentences belong to two different phases:

**P5a — commit time, append-only.** Walk topologically, write commit-time `valid_from`, and make
*recorded order == commit topological order* an enforced invariant (INV-10, §4): an ingest refuses to
write a commit older than the store's high-water mark. Every shipped read is then correct with **no
read-path change at all**, and `valid_from` carries the honest stamp for the day a valid-axis reader
exists. Retroactive backfill is **refused**, not discouraged.

**P5b — retroactive correction.** Needs a valid-axis state fold in Macrame (`reconstruct` taking
`AsOf`, or an `as_of_valid` beside it). That is an upstream change on the Q-3 precedent — implement
where it belongs, not where it is convenient. Only once it exists can a store accept history out of
order, which is what makes backfilling a branch legitimate instead of a silent reordering.

P5a is shippable on today's engine. P5b is a Macrame rung. **Recommendation: adopt P5a, record P5b
with its trigger, and do not let P5a's refusal be softened into a warning** — a warning here buys a
store that answers plausibly and wrongly.

---

## 3. Decisions this plan settles

Each is a recommendation with its reason and its reversal cost, for the maintainer to accept or
reject in §10.

- **P5-D1 — Commit time goes in `valid_from`; `recorded_at` stays the engine's ingest stamp.** The
  axis is a parameter, not an engine change (§0). INV-8 is preserved by construction, since
  `recorded_at` cannot be forged.

- **P5-D2 — The clock is the *committer* timestamp, with author time recorded beside it.** Author
  time is arbitrary with respect to this history: a rebased or cherry-picked commit carries an author
  time older than its own parent's, which would make `valid_from` non-monotonic along the branch and
  break P3's chain order (H-15). Committer time is at least the moment the commit object entered this
  history. The honest reading of the resulting axis is therefore *"when this content entered this
  branch's history"*, which is what a code-archaeology question actually means, and it is not
  *"when the author wrote it"*. Author time, author and committer identities go in the commit
  concept's `extra` (numbers and strings, no digests — P3's payload discipline), so a later reader
  can choose differently without a re-ingest. Reversal cost: one re-ingest into a fresh store.

- **P5-D3 — P5a enforces append-only ingest; P5b is a separate, upstream-gated phase.** §2.

- **P5-D4 — Migration is a fresh store, not an in-place conversion.** F-7 makes mixed stamping
  per-store illegal, so the choice is re-ingest or freeze. Re-ingest, because: the existing store's
  history is index-now stamped and cannot be retrofitted; the H-11 id respelling is baked into it,
  while a git walk re-mints canonical ids cleanly from the start; and a store that can be rebuilt from
  git is *derived data*, which today's store is not. Keep the old store as frozen history if the
  3-week observation record is wanted — it is the only copy of what CodeRadar believed before this.
  Stated costs: embeddings must be recomputed (§5 P5a.5); the old store's blobs are not copied (the
  walk re-puts them from git, content-addressed, so nothing is duplicated); anything that pinned a
  concept id from the old store must be re-pinned.

- **P5-D5 — The per-store stamping mode and the ingest high-water mark live in Macrame's
  `kv_store`.** `coderadar.stamping_mode` ∈ {`index-now`, `commit-time`} is written once at store
  creation and read at open; a mode mismatch refuses the operation rather than mixing stamps (F-7).
  `coderadar.ingest.head.{lineage}` holds the last ingested commit per lineage: it is both INV-10's
  comparison point and what makes an interrupted ingest resumable. `kv` is branch-global and outside
  the ledger, which is correct here and only here — **no blob reference may ever live in `kv`**
  (INV-3): the archive's reference scan reads ledger payloads and would reclaim bytes a `kv`-only
  reference depends on.

- **P5-D6 — Per commit, in this order:** blob puts for the commit's added/modified indexable files,
  then one flush carrying module/entity concepts, spine upserts and edges, all stamped
  `valid_from` = that commit's committer time. Put-then-assert per commit, unchanged from
  `lib.rs:983-1000` (INV-1). Deletions close the module's content; the spine is never retired
  (INV-9).

- **P5-D7 — Walk first-parent, diff against the first parent, ingest only the delta.** A merge whose
  tree equals its first parent's writes nothing (so merges do not double-count a content); a merge
  that resolved conflicts differs from its first parent and *is* ingested, which is the only way its
  resolution enters history at all. Second-parent commits are reached when their own branch is
  ingested, forked at the merge-base via `fork_branch`.

- **P5-D8 — A commit that vanishes from git (rebase, force push, shallow clone) changes nothing in
  the ledger.** The ledger keeps what it was told; that is what bitemporality is for. The ingest
  report counts such commits (`ghost_commits`) so the divergence is visible rather than inferred.
  The alternative — retracting them — would be CodeRadar deciding that something it recorded was
  never true, which is a lie of a different kind and, for blobs, would want a deleter that INV-5
  forbids.

- **P5-D9 — Parse quality gates deletions, never bytes.** Historical commits contain broken code far
  more often than a working tree does. A commit whose files do not all parse cleanly still gets its
  blob puts and its concepts; it must not run the stale sweep (`lib.rs:1135`'s existing gate, applied
  per commit). Bytes are the thing P5 exists to recover, and they are recoverable from a file that
  does not parse.

---

## 4. Invariants P5 adds

Inherits INV-1..INV-9 unchanged. Adds:

- **INV-10 — Recorded order equals commit topological order, per lineage.** The enforcement point is
  the ingest itself: before writing commit *C* on lineage *L*, compare *C* against
  `coderadar.ingest.head.{L}`; if *C* is not a descendant of it, **refuse**. Without this, the
  shipped reads (recorded-axis) answer a question nobody asked (§2). This invariant is what P5b
  removes, and nothing else may remove it.

- **INV-11 — Stamps strictly increase along a lineage.** Git committer time is second-resolution and
  ties happen (4 pairs on this repo, §0). P3's chain order and Macrame's interval guards both want
  strictly increasing stamps. The walker therefore advances a tied or regressing stamp by the
  smallest representable step and **records that it did** (`stamp_nudged: true` in the commit
  concept's `extra`), so a reader can tell a nudged stamp from a measured one. Silently coercing the
  clock would make the axis unreliable in exactly the way this phase is supposed to fix.

- **INV-12 — One store, one stamping mode, declared.** P5-D5's `kv` flag, checked at open. F-7 as a
  mechanism rather than a sentence in a document.

---

## 5. Phases

### P5a.1 — The walker (no writes)

**Goal.** Turn a repository into the ingest's input, with nothing persisted, so the input can be
tested on its own.

**Required.** In `fs/git.rs` beside `changed_files_between`:

```rust
pub struct CommitPlan {
    pub sha: String,
    pub committer_ts: String,   // RFC3339, the valid_from source (P5-D2)
    pub author_ts: String,      // metadata only
    pub parents: Vec<String>,
    pub changed: Vec<PathBuf>,  // added/modified, indexable, blob-eligible
    pub deleted: Vec<PathBuf>,
    pub tree: Vec<PathBuf>,     // the full indexable tree: the sweep universe (§0)
}

pub fn plan_ingest(repo: &str, branch: &str, from: Option<&str>) -> Result<Vec<CommitPlan>, GitError>;
pub fn blob_bytes(repo: &str, sha: &str, path: &Path) -> Result<Vec<u8>, GitError>;
```

`plan_ingest` walks first-parent, oldest-first, from `from` (the `kv` high-water mark) to the branch
tip, and returns commits in topological order. Language and blob-exclusion filtering happen here, so
the ingest never sees a path it must not store.

**Tests.** Fixture repos built with `git2` (the module already does this —
`fs/git.rs:131-198`): a linear history; a merge with no tree change against its first parent (plan
is empty for it); a conflict-resolving merge (plan is non-empty); a rename; a delete; two commits in
one second (ties reach the caller, un-nudged — nudging is the writer's job); a commit touching a
`SECRET_BLOB_EXCLUDE_DEFAULTS` path (absent from `changed`, present in `tree`).

**Done when** the plan for this repository's `main` first-parent chain is 354 commits with
1,696 changed `(commit, file)` pairs (measured 2026-10-29: `plan_ingest(repo, "main", None)`),
and `blob_bytes` round-trips a known blob byte-for-byte (verified against `git show` for
`pyproject.toml` at the `main` tip). The §0 table's 556 commits / ~2,130 pairs count all commits
reachable from `main`, including merged side-branch history; that history is not missing — P5-D7
ingests second-parent commits through their own branches (P5a.4), so the per-branch plans union to
the full reachable set.

### P5a.2 — Stamped writes

**Goal.** Let the existing write path take a stamp instead of reading the clock.

**Required.** Thread an explicit `valid_from` through the four `now_iso8601()` sites
(`graph/persistence.rs:107,499,556`; `lib.rs:1066`) — as a parameter with the current behaviour as
its default, not a global. `write_branch_content` (`storage.rs:2018`) gains the same parameter, since
P5 is its first non-`now` caller. The spine helpers already take a timestamp.

**Hazard this closes before it opens.** P4 recorded "spine `valid_from` is the flush stamp … chain
order relies on strictly increasing flush stamps, which the write path guarantees by construction".
P5 removes that construction. INV-11's nudge is the replacement, and it belongs here, at the one
place that writes a stamp.

**Tests.** Rust: a stamped flush lands the given `valid_from`; `history_of` over three
commit-stamped generations returns them in commit order; a tie is nudged, flagged, and still
strictly increasing; a regressing stamp is nudged rather than written backwards. Python: unchanged
behaviour when no stamp is passed (the whole of today's suite is this test).

### P5a.3 — The ingest op

**Goal.** One op that takes a repository and a lineage and writes history.

**Required.** `ops.ingest_history(repo, branch="main", from=None, dry_run=False)`, on the P1/P2
report-shape-as-contract rule (Q-4), with CLI `coderadar ingest-history` and **no MCP tool in the
first release** — it is a long, writing, destructive-by-migration operation, and an agent calling it
without knowing that is the H-13 shape of mistake. Per commit: INV-10 check → blob puts →
extraction from `blob_bytes` via `extract_single_pass` → one flush at the commit's stamp → stale
sweep scoped to `CommitPlan.tree`, gated per P5-D9 → `kv` high-water advance. Report:
`commits_ingested`, `commits_skipped` (with reasons), `files_parsed`, `blobs_stored`,
`blobs_deduped`, `stamps_nudged`, `ghost_commits`, `elapsed`.

**The write-amplification question this phase must answer with a number, not a guess.** A full
analyze is one flush; this is 556. Macrame's own measurements put a chunked bulk write in the
low milliseconds with the two ledger triggers at ~92% of it, and the edge path grows with a source's
out-degree. 556 flushes is therefore the cost centre, not the 2,130 parses. Measure before
optimising, and if it must be optimised, the lever is chunking commits per flush — which trades
INV-11's per-commit stamp granularity and must not be taken silently.

**Tests.** Fixture repo end-to-end: three commits, then `reconstruct` at each commit's timestamp
returns that commit's state; `history_of` returns the chain in commit order; `verify_history` reports
~100% coverage for the ingested range (the sketch's own done-when); an interrupted ingest resumes
from `kv` and is idempotent; an out-of-order commit is refused with a typed error; a store in
`index-now` mode refuses the op (INV-12).

**Status (2026-10-29): shipped on `dev` as `graph/ingest.rs` + `ops.ingest_history` + CLI
`ingest-history` (no MCP tool).** As-built deviations from the above, each read off the code:

- *Commit concepts are minted.* P5-D2's metadata (author time, both identities, `stamp_nudged`)
  needed a home, so each commit mints `commit:{sha}` (`kind = 'commit'`, trunk lineage): no
  file-path annotation (the stale sweep passes over it), no edges (P3's lesson), excluded from
  `live_concept_ids` / `all_concept_ids` and from the cold-start classifier exactly like the
  spine and the pointers.
- *Reads take recorded stamps.* `reconstruct` at a commit's *commit* stamp is empty — nothing
  was recorded then. Commit stamps are data carried in `valid_from`; the read index is the
  recorded axis, where the Nth generation in order is the Nth commit's completed state. Each
  commit's writes mint sub-generations (concept bulk, edge persists, sweep retires — the same
  shape as analyze), so per-commit recorded boundaries come from incremental runs; the tests
  fuse the done-when with the resume test that way. This is the plan's §2 "correct by
  accident" made precise: correct by construction of the walk order, read through recorded
  generations.
- *Any git branch may be walked; the lineage written is always the trunk.* INV-10 confines
  what the trunk accepts (a diverged branch fails closed), so no fork machinery was needed.
  Non-trunk lineages — fork, branch-minted concepts, branch deletion semantics — stay P5a.4's.
- *Standard excludes apply* on top of the walker's filtering, with the same matcher analyze
  walks with (a committed `node_modules` tree is not history worth keeping either).
- *Write amplification, measured:* this repository's `main` first-parent chain ingests
  354 commits / 1,696 files in 72 s (~0.2 s/commit, 4,107 generations ≈ 12 ledger writes per
  commit). No optimisation for a one-time migration.
- *INV-12 closes both directions:* the index-now write paths (`analyze`, `update_file`,
  `remove_file`) refuse commit-time stores, so a later reindex cannot mix clock stamps into
  ingested history.

### P5a.4 — Branch ingestion

**Goal.** More than one lineage, forked where git forked.

**Required.** Per git branch: `fork_branch(branch, parent)` at the merge-base, recorded once, with
the mapping in `kv`. Then P5a.3 against that lineage, from the merge-base forward. Divergent file
content goes through `write_branch_content` with the commit's stamp (P5a.2). Deletion semantics on a
branch, per-branch sweep scoping and branch-minted *new-file* module concepts are the three things
P4 explicitly left to P5 — they land here, not earlier.

**Tests.** Two lineages diverging from one base: each reads its own content at its own commit times;
the trunk never sees the branch's; `verify_history --branch` reports per-lineage coverage; the
shared-content case stores one blob row.

**Status (2026-10-29): shipped on `dev` as `ingest_branch_history` + `lineage`/`parent_ref`
through `ops.ingest_history`, CLI `--lineage`/`--parent-ref` (no MCP tool).** As-built
deviations and findings, each read off the code:

- *The lineage defaults to the git branch name; only an explicit trunk claim refuses.*
  `branch="feature"` without a lineage ingests onto the `feature` lineage (the only sane
  reading now that lineages exist); `lineage="main"` with a non-main git branch refuses
  (`TrunkBranchMismatch`). P5a.3's "walk any branch into the trunk" permissiveness is
  tightened — a trunk walk of branch content would mint trunk entity concepts the branch
  ingest can never restate (same id, D-225).
- *One flush, one generation per commit.* Pointer and tombstone upserts join the commit's
  single bulk instead of writing immediately (P4's instruments write at once); the re-point
  no-op gate comes free via `filter_unchanged`. Mid-commit sub-generations still exist on
  the trunk (blob/edge/sweep writes); the branch has none beyond the single bulk.
- *A failed concept flush halts, on both lineages.* P5a.3 continued past a bulk abort and
  counted the commit — but a chunked bulk abort is a partial write, so counting it bakes
  the gap in. Halting leaves the cursor behind the commit for a retry (ghost-halt
  precedent). Branch cursors advance stamp-before-head, so a crash between the two
  re-ingests one commit instead of skipping one.
- *The fork scan is newest-shared-point with a self-equality arm.* `graph_descendant_of`
  is strict (a commit is not its own descendant), so the first version slid the fork a
  commit too old and re-ingested the fork commit onto the branch until D-225 refused it
  mid-bulk — the e2e caught it, and the halt above is what made the catch loud instead
  of silent.
- *No cascade, no edges on branches.* The running projection accumulates own-file diffs
  only (INV-10's append-only property means no cross-run diff memory is needed — the
  same reason the trunk starts empty); relations need cross-lineage edge semantics that
  do not exist, so they are deferred, not smuggled.
- *The merge trap is defused with `cross-lineage-id`.* A trunk ingest meeting a
  branch-born id skips its concepts (bytes and spine land; the skip is auditable, never
  sweep-gating) instead of aborting the bulk at the trigger. The entities live on the
  mint lineage permanently — until the D-214 overlay exists, the trunk can never mint
  those ids. Operational consequence: ingest branches before merging them, and expect
  branch-born files to read on their lineage.
- *`history_of` stays lineage-blind; `verify_history --branch` needed no changes.*
  Branch content appears in the file's digest chain (the spine is content-addressed and
  lineage-agnostic); lineage-scoped chains need lineage-tagged spine (future). Coverage
  scopes through the existing visible unions + branch generations.
- *Two latent trunk hazards fixed in the sweep/retire choke point:* sweep candidates are
  mint-lineage rows (a trunk sweep would otherwise retire branch-minted modules on sight),
  and the retire upsert round-trips the row's mint lineage (retiring a branch row through
  a trunk upsert trips the cross-lineage trigger on its own id).

### P5a.5 — Migration, and the embedding bill

**Goal.** Make the switch an operation with a stated cost, not a surprise.

**Required.** `coderadar migrate-to-commit-time` is a *new store* (P5-D4): create, set
`stamping_mode=commit-time`, ingest `main`, then each branch, then **one** `compute_embeddings` pass
over the final state. Embeddings are already a separate op (§0) and must stay that way here:
embedding every historical generation would be the dominant cost of the entire project and would buy
a vector index over text nobody searches. The old store is left untouched; the docs say plainly that
it is the only record of what CodeRadar believed before the migration.

**Done when** a migrated store answers "what did this file look like at commit X on branch Y" with
bytes, `verify_history` reports at or near 100% for the ingested range, and the pre-migration store
still opens.

**Status (2026-10-29): shipped on `dev` as `ops.migrate_to_commit_time` + CLI
`migrate-to-commit-time` (no MCP tool).** As-built deviations, each read off the code:

- *The op is orchestration, not engine.* Branch enumeration is the one new Rust
  surface (`fs/git.rs:list_branches` + FFI — sorted short names; HEAD is not a
  branch entry so there is nothing to exclude, which the first version got wrong
  by filtering `is_head` and dropping the checked-out branch). Everything else
  reuses shipped ops: per-branch `ingest_history`, `coderadar.load` for the final
  state, the existing `compute_embeddings`, the existing per-branch
  `verify_history`. A migration that needed new engine would be a sign the
  phases below it were incomplete; it needed one 20-line walker.
- *Defaults with the trunk prepended, not just defaulted.* No `branches` means
  every local branch, trunk first; an explicit list without the trunk gains it
  (reported in `branches`, so the prepend is auditable, not silent) — forks
  compute against the parent ref and branch reads fall back to trunk rows, so a
  migration without trunk data is hollow by construction. A repository with no
  trunk refuses outright.
- *Idempotent instead of fresh-only.* The plan said "a new store"; the op does
  not enforce it — every branch ingest resumes from its `kv` cursor, so a
  re-run finishes a migration (completed branches no-op, the embeddings pass
  and the verification run again). The per-branch guards already make re-entry
  safe (index-now stores refuse per branch, foreign histories fail the cursor
  checks), and idempotency is what makes a failed embeddings pass recoverable:
  fix the backend and re-run. A fresh-path check would trade that recovery for
  a rule the guards already enforce.
- *The embeddings pass is skippable but never per-commit.* `embed=False` opts
  out (branch-unique content then stays unembedded — the one pass covers the
  trunk-tip final state only, H-19's cost reasoning applied to branches too);
  a failed pass fails the migration *after* the history is safely ingested.
- *The loaded projection is the union, so branch rows flow through writers
  that assumed the trunk.* The final state contains branch-minted entities
  (their vectors persist fine), but the embedding-*hash* flush restated those
  rows on the trunk lineage until D-225 refused them — fail-open `[diag]`,
  hashes never landed. The third choke-point fix of the project:
  `fetch_mint_lineages` (bulk, chunked like its companion) and the flush rides
  each row's mint lineage, pinned by
  `embedding_hash_flush_carries_the_mint_lineage`. (Retire in P5a.4, sweep
  candidates in P5a.4, hash flush now — every writer that restates rows it
  did not mint must carry lineage. The next one should be found by audit,
  not by e2e.)
- *The old-store test asserts logic, not bytes.* The first version hashed the db
  file; WAL mode moves bytes on checkpoint (closing a handle checkpoints), so
  the assertion caught lifecycle, not writes. The guarantee is the same
  generations and the same concepts, plus the old store still opens and
  answers.

  Self-host evidence (2026-10-29, scratch store, branches `main` + `dev`):
  362 commits, 0 skipped, 0 failed; 7,232 vectors, 0 errors; verify 100.0%
  on both lineages (4,331 + 128 generations); 1,741 s wall; ~900 MB store
  (WAL included) vs ~100 MB byte-less. `history_of` head matches `git show`
  bytes. The other 18 local branches were not migrated (stale experiments).

### P5b — Retroactive correction (upstream-gated)

**Blocked by** a valid-axis state fold in Macrame. **Unblocks** backfilling an old branch, ingesting
a second remote, and re-ingesting a range after a parser improvement — all of which P5a refuses.
Shape: `AsOf`-taking `reconstruct` upstream (Macrame's `AsOf` already carries both axes), then
`axis=` on CodeRadar's temporal reads with `recorded` as the default, so no existing caller's
question changes meaning. Not scheduled; the trigger is the first real need for an out-of-order
ingest.

---

## 6. Hazard register (continues §6 of the implementation plan)

| # | Hazard | Where | Bites in |
|---|---|---|---|
| H-14 | **The reads do not move with the axis.** Commit-time `valid_from` with recorded-axis folds means out-of-order ingest answers plausibly and wrongly, with no error anywhere | `storage.rs:1853-1856`; §2 | all of P5a — INV-10 is the only guard |
| H-15 | **Git committer time is second-resolution and ties.** Measured: 4 tied pairs on `main`. P3's chain order and P4's "strictly increasing flush stamps" both assumed a clock nobody could tie | §0 measurements; P4's as-built note | P5a.2; silent coercion would corrupt the axis |
| H-16 | **A commit-driven ingest permanently stores every committed byte of every indexable file**, including secrets a later commit scrubbed. `blob_exclude` is path-pattern based, so a hardcoded key in an ordinary `.py` is excluded by nothing. Git has them too, but git has history rewriting and the store has INV-5 (no CodeRadar-side deleter) | `graph/config.rs:39-54`; philosophy mismatch #7 | P5a.1 filtering, and the migration decision — the store becomes as sensitive as the repository, forever |
| H-17 | **Cap pairing multiplies per commit.** The 1 MiB watcher vs 8 MiB blob caps are paired by documentation only (H-6); P5 multiplies the exposure by commit count | `storage.rs:1601` + comment | P5a.3 volume |
| H-18 | **Historical commits parse worse than working trees.** WIP commits, conflict markers and pre-formatter code are normal in history. Deletions gated on clean parses (P5-D9) is correct; a sweep that is *not* so gated would retire half a tree on one broken commit | `lib.rs:1135-1143` | P5a.3 |
| H-19 | **Embedding per generation would dominate everything.** Macrame measures the embedding chunk at ~135 µs/vector, the worst bulk path in the system; 556 generations × a whole projection is a different order of cost than the ingest itself | Macrame measurement note (D-057); `lib.rs:2535` | P5a.5 — one pass over final state, never per commit |
| H-20 | **The high-water mark is outside the ledger.** `kv` is not covered by the archive's reference scan or by the ledger's guards. That is correct for a mode flag and a cursor, and it means a lost `kv` row makes an ingest *repeat* work (idempotent, recoverable) — but it must never hold anything a blob's survival depends on (INV-3) | P5-D5; Macrame `src/connection.rs:2673` | P5a.3 resumption |

---

## 7. Test plan

New files: `tests/test_ingest_history.py` (op shape, refusals, resumption, idempotence, report
contract), `tests/test_commit_stamps.py` (axis: stamps land, ties nudge, chain order follows commit
order), plus Rust pins in `fs/git.rs` (plan shapes) and `graph/persistence.rs` (stamped flush).

Must not regress: the whole of today's suite under the default stamp (P5a.2's parameter is
defaulted), `tests/test_content_spine.py` (chain order now comes from commit stamps),
`tests/test_branch_reads.py` (P5a.4 adds writers to a read-tested surface),
`tests/test_retention.py` (every new concept's payload must still name no digest), and the two
`verify_history` fixtures, which are the before-picture this phase is measured against.

The one test that would have caught §2 if it had existed earlier: ingest two commits **out of
order** and assert the op **refuses**. An implementation that writes them and lets the reads sort it
out passes every other test in this file.

---

## 8. Decision-register rows

| ID | Decision | Status |
|---|---|---|
| DR-41 | Commit-driven ingestion: commit time in `valid_from`, append-only per lineage (P5a); retroactive correction upstream-gated (P5b) | **deferred** — plan drafted 2026-10-28, awaiting §10 |
| DR-43 | *(proposed)* Valid-axis state fold in Macrame, on the Q-3 precedent: implement upstream, because F-6/INV-6 forbid a second fold in CodeRadar | proposed, blocked on a real need |

---

## 9. Sequencing

| Order | Phase | Blocked by |
|---|---|---|
| 1 | P5a.1 walker | — |
| 2 | P5a.2 stamped writes | — (independent of 1; both are prerequisites, neither needs the other) |
| 3 | P5a.3 ingest op | 1, 2 |
| 4 | P5a.4 branch ingestion | 3 |
| 5 | P5a.5 migration + embeddings | 4 |
| — | P5b | Macrame DR-43 |

P5a.1 and P5a.2 can proceed in parallel; everything after is a chain.

---

## 10. Open questions for the maintainer

- **Q-6 — Adopt P5a?** It is a semantic change to what the store means, it costs a migration, and
  §2's refusal (INV-10) is a real constraint on future ingests, not a formality. The alternative is
  to keep index-now stamping and accept that history means "what CodeRadar saw", documented — which
  is what v0.14 ships and what `verify_history` now measures honestly.
- **Q-7 — Committer time, or author time with a monotonicity repair?** P5-D2 chooses committer. A
  maintainer who wants "when the author wrote it" is choosing a non-monotonic axis and therefore
  P5b's machinery. Decide before P5a.2, because it is the stamp the spine chain is built on.
- **Q-8 — Does the pre-migration store stay?** P5-D4 keeps it as frozen history. It is 100 MB and it
  is the only record of the pre-P5 belief, including the 0.01% baseline that started all of this.
- **Q-9 — H-16: is a permanent, undeletable copy of every committed byte acceptable?** A policy
  question, not an engineering one. Mismatch #7 of the philosophy note is still open and P5 is what
  makes it load-bearing.

  **The exposure is measurable even though the policy is not, so it was measured** (2026-10-29, all
  2,874 distinct text blobs in this repository's history, twelve secret shapes: AWS access-key ids,
  GitHub classic/OAuth/fine-grained PATs, `sk-` keys, Slack tokens, private-key blocks, Google API
  keys, and `api_key`/`secret_key`/`password`/`access_token` assignment forms). **Eight blobs match,
  and all eight are false positives:** seven versions of `core_indexer/src/scaffold/mod.rs` plus
  `core_indexer/src/scaffold/secrets.rs` — the scaffolding detector's own pattern literals — and
  `tests/fixtures/python/scaffold/vibe.py`, a deliberate fixture. No real secret has been committed
  to this repository. So for *this* store, option A below costs nothing today; the question is about
  future commits and about any other repository CodeRadar is pointed at.

  Options, with what each costs:

  | | Shape | Impact |
  |---|---|---|
  | **A** | Accept as planned: path-pattern exclusion only | Full coverage, simplest build; the store becomes as sensitive as the repository, permanently. Zero measured marginal risk here |
  | **B** | Content-scan each blob before the put, skip and record | Needs a design that does not exist; false negatives are inherent; and a skipped blob is a coverage gap `verify_history` cannot tell from a bug. The scan above ran at a 100% false-positive rate, which is the honest preview of this option's cost |
  | **C** | Bounded window: ingest from a reviewed commit forward | Caps exposure to a range that was looked at, and caps recovery the same way — anything older stays byte-less. Cheap: it is P5a.1's `from` argument, so it is a per-run choice, not a design commitment |
  | **D** | Decline P5a | No migration and no new exposure; the 3.19% stays 3.19% permanently, honestly reported |

  Recommendation: **A** for this repository, **C** as the default for any repository whose history
  has not been vetted.

  One finding from the same scan, which belongs to the *present* path and not only to P5:
  `core_indexer/src/scaffold/secrets.rs` matches `*secret*` under gitignore basename semantics
  (`storage.rs:382-392`), so its bytes are skipped today. Harmless, and exactly the trade DR-30
  chose — but it means byte coverage on this repository cannot reach 100% while that file exists,
  which P5a.5's done-when has to allow for (§11).
- **Q-10 — Does `ingest-history` get an MCP tool later?** P5a.3 says no for the first release.
  Q-4's reasoning (an agent should be able to ask what it can trust) argues for exposing the
  *report*, not the write.

---

## 11. Done when

A store built by `ingest-history` answers "what did this file look like at commit X on branch Y" with
the bytes git has, `verify_history` reports at or near 100% coverage for the ingested range, both
axes mean what they say (`recorded_at` = ingest, `valid_from` = commit, never blurred), and the op
refuses out-of-order history instead of accepting it.

"At or near" is load-bearing and has a known floor: every path excluded by
`SECRET_BLOB_EXCLUDE_DEFAULTS` is byte-less by design, which on this repository is one legitimate
source file (§0, §10 Q-9). The done-when is therefore *coverage equals the non-excluded tree*, and a
`SkippedExcluded` count beside the percentage is what makes the difference auditable rather than
assumed.
