# CodeRadar — P5b and beyond: one coherent bitemporal code store

> **Plan, not a landing.** Nothing here is built. Drafted 2026-10-31 against CodeRadar `dev` at
> `c1f1ec8` (P5a, P6a and P6c built, unreleased; Macrame pin `0.19.1`) and Macrame `main` at
> `634740f` (`v0.20.0`; `dev/0.21.0` holds an unstarted 0.21.0 plan).
>
> **Updated 2026-11-01: Macrame `v0.21.0` is released** (tag `8ffba57`, `main`). It ships U-1
> (D-303) and nothing else this plan requests; §0's 0.21.0 rows, U-1, P5b.2 and §7 say what that
> changes. U-2..U-5 are not filed yet (Q-13).
>
> **Updated 2026-11-09: Macrame `0.22.0` is on `dev/0.22.0-revA` at `5288876`** (CodeRadar pins
> it there; the release tail is pending). It delivers **U-5** as D-312 (supersede in one call)
> and **U-4's substance** as D-315 adoption (a lineage asserts what it merged from, provenance
> at the mint; the merge itself remains Doctrine III's own decision, so U-4 is marked
> "delivered through adoption", not "done"). It also ships **D-314** (the concept overlay:
> per-lineage versions and cuts, the read walk, the search and archive arms) and **D-316** (a
> past instant still reads below a cut or supersede through the walk, capped at the ancestry
> cutoff). U-2 and U-3 were then unfiled *(filed 2026-11-11 — see (2) below)*; they are the
> remaining upstream path (§7 and Q-13 mark it).
>
> **Updated 2026-11-11: Macrame `v0.22.0` is released** - tag `v0.22.0` on `69336d7` (all three
> branches ff-merged there; the docs record at `1d57dae`), published to crates.io and PyPI the
> same day (release run `42984353021`, wheels run `42984353081`), the CI replication green on
> every leg (Rust `42984290027`: 1,048 / 1,026 / 1,074 across the three runners, Python
> `42984289951`: 749 + 3 skipped on all five legs). The release carried a fix round the
> 2026-11-09 note predates: python.yml's clippy leg gained mypy `--strict` on the binding
> (`67ced5a`, `69336d7`), and the py3.10 floor leg came back after M1b's `datetime.UTC` import
> had silenced it since Nov 7. CodeRadar pins the release (crates.io `0.22.0`, `605c367`).
> U-2 and U-3 are the remaining upstream path (§7 and Q-13 mark it); the v0.23.0 plan files
> them (see (2) below).
>
> **Updated 2026-11-11 (2): U-2 and U-3 are filed.** Macrame's `v0.23.0` plan takes them as D-317 (U-2, the lineage-resolved valid fold) and D-318 (U-3, the fork's valid-axis cut). The 11-11 paragraph above is history as to filing; §7's 11-11 paragraph is the operative order.
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
| *(2026-11-01)* **0.21.0 released with D-303.** Mechanism: the guard admits a backfill, the projection keeps its row, and the pair is marked in a new `concept_disorder` table that never clears; recorded reads correct only marked pairs. Schema v23 → **v24** on open; libSQL 0.9.30 → **0.10.0**; old Macrame snapshots skipped | Macrame `docs/releases/v0.21.0.md`; `src/temporal/replay.rs:1187` | **→** U-1 done; P5b.2 unblocked |
| *(2026-11-01)* 0.21.0's one break, `Overlap.within_batch` → `provenance`, does not reach CodeRadar: no use of `within_batch` or `OverlappingIntervalError` in `core_indexer` or `py_agent` | grep at `ad7d7c5` | **→** P5b.2 has no API drift from it |
| *(2026-11-01)* CodeRadar must move `libsql` with the pin: `core_indexer/Cargo.toml:50` pins `libsql = "0.9"` because `diagnostic_conn()` hands out Macrame's own connections | `core_indexer/Cargo.toml:30,47-50` | **→** P5b.2 |
| *(2026-11-01)* Merge is recorded upstream as "its own decision", out of branching's scope (Doctrine III has no neutral superseder), not as refused forever | Macrame `docs/Macrame Road to 1.0.md` §15.4, §16 | **→** U-4 needs a Macrame decision before any design (Q-15) *(2026-11-09: 0.22.0 ships D-315 adoption **instead of** the decision — U-4's substance is delivered without it; the merge itself stays Doctrine III's own decision)* |
| `migrate_to_commit_time` builds only from git and never reads the old store | `py_agent/src/coderadar/ops.py:1776-1900` | annotations and comments are not carried (§6 P8) |
| Annotations and comments are stamped by the clock, trunk-global, mode-independent | `annotations.rs:1017-1043`; `annotations-comments-proposal.md` §7 | they remain the only non-derivable data (§2 A-9) |

---

## 1. Six findings that shape everything

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
merge-base reads with the trunk's newer content wherever the branch never touched it. *(Shown by
test 2026-11-01, P5b.0: a file the trunk adds after the fork reads as present on the branch.)*

**F4 — Merges cannot be represented.** A concept id belongs to the lineage that minted it. If a
branch is ingested before it merges, which is the normal order once ingestion is live, every file
born on that branch is minted on the branch. When the merge commit reaches the trunk, the trunk may
not mint those ids (D-225), so the trunk's graph never contains the merged files. P5a.4 avoids this
during migration by ingesting the trunk first and documents it as "ingest branches before merging
them". Ongoing ingestion cannot rely on that order. *(Added 2026-10-31.)* D-6 c makes this the
common case, not an edge: every branch with commits of its own is ingested before it merges,
colleagues' PR branches included. On this repository, `dev` (26 commits of its own) is one: when it
merges, every file `dev` created arrives on the trunk as bytes without entities, reported as
`cross-lineage-id`, until U-4. Nothing is lost, since the branch keeps the rows and the bytes land,
but the trunk's symbol reads miss those files (H-28, Q-15). *(2026-11-09: 0.22.0's D-315 closes
the substance — the trunk asserts its own first version from the merge commit's bytes and the
report names `adoptions` for it; `cross-lineage-id`names only what adoption did not reach, and
the pre-overlay pin stores keep the bytes-only wording.)*

**F6 — A rename breaks identity.** *(Added 2026-10-31.)* Concept ids are built from the path, and
`plan_ingest` reports a rename as a delete plus an add. The old file's concepts close (D-2), the new
file's open under new ids, and nothing links them. Annotations and comments are keyed by the id they
are about (`annotation:{about}:{n}`, `annotations.rs:1016`) and are never re-keyed, so an annotation
on `old.py::foo` stays readable but no longer reaches the live code at `new.py::foo`. `history_of`
stops at the rename the same way. Nothing is wiped; the link is missing (A-11, P5b.7).

**F5 — A commit-time store cannot see the working tree.** INV-12 makes `analyze`, `update_file`
and `remove_file` refuse a commit-time store, so the watcher path is closed and uncommitted edits
are invisible. For a code-intelligence tool whose main caller is an agent editing code, that is the
primary use case, and the plan has no story for it.

Two upstream facts complete the picture. Macrame 0.20.0 has the valid-axis fold but no
lineage-resolved form of it, and it carries a backfill defect whose fix (0.21.0, D-303) waits on a
decision. *(2026-11-01: decided and released in 0.21.0.)*

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

**A-11. Identity survives a rename, by link, not by re-keying.** *(Added 2026-10-31.)* A rename
git detects is recorded as a link from the old id to the new one, for the file and for each entity
whose name survives it. Nothing is re-keyed: old ids, their history and their annotations stay
where they are. Reads follow the link forward, so an annotation on the old id is listed with the
new one (`via_rename`), and `history_of` continues across it. (F6)

---

## 3. Decisions for the maintainer

Each has a recommendation. D-1 is a Macrame decision, recorded here because P5b depends on it.

| # | Decision | Options | Recommendation |
|---|---|---|---|
| **D-1** | Accept Macrame 0.21.0's state rule (D-303): *a concept's state is its open belief, else its latest-starting belief* | **a** accept (flips 0.20.0 gate 7 to the valid-consistent answer) · **b** keep "row written last" (leaves the backfill defect unfixable without the projection and the fold disagreeing) | **Decided 2026-10-31: a** (Macrame register D-303); **released in Macrame 0.21.0, 2026-11-01**. It is the only rule under which a backfill — P5b's write — leaves current reads correct, and it gives the recorded default (D-3) "the state among what was recorded", never a past fragment written last |
| **D-2** | How a deletion is written | **a** close the interval at the deleting commit, `retired: true` on that closing row; a branch deleting an inherited file writes an open tombstone on its pointer · **b** close with no marker, "deleted" inferred from no successor · **c** a new open retired row at the deletion's start | **Decided 2026-10-31: a** (A-4). Already what every production path writes (`retire_entities_at`, `storage.rs:1640`, used by ingest, the sweep and annotation retire). Left for P5b.1: close the branch pointer before its tombstone, and rewrite the one fixture with shape c (`storage.rs:5626`). b would make a torn close-then-open write indistinguishable from a deletion |
| **D-3** | Default axis for temporal reads | **a** valid (`at`), recorded opt-in · **b** recorded stays the default, valid opt-in | **Decided 2026-10-31: b**, with the horizon rule of §3.1 so a git-filled store never answers empty |
| **D-4** | What a read with no `at` sees | **a** recorded observations of edits included · **b** always the committed head | **Decided 2026-10-31: a, with a1.** No `at` on the checked-out lineage = its committed history plus every recorded observation; results list `uncommitted: [paths]`; `at="HEAD"` is the committed head. **a1:** on a HEAD change, after commit sync, every file still dirty on the old lineage gets an observation back to its committed blob (`source: "checkout"`), and carried edits are recorded on the new lineage, so a lineage not checked out reads as its committed head. Edits made while CodeRadar was off are recorded at startup (`source: "startup"`), stamped when seen. Rejected: b (a mutation vanishes from current reads until committed; needs a `live=` flag everywhere) and a2 (a left lineage keeps claiming edits that moved with the tree) |
| **D-5** | A rebased or force-pushed branch | **a** a new lineage generation (`feat` → `feat#2`, forked at its new merge-base; `feat` resolves to the newest) · **b** refuse until an operator acts · **c** retract the old commits | **Decided 2026-10-31: a for branches; the trunk refuses.** Today both cases stop at `NotAncestor`/`ForkMismatch` (b by accident). A branch whose cursor left its history syncs main, forks a new generation from main now, and ingests from the new fork point; the name resolves to the newest generation; the old one stays readable, sha reads included. Exact for a rebase onto main's tip. *(2026-11-09: the first clause is delivered — with 0.22.0's
D-315 a rebased generation **adopts the ids its predecessor minted, so it gets entities, not just
bytes**, auditable in the report; a rebase onto an older base still leaks newer main content
until U-3.)* A rewritten trunk refuses with `TrunkRewritten { old, new }` and points at a rebuild (P8): `main` is the only root, so it has no generation. Rejected: b (stalls every rebase), c (contradicts P5-D8), d same lineage continued (its fork cannot move, so inherited files go stale after a rebase onto newer main) |
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

> *(2026-11-13: steps 2 and 3 below are built — P5d's first slice; step 1's sync-first
> arrives with the commit-driven half, and the H-26 nudge covers a commit that lands behind
> anyway. `at=<sha>`'s `dirty_at_commit` reporting still reads the working tree, as stated
> under the last heading.)*

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

**U-1 — 0.21.0's concept state rule (D-303).** **Done: released in Macrame `v0.21.0`
(2026-11-01).** It fixes the 0.20.0 backfill defect and is the minimum version CodeRadar adopts.
One property of the shipped rule matters from P5b.5 on: for a pair with a recorded backfill, the
recorded read answers *per instant* — a row whose `valid_from` is beyond the read instant is not
state yet. In-order ingest (INV-10) and close-then-open (P5b.1) never mark a pair, so nothing before
P5b.5 sees it (H-30).

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
as-built names, given a write path. Without it A-6 is not buildable and F4 stands. *(2026-10-31:
requested next to U-1, because D-6 c ingests branches before they merge — Q-15.)*
**(2026-11-09: delivered through D-315 adoption, not as the merge)** — 0.22.0 lets a lineage
assert concepts first minted on a lineage it cannot see, its own first version at its own stamps,
nothing copied, provenance staying at the mint. The register treats adoption as separate from the
merge, and this row's ask is what adoption provides; the merge as a Doctrine III operation is
another decision, still upstream.)

**U-5 — Supersede in one call (small; optional).** An upsert that closes the previous open start
of the same concept at its own `valid_from` and opens the new one, atomically. D-291 deliberately
refused the implicit form; this is the explicit opt-in. Without it, F2's two-flush window must be
handled on the CodeRadar side (P5b.1 does so). With it, one commit is one flush again.
**Done: 0.22.0 delivers it as D-312** (`upsert_concept_superseding`; the close and the new start
land as one write at one stamp, and the batch path carries the same close beside its row).

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

Write the tests that state F1, F3, F4, F5 and F6 against a fixture repository: a read at a
commit's own time returns that commit's content; a branch does not see a file the trunk added after
the merge-base; after merging a branch that was ingested first, the trunk sees the merged file's
entities; an uncommitted edit is visible to a no-`at` read; an annotation on an entity is listed
with that entity after its file is renamed. Mark each `xfail` with the phase that will flip it. These are the programme's
acceptance gates. They also settle F3 by demonstration rather than by reading.

*Done 2026-11-01:* `tests/test_p5b_truth.py`, six tests (F2 included: no concept holds two open
intervals), each `xfail(strict=True)` naming its phase, so a landing phase fails the run until its
marker comes off. Under `--runxfail` each fails for its stated reason: F1 reads an empty prefix; F2
finds `a.py::module` open three times; F3 sees `d.py` on the branch; F4 reports
`cross-lineage-id:feature`; F5 ends `ApplyIncomplete` on the INV-12 refusal; F6 lists no annotation
on `new.py::foo`. Writing F6 found a P6a defect, fixed in the same change: the cold loader classed
annotation and comment rows as v1 leftovers, so a store holding one refused to load. `build_graph`
then fell back to `analyze`, whose v1 sweep retired every annotation on an index-now store; a
commit-time store refuses that fallback and stopped loading. `classify_v2_concept` now skips them
(`V2ConceptClass::Note`); `tests/test_annotations.py::TestColdReload` pins both stores.

### P5b.1 — Close-then-open writes (no dependency) *(2026-11-11: `on today's pin` is history — the pin is `0.22.0`, the mechanism is D-312's one-write supersession, §7 holds the order)*

Every concept writer closes before it opens (A-4, D-2): entity and module flushes, spine rows,
branch pointers and tombstones, commit concepts where versioned, annotation and comment edits,
sweep retirement. On 0.19.1 nothing checks this, which is why it goes first: the upgrade then
changes nothing for writes. The two-flush window is handled by ordering, not by hoping: the close
flush and the open flush share the commit's stamp, the kv cursor advances only after the open
flush, and a resumed ingest re-runs the open half. If U-5 lands first, this phase uses it instead.
*(2026-11-09: it did — 0.22.0's D-312 one-write supersession; P5b.1 builds on it, and the
two-flush window shrinks to the ordering discipline above.)*
**Proves it:** INV-15 holds on the migration fixture and on a scratch self-host run *(proved 2026-11-11: fixture 17 concepts / 21 versions, self-host 354 commits / 16,701 concepts / 33,641 versions, disorder 0 both; the engine overlap check replicated at ledger level — the py binding pins v21 and cannot open a v29 store)*; the suite runs
green against the pinned Macrame build with the guard active *(2026-11-11: `0.20.0` in the prior wording is history — the proof runs on the `0.22.0` pin; the proof also fixed F2's sentinel to both spellings — the lone legacy spelling had passed vacuously since the uptake)*.

### P5b.2 — Adopt Macrame 0.22.0 (needs P5b.1; U-1 released 2026-11-01) *(2026-11-11: retitled from 0.21.0 — the pin already rides a `0.22.0` rev at `40719ac`; the registry switch closed it the same day, `605c367`)* **(done 2026-11-11)**

Move `macrame-db` to `0.22.0` and `libsql` to `0.10` in one change (`core_indexer/Cargo.toml:30,50`;
the two must match, because `diagnostic_conn()` hands out Macrame's connections). No API drift is
known: the 0.21.0 break (`Overlap.within_batch`) is unused here, and the 0.22.0 breaks ride the M6-g uptake *(2026-11-11: version retitled; the rev pin at `40719ac` became the registry pin, `605c367`)*. Rerun the full gate bundle and the
scratch migration, and require `verify_concept_intervals` to report nothing and `concept_disorder`
to stay empty on an in-order ingest *(closed 2026-11-11: [patch] retired for the crates.io pin, bundle 589 lib + 1303/1/4xf, scratch migration disorder 0 — overlap check replicated at ledger level, the py binding pins v21)*. Not 0.20.0: its backfill defect is P5b.5's write.

**P5b.1 first, strictly.** The interval guard (0.19.2+) refuses today's open-then-open writes, so
moving the pin before close-then-open breaks every second version of every concept (F2).

**The ladder is one-way.** The first open by a 0.21.0 build upgrades a store to schema v24, and a
0.19.1 build cannot open it again *(2026-11-11: the ladder has moved on — the `0.22.0` pin opens schema v29, floor v24; the frozen-store caution (copy first, H-31) still holds)*. The scratch stores do not matter; the frozen pre-migration store
(Q-8) and any store a released 0.14.0 still reads must not be opened by a P5b.2 build. Copy first,
or read the frozen store with a 0.14.0 build only (H-31).

### P5b.3a — Horizon and commit index (no dependency)

§3.1.1–3.1.2 on the `0.22.0` pin *(2026-11-11: `on today's pin` meant 0.19.1 at filing; the arm needs no valid fold so it rides the current pin)*: the per-lineage horizon in `kv`, the per-commit completion instant,
the git-history arm of the default read with its `basis` label, `BeforeHistory`, `strict=`, and
`at=<sha>` with `dirty_at_commit`. **Flips** F1's test *(written against 0.19.1)*. Must ship together with the
existing stores' backfill of the index: a store ingested before this phase rebuilds its commit
index from the `commit:{sha}` rows once, auditable.

*Done 2026-11-12:* the horizon and the per-commit completion are written by both ingest loops
(trunk and branch), the four temporal point reads (`lookup_entity_at`, `lookup_entities_at`,
`entity_source_ref_at`, `source_refs_at`) resolve their arm before folding and surface
`basis`/`sha`/`horizon`/`fold_ts`/`dirty_at_commit`, `Snapshot` carries the same labels
(`basis`, `commit`, `horizon`, `dirty_at_commit`) and `as_of(..., strict=, at=)` passes them
through, and `ops.backfill_commit_index` rebuilds a pre-phase store's index with a collision
audit. F1 flipped; `tests/test_p5b3a_arm.py` and `tests/test_p5b3a_horizon.py` pin the arm.

Two defects the tests found, both about what a completion instant must cover. (1) The loop knows
the real instant — after its last sub-generation, before the next commit's first write — and the
first import's tail backfill was replacing it with the derived (earlier) instant; the backfill
fills missing keys only, and only the migration op and K8's body heal rebuild. (2) The derivation
ignored `concept_cuts`, so a branch's sweep landed *after* the derived instant and the arm folded
a departed entity back to life; the map reads cuts now, which is also what makes a migrated
store's arm correct. A snapshot stays lazy: the arm resolves at the first read, not at
`as_of` construction, so no commit-index scan is paid to create one.

### P5b.3 — Valid-axis reads (blocked by P5b.2; branch reads by U-2)

Every temporal op (`as_of`, `entities_at`, `source_refs_at`, `changes_between`, `verify_history`,
`history_of`'s intervals) gains an explicit `valid=` (A-2, D-3, INV-16), lowered to
`reconstruct_as_of`. The recorded default and P5b.3a's horizon rule stay. The git-history arm
switches from the commit index to the valid fold only when P5b.5 lifts INV-10. Trunk reads ship as
soon as P5b.2 lands. Branch reads wait for U-2 and meanwhile refuse on the valid axis with an error
that names U-2, rather than answering for the trunk.

*(Trunk landed 2026-11-12. `as_of` and `changes_between` take `valid=`, `CodeGraph.as_of` and
`Snapshot` pass it through, and the `_core` reads (`lookup_entity_at`, `lookup_entities_at`,
`entity_source_ref_at`, `source_refs_at`, `content_hashes_at`) take it as a flag, so MCP/CLI/Python
agree by construction. On the valid axis `resolve_valid_read` names the instant itself as the fold
instant and `basis: "valid"`: no horizon, no commit index, which is exactly what P5b.5 needs. The
fold is Macrame's `reconstruct_as_of(AsOf::valid_at(..))`, trunk-only. `tests/test_p5b3_valid.py`
pins the strongest available statement — the two arms agree at an instant between two commits, one
resolved through the commit index and one through the rows' own intervals — plus a deletion's
disappearance, `changes_between` over two valid instants, the branch refusal, the `at=` refusal, the
snapshot's `basis`, and the rendered read naming the axis.)*

*(Two plan items are deliberately not taken here. `history_of`'s intervals are the valid axis
already: the spine's `valid_from`/`valid_to` are commit stamps, minted by B-item 11, so a `valid=`
flag there would restate the only axis the read has. `verify_history` samples *recorded*
generations by design — its question is "did every indexing act leave reconstructable bytes" — and
the valid axis has no generation list to sample; a valid variant would be a different audit, filed
when a consumer asks for one.)*

### P5b.4 — Commits as addresses (closed 2026-11-14)

`at` accepts a sha or a ref (A-3), resolved through the commit concept to its lineage and committer
time, with the INV-11 nudge applied so a nudged stamp resolves to itself. An unknown sha refuses;
it never rounds to the nearest commit.

*(2026-11-14, landed — and the one thing the design note could not know.)* The address resolves to
*one* commit and to *both* axes' instants, because the axes fold at different times: the recorded
axis folds at the commit's **completion** instant (`commit_done` — when the ledger learned it, wall
clock, which is what a recorded fold at that instant sees), while the valid axis folds at the
commit's **own stamp** (its rows' `valid_from`, nudge included). A read that folded the valid axis
at the completion instant would answer the present for a commit from last year — the first test
written for this caught it, and that is why the resolver returns a pair.

Names resolve to the tip the store records for them, in three keys: the name's own
`branch.tip.{name}` (it keeps tracking the tip after a rewrite moves the name), else the lineage's,
else the lineage's **ingest cursor** — a generation has no tip key of its own, and its cursor is
exactly the newest commit it completed. A name that points at another lineage's commit (D-6 c)
takes its owner from the commit, not from the name.

The implementation is shared with the recorded arm: `resolve_address` is the one resolver, so a sha
and a name mean the same thing on both axes, and the two P5b.4 placeholders (`RefNeedsP5b4`,
`ValidAtNeedsP5b4`) are gone rather than kept as aliases. The valid arm still belongs to the trunk:
an address that resolves into a branch lineage refuses with `ValidBranchNeedsU2` — the same refusal
a branch *name* gets, for the same reason.

### P5b.5 — Out-of-order ingest (blocked by P5b.3)

The original P5b. Lift INV-10 for INV-10′: backfill a branch that predates the store, ingest a
second remote, re-ingest a range after a parser fix (re-asserting existing starts, which is a
correction on the recorded axis and leaves valid-axis answers for untouched files unchanged).
**Proves it:** ingesting a fixture's commits in shuffled order yields the same valid-axis answers
as ingesting them in order, at every commit, and different recorded-axis answers exactly where the
order differed. That pair of assertions is Doctrine VIII as a test.

*(2026-11-13, slice 1 landed: the write shape. Probes first, because the answer was not where the
plan assumed. Macrame's own write path already accepts a **closed** version written before an
existing open one — `UPSERT_CONCEPT`'s `WHERE excluded.valid_from >= concepts.valid_from` declines
the projection update, the row is logged, and the pair is marked for the next read's correction
(D-303) — so the engine needs no change. The refusal was CodeRadar's: `upsert_concepts_bulk`'s W1
classification had one arm per forward case and a fallback that refused every earlier start. That
fallback now has a named arm: an earlier start is accepted when the row is **closed**, not a
retirement, and `valid_to <= live.valid_from` — a version whose successor it abuts, which is
INV-15's chain shape. Anything else (open in the past, a span inside the live belief, a backfilled
retirement) still refuses, before anything is written.
`a_closed_version_may_backfill_before_the_live_open_one` pins it end to end: two chained
backfills, the valid axis answering each interval, the live projection unchanged,
`verify_concept_intervals` clean, and both malformed shapes refused.)*

*(What remains, and the shape it must take. The ingest half is a **prefix window**: `--from` a
commit that is an ancestor of the lineage's cursor, with every planned commit strictly before it,
walked forward with a one-change-per-file lookahead so each written version closes at its
successor's start (the next change inside the window, or the live row's start, or the retirement
that ended it). Reversing the walk was rejected: the sweep, the retire acts and the spine all read
forward, and reversing them would rewrite four subsystems to answer one question. **Interior
backfill is deliberately out**: inserting a window between two ingested ranges would have to move
an existing closed interval's end, and a closed interval's end is a belief the ledger already
recorded — a correction to history, a different act from extending it, and it needs its own
decision rather than a side effect of P5b.5. The proof will therefore be: a history ingested as
contiguous chunks newest-first answers every commit's valid axis identically to a single in-order
ingest, with the recorded axis differing exactly where the chunk order differed — Doctrine VIII's
pair with prefix windows, the shape the plan's "backfill a branch that predates the store" case
actually needs.)*

*(2026-11-13, slice 2 designed against the code — the writer's shape, before it is written.
Five decisions the walk forced:)*

*1. `--from` stays **exclusive**, and a window that must reach the root says so.* Today `from` is
the last ingested commit (resume); a window `(from, cursor)` therefore cannot name the root commit
itself. Rather than give one flag two meanings (resume = exclusive, backfill = inclusive), the window
keeps the exclusive spelling and the root case is `--from-root` / `from = <root>^` — a spelling the
walker already resolves. The report always names the window it walked (`window: {from, to, commits}`),
so a caller never has to infer it.

*2. The close is computed per **file**, not per id.* Every row a commit's snapshot writes closes at
that file's next change — the next commit in the window that changes or deletes it, else the
incumbent's own start (the trunk row the cursor wrote), else the start of the deletion that ended it.
That is exact because a file's rows are minted together per snapshot: when the file changes, all of
its entities are re-asserted at the new stamp, and the ones that disappeared are closed there too.
Per-id lookahead would compute the same instant for every row of one file, at N times the cost.

*3. A window writes **closed** rows only.* The last window version ends where the incumbent begins
(`valid_to == live.valid_from`, the abutting chain slice 1 taught the guard to accept), so nothing
the window writes is ever open and the live belief keeps its own interval untouched. This is also
what makes the write audit-able: every row it lands carries both ends.

*4. What a window deliberately does not write,* each for a reason: **no sweep** (a window's "file
absent" is a fact about an interval, expressed by closes, not a retirement act — the sweep's act
would retire the trunk's present); **no edges** (edge persistence is forward-only today, and inventing
a closed edge interval here would be a new shape this phase does not need — the report says
`edges_skipped`); **no cursor, stamp or branch-tip movement** (all three name the newest belief, and a
window is older than it); **no horizon rewrite** (same reason). It does write the commit concept and
the window commit's `commit_done` mark **at the instant the ledger learned it** — which is precisely
the recorded-axis difference Doctrine VIII expects to see.

*5. The proof is two stores over one fixture.* Store A ingests a linear history in order. Store B
starts at the fixture's fork point (a store that predates its own history, the case the plan names),
then window-backfills `(root, fork)` in contiguous chunks newest-first. Every commit's valid-axis
answers must agree between A and B; the recorded axis must differ exactly where the chunk order did.
The interval assertions are the real test: `verify_concept_intervals` clean, and each file's valid
interval ends where the next change begins (no gaps, no overlaps).)*

*(2026-11-14, slice 2 landed — the writer. Five facts the code decided, four of them
against the design note above.)*

*`--to`, not `--from-root`.* The window's end is the parameter a caller needs (it must name the
lineage's belief start), and `from=None` already spells the root, so one new option beat a second
meaning for `--from`. Every run reports `belief_start`; a window's `to` is that value, and after the
run it is the window's own first commit — newest-first chunks abut with no boundary a caller can
misplace. A window also reports `window: {from, to, commits, edges_skipped}`.

*The window's first commit writes its **tree**, not a diff.* A prefix window is the store's new
root: there is no earlier ingested tree for unchanged files to inherit from, so the first window
commit writes everything it holds (the walker already ships `tree`).

*The successor is the store's belief start, not just the plan's next touch.* The boundary chunk
writes full trees, so a file that no post-window commit touches still has a belief row at the
boundary stamp; the window's last version must close there. `next_trunk_start` reads all three
places the trunk keeps a version's span — `concepts`, `concept_versions`, and `transaction_log`
(the W1 path's own history: a displaced row is logged with its span). Without the log the query
finds only the newest row and the close lands too late.

*Two guard generalisations, both the chain's invariant rather than the incumbent's shape.*
`upsert_concepts_bulk` now accepts a closed version written before a **settled** row (the backfill
arm asked the incumbent to be open; a chunk boundary can leave a closed current row — content
versions do). And `act_is_new` keeps an identical-content row whose start precedes the resolved
row's: the unchanged gate would otherwise drop a byte-identical backfill and the interval would
never be written.

*One act the window does perform.* Where its last version for an id closed because the id's life
ended before the tip, the forward run would have left a tombstone at that instant; the window writes
that same tombstone, at that same instant (content from the row itself). Without it the trunk's
current row would be a closed, non-retired shape no read expects, and the live projection would keep
listing a deleted entity. Nothing else is written: no edges, no sweep, no cursor, stamp, tip or
horizon.

*Proof.* `tests/test_p5b5_window.py` (six tests): one history with an add, a modify, a deletion and a
file never touched again, ingested in order into store A and as two newest-first chunks into store
B (which starts at a later commit, the "store that predates its own history" case). Every commit's
valid-axis digest and the current state agree; the refusals (wrong end, no belief, off-chain, empty
window) and a dry run that writes nothing are pinned. At the storage level,
`a_closed_version_may_backfill_before_a_settled_one` pins the chain shape and the unchanged-gate
arm.*

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
Branch-minted files report `cross-lineage-id` on the new generation until U-4. *(2026-11-09:
0.22.0's D-315 has the new generation adopt its predecessor's minted ids — entities, not bytes;
the report names what adoption did not reach.)*

**Proves it:** a merged branch reads equal to the trunk at its tip sha; a rebase-onto-tip fixture
reads exactly on the new generation, and every sha of the old one still resolves; a stale-branch
fixture reports the expected `parent_drift`.

*Slice A landed 2026-11-12 (selection, addresses, drift).* `fs::git::branch_plan` names every
local and remote-tracking branch with the one fact selection needs — is its tip on the trunk's
first-parent chain — and `migrate_to_commit_time`'s default set now follows D-6 c: one lineage per
distinct tip that the trunk does not already hold (local spelling wins a tie, so a remote name
never duplicates a lineage and never trips `cross-lineage-id`), every other name an address. The
address map is recorded once at migration (`coderadar.branch.tip.{name}`), each commit records the
lineage that holds it (`coderadar.commit_lineage.{sha}`), so a read resolves a name to its tip's
completion in one lookup and folds on the *owner* lineage — the name itself has no lineage Macrame
could fold, which is the one thing the first cut got wrong. `parent_drift` is recomputed at every
ingest of either lineage (`fs::git::parent_drift_count`, kv `coderadar.parent_drift.{lineage}`):
the trunk grows, so the number a read reports has to be today's. Reads refuse an unknown name with
`UnknownBranch` on a commit-time store and stay unchanged on an analyze-only one, which records no
tips and no horizon. Ten Python tests (`tests/test_p5b6_selection.py`) and two git-native Rust
tests pin it.

*Slice B landed 2026-11-12 (generations, D-5).* A branch whose cursor left its history now takes a
new generation instead of refusing: a rebase (the merge base moved, `ForkMismatch`) and a rewrite
that keeps the base (the cursor is off the chain, `NotAncestor`) both start `{branch}#{n+1}`, after
syncing the trunk so the new generation forks at today's tip. `kv
coderadar.ingest.generation.{branch}` counts them and `coderadar.branch.lineage.{branch}` is what a
read resolves through, so the name answers from its newest generation while every sha of the older
one still resolves through the commit index (`at=<sha>`, which reports the owning lineage). Two
shapes are deliberately *not* generations: a rewritten trunk refuses with `TrunkRewritten` (no
generation repairs history the trunk no longer shares — rebuild from git), and a branch whose tip
has moved onto the trunk's own chain refuses with `BranchNowMerged` (D-6 c: that name is an
address now, and an empty generation would answer nothing). Generation lineage names contain `#`,
which Macrame's kv charset (D-280) does not allow, so keys spell an unsafe name hex-encoded behind
a `:` marker; plain names keep their old spelling byte for byte, which is what keeps a store
ingested before this phase on its cursors. `tests/test_p5b6_generations.py` pins the rebase, the
rewrite, the old generation's readability and the trunk refusal.

### P5b.7 — Rename links (no dependency)

A-11 on today's pin. `plan_ingest` turns on git's rename detection (`CommitPlan.renamed:
Vec<(old, new)>`, similarity threshold per Q-16). The ingest closes the old file's concepts as
today (D-2) and writes one link row per rename, `renamed:{old_id}`, naming the new id, the commit
and the similarity, for the module and for each entity whose name exists in both files. Links are
ordinary rows, so they have history, and a later rename chains. Annotation and comment listings,
`history_of` and id lookups follow the chain forward and mark what they found with `via_rename`. The
watcher and mutation paths do the same for a rename they observe (`source` as in §3.1.3). An
annotation is never moved.
**Flips** F6's test.

*(Landed 2026-11-12. The mechanism is git's own detection at its default 50 % (Q-16), turned on in
`plan_ingest` — `CommitPlan.renamed` names the pair, and the ingest writes one row per id under
`renamed:{old_id}` naming the new id, the commit and the score, for the module and for every entity
whose name survives the rename. The link is ordinary rows: the same payload is also written under
`became:{new_id}`, which is a lookup path rather than a second fact, because the reads walk
*backwards* too and an id lookup is the only walk this store does cheaply. Rows of kind `rename`
join the INV-13 exclusions, so no listing, search universe or coverage set sees them.)*

*(The similarity is honest rather than complete: git2 does not expose the score it computed — its
own `DiffDelta::similarity` is commented out upstream — so a link records `100` only when the blobs
are byte-identical and `null` when the pair needed content changes to match. A measured-looking
number nobody measured would be worse than the gap; Q-16's point (a wrong pair is visible) survives
because the pair and the commit are on the row.)*

*(Reads: `annotations_on` and `comments_on` walk the chain both ways — an annotation minted before
the rename lives on the old id, one minted after on the new — and every version reached through a
link carries `via_rename {from, to, commit, similarity}`, so a direct target's response is unchanged
in shape. `history_of` walks backwards and appends each older file's chain below the newer one,
newest-first, marking those versions with the hop and with the `file` they belong to. `find_entity`
resolves a retired id forward through `_core.rename_chain` and marks the hit. A cycle (a file
renamed back to an earlier name) stops at the first id already visited — the id inside the cycle is
the answer. `tests/test_p5b7_renames.py` pins the report, the unclaimed score, a two-rename chain,
both annotation directions, the comment listing, `history_of` across the chain and the id lookup.)*

### P5c — Lineage fidelity (blocked by U-3; U-4's substance delivered by D-315)

Branches fork at their merge-base on the valid axis (A-5); merges assert merged files on the
receiving lineage (A-6); a new generation takes over the ids its predecessor minted (D-5, via
U-4); `parent_drift` drops to 0 on every lineage (D-6). P5a.4's "ingest branches before merging them" and the `cross-lineage-id` skip are
retired, along with the lineage-blind `history_of`, which gains a lineage-tagged chain.
**Flips** F3's and F4's tests. *(2026-11-09: the adoption arm is already writable — 0.22.0's
D-315 has the receiving lineage assert merged files' concepts and a windowing generation adopt
its predecessor's minted ids. What remains of P5c is the valid-axis cut and reads (U-2, U-3)
and `parent_drift`'s drop to 0.)*

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

*(2026-11-13: the observation half landed — F5 flipped. An index-now write on a `commit-time`
store is no longer refused: INV-12 narrowed to "every row's axes mean what its `source` says", so
`update_file`, `remove_file` and `analyze` write **observation rows** stamped at the instant the
writer saw the tree, tagged in `extra.coderadar` (`source` + `tool`/`plan_id`/`attempt_id`), and the
previous interval closes where the new one opens. `update_file` takes the provenance from its
caller, so a mutation's rows say `mutation` and name the plan and attempt; a bare call says
`update`. The sync summary reports `observations: [{file, observed_at, source}]` and a
synchronized mutation is `Applied` again. One clock read per write is the whole event: the entity
flush and the spine take the same instant, and provenance is **provenance-blind** to the
unchanged-filter — a no-op reindex that finds identical bytes asserts nothing, or every no-op
would churn `valid_from` (the spine test caught exactly that).*

*(H-26's other half, also landed: the store records the observation instant in
`coderadar.observed.main` (monotonic), and both ingest arms seed INV-11's nudge chain with it —
so a commit discovered afterwards that would open a start behind a recorded belief is nudged past
it and counted in `stamps_nudged`, never written backwards. What remains of P5d is the
commit-driven half: the ref watcher / post-commit hook, checkout switching the observation's
lineage after writing the left lineage's dirty files back to its head (D-4 a1), and startup
catching up on what changed while CodeRadar was off.)*

*(2026-11-13, slice B first half: an observation names its lineage. `ObservationSource`
carries `lineage`; a bare `update_file`/`remove_file` resolves it from
`coderadar.git.checkout` (the checkout record), else `main`. A branch observation moves its
whole flush onto the branch's row space — its own mint as W1, an inherited id as an overlay
version (W2) — retires through the branch's own act, persists no edges (P5a.4's rule), and
seeds only `coderadar.observed.{lineage}`, which the branch ingest arm reads beside the
trunk's. The report names the lineage. Two boundaries are choices, not omissions: an
index-now store ignores the record (one lineage by construction), and `analyze` refuses a
branch checkout by name — its flush, sweep and edge pass are trunk-shaped, so reindexing a
branch tree through it is exactly the assertion A-6 exists to prevent; the checkout sweep
will decide whether the act takes a lineage or stays a trunk act.)*

*(2026-11-13, the commit-driven half landed: `ops.sync_git` is A-8 as one op. It ingests the
trunk, then every branch whose tip has commits of its own (D-6 c's selection), records merged
names as commit addresses, and then sweeps observations: a checkout returns the left lineage's
recorded dirty files to *its* head (`source: "checkout"`) and records the carried edits on the
new lineage; a same-lineage sync records edits made while CodeRadar was off
(`source: "startup"`) and reverts, writing nothing for bytes the last sync already recorded.
Per-branch refusals ride the report instead of aborting the run, and a detached HEAD's sweep is
skipped with that reason — there is no lineage to name. The marks (`coderadar.git.checkout`,
`.head`, `.dirty` as `{path: sha256}`) are what the sweep reads next time; the dirty set is the
left lineage's edit list. `GitWatcher` polls HEAD and calls it (`watch --git` starts one);
`poll_once()` is public so a daemon can drive it without a thread. The sweep writes the store
and does not reload the graph — a checkout changes which tree the graph describes, so the caller
owns the reload. Not yet wired: the mutation path does not sync-first before its observation
(H-26's nudge keeps the chain ordered meanwhile), and `analyze` keeps its refusal for a branch
checkout until the reload step is the caller's by construction.)*

### P6b — Aggregation and vectors (no dependency)

As proposed: `open_items` over annotations, annotations and comments as embed targets (current
version only), the search-side join that answers "why" next to "what". Unchanged by this plan
except that its writes follow P5b.1's discipline.

*(2026-11-13, two of three landed; the third is deferred with its reason named. `open_items`
aggregates the unchecked items of live annotation bodies (§4's list grammar is what makes an
aggregate possible), scope = an id or an ancestor of one (`a.py` is the file, `a.py::C` reaches
`a.py::C.m`, `pkg/` the directory), current version only by construction — `concepts` holds one
row per id at its live body, so a superseded body's items are simply not there, and a retired row
contributes nothing. The join shipped on all three search paths (`search`, `search_similar`,
`search_hybrid`): each hit may carry `notes` — the annotations and comments anchored to it, to
an enclosing id or to its file — so a result says why beside what. **The vector half is
deferred**, and this is a choice, not an omission: annotation rows are concepts, not projection
entities, so a vector keyed by an annotation id is scored by `search_similar` and then dropped by
`materialize_scored`'s typed-map lookup (the note would crowd out a code hit and vanish); putting
notes in the projection puts them inside `graph_stats` and every entity universe — exactly the
INV-13 line P6a drew and pinned; and a parallel per-model table collides with
`model_tables_at_dim`'s one-model-per-store contract. The remedy is a deliberate decision about
note embeddings — a projection map with its own count semantics, or an engine-side multi-space
search (a Macrame ask, 0.23+) — and the relational join already answers the user-facing question.
Gates: 598 lib, pytest 1390/1/1xf.)*

### P8 — The data boundary (no dependency)

Export and import for annotations and comments, keyed by canonical target id, carrying their
version chains. A `rebuild` op that re-ingests from git into a fresh store and re-imports (A-9,
INV-18). This turns Q-9's remedy into a command, and it makes every future store change (a schema
step, a parser overhaul, a scrubbed history) a rebuild rather than a migration.

*(2026-11-13, closed. `export_annotations` writes one document — `coderadar.annotations.v1` —
with every row's id, kind, about, n, mint lineage and its whole chain (body, start, close,
retire, oldest first); `import_annotations` validates every row before its first write, replays
each chain oldest-version-first through the write path's own `ConceptUpsert`, then reads every row
back and compares it to the file before reporting success. Bodies are taken verbatim: re-linting
an import would let a newer linter rewrite stored history, which the boundary must never do. An id
that already exists refuses (naming it) unless `on_conflict="skip"`, which reports what it left
alone — a merge is explicit, never silent. `ops.rebuild` is the command: move the store aside
(`coderadar.db.pre-rebuild-<stamp>`, never deleted), export from the backup, re-ingest with the
migration's own D-6 c selection (the two ops now share `_lineage_plan`), import, verify. The op
needs a process that has not opened the store — the engine holds a store file for the life of the
process that opened it, so the move would refuse — hence a loaded graph refuses up front and the
CLI spelling is `rebuild-store`, a fresh invocation. `rebuild` itself is not the command name
because 0.13 removed a pre-0.13 alias of `reindex --full` by that name and the drift test forbids
teaching it: same concept, different act. INV-18 is `tests/test_p8_boundary.py`: a store with a
revised chain, a retired row and a comment is rebuilt in a subprocess, and the annotation chains,
the entity set and the file's history read identically afterwards. Gates: 596 lib, pytest
1377/1/1xf.)*

### P9 — Surfaces (follows each phase it exposes)

MCP read tools gain `at`/`recorded` with P5b.3 and sha addressing with P5b.4. Q-10's answer:
expose the ingest *report* (lineage cursors, ghost commits, coverage), never the write. CLI,
QUICKREF, SKILL and server instructions move in the same commit as the op, as the drift tests
already require.

---

## 7. Sequencing

| Order | Phase | Blocked by | Can run beside |
|---|---|---|---|
| 1 | P5b.0 truth tests *(done 2026-11-01; F2 and F4 flipped with the 0.22.0 uptake and the adoption heal, F1/F3/F5/F6 still `xfail`)* | — | everything |
| 2 | P5b.1 close-then-open *(closed 2026-11-11 — mechanism via D-312's one-write supersession (M6-g slice 1); INV-15 proved on the migration fixture (17 concepts / 21 versions) and a scratch self-host run (354 commits, 16,701 concepts / 33,641 versions), disorder 0 both; F2's sentinel fixed to both spellings after the proof caught its vacuous pass)* | — | P5b.3a, P6b, P8 |
| 2 | P5b.3a horizon + commit index *(closed 2026-11-12 — horizon + per-commit completion in `kv`, the git-history arm of every temporal read with its `basis`/`sha`/`horizon`, `BeforeHistory`/`BeforeHorizon`, `at=<sha>` with `dirty_at_commit`; F1 flipped. Two defects the tests caught: the first import's tail backfill clobbered the completion the loop had just written (now fill-only), and the derived completion ignored `concept_cuts`, so a branch's sweep landed after the instant the arm folds (the derivation reads cuts now). Gates: 589 lib, pytest 1324/1/3xf)* | — | P5b.1, P6b, P8 |
| 2 | P6b aggregation + "why" join *(closed 2026-11-13 — `open_items` (live annotation items, scope = id or ancestor, annotations only), the notes join on `search`/`search_similar`/`search_hybrid` (entity, enclosing ids and file), hardening through the `open-items` CLI/MCP/skill surfaces. Deferred by decision: notes as embed targets (concepts are not projection entities; `materialize_scored` drops them, and a notes map would cross INV-13's count line). Gates: 598 lib, pytest 1390/1/1xf)* | — | everything in order 2 |
| 2 | P8 data boundary *(closed 2026-11-13 — `export_annotations`/`import_annotations` (`coderadar.annotations.v1`: whole chains, verbatim bodies, validate-before-write, read-back verification, explicit conflict policy), `ops.rebuild` + `coderadar rebuild-store` (move aside → export from the backup → re-ingest → import → verify; one-shot per process by the engine's file lock), `_lineage_plan` shared with the migration. INV-18 test: 4 rows / 5 versions rebuild identically in a subprocess. Gates: 596 lib, pytest 1377/1/1xf)* | — | P6b |
| 2 | P5b.7 rename links *(closed 2026-11-12 — git detection at the default 50 %, `CommitPlan.renamed`, one row per id under `renamed:{old_id}` plus the reverse `became:{new_id}` key, `rename` in the INV-13 exclusions; reads follow the chain and mark `via_rename` (annotations/comments both directions, `history_of` across the chain, `find_entity` via `_core.rename_chain`). The score is `100` only for a byte-identical pair and `null` otherwise — git2 exposes no score. F6 flipped. Gates: 592 lib, pytest 1346/1/2xf)* | — | everything in order 2 |
| 3 | P5b.6 branch names + generations *(closed 2026-11-12 — D-6 c selection: one lineage per own-commit tip, every other name an address read through the commit index (`parent_drift` reported); D-5 generations: a rebase or rewrite starts `{branch}#n`, the trunk refuses `TrunkRewritten`, a tip moved onto the trunk refuses `BranchNowMerged`. Two defects the tests caught: the address fold ran on the name (Macrame refuses a lineage-less name) and `#` is illegal in kv keys (D-280) — keys hex-encode unsafe names behind a `:` marker, plain names unchanged. Gates: 591 lib, pytest 1338/1/3xf)* | P5b.3a | P5d, P5b.2 |
| 3 | P5d live tree *(observation writes closed 2026-11-13 — INV-12 narrowed, source-tagged rows, one instant per write, `observations` in the sync summary, F5 flipped; H-26's nudge seed in both ingest arms. Lineage routing landed 2026-11-13 — a bare write lands on the recorded checkout (`coderadar.git.checkout`), a branch observation moves onto the branch's row space (W1 mint / W2 overlay), retires through the branch's act, persists no edges and seeds only its own mark; `analyze` refuses a branch checkout by name. Gates: 595 lib, pytest 1365/1/1xf. Commit-driven half closed 2026-11-13 — `ops.sync_git` ingests the trunk and every branch with own commits, records addresses, sweeps a checkout (left lineage back to its head, carried edits on the new one) and startup (`source: "startup"`), marks `checkout`/`head`/`dirty`; `GitWatcher` polls HEAD (`watch --git`). Gates: 596 lib, pytest 1370/1/1xf. Not wired: the mutation path's sync-first (H-26's nudge covers it) and the caller-owned graph reload after a checkout.)* | P5b.1 | P5b.2 |
| 3 | P5b.2 adopt 0.22.0 *(closed 2026-11-11 — [patch] retired, the lock rides crates.io `0.22.0`; engine identical to the rev pin (no src/ diff 40719ac to tag); gates 589 lib + pytest 1303/1/4xf, scratch migration disorder 0)* | P5b.1 (U-1 released 2026-11-01) | P5d |
| 4 | P5b.3 valid reads (trunk) *(closed 2026-11-12 — `valid=` on `as_of`, `changes_between`, the `Snapshot` and the five `_core` reads; `resolve_valid_read` names the instant and `basis: "valid"`, lowered to Macrame's `reconstruct_as_of(AsOf::valid_at)`, trunk-only; a branch refuses `ValidBranchNeedsU2`, `at=` refuses `ValidAtNeedsP5b4`. `history_of` needs no flag (the spine is the valid axis) and `verify_history` samples recorded generations by design — both stated in §6. Gates: 592 lib, pytest 1353/1/2xf)* | P5b.2 | — |
| 5 | P5b.4 sha/ref addressing *(closed 2026-11-14 — `at=` resolves on both axes through one resolver: a sha via the commit concept's owner, a name via its own tip key, its lineage's tip, or its ingest cursor (a generation's only tip record). The two axes fold at different instants — recorded at `commit_done`, valid at the commit's own stamp — so the address carries both; the valid arm still refuses an address into a branch lineage (`ValidBranchNeedsU2`). `RefNeedsP5b4` and `ValidAtNeedsP5b4` retired. Six tests: name=sha, trunk name, rewrite+generation, unknown name on both axes, unknown sha, timestamp restatement) | P5b.3 | P5b.5 |
| 5 | P5b.5 out-of-order ingest *(slice 1 closed 2026-11-13 — the write shape: `upsert_concepts_bulk` accepts a closed, non-retirement version starting before the live open one when `valid_to <= live.valid_from` (the abutting chain), leaving the projection and the open belief untouched; the engine already supported it, the forward-only fallback was CodeRadar's. Malformed shapes still refuse before any write. **[2026-11-14] Slice 2 closed — the ingest half and the Doctrine VIII proof: `ingest_history(..., to=<belief start>)` walks a prefix window forward, writes each commit's rows closed at the file's next belief (the next in-window touch, else the store's own next start, else open), writes the boundary chunk's tree as the window's root, performs no sweep, edge write, cursor/stamp/tip/horizon move, and tombstones only where the id's life ended before the tip; six tests compare a full ingest against two newest-first chunks commit by commit (`tests/test_p5b5_window.py`).** **the release tree went CI-green 2026-11-13 at `2f50e38`** — all six jobs on both platforms, the first green run since 2026-10-29. The blocker was the store's contract, not P5b.5: a second `Database` on a live path is refused on POSIX and a mid-process close empties later reads, so `CodeGraphStore` hands out one engine per path per process (`bd353ac`, `613bd68`) and two horizon tests cross a process boundary (`2f50e38`). The cut landed as `4d3cf87` (versions, changelog, baselines, ARCHITECTURE §14.2), then a review of that tree fixed the registry's last race (registration under one lock), keyed it by the filesystem's own spelling with a lexical alias, added the macOS Python leg and a tag-vs-version gate before publishing (`1e37a4f`, `f383cc5`, `3d27232`; seven jobs green).)* | P5b.3 | P5b.4 |
| 6 | P5b.3 branch reads | U-2 | — |
| 7 | P5c lineage fidelity | U-3 (U-4's substance: D-315, 0.22.0) | — |
| — | P9 | each phase it exposes | — |

The critical path is upstream: U-1 gated the pin and is released (0.21.0, 2026-11-01); U-2 gates
branch reads, U-3 and U-4 gate P5c. U-4 is also what lets an ingested branch merge without losing
entities on the trunk (F4, H-28), so it is the next request, ahead of U-2 (Q-15). Macrame records
merge as its own decision under Doctrine III, so U-4 starts with that decision, not a design. Everything CodeRadar can do alone (P5b.0, P5b.1,
P5b.3a, P5b.6, P5b.7, P5d, P6b, P8) comes first, so the
upstream work never waits on CodeRadar and CodeRadar never idles on upstream.
*(2026-11-09 revised: U-4's substance is delivered — D-315 adoption ships in 0.22.0 without
taking the merge decision, so the next request is **U-2**, ahead of U-3; the unfiled upstream
path is U-2 then U-3.)*
*(2026-11-11 revised again: Macrame `v0.22.0` shipped and its `v0.23.0` plan **files U-2 and
U-3** — the upstream path is no longer unfiled, it has a cycle. The 0.22.0 uptake absorbed
two order-2 items' mechanisms, so the order to work in is now explicit. **(1)** The release-tail
doc sweep: the CHANGELOG's Unreleased section names neither the M6-g slices nor the K8 rounds,
and the P5b.1 row's story (draft replaced by D-312) lives only in the commit messages.
**(2)** P5b.1's formal close: the mechanism is adopted; what remains is the INV-15 proof on the
migration fixture and a scratch self-host run, then the row's status marker. **(3)** P5b.2's
formal close: switch the pin from the git rev to the published `macrame-db = "0.22.0"`, rerun
the gate bundle and the scratch migration, `verify_concept_intervals` clean. **(4)** P5b.3a,
the next build item — P5b.6, a release gate (D-7 d), waits on it; F1 flips here. **(5)** P5b.6
(branch names, addresses, generations). **(6)** P5b.7 (F6), P6b and P8, in any order beside. *(P5b.7 closed 2026-11-12; P8 closed 2026-11-13; P6b closed 2026-11-13, its vector half deliberately deferred with the reason recorded in its section.)*
**(7)** P5b.3's trunk valid reads once (3) lands; the branch leg of that phase waits for U-2.
*(Closed 2026-11-12 — the trunk leg shipped; the branch leg is P5b.3-branch, still U-2.)*
**(8)** P5d (F5) once P5b.1 closes. **(9)** The 0.15.0 release, once P5b.1, P5b.3a and P5b.6
have landed — it needs nothing from Macrame. What stays blocked on the 0.23 cycle: P5b.3's
branch reads (U-2), P5c's valid-axis cut and reads (U-3), F3's flip, P5b.4/P5b.5's full value.
Everything else runs beside the Macrame cycle — the sequencing law's own shape: CodeRadar
never idles on upstream.)*
*(2026-11-11 (3): item (2) closed — P5b.1's INV-15 proof ran green on both legs, disorder 0 both, F2's sentinel fixed; the table row and §6 P5b.1 carry the record.)*
*(2026-11-11 (4): item (3) closed — P5b.2's registry switch ran green across the bundle; the row carries the figures.)*
*(2026-11-12 (5): item (4) closed — P5b.3a landed and F1 flipped. The arm folds the last commit at or before the
read instant at that commit's completion, and every result now names its basis. Two ledger facts the tests forced
into the open: a backward reach (`now`) is the only instant that is strictly after a commit's last sub-generation
*and* before the next commit's first write, so the loop's own completion must survive the tail backfill (fill-only);
and a derived completion is wrong unless it reads `concept_cuts`, because a branch's sweep lands after its concept
flush. Next: item (5) P5b.6, then P5b.7/P6b/P8 beside, then the 0.15.0 release.)*

**Suggested releases.** 0.15.0: P6a, P6c, P5b.0, P5b.1, P5b.3a, P5b.6, and with them P5a's two
commands at store format 1 (D-7 d) — the first release in which a git-filled store answers every
past time. If 0.15.0 is cut before those three phases, it ships without the commands.
*(Revised 2026-11-01, 0.21.0 released; 2026-11-11: P5b.2 is now the 0.22.0 registry switch; 2026-11-13: P5d, P5b.7 and P8 landed before
any cut, so the next release carries them — the 0.16.0 lane below collapses into it, and 0.16.0 is P6b + P9. The
0.15.0 gate (P5b.1, P5b.3a, P5b.6) has been met since `1969fa6`; nothing blocks the cut but the choice of when.)* 0.16.0: P6b + P9. 0.17.0: P5b.3 (trunk),
P5b.4, P5b.5. Branch valid reads when U-2 exists; P5c when U-3 exists (U-4's substance rides
D-315).

---

## 8. The one ordering constraint that touches the existing store

Not migration planning, only a dependency to respect: **do not make a commit-time store the store of
record before P5d.** Until observation writes exist, a commit-time store blinds the watcher, ends
every mutation `ApplyIncomplete`, and hides every uncommitted edit (F5). The production migration of the self-host store waits for P5d; whichever of
Q-8's answers then applies (keep frozen, decided 2026-10-31) is unaffected.

**K8 — the post-merge re-ingest for hollow stores (P5a.5's step, spec'd 2026-11-09).** A store
whose trunk walk ran while the engine was pre-D-315 holds merged files as bytes and spine with
their concepts skipped, and its trunk cursor sits past those merge commits, so a resume never
heals it — and under the 0.22 pin the hollow state cannot even be produced by today's ops (a
trunk walk meeting a branch-born id either adopts inline, when the branch lineage is already
minted, or mints it as ordinary when nothing is minted yet). The carrier is therefore a
**named-commit re-walk, not a cursor rewind**: `reingest_merged_commits` (Rust FFI beside
`ingest_history`; a py `ops` wrapper; the migration op calls it once after the branch ingests)
walks the trunk's first-parent chain, keeps the same INV-11 nudge chain state, and processes
every commit — the hollow shape is per FILE, not per merge, and a post-merge trunk edit of a
hollow id was skipped exactly like the merge itself — only the files whose module ids are
hollow on the main lineage (`mint_lineage(id)` names another lineage → adoption at the
commit's own nudged stamp; a re-assert when the trunk spine already claims it, which is why a
later edit heals at its own commit rather than freezing at merge-day; no mint anywhere →
named `no-mint`, per §10 item 1 of the Amendment 2 spec), only files still present in HEAD's
tree (the absent ones are named `gone-by-head`, not half-minted). One bulk flush per commit;
scoped-edge persistence from an own-files-only cascade (the branch walk's rule, not a
head-state approximation — edges between a healed file and files outside the heal's set do
not resolve, so a healed file's link to an unchanged trunk file arrives the next time that
file's own ingest re-asserts it); the commit concept and the spine touched nothing (they
landed at the original walk); the `kv` cursor untouched. A second run re-adopts nothing (the
re-asserts land byte-identically and `filter_unchanged` drops them) — the heal is idempotent
by construction, and P-9's pricing counts its write volume under the same D-299 keep law. The unit tests are Rust-side (`kv_put_sync` forges the
past-the-merge cursor shape; py has no kv surface), the py layer takes the wrapper and the
report contract.

*(2026-11-09: built. Rust op `reingest_merged_commits` in `graph::ingest` beside
`ingest_history`, trunk-only and store-required, INV-12-bound; py wrapper
`ops.reingest_merged_commits`, no MCP tool (H-13); `migrate_to_commit_time` calls it once
after the branch ingests and reports it as `merge_heal`, its counts kept out of the
migration totals — the heal re-walks commits the trunk ingest already counted. Two build
notes the spec's wording narrows: the already-adopted pulse reads the TRUNK SPINE's claim
(`concepts` under either trunk spelling, an overlay version, a branch-dispatched log row),
not the mint row alone, because the adoption retouches nothing — `concepts` keeps the mint
lineage's row forever — and the already-adopted re-run names the file audit-side rather
than silently skipping; and the py ingest path writes the healed version with
`valid_from` at the merge's own stamp and `recorded_at` at the heal's day, the same
bitemporal split every ingest bulk write carries (§H-21's now-stamps, F1's truth). The
fixture-built test order mirrors the operator order: the branch ingest runs BEFORE the
merge lands, because once main reaches the branch tip `fork_point` slides past the branch
tip and a post-merged branch ingest walks nothing.*

*(2026-11-10: the review round. Finding 1 — once the trunk adopted a merged entity, the
NEXT trunk commit editing it was refused and the ingest stalled — the trunk arm of the
write decision read the write's own state from the `concepts` table, where an adopted id
holds no row; it now reads Macrame's per-branch resolution for `main` at each write's own
stamp, the same read a branch writer runs. Finding 2 — the heal re-read only merge commits
and its healed set froze at merge-day content — the whole first-parent chain walks, and a
healed id's later edit re-asserts at its own commit through the fixed read. Finding 3 —
the dry run promised "would adopt" for a file the trunk later deleted while the real run
said deleted — both runs now classify per file before anything writes and carry the same
verdict, `gone-by-head` under the dry run's `dry-run:` prefix. Finding 4 — a commit counted
done when its bulk flush refused — the counters only move on a settled flush while `head`
still names the commit the run reached. Finding 6's wording, not fields: the healed set's
edges to files outside the heal's set are not written and nothing in the JSON announces
that; the py and lib report contracts state it. Same day, one shape deeper: the trunk
arm's resolution turned out to read Macrame's overlay-scan slot-swap — the resolution row's
`extra` answered the model's slot (Macrame `concept_lineage.rs::overlay_scan`'s consumer),
so the unchanged filter believed every resolved row changed and a re-assert refused as
same-start; the slot fix lands upstream (40719ac) with its own reader-side pin.)*

---

## 9. Hazards (continue H-14..H-20)

| ID | Hazard | Mitigation |
|---|---|---|
| H-21 | **The two-flush window.** Between the close flush and the open flush a concept is valid nowhere | P5b.1: shared stamp, cursor after the open flush, resumable; U-5 removes it *(2026-11-09: U-5 is D-312, ships in 0.22.0; P5b.1 builds on it)* |
| H-22 | *(Withdrawn 2026-10-31 with the overlay; see H-26.)* | — |
| H-26 | **Observations ahead of a late commit.** A commit fetched from elsewhere can carry a committer time earlier than an edit already observed on the same lineage, which would open a start behind the open one | Sync before every observation write (§3.1.3); a commit that still lands behind is nudged after the latest start and flagged `stamp_nudged`, as INV-11 does for ties *(2026-11-13: the nudge seed exists — `coderadar.observed.main`, read by both ingest arms; sync-first arrives with P5d's commit-driven half and P5b.5 lifts the constraint by folding the valid axis instead of appending)* |
| H-27 | **The horizon is a `kv` row.** Lost, every past read falls back to the recorded arm and answers empty again | Rebuildable from the ledger (the first `commit:` row per lineage); `verify_history` reports a missing horizon on a git-filled store as an error, not a warning |
| H-28 | **Merging an ingested branch before U-4.** Files the branch minted reach the trunk as bytes without entities; the trunk's symbol reads miss them | The ingest report lists them (`cross-lineage-id`) and the merge commit's summary counts them; a trunk read for such a path reports `minted_on: <lineage>` instead of answering absent; U-4 requested next to U-1 (Q-15) *(2026-11-09: 0.22.0's D-315 resolves the substance — the trunk asserts its own first version and reads answer; this mitigation is the wording for pre-overlay pin stores)* |
| H-29 | **A rename git does not detect.** Below the similarity threshold, or a rename combined with a large edit, stays a delete plus an add, and annotations stay on the old id | Annotations on a closed id are listed by `annotations --orphaned`; a manual `link-rename old new` writes the same link row P5b.7 writes |
| H-30 | **Per-instant answers for backfilled pairs (0.21.0's D-303).** Once P5b.5 writes out of order, a marked pair's recorded read at an instant before a row's `valid_from` does not see that row as state; a commit whose committer clock runs ahead of the ingest clock would not be state until real time reaches it | INV-11's nudge already keeps a commit's start at or after its lineage's latest start; P5b.5 additionally clamps a start beyond its own recorded instant and flags it `stamp_nudged` |
| H-31 | **The v24 ladder is one-way.** A 0.21.0 build upgrades a store on first open; a 0.14.0 (pin 0.19.1) build then cannot read it | P5b.2 notes it; the frozen pre-migration store (Q-8) is read only by a 0.14.0 build or from a copy; the 0.16.0 release notes say it |
| H-23 | **Valid-axis branch reads before U-2.** Answering a branch question from a trunk-only fold is F-6's failure in a new place | P5b.3 refuses branch reads on the valid axis, naming U-2 |
| H-24 | **Lineage generations multiply.** Every rebase adds a lineage, and lineages are append-only | Generations are cheap rows; D-5's resolution keeps the name pointing at the newest; measure on a rebase-heavy fixture before P5c ships |
| H-25 | **Clock-stamped annotations in a commit-time store.** A valid-axis read at a commit instant never sees an annotation written later, which is right, and is surprising | Document it in the read's rustdoc and QUICKREF; annotation reads take no `at` in v1 |

---

## 10. Done when

A store built and kept current by git answers, on every lineage: what the code was at any commit or
date, what CodeRadar believed at any instant (recorded axis, never empty for a time git has history
for, §3.1), and what the code is right now including uncommitted edits (recorded observations). A branch sees its parent only up to its merge-base,
a merge brings merged files into the receiving lineage, a rename carries annotations and history
to the new id, a rebase leaves the old history readable,
and ingesting history in any order yields the same valid-axis answers. Every concept has at most one
open interval. The store can be rebuilt from git plus an annotation export, and the rebuild answers
identically. All P5b.0 truth tests pass without `xfail`.

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
  *(Revised 2026-10-31 with Q-15: U-4 now goes with U-1, ahead of U-2.)* *(2026-11-01: U-1
  shipped in 0.21.0 from Macrame's own plan; U-2..U-5 are still unfiled, and U-4 is next.)*
  *(2026-11-09: 0.22.0 shipped U-5 (D-312) and U-4's substance (D-315 adoption, marked
  "delivered through adoption, not as the merge"); U-2 and U-3 remain unfiled, and **U-2 is
  next** — P5b.3's branch reads.)*
  *(2026-11-11 (2): filed — Macrame's `v0.23.0` plan takes U-2 as D-317 and U-3 as D-318; the filing-order recommendation above is history, §7's 11-11 paragraph is the operative order.)*
- **Q-15 — Merging an ingested branch before U-4** (F4, H-28). **a** request U-4 now, next to U-1,
  and until it lands accept the gap with the H-28 reporting · **b** an interim engine rule letting
  the trunk mint a branch-minted id under its own ownership, the branch's copy kept as history (also
  a Macrame change, barely cheaper than U-4) · **c** ingest no branch until U-4 exists (reverses
  D-6 c and blinds every branch read). Recommendation: **a**. On this repository, `dev` merging to
  `main` is the first time it bites. *(2026-11-01:)* Macrame's Road to 1.0 (§15.4, §16) records
  merge as "its own decision" because Doctrine III has no neutral superseder. For CodeRadar's case
  there is one — git's merge commit says which content wins — so the U-4 request should propose
  that the caller names the superseder (the merge commit), not the engine. *(2026-11-09: 0.22.0's
  D-315 took a fourth path — the receiving lineage asserts its own first version and the provenance
  row stays at the mint, which for CodeRadar's ingest answers Q-15 without a Doctrine III
  decision; the superseder question rides any future merge op, unfiled.)*
- **Q-16 — Rename detection threshold** (P5b.7). git's default similarity is 50%. Lower finds
  more renames and links more wrong pairs; a wrong link is visible (`via_rename` names the commit
  and similarity) and correctable by a later link row. Recommendation: git's default, the similarity
  recorded on every link, and H-29's manual link for the rest.
