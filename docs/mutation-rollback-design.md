# Mutation apply semantics: guarded rollback and truthful synchronization

**Status:** implemented on `dev` (decision DR-35). This revision is the
as-built contract: guarded compensation plus verified synchronization for
`replace_body`, `update_signature`, `rename`, and `create_entity`.
**Target decision:** DR-35 — guarded mutation compensation + explicit
synchronization outcomes.
**Landing:** design `0c2493a`; implementation `4d5cb1c`; version bump
`8fa427b`; OKFgraph register concept `coderadar-decision-register` row
DR-35. Verified: Rust 503 tests, Python 1164 passed + 1 skipped, round-2
battery 123/123.
**Closes:** the carried semantics note in `docs/CODERADAR_BUGS_QUIRKS.md` #1,
and corrects the stale rollback descriptions in `docs/ARCHITECTURE.md` §§4/9.
**Scope:** `replace_body`, `update_signature`, `rename`, and `create_entity`,
from `MutationEngine::apply` through `CodeGraph.apply`, ops, MCP, CLI, and the
named `ToolRouter` seam.

---

## 1. Corrected baseline

The first draft inherited an incorrect premise from the v0.12 disposition:
**automatic rollback on introduced syntax errors already exists.**

- Commit `24c3f46` added stale-write rejection and automatic rollback on tainted
  updates.
- `MutationEngine::apply` currently performs policy → stale checks → backups →
  per-file temp+rename writes → post-write parse verification → restore on
  failure (`core_indexer/src/mutation/mod.rs`).
- Rust and Python `test_apply_rolls_back_tainted_update` both pin restoration of
  the original source.
- `ARCHITECTURE.md:332-335` therefore contradicts itself when it describes
  restore-on-failure and then says auto-rollback still needs implementation.
  The MCP description at `mcp/server.py:768` also overpromises that disk and
  graph update “atomically.”

The remaining problem is **not adding rollback**. It is defining and hardening
an existing compensation protocol so that:

1. rollback cannot silently destroy another writer's work;
2. a reported status tells the truth about disk, graph, and recovery state;
3. multi-file failures have one coherent whole-plan policy;
4. graph/ledger synchronization does not depend on an optional watcher;
5. failure recovery never deletes the only usable pre-image.

## 2. Review findings and where this design resolves them

| Finding | Evidence / consequence | Resolution |
|---|---|---|
| Blind restore can overwrite external edits | `rollback_all(&backups)` restores every backup, even for files never installed, with no current-content hash check (`mutation/mod.rs:2237,2277,2314,2351-2354`) | Per-file attempt state; restore only own installs, and only when the current hash still equals this attempt's installed hash (§5.5) |
| Restore is non-atomic and backup deletion is unconditional | `fs::copy` restores in place; copy and delete errors are discarded | Atomic restore + read-back verification; delete a backup only after its restored state is verified (§5.5) |
| `RolledBack` can be false | Restore failure currently still returns `RolledBack` | Add `RollbackIncomplete`; `RolledBack` requires complete verified restoration plus required graph sync (§4) |
| `backup_path` can name a deleted file | Phase 4 returns `backups.first()` after `rollback_all` deleted it | Recovery paths must exist; retain backups on incomplete recovery, clean them on verified success (§§5.5, 7) |
| `Applied` does not prove graph/ledger sync | `CodeGraph.apply` ignores `UpdateReport` values and swallows exceptions (`__init__.py:1052-1077`); concept/edge persistence errors are discarded (`projection_ops.rs:895-902`) | Explicit sync summary, source-hash checks, `ApplyIncomplete`, and propagation of persistence failures (§§6–7) |
| The first draft misstated the resync input | The facade iterates `plan.affected_files`, not `result.files_written` | Sync set is the canonical union of affected files and files installed/restored by the attempt (§6) |
| Watcher convergence is not guaranteed | Watching is explicitly started (`cli.py watch`), not an automatic MCP property; write guards are process-local | Public apply performs synchronous resync; watcher events remain an additional external-edit path, never a correctness dependency (§6) |
| DR-11 was the wrong ledger reference | DR-11 is embeddings; DR-7 closed `MutationLog` by deletion | No mutation-attempt rows in the temporal code ledger. Source facts/generations remain immutable history; any audit log needs a separate decision (§6.4) |
| Old G10 was factually wrong | Empty `expected_hash` is rejected for every non-`create_entity` plan (`mutation/mod.rs:478-486`) | Delete that fallback claim. Keep the create exception, but guard every file with a full-file pre-hash, including insertion-only creates (§5.1) |
| Pre-rename re-hash is not mutual exclusion | The check/rename race remains; fixed tmp/backup names collide across attempts | Unique per-attempt artifacts, per-root serialization, and an explicitly bounded cross-process guarantee (§5.7) |
| Verification failure can look clean | Missing grammar, parser failure, and post-write read failure all become `None` | Tri-state verification; unavailable verification after installation triggers compensation, never silent `Applied` (§5.4) |
| Result shape did not cover multi-file outcomes | One global reason cannot describe some restored / some conflicted / some untouched files | Structured primary failure + per-file outcomes + sync summary across the full FFI/Python/surface path (§7) |
| “Single-file safe” / “multi-file atomic” were too broad | Crash orphans and concurrent attempts remain possible; current MCP text says atomic | Atomic visibility per file only; whole-plan compensation on ordinary failure; no crash or cross-process transaction claim (§§4, 10) |

## 3. Architecture constraints

These are design inputs, not optional implementation details.

1. **Honesty over coverage.** A missing verification or synchronization
   capability is reported; it never becomes plausible success.
2. **One operation across surfaces.** Dicts cross PyO3 (DR-4), while the public
   Python result and MCP/CLI renderers must preserve the same outcome (DR-17).
3. **Macrame owns time.** The ledger holds code facts and source generations.
   It is not an operation journal, and rollback must not rewrite history.
4. **History is bytes, facts are rows.** A restored file indexed after rollback
   is a new generation containing the old bytes, not deletion of an earlier
   generation.
5. **The facade owns post-apply refresh today.** The Rust disk engine does not
   itself mutate the projection; `CodeGraph.apply` performs two-pass incremental
   updates for multi-file plans and invalidates derived embeddings.
6. **Watching is optional.** No correctness postcondition may depend on a
   watcher running, observing an event, or sharing the same process.
7. **The trust boundary is the FFI.** Policy and disk safety cannot exist only
   in a Python caller; direct `apply_mutation` callers receive the same guards.

## 4. Proposed public contract

### 4.1 Operation guarantees

- **Atomicity:** individual file replacement/restoration uses same-directory
  temp+rename for atomic visibility. A multi-file plan is **not** a filesystem
  transaction and is not crash-atomic.
- **Ordinary failures:** once any file has been installed, a later write or
  verification failure requests compensation for **every file installed by this
  attempt**. Multi-file rename does not degrade into independent partial commits.
- **Ownership:** compensation may restore only bytes installed by this attempt,
  and only while the current bytes still match this attempt's installed hash.
- **Recovery:** a backup is deleted only after the corresponding applied or
  restored state has been verified. Any incomplete recovery retains a real
  backup and reports its path.
- **Synchronization:** public apply does not claim success until the required
  graph refresh has succeeded for the expected source bytes. Synchronization
  failure after a valid disk apply does **not** trigger source rollback: the
  result is `ApplyIncomplete`, with a reindex/update remedy.
- **History:** rollback never retracts or deletes historical observations merely
  to make an attempt appear not to have happened.

“Current content” always means **at the operation's last verified read**. No
portable API can prevent another process from changing a file after that read.

### 4.2 Status meanings

| Status | Required meaning |
|---|---|
| `Applied` | Every edit was installed, post-write verification passed, and required graph/persistence sync completed for the expected installed bytes |
| `RolledBack` | The operation did not keep plan bytes. Every file installed by the attempt was restored and read-back verified; any required restored-state graph sync also completed. Early read/prepare/backup failures may use this status with zero installed files for compatibility |
| `RejectedPolicy` | Policy refused the plan before target-source writes. Policy evaluation may still canonicalize paths or inspect git state |
| `RejectedStale` | Stale content was detected and no plan bytes remain. A late stale detection may follow successful verified compensation; the per-file report preserves that distinction |
| `ApplyIncomplete` | Disk apply reached a valid/accepted state, but the complete apply postcondition was not met—for example graph sync or persistence failed, or a concurrent modification was observed during sync. Do not blindly retry the old plan |
| `RollbackIncomplete` | Compensation or restored-state synchronization could not be proven complete. This includes restore failure, current-content conflict, unknown final bytes, and post-rollback sync failure. Requires the structured recovery action, not an identical retry |

These are **public facade statuses**, not states the disk engine can determine on
its own. `MutationEngine` must return an internal `DiskMutationResult` with a
`disk_status` (`Installed`, `Restored`, `NotInstalled`, `RejectedStale`,
`RejectedPolicy`, or `DiskIncomplete`). `apply_mutation` exposes that field as
`disk_status`; `CodeGraph.apply` constructs the final public `status` only after
synchronization. Direct FFI callers therefore see an honest disk outcome, not a
premature final `Applied`. The old boolean `applied` and substring-based status
mapping are removed at this boundary.

`RolledBack` with non-empty restore errors is forbidden. `Applied` with failed
or unattempted required sync is forbidden. These become constructor/test
assertions, not merely renderer conventions.

### 4.3 Dry run and planning

Dry-run behavior, diff generation, `unverified_sites`, call-site cascades, and
policy planning checks remain unchanged. Operational failures after an apply
request return a structured result; they are not mislabeled as syntax errors.

## 5. Disk-attempt design

### 5.1 Attempt and per-file state

`apply` generates its own attempt id (do not trust a caller-supplied/empty plan
id). Temporary and backup siblings include that id and remain in the target
directory so rename stays on the same filesystem:

```text
<file>.coderadar-<attempt_id>.tmp
<file>.coderadar-<attempt_id>.bak
```

Each target file has an internal attempt record equivalent to:

```rust
struct FileAttempt {
    file: String,
    original: Vec<u8>,
    pre_hash: String,                 // full file, always recorded
    prepared: Vec<u8>,
    prepared_hash: String,
    installed: bool,
    backup_path: PathBuf,
    state: FileMutationState,
    final_hash: Option<String>,
    error: Option<MutationErrorDetail>,
}
```

The full-file hash is an optimistic-concurrency guard, separate from the
planner's byte-span `expected_hash` and from Macrame's SHA-256 blob identity.
It closes the `create_entity` gap: an empty insertion span cannot prove that
the surrounding file stayed unchanged, but the full-file pre-hash can detect
such a change.

Per-file public states are:

- `Untouched` — this attempt never installed the file;
- `Installed` — plan bytes were installed and not subsequently restored;
- `Restored` — the original bytes were restored and read-back verified;
- `ExternallyChanged` — current bytes no longer match this attempt's install;
- `RestoreFailed` — restoration was attempted but failed;
- `Unknown` — the final content could not be established.

### 5.2 Phase 0 — policy

Keep policy at the Rust trust boundary: enabled flag, edit limit, clean-git
option, root confinement, deny-wins allow-list, and non-empty span hashes for
all non-create tools. No target source is written. The old phrase “nothing is
read” is narrowed because policy may canonicalize paths or inspect git.

### 5.3 Phases 1–3 — read, prepare, back up, install

1. **Read and prepare every file before installing any file.** Record original
   bytes and full-file pre-hash; verify every non-empty span hash; apply all
   edits in memory. Any prepare error occurs before plan bytes reach disk.
2. **Back up every target.** Write/copy the unique backup and verify its hash
   against `pre_hash`. Backup failure cleans only this attempt's unneeded
   artifacts; no restore is attempted for a file never installed.
3. **Late stale check.** Immediately before installing each file, re-read it and
   compare its full hash with `pre_hash`. A mismatch is `stale_content`, for
   create as well as hash-guarded edits.
4. **Install atomically.** Write the prepared bytes to the unique temp sibling,
   close it, then rename over the target. Only a successful rename sets
   `installed=true` and the installed hash.
5. **Watcher suppression.** Register the expected prepared hash immediately
   before rename, rather than after it, so there is no unsuppressed post-rename
   window. An event observing old/external bytes does not hash-match and is not
   dropped. Clear the normalized guard key on failed installation.

The late check narrows the external-writer race; it is not claimed to eliminate
it (§5.7).

### 5.4 Phase 4 — post-write verification

For every installed file:

1. Re-read disk and compare the full hash with the installed hash. A mismatch
   is `concurrent_modification`, not proof that this attempt introduced a parse
   error. Compensation must not overwrite that file.
2. Parse original and installed bytes with the file's tree-sitter grammar.
3. Produce one of:
   - `Passed` — no clean→error transition was introduced (including the
     documented pre-existing-error case);
   - `IntroducedError(SyntaxDiagnostic)` — original parsed cleanly and installed
     bytes contain an error;
   - `Unavailable { stage, message }` — grammar/parser/read capability failed.

`IntroducedError` and `Unavailable` both request whole-plan compensation.
`Unavailable` must never serialize as a clean verification. There is no
semantic/type/import verification in this design.

### 5.5 Phase 5 — guarded, atomic compensation

When compensation is requested, process every file with `installed=true`:

1. Re-read the target.
2. If its hash differs from this attempt's installed hash, mark
   `ExternallyChanged`; **do not write it** and retain its backup.
3. If it still matches, restore the original bytes through a fresh unique
   temp+rename. Do not restore with an in-place `fs::copy`.
4. Re-read and require `final_hash == pre_hash` before marking `Restored`.
5. Delete the backup only after verified restoration. On conflict, restore
   failure, or unknown final state, retain the backup and return its real path.
6. Never restore a target that was only backed up or prepared but not installed.

If every installed file is restored, the disk outcome is complete. Otherwise it
is `RollbackIncomplete`; other restorable files should still be restored, with
per-file outcomes preserving exactly what happened.

Backups belonging to untouched or successfully restored files may be cleaned.
Backups needed for incomplete recovery must survive result construction and
process exit. No automatic orphan scanner or startup cleanup is introduced.

### 5.6 Success cleanup

After every installed file passes verification, remove this attempt's backups.
A later graph-sync failure becomes `ApplyIncomplete`; the recovery action is to
repair synchronization, not to restore old source. Temporary files from failed
writes may be removed when they are no longer needed for diagnosis; backup
retention follows §5.5, never a blanket cleanup.

### 5.7 Concurrency boundary

- Add a process-wide per-root **disk-attempt mutex** in Rust so direct FFI
  attempts cannot share temp/backup artifacts or interleave the disk phases.
- Add a process-wide per-root **operation mutex** shared by all Python
  `CodeGraph` instances, held across disk apply + facade synchronization. An
  instance-local lock is insufficient because the Rust graph is process-global.
- Define lock ordering before implementation; disk/operation locks must not be
  acquired while holding a conflicting `GLOBAL_GRAPH.write()` guard.
- Cross-process filesystem compare-and-swap is not available here. Unique
  artifacts prevent artifact collision and hash checks detect observed races,
  but an external editor that does not cooperate with CodeRadar can still win a
  sufficiently narrow check/rename race. This is documented, not called atomic.
- No cross-process lock, journal, or watcher dependency is added in this slice.

## 6. Graph, persistence, and history semantics

### 6.1 Synchronization input and ordering

For a disk-`Applied` result or a completely restored result that installed at
least one file, the facade synchronizes:

```text
canonically_deduplicated(plan.affected_files ∪ attempt.installed_or_restored_files)
```

This preserves the existing reason for using `affected_files` (call-site and
re-export resolution) while ensuring every actually touched file is covered.
It runs the existing second pass for multi-file plans so cross-file resolution
converges. A rejection with zero installed files needs no sync.

Do not run blind “success” synchronization after an incomplete/conflicted disk
rollback and then claim the graph is correct. Report sync as `NotAttempted` or
`Unknown`; after manual recovery, `update_file`/`reindex` is the remedy.

### 6.2 Prove what was synchronized

`UpdateOutcome` / `UpdateReport` gains the full `source_hash` of the bytes
actually parsed. For files installed or restored by the attempt, the facade
compares that hash with the expected final hash (installed or original). A
mismatch is `concurrent_modification`, not successful synchronization.

The facade aggregates every report instead of discarding it:

- operational update exception / `fully_applied=false` caused by an update
  error → sync failure;
- recovered parse (`Partial`/`Tainted`) → explicit partial sync;
- concept/edge persistence error → persistence failure;
- framework-route refresh or derived-data invalidation failure → explicit
  warning or partial state according to the reported capability, never silent;
- storeless operation → persistence `NotApplicable`, not failure.

`projection_ops.update_file` must stop discarding concept/edge persistence
errors when they are needed for this result. Blob policy remains separate:
configured `Disabled`/oversize/excluded outcomes are notices, while an actual
store error must be surfaced rather than hidden in stderr. This does not turn
blob storage into a mutation-attempt journal.

### 6.3 Overall status after synchronization

- Disk applied + complete expected-byte graph/persistence sync → `Applied`.
- Disk applied + partial/failed/conflicted sync → `ApplyIncomplete`; leave the
  valid source in place and name `update_file`/`reindex` or re-plan as remedy.
- Complete disk rollback + complete restored-state sync (or zero files
  installed) → `RolledBack`, or `RejectedStale` when stale was the primary
  refusal.
- Complete disk rollback + failed/partial restored-state sync →
  `RollbackIncomplete`, with per-file disk states still saying `Restored`.
- Incomplete disk rollback → `RollbackIncomplete` regardless of sync; never
  upgrade it because one later operation happened to succeed.

### 6.4 Ledger and embeddings

- DR-7 stays closed: **no `MutationLog` and no mutation-attempt facts** are
  added to the Macrame code ledger. `attempt_id` is diagnostic result metadata,
  not a temporal assertion.
- Rollback resync uses ordinary `update_file` assertions and source blobs. It
  may create a new recorded generation containing the original bytes. If a
  watcher or another caller indexed transient broken content, that observation
  remains history; rollback does not delete it.
- Derived embedding invalidation remains part of public post-apply processing
  for applied/restored files. Its outcome belongs in the sync summary. The
  implementation must audit the DR-11 per-model-table path rather than assuming
  that clearing an in-memory vector invalidates persisted vectors.
- A separate operational audit log is out of scope and would require its own
  decision, retention policy, and surface.

## 7. Result and surface contract

### 7.1 Structured result

Illustrative public result shape; the internal disk outcome is mapped into it
only after synchronization. Names may be adjusted during implementation, but
the information content is required:

```rust
struct MutationResult {
    status: MutationStatus,
    attempt_id: String,
    failure: Option<MutationFailure>, // stable stage + code + human message
    files: Vec<MutationFileResult>,   // includes untouched/restored/conflicted
    files_written: Vec<String>,       // compatibility projection, see below
    sync: MutationSyncSummary,
    syntax_errors: Vec<SyntaxDiagnostic>, // actual syntax diagnostics only
    backup_path: Option<String>,      // deprecated single-file compatibility
}

struct MutationFailure {
    stage: MutationStage, // policy/read/prepare/backup/write/verify/restore/
                          // graph_sync/persistence/derived_data
    code: MutationFailureCode,
    message: String,
}

struct MutationFileResult {
    file: String,
    state: FileMutationState,
    pre_hash: String,
    installed_hash: Option<String>,
    final_hash: Option<String>,
    recovery_path: Option<String>,
    error: Option<MutationErrorDetail>,
}

struct MutationSyncSummary {
    state: SyncState, // NotNeeded | NotAttempted | Synchronized | Partial | Failed
    attempted_files: Vec<String>,
    updated_files: Vec<String>,
    errors: Vec<MutationErrorDetail>,
    warnings: Vec<MutationErrorDetail>,
}
```

Rules:

- `files_written` is derived, not independently fabricated. It contains files
  known at the last verification to retain this attempt's plan bytes. It is
  empty after complete rollback/rejection, and non-empty on `ApplyIncomplete`
  when plan bytes remain. `files` is authoritative for ambiguous cases.
- `backup_path` is `None` on complete apply/rollback/rejection. On incomplete
  recovery it may expose the first existing recovery path for old callers;
  `files[].recovery_path` is the multi-file source of truth. Python receives
  `None`, not an empty-string sentinel.
- `syntax_errors` stops carrying policy, backup, I/O, and restore failures.
  Those use `failure` and per-file/sync errors with stable machine codes.
- The existing Rust `ReindexSummary` is removed from this result or explicitly
  deprecated internally: it never crosses the FFI and currently contains
  placeholder/zero values. The real sync summary replaces it.

### 7.2 Propagation requirements

Every new field must deliberately traverse:

```text
MutationEngine::apply (disk_status)
  → apply_mutation PyO3 dict
  → CodeGraph.apply (final status) / MutationResult dataclass(es)
  → ops._edit
  → render.mutation_applied and CLI --format json
  → mutation/tool_router.py
  → bundled skills and surface/parity tests
```

Python additions have defaults so existing programmatic `MutationResult(...)`
construction remains source-compatible. The status `Literal` includes both
incomplete statuses. Status mapping uses explicit values, not substring matching.

### 7.3 Human and automation behavior

- **Renderer:** “Graph updated: yes” only for `sync=Synchronized`. Distinguish
  disk restored / graph stale, source kept / sync failed, and manual recovery.
  Print every existing recovery path for incomplete rollback.
- **Python:** expected operational outcomes return `MutationResult`; interface
  misuse and unexpected FFI failures may still raise. `ops._edit` no longer says
  “nothing was written” when it means “no plan bytes were kept.”
- **MCP:** text contains the same status/failure/sync/recovery facts. Remove the
  unqualified “writes the file AND updates the graph atomically” description.
- **CLI:** JSON retains the complete structure. After emitting the result,
  non-zero exit statuses are proposed: 0 `Applied`; 1 `RolledBack` and both
  incomplete statuses; 2 stale/policy refusals. Dry runs remain 0. This is a
  deliberate CLI behavior change to pin in the round-2 battery.
- **ToolRouter:** `success` is true only for `Applied`; it propagates failure,
  per-file, and sync information rather than reducing everything to one string.

### 7.4 Recovery guidance

| Outcome | Agent action |
|---|---|
| `RejectedPolicy` | Change the plan or configuration; do not retry unchanged |
| `RejectedStale` | Re-read current source and produce a fresh plan |
| `RolledBack` after syntax/verify failure | Fix the replacement or verification capability; do not resend identical bytes |
| Early I/O `RolledBack` | Retry only when the specific error is plausibly transient; permissions/disk-full are not automatically “retryable” |
| `ApplyIncomplete` with sync failure | Keep source, repair the named store/update problem, then `update_file`/`reindex`; do not blind-apply the old plan |
| Incomplete with concurrent modification | Treat current disk as another writer's work; re-read and re-plan |
| `RollbackIncomplete` | Stop automatic retries, inspect per-file states and retained backups, recover manually, then resync |

## 8. Implementation plan

Land as reviewable vertical slices; do not release the feature until all slices
are green. No version bump for this design document. Implementation follows
DR-26's feature-bump rule and registers DR-35 in the same session.

### Slice 1 — synchronization truth and public result scaffold

**Files:** `types.rs`, `graph/projection_ops.rs`, `lib.rs`,
`py_agent/src/coderadar/__init__.py`, `ops.py`, `render.py`, `cli.py`,
`mutation/tool_router.py`, associated tests.

1. Add `source_hash` and structured operational/persistence outcomes to
   `UpdateOutcome` / `UpdateReport`.
2. Propagate concept/edge store failures needed by mutation results; retain
   configured blob skips as notices.
3. Replace ignored `CodeGraph.apply` reports/exceptions with an aggregate sync
   summary, including the multi-file second pass and expected-byte checks.
4. Introduce the internal `disk_status` and expanded public statuses/result
   dataclasses/dicts with defaults. Map existing disk outcomes honestly before
   adding the new disk machinery.
5. Update renderer, CLI JSON/exit behavior, MCP text, and ToolRouter in the same
   slice so no surface continues to print unconditional success.

**Exit tests:** forced update and store failures produce `ApplyIncomplete`;
restored-state sync failure produces `RollbackIncomplete`; renderer never says
graph updated on partial/failed sync; JSON and Python carry identical statuses.

### Slice 2 — guarded disk attempts and verification

**Files:** primarily `mutation/mod.rs`, with focused support in
`mutation/edit.rs` / `write_guard.rs` only if needed.

1. Add attempt ids, unique artifacts, full-file hashes, and `FileAttempt` state.
2. Prepare all files before installation; verify backups; add late full-hash
   stale checks for every tool, including create.
3. Replace blanket `rollback_all` with installed-only, hash-guarded,
   temp+rename restoration and read-back verification.
4. Retain and report backups on every incomplete recovery; clean them only on
   verified success.
5. Implement tri-state post-write verification and route unavailable checks to
   compensation rather than `Applied`.
6. Populate per-file states/final hashes and derive `files_written`.

**Exit tests:** the complete disk matrix in §9 passes on Windows and Ubuntu,
including deterministic external-edit and restore-failure injection.

### Slice 3 — serialization and watcher boundaries

**Files:** `mutation/mod.rs`, `lib.rs` lock wiring, Python facade lock registry,
`fs/watcher.rs`, `mutation/write_guard.rs` as required.

1. Add and document per-root disk and operation mutexes with deterministic lock
   ordering.
2. Move write-guard registration before rename and normalize guard clearing.
3. Prove two same-process attempts cannot share artifacts or interleave disk
   phases; prove an event for external bytes is not suppressed.
4. Document the remaining cross-process check/rename race without weakening the
   no-blind-restore guarantee.

**Exit tests:** barrier/hook-based concurrency tests (no sleeps) plus existing
watcher and mutation suites; no deadlock with analyze/update/reindex.

### Slice 4 — documentation, dogfood, and release gates

1. Correct `ARCHITECTURE.md` §§4/9, `CodeGraph.apply` docs, and the MCP tool
   descriptions; update BUGS_QUIRKS #1 disposition with the final evidence.
2. Update `docs/open-items.md` item 6, CHANGELOG Unreleased, bundled MCP/CLI
   skills (status and recovery matrix), and this design with landing hashes.
3. Add DR-35 to `ARCHITECTURE.md` §16 and the OKFgraph decision register in the
   same session; apply DR-26's version policy.
4. Extend `battery_round2.py` for CLI exit codes and surface parity. Run the
   manual mutation battery only against its throwaway demos; do not make the
   destructive battery a CI job.
5. Run full Rust + Python suites, `cargo fmt --check`,
   `clippy --all-targets -- -D warnings`, `ruff check`, and the CI dogfood job.

## 9. Required test matrix

Fault injection must be deterministic (`#[cfg(test)]` phase hooks or a small
fault-injecting filesystem adapter), not timing sleeps. Tests assert disk bytes,
result state, and recovery artifacts together.

### 9.1 Disk and compensation

| Case | Required assertion |
|---|---|
| Single-file introduced syntax error | Original bytes restored; `RolledBack`; backup removed |
| Multi-file failure after file 1 installs | File 1 restored; every untouched file remains byte-identical |
| External edit to an untouched target during a failing plan | External bytes survive; no backup is restored over them |
| External edit after this attempt installs | Hash mismatch; external bytes survive; `ExternallyChanged`; backup retained; `RollbackIncomplete` |
| Late stale before any install | `RejectedStale`; no plan bytes; no unnecessary restore |
| Late stale after an earlier install | Earlier install restored; final status `RejectedStale` only if compensation+sync complete |
| Restore write failure | Not `RolledBack`; real backup retained; per-file `RestoreFailed`/`Unknown`; remaining plan bytes reported accurately |
| Restore read-back mismatch | Not `RolledBack`; backup retained; manual recovery required |
| Backup write/read-back failure before install | No target changed; only this attempt's artifacts cleaned |
| Post-write read/parser/grammar unavailable | Never `Applied`; own installs compensated; structured `verification_unavailable` |
| Pre-existing syntax error, no clean→error transition | No false introduced-error rollback; sync may be explicitly partial |
| Successful apply | Prepared/final hashes agree; backups/temp artifacts cleaned |
| `create_entity` with changed surrounding file | Full-file late hash detects the change despite empty insertion-span hash |
| Concurrent same-process attempts | No shared temp/backup artifact; deterministic serialized outcomes |

### 9.2 Graph, persistence, and history

- Applied + update exception / partial update / persistence error →
  `ApplyIncomplete`, source preserved, structured sync error, no “Graph updated.”
- Complete rollback + restored-state sync error → `RollbackIncomplete` with
  `files[].state=Restored` and the reindex remedy.
- Expected installed/restored hash differs from `UpdateReport.source_hash` →
  concurrent-modification outcome, not successful sync.
- Multi-file affected-file two-pass behavior remains pinned (R2-17 regression).
- Storeless apply reports persistence `NotApplicable`, not failure.
- If transient content was indexed before rollback, restored resync creates a
  new old-bytes generation; no history rows/blobs are deleted.
- Derived-vector invalidation is tested against the persisted per-model source
  of truth, not merely the in-memory projection.

### 9.3 FFI and surfaces

- Rust → FFI → Python preservation of status, failure code, per-file recovery
  path, sync state, and `None` backup semantics.
- Existing `MutationResult(...)` constructors still work through defaults.
- MCP text for every status; CLI JSON parity and proposed exit codes; ToolRouter
  success/error behavior; skill examples match the recovery matrix.
- `syntax_errors` contains syntax diagnostics only; operational failures use
  structured errors.
- Existing `test_apply_rolls_back_tainted_update` remains, strengthened to
  assert final hash and artifact cleanup rather than only source equality.

## 10. Explicit non-goals

- Semantic verification, typechecking, import validation, or running project
  tests after every mutation.
- Multi-file crash atomicity, write-ahead manifests, startup recovery scans,
  automatic orphan deletion, or cross-process transaction guarantees.
- Retaining backups after verified success. Retention on incomplete recovery is
  a safety requirement, not a general forensic backup feature.
- Mutation-attempt rows in Macrame, resurrection of DR-7's `MutationLog`, or
  rewriting temporal history during rollback.
- Changes to planning, dry-run diffs, `unverified_sites`, or call-site cascade
  semantics.
- Depending on a watcher for correctness, or suppressing restored-content
  events merely to hide synchronization work.

## 11. Approval checklist

The implementation-ready recommendation is to approve all of the following as
one contract:

1. `Applied` / `RolledBack` require their synchronization postconditions;
   incomplete outcomes are explicit rather than folded into success.
2. Valid source remains in place on post-apply graph-sync failure
   (`ApplyIncomplete`); source rollback is for write/verification failure, not
   a pretend distributed transaction across disk and SQLite.
3. Restoration is installed-only, installed-hash-guarded, atomic, and read-back
   verified; incomplete recovery retains backups.
4. Ordinary multi-file failure keeps whole-plan compensation intent; crash and
   cross-process atomicity remain explicit non-goals.
5. The temporal code ledger records indexed source generations only; no
   mutation-attempt log is introduced under DR-11 or DR-7.
6. CLI non-zero mutation-failure exits and the two new public statuses are
   accepted as the deliberate surface changes for this feature.
