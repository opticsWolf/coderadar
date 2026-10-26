# Mutation auto-rollback: apply-semantics design (DRAFT)

**Status:** draft for agreement — no code until the open questions in §6 are decided.
**Closes the carried half of:** `docs/CODERADAR_BUGS_QUIRKS.md` #1 (v0.12 disposition:
"rollback-instead-of-diagnose carried (apply-semantics change, needs own design)").
**Scope:** the four mutating tools (`replace_body`, `update_signature`, `rename`,
`create_entity`) via `MutationEngine::apply` (`core_indexer/src/mutation/mod.rs`).

---

## 1. Background

#1: `replace_body` wrote every body line dedented one level (except line 0),
producing an unparseable file, while dry-run rendered correctly. v0.12 fixed the
**root cause** (`normalize_body_for_splice` re-based; Rust regression
`test_body_replacement_flush_first_line_no_double_indent`).

What v0.12 explicitly did *not* do: make a failed write undo itself. The carried
item is the **apply-semantics** question — what "the mutation did not happen"
means across disk, graph, ledger, and status — not the restore plumbing.

## 2. What already exists (mechanism, not semantics)

`apply` (`mutation/mod.rs`, `pub fn apply` ~line 2089) already implements six
phases. This design changes none of their plumbing, only what they promise:

| Phase | Behaviour today |
|---|---|
| 0 — Policy | `check_policy` fails → `RejectedPolicy`, nothing read or written |
| 1 — Stale check | File re-read from disk; every non-empty `expected_hash` re-verified against the fresh read. Mismatch → `RejectedStale`, nothing written |
| 2 — Backup | Every target file copied to `<file>.coderadar-bak`. Backup failure → earlier backups removed, `RolledBack`, nothing written |
| 3 — Write | Edits applied in memory, written via temp-file + rename (atomic **per file**), `write_guard.suppress(new_hash, 5s)` per file. Apply/write failure → `rollback_all`, `RolledBack` |
| 4 — Post-write verify | `verify_parse_introduced_error`: tree-sitter parse of the **bytes re-read from disk** (not the in-memory string — this is the right choice; it closes the original dry-run/apply divergence). Diagnostic only if the mutation *introduced* the error (`after == error && before == clean`). Tainted → `rollback_all`, `RolledBack` |
| 5 — Success | Backups deleted, `Applied` |

Two facts that simplify everything below:

- **The engine never mutates the in-memory graph.** `apply` touches disk +
  `write_guard` only. Graph resync is the caller's job (`coderadar_update_file` /
  `reindex` / file watcher). Consequence: on rollback there is *no in-engine
  graph state to undo* — a caller that reindexes `files_written` (empty on
  rollback) converges by doing nothing. The watcher path also converges: the
  Phase-3 write is suppressed, but the rollback restore is *not* suppressed, so
  the watcher re-indexes the restored (original) content.
- **`MutationStatus::RolledBack` already exists** and already fires on all four
  failure paths. The gap is what it *means*, not that it fires.

## 3. Gaps (each needs a decision, not just code)

### G1. TOCTOU between Phase-1 read and Phase-3 write

Phase 1 reads + hash-checks; Phase 3 writes via rename without re-verifying. An
external edit landing in that window is silently overwritten. The window is
milliseconds on local disk, real on network filesystems — and `rename`'s
atomicity does not help (it atomically installs the *stale* content).

- Option A: re-hash the span just before rename; mismatch → abort + `rollback_all`
  (nothing of this plan was installed yet for that file) + `RejectedStale`.
- Option B: file locking (platform-specific, lease pitfalls — not recommended).
- Option C: accept the race, document the window.
- **Recommendation: A.** It reuses the existing `RejectedStale` path; cost is one
  read per file already read in Phase 1.

### G2. Multi-file plans are not atomic, and crashes leave orphans

Per-file temp+rename means a process death mid-loop leaves files 1..k new and
k+1..n old, with orphan `.coderadar-bak` files and no recovery pass. Single-file
plans (the common case: body/signature edits) are safe; **multi-file `rename` is
the exposed case**.

- Option A: write-ahead manifest (plan id + file list) before Phase 3; startup
  recovery completes or rolls back. Real crash-consistency, real complexity.
- Option B: document "multi-file plans are best-effort atomic; orphans are
  `<file>.coderadar-bak` and restore is a copy". Matches current backup
  visibility but leaves agents to discover it.
- Option C: restrict rollback guarantees to single-file plans; multi-file returns
  per-file outcomes (see G6) so the caller can repair.
- **Recommendation: C + B.** A is disproportionate: mutations are agent-driven
  and re-runnable; the honest contract is per-file results, not transactions.

### G3. `backup_path` points at a deleted file

On the Phase-4 (tainted) path, `apply` returns
`backup_path: backups.first()` — but `rollback_all` already restored **and
deleted** every backup. Every other rollback path returns `backup_path: None`.
So the one field that says "here is your safety net" names a file that does not
exist.

- Option A: return `None` everywhere on rollback (backups are an internal
  mechanism, never a caller-visible artifact).
- Option B: keep the backup on the tainted path (skip deletion), return the real
  path, add a GC policy (age/size cap, cleaned on next apply or startup).
- **Recommendation: A.** Forensics value is low (the restored content *is* the
  pre-image; the diff the agent needs is plan-vs-disk, reproducible via
  dry-run). B trades litter + a new lifecycle for little.

### G4. Rollback's own failures are silent

`rollback_all` uses `let _ =` on both copy and delete. Disk-full / permission
failure during restore → status still `RolledBack`, disk state unknown, nobody
told.

- **Recommendation:** collect per-file restore errors into the result
  (new `rollback_errors: Vec<…>` or a `RolledBackFailed`-style status — see G6).
  A rollback that reports itself failed must be LOUD: the disk may hold the
  broken content with all backups deleted. This is the one path where paging the
  human (not just the agent) is defensible.

### G5. `files_written: []` erases "touched then restored"

All rollback paths return empty `files_written`, so the ledger / caller cannot
distinguish "never touched" from "installed then restored". For DR-11 ledger
honesty the attempt happened and should be recorded as an attempt.

- **Recommendation:** keep `files_written` as "files whose current content came
  from this plan" (empty on rollback — correct for resync callers), and record
  the attempt + outcome in the ledger / result metadata instead (see G6 `reason`).
  Do not overload `files_written` with history.

### G6. `RolledBack` conflates five causes

Read-fail, backup-fail, apply-fail, write-fail, tainted-verify all return the
same status with the cause only in free-text `syntax_errors[].message`. An agent
deciding "retry identically" vs "fix the edit" vs "page the human" must parse
English.

- **Recommendation:** keep the enum stable (MCP/CLI/skills already render it),
  add a structured `reason` (or `stage: read|backup|write|verify`, plus the
  G4 `rollback_errors`). Renderers print the reason; machines branch on it.
  Retry policy then falls out: `verify`-stage → do not retry identically;
  `read`/`backup`/`write`-stage → retryable; non-empty `rollback_errors` →
  stop and escalate.

### G7. Verify scope is (correctly) narrow — state it

`verify_parse_introduced_error` checks only *introduced* tree-sitter errors:
a mutation to an already-broken file that makes it worse passes; languages
without a grammar pass silently; nothing semantic (imports, types) is checked.

- **Recommendation:** keep the scope (introduced-syntax-only is the defensible
  line — the engine must not veto edits to files that were already broken, and
  must not pretend to typecheck). Document it in the tool descriptions so agents
  keep running their own tests post-apply. Semantic verification is explicitly
  a non-goal (§5).

### G8. Stale promises in the Python layer

- `ops._edit` comment: "Any failure raises MutationFailed … nothing was
  written." On rollback something *was* written then restored — "nothing was
  written" is false and matters (mtime changed, watcher fired, `.bak` may have
  existed transiently). Reword to "disk content is unchanged".
- `render.mutation_applied` (#2's single-source fix): confirm the `RolledBack`
  rendering names the reason (G6) and never reads as applied. No enum change,
  just the new field threaded through.

### G9. Write-guard / watcher interplay, stated

Suppress is keyed on the broken-content hash for 5s; the rollback restore
changes content+mtime and is unsuppressed → watcher re-indexes original content.
Residual risk: watcher fires *between* Phase-3 write and Phase-4 restore and
indexes broken content for milliseconds-to-seconds. Acceptable (converges on the
restore event), but state it — do not "fix" by suppressing the restore, which
would leave the graph showing pre-mutation content that *happens* to be right
for the wrong reason and mask the event from staleness tracking.

### G10. Hash-less edits widen G1

Per §2.3, empty `expected_hash` skips the Phase-1 check (falls back to
`unverified_sites`). Those edits ride G1's window with no net at all.

- **Recommendation:** accept + document (planning always emits hashes; empty is
  the legacy/manual path), and require non-empty hashes for multi-file plans
  where G2 already weakens guarantees. Cheap, no migration.

## 4. Proposed result shape (illustrative, not committed)

```rust
// Additive only: no existing field changes meaning, no enum variant removed.
pub struct MutationResult {
    pub status: MutationStatus,   // unchanged (Applied | RolledBack | Rejected* )
    pub reason: RollbackStage,    // NEW: none | read | backup | write | verify
    pub rollback_errors: Vec<RollbackError>, // NEW: G4 — empty means restore clean
    pub files_written: Vec<String>, // unchanged meaning: current content from plan
    pub syntax_errors: Vec<SyntaxDiagnostic>, // unchanged
    pub reindex: ReindexSummary,  // unchanged
    pub backup_path: Option<String>, // always None on rollback (G3-A)
}
```

Retry policy (for skills/docs, not code): `verify` → edit, don't resend;
`read|backup|write` with empty `rollback_errors` → safe to retry;
non-empty `rollback_errors` → stop, escalate to human.

## 5. Non-goals

- Semantic verification (typecheck/import-resolution post-apply).
- Crash-consistency across process death (G2-A rejected; document instead).
- Backup retention/forensics (G3-A rejected).
- Changing dry-run (plan-only, unaffected) or the `RejectedStale`/`RejectedPolicy`
  paths.

## 6. Open questions (agreement needed before code)

1. **G1-A + G10:** re-hash before rename, and require hashes on multi-file plans?
2. **G2-C:** per-file outcomes instead of transactional language in docs/status?
3. **G3-A:** `backup_path` always `None` on rollback — any forensics objection?
4. **G4/G6:** new `reason` + `rollback_errors` fields, enum stable — acceptable
   additive shape? Who renders `rollback_errors` (MCP text full, CLI full, ledger?).
5. **G5:** does the DR-11 ledger record mutation attempts (including rollbacks),
   or only applied mutations? (Ledger-honesty vs ledger-noise.)
6. **Escalation:** non-empty `rollback_errors` → what is the MCP/CLI surface?
   Error text only, or a distinct exit path agents can branch on?

## 7. Acceptance criteria (when approved)

- Rust tests: TOCTOU re-hash aborts (inject a change between Phase 1 and 3 —
  test-only hook or file-swap race); multi-file partial returns per-file
  outcomes; `backup_path` is `None` on every rollback path; simulated restore
  failure surfaces in `rollback_errors` (failing fs shim); tainted multi-file
  plan restores *all* files, not just the tainted one.
- Python tests: `_edit` comment true (`disk content unchanged` asserted by hash
  before/after a tainted apply); `mutation_applied` renders `RolledBack` +
  reason distinctly from `Applied` (follow the `TestDiagnoseContent` pattern).
- Docs: tool descriptions state the introduced-syntax-only verify scope (G7);
  skills document the retry policy (§4); BUGS_QUIRKS #1 disposition updated to
  Fixed with this doc linked; §3.2 watch-list entry (if any) closed.
- Full suite green + clippy/rustfmt/ruff clean, per standing policy.
