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

**A-2. Reads name their axis and default to recorded.** *(Revised 2026-10-31, D-3 decided.)* A
temporal read's default question stays "what had CodeRadar recorded by then". Valid-axis reads
(`valid=`) and commit addresses (`at=<sha>`) are explicit. A git-filled store must still never answer
empty for a time it has history for. §3.1 is the rule that guarantees it. (F1)

**A-3. Commits are addresses.** `at` accepts a timestamp, a commit sha, or a ref. A sha resolves
through its commit concept to `(lineage, committer time)`. Users ask about commits, not instants.
(F1)

**A-4. Intervals are real.** A version of a concept is `[its start, the next version's start)`.
Writing a new version first closes the open one. Deleting a file closes its concepts' intervals at
the deleting commit. `retired` stays as a marker on the closing row, never as a second open row.
The one open "absent" row is a branch's tombstone for a file it inherited: the branch cannot close
the parent's concept (D-225), and absence on the branch holds until the file is re-added. A re-added
file opens a new interval on the same id (D-2).
At most one interval per concept is open, and successive versions abut. (F2, and the shape D-291
already enforces)

**A-5. A lineage is a git branch, forked at its merge-base on the valid axis.** A branch inherits
its parent's state *as valid at the merge-base commit*, not as recorded when the fork ran. Commits
after the merge-base on the parent are invisible to the branch until a merge brings them in. (F3)

**A-6. Identity follows the file, and a merge is an operation.** When a merge commit brings
branch-born files into a lineage, that lineage asserts them under their canonical ids. A concept id
names a file or entity, never a lineage (Macrame D-214's own principle, extended to merges). (F4)

**A-7. Observed edits are recorded.** *(Revised 2026-10-31, replaces the overlay.)* A successful
mutation, and a watcher-seen edit, is an event that CodeRadar records in the ledger, stamped at the
moment it was observed on both axes and tagged with where it came from (§3.1.3). Under a recorded
default, that is what "what had CodeRadar recorded by then" has to include. Commit-addressed reads
stay commit-pure by reporting, not hiding, what was observed but uncommitted. (F5)

**A-8. Ingestion is driven by git, continuously.** A ref change (commit, merge, branch creation,
checkout) triggers an incremental ingest of the affected lineages, before any later observation is
recorded (H-26). A checkout also returns the left lineage's dirty files to its committed content
by observation (D-4 a1). A branch
rewritten by a rebase or force push becomes a new generation of that lineage; the old one stays
readable. A rewritten trunk refuses (D-5). Nothing recorded is ever retracted (P5-D8).

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
| **D-1** | Accept Macrame 0.21.0's state rule (D-303): *a concept's state is its open belief, else its latest-starting belief* | **a** accept (flips 0.20.0 gate 7 to the valid-consistent answer) · **b** keep "row written last" (leaves the backfill defect unfixable without the projection and the fold disagreeing) | **Decided 2026-10-31: a** (Macrame register D-303). It is the only rule under which a backfill — P5b's write — leaves current reads correct, and it gives the recorded default (D-3) "the state among what was recorded", never a past fragment written last |
| **D-2** | How a deletion is written | **a** close the interval at the deleting commit, `retired: true` on that closing row; a branch deleting an inherited file writes an open tombstone on its pointer · **b** close with no marker, "deleted" inferred from no successor · **c** a new open retired row at the deletion's start | **Decided 2026-10-31: a** (A-4). Already what every production path writes (`retire_entities_at`, `storage.rs:1640`, used by ingest, the sweep and annotation retire). Left for P5b.1: close the branch pointer before its tombstone, and rewrite the one fixture with shape c (`storage.rs:5626`). b would make a torn close-then-open write indistinguishable from a deletion |
| **D-3** | Default axis for temporal reads | **a** valid (`at`), recorded opt-in · **b** recorded stays the default, valid opt-in | **Decided 2026-10-31: b**, with the horizon rule of §3.1 so a git-filled store never answers empty |
| **D-4** | What a read with no `at` sees | **a** recorded observations of edits included · **b** always the committed head | **Decided 2026-10-31: a, with a1.** No `at` on the checked-out lineage = its committed history plus every recorded observation; results list `uncommitted: [paths]`; `at="HEAD"` is the committed head. **a1:** on a HEAD change, after commit sync, every file still dirty on the old lineage gets an observation back to its committed blob (`source: "checkout"`), and carried edits are recorded on the new lineage, so a lineage not checked out reads as its committed head. Edits made while CodeRadar was off are recorded at startup (`source: "startup"`), stamped when seen. Rejected: b (a mutation vanishes from current reads until committed; needs a `live=` flag everywhere) and a2 (a left lineage keeps claiming edits that moved with the tree) |
| **D-5** | A rebased or force-pushed branch | **a** a new lineage generation (`feat` → `feat#2`, forked at its new merge-base; `feat` resolves to the newest) · **b** refuse until an operator acts · **c** retract the old commits | **Decided 2026-10-31: a for branches; the trunk refuses.** Today both cases stop at `NotAncestor`/`ForkMismatch` (b by accident). A branch whose cursor left its history syncs main, forks a new generation from main now, and ingests from the new fork point; the name resolves to the newest generation; the old one stays readable, sha reads included. Exact for a rebase onto main's tip. Until U-4, files the branch minted itself land as bytes without entities on the new generation (`cross-lineage-id`, reported); a rebase onto an older base leaks newer main content until U-3. A rewritten trunk refuses with `TrunkRewritten { old, new }` and points at a rebuild (P8): `main` is the only root, so it has no generation. Rejected: b (stalls every rebase), c (contradicts P5-D8), d same lineage continued (its fork cannot move, so inherited files go stale after a rebase onto newer main) |
| **D-6** | Which branches are ingested | **a** every local branch plus remote-tracking branches, with a deny-list · **b** local only (today) · **c** only branches with commits of their own; every other name is a commit address | **Decided 2026-10-31: c.** A branch, local or remote-tracking, whose tip lies on an ingested lineage's first-parent chain gets no lineage: its name resolves to that tip commit and reads go through the commit index (§3.1.2), exact today with no fork and no F3 leak. A branch with commits of its own is a lineage. Every branch read reports `parent_drift`, the parent commits after its fork point it can see (0 on main's tip), until U-3. Deny-list kept, empty by default; a deleted branch's name resolves to its last tip. Measured on this repository: 17 local branches, 15 fully merged; 16 remote-tracking, 13 equal to local, none remote-only — so main plus 3 lineages instead of 33, every name still readable. Rejected: a and b (a merged branch would fork now and read as today's main, 203–518 commits wrong here, unlabelled; a also duplicates local lineages and trips `cross-lineage-id`) |
| **D-7** | Release policy for the commit-time path until P5b.3 and P5d land | **a** ship `ingest-history` and `migrate-to-commit-time` marked experimental · **b** hold them out of releases until P5b.3 and P5d · **c** ship as is · **d** ship when the written shape is final: P5b.1, P5b.3a and P5b.6 landed | **Decided 2026-10-31: d.** A store P5a builds today is not in its final shape: open-open overlaps (F2) that the valid fold cannot read and Macrame does not repair, no horizon or commit index (F1), and every local branch as a lineage, merged ones included (D-6) — so it needs a rebuild from git. After those three phases (none upstream-blocked) later phases change reads, not writes. The store records `coderadar.commit_time.format = 1` in `kv`; a store without it, or older, refuses temporal reads with "rebuild from git". The release note states §8 in one sentence. A release cut before the three phases leaves both CLI commands out and keeps the ops internal. Rejected: a (every store built needs a rebuild), b (waits on U-1 for a format that is settled earlier), c |

### 3.1 D-3 decided: recorded stays the default — and a git-filled store never answers empty

**The problem, precisely.** A store filled from git on day *E* holds commits from months before
*E*, but every row was recorded on *E*. A recorded-axis read at any `t < E` folds an empty prefix of
the ledger and returns nothing, although the store holds exactly the history the caller is asking
about. The fix is not to change what the default read means where CodeRadar was watching. It is
to define what the read means where CodeRadar was not.

**What is not available.** Stamping imported rows with `recorded_at` = commit time. The engine
stamps `recorded_at`, a monotonic trigger floors it, and INV-8 forbids it: it would claim CodeRadar
knew in August what it learned in October.

#### 3.1.1 The knowledge horizon

Every lineage gets a **horizon** *H(L)*: the recorded instant at which its first import into this
store completed, caught up to the lineage's head at that time. It is written once into `kv`
(`coderadar.horizon.{lineage}`) when the first ingest of *L* finishes, and never moved. A store
filled by `analyze` alone (index-now) has no horizon; its reads are unchanged.

The default read `as_of(t)` on lineage *L* then has two arms, chosen by one comparison:

| Arm | When | Answers | `basis` in the result |
|---|---|---|---|
| **Recorded** | `t >= H(L)`, or no horizon | the recorded fold at *t*, exactly as today | `"recorded"` |
| **Git history** | `t < H(L)` | the state of the last commit on *L* whose committer time is at or before *t* | `"git-history"`, with the commit's sha |

Every result names its `basis` and *H(L)*, so a caller can always tell an observation from a
reconstruction. A time before the lineage's first commit is not empty either: it refuses with
`BeforeHistory { first_commit, horizon }`, which is a reason, not a blank. A `strict=True` read
refuses on the git-history arm too (`BeforeHorizon`), for callers who want the recorded question
and nothing else.

The rule keeps D-3's meaning wherever it has one. After *H(L)*, CodeRadar was watching: commits are
ingested as they land (A-8) and edits are recorded as they happen (§3.1.3), so the recorded fold at
*t* is what was known at *t*. Before *H(L)*, CodeRadar knew nothing, and the only honest non-empty
answer is what git says, labelled as such.

#### 3.1.2 The git-history arm runs on today's engine

It needs no valid-axis fold and no Macrame upgrade, because of INV-10. While ingest is append-only
per lineage, the recorded instant at which commit *C*'s ingest completed is a point at which the
ledger holds exactly the state at *C*. So the arm is two steps, neither of them a fold CodeRadar
owns (F-6 holds):

1. **Commit index.** Resolve *t* to the last commit on *L* with committer time ≤ *t*, and that
   commit to its **completion instant** — the recorded instant after its last sub-generation (the
   concept flush, then edges and sweep) and before the next commit's first write. The ingest
   records it explicitly in `kv` (`coderadar.commit_done.{lineage}.{sha}`) rather than inferring
   it from neighbouring rows. The commit concept `commit:{sha}` is already written in the commit's
   own flush (`graph/ingest.rs:720,1408`), so the index can be rebuilt from the ledger if `kv` is
   lost (H-20).
2. **Recorded fold** at the completion instant, through the existing `reconstruct_on`.

The same index serves `at=<sha>` (A-3). When P5b.5 lifts INV-10, recorded order stops matching
commit order and step 2 is replaced by the valid fold (`reconstruct_as_of`, valid = committer time
of *C*). The read's API and its `basis` label do not change, only how it is computed.

Branch caveat: until P5c, a branch read on this arm inherits whatever F3 leaks, because the
recorded fold at the completion instant sees the parent as it was at the fork. The arm makes the
answer non-empty; P5c makes it right for branches.

#### 3.1.3 The mutation event, and edits on a git-filled store

**Today.** After a mutation installs its files, `CodeGraph.apply` re-records them through
`_synchronize_mutation_files` → `update_file` (`py_agent/src/coderadar/__init__.py:1151-1215`).
On an index-now store that is already the "record on success" event. On a commit-time store
`update_file` refuses (INV-12, `lib.rs:1316`), so every successful edit ends `ApplyIncomplete`.

**Proposed.** A successful mutation emits one **observation write**:

1. **Sync first.** Ingest any commits that have landed on the checked-out lineage since its cursor,
   so the observation's start is later than every commit start on the lineage (the D-291 chain stays
   in order, and INV-11's nudge covers clock skew).
2. **Record.** Re-extract the affected files and write their concepts with `valid_from` =
   `recorded_at` = now, closing the previous open interval first (P5b.1). Each row carries
   provenance in `extra`: `{"source": "mutation", "tool", "plan_id", "attempt_id"}`.
3. **Report.** The mutation's `sync` summary reports the observation generation, and the result is
   `Applied`.

The watcher and `analyze` write the same kind of row with `"source": "watcher"` /
`"source": "analyze"`. INV-12 narrows from "one stamping mode per store" to **"every row's axes
mean what its `source` says"**: commit rows carry commit time, observation rows carry the
observation instant, and no row carries a mix. The mode flag stays, and now records how the store
was filled (`git` or `observed`), not what it may accept.

**When the edit is committed,** the commit's rows open a new start at committer time and supersede
the observation's. If the edit is discarded instead, the watcher records the reverted file as
another observation. Nothing is retracted.

**What a commit-addressed read does with observations.** `at=<sha>` folds at the commit's
completion instant. If files were observed dirty at that instant and the commit did not include
them, the fold shows their observed content, which is true of the working tree and not of the
commit. The read therefore **reports** them (`dirty_at_commit: [paths]`), rather than hiding them,
and `strict=True` refuses. Making `at=<sha>` exactly commit-pure means keeping observations on a
lineage of their own, which needs a fork that can move with its parent (U-3). It is deferred to P5c.

#### 3.1.4 What this changes elsewhere in the plan

- **P5d** becomes observation writes plus commit-driven sync. The in-memory overlay and Q-12 are
  withdrawn.
- **INV-16** reads: every temporal read names its axis internally, and every result names its
  `basis`.
- **INV-17** reads: the ledger holds committed history and observed edits, each tagged with its
  source. Nothing is stamped on an axis it does not belong to.
- **H-22** is replaced by H-26 (§9).
- **The horizon and commit index ship before Macrame moves** (new phase P5b.3a, §6). The empty
  answers end on 0.19.1.

---

## 4. Upstream requests (Macrame)

CodeRadar cannot build any of these (A-10). They are listed in the order P5b needs them.

**U-1 — 0.21.0's concept state rule (D-303).** Planned upstream; D-1 accepted 2026-10-31, so it is
unblocked, not yet implemented. It fixes the 0.20.0
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
- **INV-16 — Every temporal read names its axis, and every result names its basis.** Internally
  there is no "timestamp" parameter whose axis is implied. The public default is recorded (D-3);
  results say whether they came from the recorded fold or from git history (§3.1.1).
- **INV-17 — Every row's axes mean what its source says.** Commit rows carry commit time;
  observation rows (mutation, watcher, analyze) carry the instant they were observed; annotations
  and comments are present-tense by design. No row mixes the two (§3.1.3).
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

### P5b.3a — Horizon and commit index (no dependency)

§3.1.1–3.1.2 on today's pin: the per-lineage horizon in `kv`, the per-commit completion instant,
the git-history arm of the default read with its `basis` label, `BeforeHistory`, `strict=`, and
`at=<sha>` with `dirty_at_commit`. **Flips** F1's test on 0.19.1. Must ship together with the
existing stores' backfill of the index: a store ingested before this phase rebuilds its commit
index from the `commit:{sha}` rows once, auditable.

### P5b.3 — Valid-axis reads (blocked by P5b.2; branch reads by U-2)

Every temporal op (`as_of`, `entities_at`, `source_refs_at`, `changes_between`, `verify_history`,
`history_of`'s intervals) gains an explicit `valid=` (A-2, D-3, INV-16), lowered to
`reconstruct_as_of`. The recorded default and P5b.3a's horizon rule stay. The git-history arm
switches from the commit index to the valid fold only when P5b.5 lifts INV-10. Trunk reads ship as
soon as P5b.2 lands. Branch reads wait for U-2 and meanwhile refuse on the valid axis with an error
that names U-2, rather than answering for the trunk.

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

### P5b.6 — Branch names: selection, addresses, generations (needs P5b.3a)

**Selection (D-6 c).** The ingest set is the trunk plus every branch, local or remote-tracking,
whose tip is not on an ingested lineage's first-parent chain, minus the deny-list. Every other name
is an address: resolved to its tip commit and read through the commit index, which is why this
phase needs P5b.3a. The commit index records which lineage holds each sha, so resolution is one
lookup. Every branch read reports `parent_drift`. `migrate_to_commit_time`'s default branch set
follows the same rule.

**Generations (D-5).** Detect a branch cursor that left its history, sync main, fork the next
generation now, ingest from the new fork point, and resolve the name to the newest generation in
`kv` (`coderadar.ingest.generation.{branch}`). A rewritten trunk refuses with `TrunkRewritten`.
Branch-minted files report `cross-lineage-id` on the new generation until U-4.

**Proves it:** a merged branch reads equal to the trunk at its tip sha; a rebase-onto-tip fixture
reads exactly on the new generation, and every sha of the old one still resolves; a stale-branch
fixture reports the expected `parent_drift`.

### P5c — Lineage fidelity (blocked by U-3 and U-4)

Branches fork at their merge-base on the valid axis (A-5); merges assert merged files on the
receiving lineage (A-6); a new generation takes over the ids its predecessor minted (D-5, via
U-4); `parent_drift` drops to 0 on every lineage (D-6). P5a.4's "ingest branches before merging them" and the `cross-lineage-id` skip are
retired, along with the lineage-blind `history_of`, which gains a lineage-tagged chain.
**Flips** F3's and F4's tests.

### P5d — The live tree (needs P5b.1)

*(Revised 2026-10-31.)* Observation writes (A-7, D-4, INV-17, §3.1.3): the mutation event, the
watcher and `analyze` record edits on a git-filled store, tagged with their source, after syncing
pending commits; a successful mutation reports `Applied` again. Then commit-driven sync (A-8): a
ref watcher on `.git/HEAD` and `.git/refs`, or a post-commit hook, runs the incremental ingest for
the affected lineages. Checkout switches which lineage observations are written to, after
recording the left lineage's dirty files back to its head (D-4 a1); startup records what changed
while CodeRadar was off. Needs P5b.1
because every observation opens a new start. **Flips** F5's test. Independent of every upstream
request.

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
| 2 | P5b.1 close-then-open | — | P5b.3a, P6b, P8 |
| 2 | P5b.3a horizon + commit index | — | P5b.1, P6b, P8 |
| 2 | P6b, P8 | — | each other |
| 3 | P5b.6 branch names + generations | P5b.3a | P5d, P5b.2 |
| 3 | P5d live tree | P5b.1 | P5b.2 |
| 3 | P5b.2 adopt 0.21.0 | U-1 (D-1), P5b.1 | P5d |
| 4 | P5b.3 valid reads (trunk) | P5b.2 | — |
| 5 | P5b.4 sha addressing | P5b.3 | P5b.5 |
| 5 | P5b.5 out-of-order ingest | P5b.3 | P5b.4 |
| 6 | P5b.3 branch reads | U-2 | — |
| 7 | P5c lineage fidelity | U-3, U-4 | — |
| — | P9 | each phase it exposes | — |

The critical path is upstream: U-1 gates the pin, U-2 gates branch reads, U-3 and U-4 gate P5c.
Everything CodeRadar can do alone (P5b.0, P5b.1, P5b.3a, P5b.6, P5d, P6b, P8) comes first, so the
upstream work never waits on CodeRadar and CodeRadar never idles on upstream.

**Suggested releases.** 0.15.0: P6a, P6c, P5b.0, P5b.1, P5b.3a, P5b.6, and with them P5a's two
commands at store format 1 (D-7 d) — the first release in which a git-filled store answers every
past time. If 0.15.0 is cut before those three phases, it ships without the commands. 0.16.0: P5d and P8. 0.17.0:
P5b.2–P5b.5 on 0.21.0. 0.18.0: P5c, when U-3 and U-4 exist.

---

## 8. The one ordering constraint that touches the existing store

Not migration planning, only a dependency to respect: **do not make a commit-time store the store of
record before P5d.** Until observation writes exist, a commit-time store blinds the watcher, ends
every mutation `ApplyIncomplete`, and hides every uncommitted edit (F5). The production migration of the self-host store waits for P5d; whichever of
Q-8's answers then applies (keep frozen, decided 2026-10-31) is unaffected.

---

## 9. Hazards (continue H-14..H-20)

| ID | Hazard | Mitigation |
|---|---|---|
| H-21 | **The two-flush window.** Between the close flush and the open flush a concept is valid nowhere | P5b.1: shared stamp, cursor after the open flush, resumable; U-5 removes it |
| H-22 | *(Withdrawn 2026-10-31 with the overlay; see H-26.)* | — |
| H-26 | **Observations ahead of a late commit.** A commit fetched from elsewhere can carry a committer time earlier than an edit already observed on the same lineage, which would open a start behind the open one | Sync before every observation write (§3.1.3); a commit that still lands behind is nudged after the latest start and flagged `stamp_nudged`, as INV-11 does for ties |
| H-27 | **The horizon is a `kv` row.** Lost, every past read falls back to the recorded arm and answers empty again | Rebuildable from the ledger (the first `commit:` row per lineage); `verify_history` reports a missing horizon on a git-filled store as an error, not a warning |
| H-23 | **Valid-axis branch reads before U-2.** Answering a branch question from a trunk-only fold is F-6's failure in a new place | P5b.3 refuses branch reads on the valid axis, naming U-2 |
| H-24 | **Lineage generations multiply.** Every rebase adds a lineage, and lineages are append-only | Generations are cheap rows; D-5's resolution keeps the name pointing at the newest; measure on a rebase-heavy fixture before P5c ships |
| H-25 | **Clock-stamped annotations in a commit-time store.** A valid-axis read at a commit instant never sees an annotation written later, which is right, and is surprising | Document it in the read's rustdoc and QUICKREF; annotation reads take no `at` in v1 |

---

## 10. Done when

A store built and kept current by git answers, on every lineage: what the code was at any commit or
date, what CodeRadar believed at any instant (recorded axis, never empty for a time git has history
for, §3.1), and what the code is right now including uncommitted edits (recorded observations). A branch sees its parent only up to its merge-base,
a merge brings merged files into the receiving lineage, a rebase leaves the old history readable,
and ingesting history in any order yields the same valid-axis answers. Every concept has at most one
open interval. The store can be rebuilt from git plus an annotation export, and the rebuild answers
identically. All five P5b.0 truth tests pass without `xfail`.

---

## 11. Open questions

- **Q-11 — D-1..D-7** (§3). D-1 (a), D-2 (a), D-3 (b), D-4 (a, a1), D-5 (a; trunk refuses), D-6 (c) and D-7 (d)
  decided 2026-10-31. All seven are decided.
- **Q-12** — *withdrawn 2026-10-31* with the overlay (§3.1.4).
- **Q-14 — Does the git-history arm bind belief at the horizon or at now?** §3.1.1 answers from
  the store's current belief, so a later re-ingest after a parser fix changes past answers, as it
  should for a reconstruction. Binding at *H(L)* would make them reproducible forever, and would
  keep serving a parse the store has since corrected. Recommendation: current belief, with the
  `recorded=` parameter available for anyone who needs reproducibility.
- **Q-13 — Who files U-2..U-5.** One request per item in Macrame's `docs/`, or one combined
  "lineage fidelity" request for U-2..U-4? Recommendation: U-2 alone first, since P5b.3's branch
  reads need it soonest, then U-3 and U-4 together, since they share the fork model.
