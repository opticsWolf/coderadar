# CodeRadar — P5b and beyond: one coherent bitemporal code store

> **Plan, not a landing.** Nothing here is built. Drafted 2026-10-31 against CodeRadar `dev` at
> `c1f1ec8` (P5a, P6a and P6c built, unreleased; Macrame pin `0.19.1`) and Macrame `main` at
> `634740f` (`v0.20.0`; `dev/0.21.0` holds an unstarted 0.21.0 plan).
>
> **Scope.** Everything after P5a that the temporal architecture needs in order to mean what it
> says: reads on the axis the store writes, a write discipline the current engine accepts, branches
> and merges that behave like git's, the uncommitted working tree, and the boundary between data
> rebuilt from git and data only the store holds. Migration of the existing store is out of scope
> by request; §8 notes the one place where ordering still matters.
>
> **Supersedes** the P5b paragraph of `temporal-p5-commit-ingestion.md` §5, which framed P5b as
> "retroactive correction, triggered by the first real need". §1 shows P5b is the read-side
> completion of P5a, needed by every commit-time store.

---

## 0. Verification header (code truth at `c1f1ec8` / `v0.20.0`)

Every claim that constrains the design was read in the code today. **→** marks the ones that
changed the plan.

| Claim | Where | Consequence |
|---|---|---|
| Every CodeRadar temporal read resolves through `reconstruct_on(ts, branch)`, a recorded-axis fold | `core_indexer/src/lib.rs:1892,2691,3256`; `storage.rs:2287-2300` | **→** F1 |
| No test reads a commit-time store by commit time; they read at generation (recorded) instants | `tests/test_ingest_history.py:142,146`; `tests/test_migrate_commit_time.py:96-97` | **→** F1 is untested, not disproven |
| Every concept write opens an interval: `valid_to(TS_OPEN)` | `graph/ingest.rs:1568,1767`; `annotations.rs:1040,1141`; `graph/persistence.rs` (all `*_at` writers) | **→** F2 |
| Unchanged concepts are not rewritten (`filter_unchanged` ignores `valid_from`/`valid_to`) | `storage.rs:690-720` | only *changed* concepts open a new start, which keeps F2's cost bounded |
| Macrame ≥ 0.19.2 refuses a new open start while the previous one is open (`ConceptSingleOpen`); the caller must close first, in a separate call, because two versions of one concept cannot share a call (monotonic `recorded_at` trigger) | Macrame `tests/concept_interval_guard_tests.rs:188-230`; register D-291 | **→** F2 |
| A lineage forks at a **recorded** instant: `forked_at` is the clock at `fork()`, visibility is `recorded_at <= cutoff`, and a fork cannot precede its parent's | Macrame `src/graph/lineage.rs:49-80`; `src/branch.rs:547-587` | **→** F3 |
| A branch ingest treats the parent's whole visible state at the fork as inherited | `graph/ingest.rs:1059-1072` (`inherited` = visible union minus own), `:836-860` | **→** F3 |
| Concept identity belongs to the minting lineage; a branch-minted id can never be minted by the trunk | Macrame D-214, D-225; P5 plan §5 P5a.4 as-built ("merge trap") | **→** F4 |
| Macrame has no merge or promote operation | Macrame `src/connection.rs` (no `merge*`/`promote*` API) | **→** F4 needs upstream work |
| A commit-time store refuses `analyze`, `update_file`, `remove_file` | `lib.rs:283-300` (`store_is_commit_time`), `:784-792` | **→** F5 |
| `reconstruct_as_of(at: AsOf)` exists and takes **no lineage**; `reconstruct_on` does not resolve concepts by lineage | Macrame `src/connection.rs:3641`, `reconstruct_on` rustdoc | **→** U2 |
| 0.20.0 known defect: a backfill overwrites the live `concepts` projection | Macrame `README.md:334` at `v0.20.0` | **→** U1: 0.21.0 is the floor |
| 0.21.0 proposes "a concept's state is its open belief, else its latest-starting belief" (D-303) and needs the maintainer's decision | Macrame `docs/Macrame Update Plan v0.21.0.md` §2.4 | **→** D-1 |
| `migrate_to_commit_time` builds only from git and never reads the old store | `py_agent/src/coderadar/ops.py:1776-1900` | annotations and comments are not carried (§6 P8) |
| Annotations and comments are stamped by the clock, trunk-global, mode-independent | `annotations.rs:1017-1043`; `annotations-comments-proposal.md` §7 | they remain the only non-derivable data (§2 A-9) |

---

## 1. Five findings that shape everything

**F1 — P5a's commit times cannot be queried.** P5a writes commit time into `valid_from`, but every
read folds on `recorded_at`, and in a commit-time store every `recorded_at` is the day of the
ingest. `as_of("2026-09-15")` on a store ingested on 2026-10-29 therefore folds an empty prefix of
the ledger, whatever the commits say. INV-10 makes recorded *order* equal commit order; it does not
make recorded *instants* equal commit instants, and the reads take instants. What works today is
what never takes a commit instant: `history_of` (chain order), `verify_history` (generations), and
reads at a generation stamp a caller has no way to find from a sha. So P5b is not an optional
backfill feature. It is the half of P5a that makes the commit-time axis readable.

**F2 — The Macrame pin cannot move without a write-discipline change.** From 0.19.2, Macrame
refuses a concept upsert that opens a later start while an earlier one is still open, and tells the
caller to close the first start before opening the next. CodeRadar opens every version with
`valid_to = TS_OPEN` and never closes one. On 0.20.0, the second version of every changed module,
entity, spine row, pointer and annotation would be refused. Closing must also be its own call,
because two versions of one concept cannot share a call. A per-commit flush becomes two ordered
flushes, with a window between them in which the concept is closed and its successor not yet open.

**F3 — A branch can inherit trunk content its git branch never had.** Macrame cuts a fork by
recorded time, at the instant `fork()` runs. Git forks at a commit. The migration ingests the whole
trunk and then forks each branch, so every trunk row is recorded before the cutoff, and the branch
ingest classifies the trunk's whole visible state as inherited. A file the trunk added after the
merge-base therefore reads as present on the branch, and a file the trunk changed after the
merge-base reads with the trunk's newer content wherever the branch never touched it. Read from the
code, not yet shown by a test; P5b.0 writes that test first.

**F4 — Merges cannot be represented.** A concept id belongs to the lineage that minted it. If a
branch is ingested before it merges, which is the normal order once ingestion is live, every file
born on that branch is minted on the branch. When the merge commit reaches the trunk, the trunk may
not mint those ids (D-225), so the trunk's graph never contains the merged files. P5a.4 avoids this
during migration by ingesting the trunk first and documents it as "ingest branches before merging
them". Ongoing ingestion cannot rely on that order.

**F5 — A commit-time store cannot see the working tree.** INV-12 makes `analyze`, `update_file`
and `remove_file` refuse a commit-time store, so the watcher path is closed and uncommitted edits
are invisible. For a code-intelligence tool whose main caller is an agent editing code, that is the
primary use case, and the plan has no story for it.

Two upstream facts complete the picture. Macrame 0.20.0 has the valid-axis fold but no
lineage-resolved form of it, and it carries a backfill defect whose fix (0.21.0, D-303) waits on a
decision.

---

## 2. The target architecture

The stable shape every phase in §6 converges on. Each principle names the finding it answers.

**A-1. Two axes, one meaning each, everywhere.** *Valid* time is git's: the committer time of the
commit that made a fact true on a lineage. *Recorded* time is CodeRadar's: when it learned the
fact. No row, read or report uses either axis to mean the other. (INV-8, unchanged, now enforced
by the reads as well as the writes.)

**A-2. Reads name their axis and default to valid.** Every temporal read takes `at` (valid; the
user's question: "at this commit, on this date") and an optional `recorded` (the audit question:
"as CodeRadar believed then"). Omitting both is "now on both". The recorded-only form stays,
spelled explicitly. (F1)

**A-3. Commits are addresses.** `at` accepts a timestamp, a commit sha, or a ref. A sha resolves
through its commit concept to `(lineage, committer time)`. Users ask about commits, not instants.
(F1)

**A-4. Intervals are real.** A version of a concept is `[its start, the next version's start)`.
Writing a new version first closes the open one. Deleting a file closes its concepts' intervals at
the deleting commit. `retired` stays as a marker on the closing row, never as a second open row.
At most one interval per concept is open, and successive versions abut. (F2, and the shape D-291
already enforces)

**A-5. A lineage is a git branch, forked at its merge-base on the valid axis.** A branch inherits
its parent's state *as valid at the merge-base commit*, not as recorded when the fork ran. Commits
after the merge-base on the parent are invisible to the branch until a merge brings them in. (F3)

**A-6. Identity follows the file, and a merge is an operation.** When a merge commit brings
branch-born files into a lineage, that lineage asserts them under their canonical ids. A concept id
names a file or entity, never a lineage (Macrame D-214's own principle, extended to merges). (F4)

**A-7. The working tree is a view, never history.** Uncommitted state lives in an overlay above
the ledger state at the checked-out lineage's head. `analyze` and the watcher update the overlay;
nothing uncommitted is ever stamped into the ledger. Reads with no `at` see the overlay when one
exists. Temporal reads never do. (F5)

**A-8. Ingestion is driven by git, continuously.** A ref change (commit, merge, branch creation,
checkout) triggers an incremental ingest of the affected lineages and rebases the overlay. History
rewritten by a rebase or force push becomes a new generation of that lineage; the old one stays
readable. Nothing recorded is ever retracted (P5-D8).

**A-9. The store is git-derived, except for what people wrote into it.** Everything but
annotations and comments can be rebuilt from git. Those two kinds are the store's own data, with
export and import as first-class operations. Rebuilding is then a supported operation and the
answer to H-16: if a secret is ever committed, rewrite git, rebuild, re-import.

**A-10. One fold, in the engine.** CodeRadar never folds (INV-6, F-6). Every capability above that
needs a fold Macrame does not have becomes an upstream request (§4), not a CodeRadar workaround.

---

## 3. Decisions for the maintainer

Each has a recommendation. D-1 is a Macrame decision, recorded here because P5b depends on it.

| # | Decision | Options | Recommendation |
|---|---|---|---|
| **D-1** | Accept Macrame 0.21.0's state rule (D-303): *a concept's state is its open belief, else its latest-starting belief* | **a** accept (flips 0.20.0 gate 7 to the valid-consistent answer) · **b** keep "row written last" (leaves the backfill defect unfixable without the projection and the fold disagreeing) | **a.** It is the only rule under which a backfill — P5b's write — leaves current reads correct |
| **D-2** | How a deletion is written | **a** close the interval at the deleting commit, `retired: true` on that closing row · **b** today's separate retired row with its own start (refused from 0.19.2) | **a** (A-4) |
| **D-3** | Default axis for temporal reads | **a** valid (`at`), recorded opt-in · **b** recorded stays the default, valid opt-in | **a** (A-2). Users ask about commits and dates. **b** keeps F1 as the default answer |
| **D-4** | What a read with no `at` sees | **a** the working-tree overlay when present, else the head · **b** always the committed head | **a** (A-7). An agent mid-edit means the code in front of it |
| **D-5** | A rebased or force-pushed branch | **a** a new lineage generation (`feat` → `feat#2`, forked at its new merge-base; `feat` resolves to the newest) · **b** refuse until an operator acts · **c** retract the old commits | **a.** **b** stalls every rebase workflow; **c** contradicts P5-D8 |
| **D-6** | Which branches are ingested | **a** every local branch plus remote-tracking branches, with a deny-list · **b** local only (today) | **a**, with the deny-list defaulting to nothing |
| **D-7** | Release policy for the commit-time path until P5b.3 and P5d land | **a** ship `ingest-history` and `migrate-to-commit-time` marked experimental · **b** hold them out of releases · **c** ship as is | **a**, with the docs stating F1 and F5 in one sentence each |

---

## 4. Upstream requests (Macrame)

CodeRadar cannot build any of these (A-10). They are listed in the order P5b needs them.

**U-1 — 0.21.0's concept state rule (D-303).** Planned upstream; waits on D-1. It fixes the 0.20.0
backfill defect and is the minimum version CodeRadar adopts.

**U-2 — A lineage-resolved valid fold.** `reconstruct_as_of` takes no lineage, and `reconstruct_on`
does not resolve concepts by lineage. CodeRadar needs "state of lineage L, valid at V, as believed
at R" in one call: the valid-axis counterpart of D-259. Without it, every branch read on the valid
axis is either trunk-only or a CodeRadar-side fold (F-6).

**U-3 — Forks with a valid-axis cut.** A fork that records the instant at which the child stops
seeing its parent *on the valid axis* (the merge-base commit time), honoured by U-2's fold. Today's
recorded cut is correct for "a branch of beliefs" and wrong for "a branch of code" (F3). The two
cuts can coexist: recorded for what the branch could know, valid for what was true on it.

**U-4 — Merge.** An engine operation that lets a lineage assert concepts first minted on a lineage
it merges from, under the same id, with provenance recorded. This is the D-214 overlay the P5a.4
as-built names, given a write path. Without it A-6 is not buildable and F4 stands.

**U-5 — Supersede in one call (small; optional).** An upsert that closes the previous open start
of the same concept at its own `valid_from` and opens the new one, atomically. D-291 deliberately
refused the implicit form; this is the explicit opt-in. Without it, F2's two-flush window must be
handled on the CodeRadar side (P5b.1 does so). With it, one commit is one flush again.

Each is filed as its own request document in Macrame's `docs/`, in the shape of
`macrame-0.20.0-valid-axis-state-fold-request.md`, which worked: it became 0.20.0.

---

## 5. Invariants

Inherits INV-1..INV-14. INV-10 is replaced; the rest are new.

- **INV-10′ — Per lineage, valid intervals form a chain.** Replaces "recorded order equals commit
  order". Out-of-order ingest becomes legal once reads use the valid axis (P5b.3) and Macrame
  guards the chain (D-291 and U-1). INV-10 stays enforced until both are true. P5b.5 lifts it.
- **INV-15 — One open interval, abutting versions.** For every concept, at most one interval is
  open, and each version's `valid_to` equals its successor's `valid_from`. Checked after every
  ingest by `verify_concept_intervals`, which must report nothing.
- **INV-16 — Every temporal read names its axis.** Internally there is no "timestamp" parameter
  whose axis is implied. The public default is valid (D-3), and the recorded form is spelled.
- **INV-17 — The ledger holds only committed history.** Uncommitted content is never stamped.
  Annotations and comments are the exception, stamped present-tense by design (INV-14 programme).
- **INV-18 — Rebuildable from git plus exported user data.** A store rebuilt from the same
  repository and the same annotation/comment export answers every code read identically. Tested,
  not asserted.

---

## 6. Phases

Each phase lists what it delivers, what blocks it, and what proves it. The order is in §7.

### P5b.0 — Truth tests first (no dependency)

Write the tests that state F1, F3, F4 and F5 against a fixture repository: a read at a commit's
own time returns that commit's content; a branch does not see a file the trunk added after the
merge-base; after a merge the trunk sees the merged file; an uncommitted edit is visible to a
no-`at` read. Mark each `xfail` with the phase that will flip it. These are the programme's
acceptance gates. They also settle F3 by demonstration rather than by reading.

### P5b.1 — Close-then-open writes, on today's pin (no dependency)

Every concept writer closes before it opens (A-4, D-2): entity and module flushes, spine rows,
branch pointers and tombstones, commit concepts where versioned, annotation and comment edits,
sweep retirement. On 0.19.1 nothing checks this, which is why it goes first: the upgrade then
changes nothing for writes. The two-flush window is handled by ordering, not by hoping: the close
flush and the open flush share the commit's stamp, the kv cursor advances only after the open
flush, and a resumed ingest re-runs the open half. If U-5 lands first, this phase uses it instead.
**Proves it:** INV-15 holds on the migration fixture and on a scratch self-host run; the suite runs
green against a local Macrame 0.20.0 build with the guard active.

### P5b.2 — Adopt Macrame 0.21.0 (blocked by U-1)

Move the pin, resolve whatever API drift remains, rerun the full gate bundle and the scratch
migration, and require `verify_concept_intervals` to report nothing. Not 0.20.0: its backfill
defect is P5b.5's write.

### P5b.3 — Valid-axis reads (blocked by P5b.2; branch reads by U-2)

Every temporal op (`as_of`, `entities_at`, `source_refs_at`, `changes_between`, `verify_history`,
`history_of`'s intervals) takes `at` and `recorded` (A-2, D-3, INV-16), lowered to
`reconstruct_as_of`. Trunk reads ship as soon as P5b.2 lands. Branch reads wait for U-2 and
meanwhile refuse on the valid axis with an error that names U-2, rather than answering for the
trunk. **Flips** F1's test.

### P5b.4 — Commits as addresses (blocked by P5b.3)

`at` accepts a sha or a ref (A-3), resolved through the commit concept to its lineage and committer
time, with the INV-11 nudge applied so a nudged stamp resolves to itself. An unknown sha refuses;
it never rounds to the nearest commit.

### P5b.5 — Out-of-order ingest (blocked by P5b.3)

The original P5b. Lift INV-10 for INV-10′: backfill a branch that predates the store, ingest a
second remote, re-ingest a range after a parser fix (re-asserting existing starts, which is a
correction on the recorded axis and leaves valid-axis answers for untouched files unchanged).
**Proves it:** ingesting a fixture's commits in shuffled order yields the same valid-axis answers
as ingesting them in order, at every commit, and different recorded-axis answers exactly where the
order differed. That pair of assertions is Doctrine VIII as a test.

### P5c — Lineage fidelity (blocked by U-3 and U-4)

Branches fork at their merge-base on the valid axis (A-5); merges assert merged files on the
receiving lineage (A-6); rebases become lineage generations (D-5); remote-tracking branches join the
ingest set (D-6). P5a.4's "ingest branches before merging them" and the `cross-lineage-id` skip are
retired, along with the lineage-blind `history_of`, which gains a lineage-tagged chain.
**Flips** F3's and F4's tests.

### P5d — The live tree (no upstream dependency)

The working-tree overlay (A-7, D-4, INV-17): an in-memory layer over the head state of the
checked-out lineage. The watcher, `analyze` and `update_file` write the overlay on a commit-time
store instead of refusing. Then commit-driven sync (A-8): a ref watcher on `.git/HEAD` and
`.git/refs`, or a post-commit hook, runs the incremental ingest for the affected lineages and
rebases the overlay onto the new head. Checkout switches the overlay's base lineage.
**Flips** F5's test. Independent of P5b.1–P5b.5 and of every upstream request, so it can run in
parallel with them.

### P6b — Aggregation and vectors (no dependency)

As proposed: `open_items` over annotations, annotations and comments as embed targets (current
version only), the search-side join that answers "why" next to "what". Unchanged by this plan
except that its writes follow P5b.1's discipline.

### P8 — The data boundary (no dependency)

Export and import for annotations and comments, keyed by canonical target id, carrying their
version chains. A `rebuild` op that re-ingests from git into a fresh store and re-imports (A-9,
INV-18). This turns Q-9's remedy into a command, and it makes every future store change (a schema
step, a parser overhaul, a scrubbed history) a rebuild rather than a migration.

### P9 — Surfaces (follows each phase it exposes)

MCP read tools gain `at`/`recorded` with P5b.3 and sha addressing with P5b.4. Q-10's answer:
expose the ingest *report* (lineage cursors, ghost commits, coverage), never the write. CLI,
QUICKREF, SKILL and server instructions move in the same commit as the op, as the drift tests
already require.

---

## 7. Sequencing

| Order | Phase | Blocked by | Can run beside |
|---|---|---|---|
| 1 | P5b.0 truth tests | — | everything |
| 2 | P5b.1 close-then-open | — | P5d, P6b, P8 |
| 2 | P5d live tree | — | P5b.1, P6b, P8 |
| 2 | P6b, P8 | — | each other |
| 3 | P5b.2 adopt 0.21.0 | U-1 (D-1), P5b.1 | P5d |
| 4 | P5b.3 valid reads (trunk) | P5b.2 | — |
| 5 | P5b.4 sha addressing | P5b.3 | P5b.5 |
| 5 | P5b.5 out-of-order ingest | P5b.3 | P5b.4 |
| 6 | P5b.3 branch reads | U-2 | — |
| 7 | P5c lineage fidelity | U-3, U-4 | — |
| — | P9 | each phase it exposes | — |

The critical path is upstream: U-1 gates the pin, U-2 gates branch reads, U-3 and U-4 gate P5c.
Everything CodeRadar can do alone (P5b.0, P5b.1, P5d, P6b, P8) has no dependency and comes first,
so the upstream work never waits on CodeRadar and CodeRadar never idles on upstream.

**Suggested releases.** 0.15.0: P6a, P6c, P5a (experimental per D-7), P5b.0, P5b.1. 0.16.0: P5d
and P8. 0.17.0: P5b.2–P5b.5 on 0.21.0. 0.18.0: P5c, when U-3 and U-4 exist.

---

## 8. The one ordering constraint that touches the existing store

Not migration planning, only a dependency to respect: **do not make a commit-time store the store of
record before P5d.** Until the overlay exists, a commit-time store blinds the watcher and every
uncommitted edit (F5). The production migration of the self-host store waits for P5d; whichever of
Q-8's answers then applies (keep frozen, decided 2026-10-31) is unaffected.

---

## 9. Hazards (continue H-14..H-20)

| ID | Hazard | Mitigation |
|---|---|---|
| H-21 | **The two-flush window.** Between the close flush and the open flush a concept is valid nowhere | P5b.1: shared stamp, cursor after the open flush, resumable; U-5 removes it |
| H-22 | **The overlay diverges from the ledger.** An overlay built on a head that has since moved answers for a tree nobody has | P5d rebases on every ref change; a read reports the overlay's base sha |
| H-23 | **Valid-axis branch reads before U-2.** Answering a branch question from a trunk-only fold is F-6's failure in a new place | P5b.3 refuses branch reads on the valid axis, naming U-2 |
| H-24 | **Lineage generations multiply.** Every rebase adds a lineage, and lineages are append-only | Generations are cheap rows; D-5's resolution keeps the name pointing at the newest; measure on a rebase-heavy fixture before P5c ships |
| H-25 | **Clock-stamped annotations in a commit-time store.** A valid-axis read at a commit instant never sees an annotation written later, which is right, and is surprising | Document it in the read's rustdoc and QUICKREF; annotation reads take no `at` in v1 |

---

## 10. Done when

A store built and kept current by git answers, on every lineage: what the code was at any commit or
date (valid axis), what CodeRadar believed at any instant (recorded axis), and what the code is
right now including uncommitted edits (overlay). A branch sees its parent only up to its merge-base,
a merge brings merged files into the receiving lineage, a rebase leaves the old history readable,
and ingesting history in any order yields the same valid-axis answers. Every concept has at most one
open interval. The store can be rebuilt from git plus an annotation export, and the rebuild answers
identically. All five P5b.0 truth tests pass without `xfail`.

---

## 11. Open questions

- **Q-11 — D-1..D-7** (§3). D-1 is the one an upstream release waits on.
- **Q-12 — Overlay persistence.** Should the overlay survive a process restart (a sidecar file
  outside the ledger), or be rebuilt from the working tree on start? Rebuilding is simpler and is
  the recommendation until a cold start is measured as too slow.
- **Q-13 — Who files U-2..U-5.** One request per item in Macrame's `docs/`, or one combined
  "lineage fidelity" request for U-2..U-4? Recommendation: U-2 alone first, since P5b.3's branch
  reads need it soonest, then U-3 and U-4 together, since they share the fork model.
