# CodeRadar — Temporal Code Reconstruction: Architecture & Implementation

> **Date:** 2026-10-27 · **Baseline:** v0.13.0 (`fd6ac00`, `main`) on `macrame-db 0.19.1`
> **Status:** plan, partly built. P0 landed (`ff5fb96`, §12); P1's prototype
> (`scripts/verify_history.py`) predates the plan (`a1ab115`). Nothing else in
> §5 is implemented. No decision-register rows exist yet; §8 is what
> to add before code lands.
> **Derives from:** [`temporal-code-reconstruction.md`](temporal-code-reconstruction.md) (the
> analysis and its verdicts — where this file disagrees with it, this file is newer and wins) and
> [`code-storage-vs-macrame-philosophy.md`](code-storage-vs-macrame-philosophy.md) (doctrine
> conformance, whose five contracts are carried here as §3 invariants).
> **Governs:** store-and-retention behaviour ([`store-and-retention.md`](store-and-retention.md))
> once P5 changes what triggers a write.
> **Decisions:** Q-1..Q-5 answered 2026-10-28 (§11); DR-39/P3b **withdrawn** on a false premise
> (§5, §8); DR-41 deferred; DR-42 added.
> **Method:** every claim in §0 was re-checked against the working tree at the baseline above.
> File:line citations are the check, not the plan text.

---

## 0. Verification header (code truth at `fd6ac00`)

| Claim | Code check 2026-10-27 | Consequence for the plan |
|---|---|---|
| Blob is one row per distinct content | `storage.rs:196` `put_source_blob` on every analyze (`lib.rs:956`) and update (`projection_ops.rs:828`); address is `sha256_hex` (`Macrame src/blob.rs:104`) | storage ceiling is the content union of history; no delta scheme needed (§4 F-1) |
| Digest rides versioned `extra` | `coderadar_extra` writes `extra.coderadar.source_blob` (`storage.rs:1737-1748`) | the *reference* is bitemporal, the bytes are timeless and branch-global |
| `entity_source_ref_at` folds per call | `storage.rs:958` calls `self.reconstruct(timestamp)`, then reads one concept | **P0 exists because of this**; measured 0.36 s/call on the self-host store |
| `lookup_entities_at` folds once but loses `extra` | `lib.rs:1752` one `reconstruct`; `entity_ref_to_dict` (`ffi_convert.rs:243`) builds from `ProjectedGraph`, which carries no `extra` | no batched digest route from Python today |
| Per-entity body hash already versioned | `annotation::CONTENT_HASH` in content JSON (`storage.rs:1815`, writers `2014-2091`) **and** mirrored to `extra.coderadar.content_hash` (`storage.rs:1737`) | P2 needs no schema change |
| No source text in the ledger — the blob is the only copy | the `source` key the writers emit (`storage.rs:2090`, `2121`, `2202`, `2219`) is `SourceType`, an `impl`/`stub` enum (`types.rs:634-639`); `concepts.content` carries metadata and byte spans only | **span + blob is the only read path at any granularity**; P3b/DR-39 withdrawn, and §2.5's 0% coverage means no recoverable text at all |
| `concepts.id` is UNIQUE | hot schema: `id TEXT NOT NULL UNIQUE` | the hot table holds only current versions; **all** history is log replay, so every historical read costs a fold |
| Macrame exposes no generation listing | no `DISTINCT recorded_at` or log-listing `pub async fn` in `Macrame/src/connection.rs` | P1 needs `recorded_generations` in CodeRadar (§5 P1.1), or upstream it (§11 Q-3) |
| Non-fold SQL via diagnostic conn is in-pattern | `current_seq_anchor` runs `SELECT MAX(seq_id)` on `transaction_log` (`storage.rs:1575`), with the stated reason that folding to read one integer costs seconds | `recorded_generations` is the same shape and needs no new doctrine |
| Stale sweep finds ids two ways | `live_concept_ids_for_file` = `extra.coderadar.file_path` match (`storage.rs:1333`) ∪ id-prefix range `{file}::` (`storage.rs:1347`) | **P3 hazard H-1**: a spine concept carrying `file_path` would be retired as stale |
| Structural edge sweep is a fixed list of 8 | `retire_stale_edges` `const STRUCTURAL: [&str; 8]` (`storage.rs:1437-1446`) — `CONTAINS` is not in it | a new `HAD_CONTENT` edge is **not** swept; P3 must close it explicitly (H-2) |
| The framework hook sweeps by exclusion | `open_synthetic_triples` takes everything `NOT IN ('CALLS','IMPORTS','EXTENDS','OVERRIDES')` (`storage.rs:1271`) | **H-3**: `HAD_CONTENT` would be captured as "synthetic" and pair-retired |
| Edge vocabulary is a const module | `storage.rs:40-49` `edge_type` with 9 constants | `HAD_CONTENT` is a one-line addition plus the two sweep fixes above |
| Byte coverage on the self-host store | `scripts/verify_history.py`: ok=1, no_digest=5078, unresolvable=0, mismatch=0 over 15 of 6442 generations | the write-path gap is total here; P5 is the only route to recovering it |
| `valid_from` is index-now | `now_iso8601()` (`storage.rs:68-79`) used for concepts and edges | P5 is a semantic change, not an optimisation (§3 INV-8) |
| Retirement preserves history | `retire_entities` carries title/content/extra forward with `valid_to = now`, `retired = 1` (`storage.rs:1039`) | spine concepts must be excluded, not retired (H-1) |
| Blob cap pairing is documented, not enforced | 8 MiB `BLOB_MAX_BYTES` (`storage.rs:1601`) vs 1 MiB `[watch] max_file_size_bytes` | P5 multiplies oversize skips; decide the pairing before per-commit ingest |

---

## 1. What is being built, and why

**Capability statement.** Given a file (or an entity) and a point in time — and, after P4, a
branch — CodeRadar can state what the code *was*, prove it byte-for-byte, and say what changed
between any two such points. Today it can do the first for one entity at one instant, cannot do the
third at all, and on the repo's own store cannot do the first either, because the bytes were never
recorded (§0, last-but-two row).

**The three gaps, in the order they bite.**

1. **Write-path coverage.** The ledger describes files it recorded no bytes for. Measured at 100%
   on this store. Fixed going forward by a full analyze; never fixed backwards except by P5.
2. **No queryable version spine.** "Every content this file ever had" means folding at guessed
   timestamps and comparing `extra`. It should be an edge walk.
3. **Sampling frequency.** `valid_from = index-now` makes the timeline *when CodeRadar looked*, not
   *when the code changed*. Contents between two runs exist nowhere, in any store, ever.

**What is explicitly *not* broken**, so that no phase "fixes" it: the blob design, the archive's
reference scan, the hot/cold retention split, and put-then-assert ordering. §0 confirms zero
unresolvable digests and zero mismatches on a store with 6,442 generations. The analysis doc's
verdict stands: adopt the blob design as specified; the risk was never philosophical.

---

## 2. Target architecture

```
                 ┌─ P5 ingestion ──────────────────────────────────────┐
                 │ git commits/branches drive writes                   │
                 │ valid_from = commit time, recorded_at = ingest time │
                 └───────────────────┬────────────────────────────────-┘
                                     │ asserts
  read APIs    ┌────────────────────┴─────────────────────────────────┐
  (P1,P2,P4)   │ verify_history · changes_between · Snapshot.read_*   │
               │ branch-aware: reconstruct_on(lineage, T)             │
               └───────────────┬──────────────────┬──────────────────-┘
                               │ one fold per T   │ hash compare, no bytes
  spine (P3)   ┌───────────────┴──────────────────┴──────────────────-┐
               │ {path}@{digest}  kind=content_version   (timeless)    │
               │   extra: source_blob, size, line_count, parent_digest │
               │ {path}::module --HAD_CONTENT--> …   [valid interval]  │
               └───────────────┬──────────────────────────────────────┘
  references   ┌───────────────┴──────────────────────────────────────┐
  (today)      │ extra.coderadar.source_blob on the module concept    │
               │ extra.coderadar.content_hash per entity (xxHash64)   │
               └───────────────┬──────────────────────────────────────┘
  bytes        ┌───────────────┴──────────────────────────────────────┐
  (today)      │ Macrame blobs: sha256(content) → bytes, hot ⇄ cold   │
               │ branch-global, no refcount, liveness by log scan     │
               └──────────────────────────────────────────────────────┘
  primitive    source_refs_at(ids, T)  — one reconstruct, N refs  (P0)
```

Four layers, bottom to top: **bytes** are content-addressed and timeless; **references** to them
are versioned; the **spine** makes the reference history walkable; **read APIs** answer questions
over it. P5 changes only what drives writes into the stack — not the stack.

The layering is the architecture's whole claim: a change at one level must not require a change at
another. Delta blobs break it (bytes would reference bytes, §4 F-1). Stored diffs break it (a
derived answer would become a record, F-3). Per-branch blob copies break it (timeless data gaining
a lineage, F-5).

---

## 3. Invariants

Non-negotiable. Each is stated with where it is enforced today, because a plan that restates an
invariant without naming its enforcement point is a wish.

- **INV-1 — Put bytes before asserting the digest.** Enforced at `lib.rs:949-962`. A crash leaves
  orphan bytes (bounded, aged out by the scan); the reverse ordering strands a live digest over
  absent bytes. Any new write path (P3 spine, P5 ingestion) asserts digests **after** the put that
  created them, in that order, no exceptions for batching.
- **INV-2 — Digests are 64 lowercase hex, everywhere, always.** `validate_digest`
  (`Macrame src/blob.rs:84`) refuses anything else rather than folding case. The archive's
  reference scan matches that text and nothing else (D-287), so an uppercased, truncated, base64 or
  prefixed digest is an invisible reference and its bytes are reclaimable. This is the retention
  mechanism, not a formatting preference.
- **INV-3 — The reference lives in a ledger payload, never only inside blob bytes.** The scan reads
  hot log payloads; it never reads blob contents. Anything a blob's survival depends on must be
  named in `extra`, `content` or link properties.
- **INV-4 — Bytes are branch-global; references are per-lineage.** Blobs and `kv_store` carry no
  `branch_id` by design (D-213/D-214). P4 resolves the digest through the reader's lineage and
  *then* fetches branch-global bytes, in that order.
- **INV-5 — No CodeRadar-side blob GC, ever.** The archive session is the only deleter. A refcount
  is the design Macrame rejected (D-281 gate 3) and CodeRadar must not reintroduce it under another
  name ("cleanup", "vacuum", "prune").
- **INV-6 — Folds belong to the engine.** Doctrine I. Every at-T answer comes from `reconstruct` /
  `reconstruct_on`. `scripts/verify_history.py` violates this deliberately and is the one
  exception, scoped to a measurement script that is not imported by anything (§5 P1).
- **INV-7 — Fidelity is explicit.** A read either returns the recorded bytes, or says which of the
  named reasons it cannot (`ContentUnavailable` with cause, `None` for not-in-graph,
  `EngineError` for a broken ledger). Never a silent present-tense answer, never a truncation
  without a count. Enforced at `__init__.py:1556-1600`.
- **INV-8 — The two clocks stay separate.** `recorded_at` is when CodeRadar learned it; `valid_from`
  is when it was true in the world. Code history means **recorded** time until P5 lands, and P5's
  whole content is making `valid_from` mean commit time. No phase may blur them to make a query
  easier.
- **INV-9 — Spine concepts are timeless and never retired.** A content that existed existed. What
  ends is the *edge* saying the file currently has it (H-1, H-2).

---

## 4. Forbidden, with the failure mode

These are not style preferences. Each has a mechanism by which it silently corrupts history.

- **F-1 — Delta blobs.** A delta's base digest would live inside another blob's bytes, which the
  reference scan never reads (INV-3). The base becomes archive-eligible, then cold-file-loss
  eligible, while a live delta depends on it. The failure is silent and arrives one archive session
  late. *If ever unavoidable:* publish the base digest in the spine concept's `extra`, materialise
  a full snapshot every N links, and write the chain-depth bound into the schema docs — but the
  recommendation is don't: content addressing already dedups the unchanged majority.
- **F-2 — Compression inside CodeRadar.** The address *is* `sha256(stored bytes)` and the hot table
  CHECKs `size` against the byte length. Compressing above the addressing layer breaks the address,
  the CHECK, and `blob_get`'s meaning at once. Keeping a plaintext digest beside a compressed blob
  address breaks verification instead. Compression belongs below addressing, i.e. inside Macrame,
  as its own decision.
- **F-3 — Stored diffs.** A diff is derived (doctrine VI). Stored, it becomes a second record that
  can disagree with the two contents it claims to relate, and nothing can adjudicate. Compute on
  read; cache keyed `(digest_a, digest_b)` only, never by a mutable id.
- **F-4 — A second reclamation path.** See INV-5.
- **F-5 — Per-branch blob copies or branch-scoped blob GC.** Makes timeless data lineage-bound and
  reintroduces F-4 by the back door.
- **F-6 — Re-implementing the fold in shipped code.** Two fold implementations will diverge, and the
  one in Python will be the wrong one (INV-6). The prototype's log replay must not graduate into
  `ops.py`; P0 exists so that it does not have to.
- **F-7 — `valid_from = now` once P5 lands.** Mixing index-now and commit-time stamps in one
  ledger makes valid-time queries answer neither question. P5 is all-or-nothing per store, with a
  recorded migration decision (§8 DR-40).
- **F-8 — Trusting `content_hash` as proof of identity.** xxHash64 is 64-bit and
  non-cryptographic. It is adequate for *detecting* change and for ranking what to inspect. It must
  never gate a write whose correctness matters, and must never be reported to a user as "identical".
  When proof is needed, compare bytes.
- **F-9 — Letting a blob failure fail an index.** Today a `blob_put` error logs a diag line and
  indexing continues (`lib.rs:963`). That is correct: structural coverage is worth more than byte
  coverage, and the gap is visible through P1. Do not "harden" this into a hard failure.

---

## 5. Phases

Each phase: **goal**, **required** (files and signatures), **hazards**, **tests**, **done when**.
Phases are ordered by dependency; §9 is the sequencing table.

### P0 — `source_refs_at`: one fold, N refs  (DR-36)

**Goal.** Make a history sweep affordable without re-implementing the fold. This phase exists
solely because of the §0 row "folds per call": 0.36 s × files × generations is ~35 minutes for a
twelve-generation sample of 490 modules, and the only Python alternative is F-6.

**Required.**

1. `core_indexer/src/storage.rs` — extract the body of `entity_source_ref_at` (`storage.rs:953`)
   into a state-taking helper, so there is exactly one implementation of "ref out of a folded
   state":

   ```rust
   fn source_ref_from_state(
       state: &macrame::temporal::MaterializedState,
       entity_id: &str,
   ) -> macrame::Result<Option<SourceRef>>;

   pub fn entity_source_ref_at(&self, entity_id: &str, timestamp: &str)
       -> macrame::Result<Option<SourceRef>>;            // unchanged signature, now a 2-line caller

   pub fn source_refs_at(&self, entity_ids: &[String], timestamp: &str)
       -> macrame::Result<(std::collections::HashMap<String, SourceRef>, bool)>;
   ```

   The `bool` is `predates_recorded_history`, mirroring `entities_at` (`lib.rs:1755`) rather than
   inventing a second spelling. Ids absent at T are absent from the map — not `None` entries:
   "not in the graph" and "in the graph without bytes" are different answers and only the second is
   a gap (§3 INV-7).

2. `core_indexer/src/lib.rs` — one `#[pyfunction]` beside `entity_source_ref_at` (`lib.rs:1898`),
   registered at `lib.rs:75`, returning `{"refs": {id: ref_dict}, "predates_recorded_history": bool}`
   with the same ten keys per ref the single call already emits. Fold under `py.allow_threads`, as
   `entities_at` does (`lib.rs:1752`) — a slow reconstruct must not hold the GIL or block a writer.

3. No Python surface. P0 is a primitive; P1 and P2 are its only callers.

**Hazards.** The refactor must not change `entity_source_ref_at`'s observable behaviour — it is
what `Snapshot._read_raw` depends on (`__init__.py:1553-1554`), including the four distinct failure
messages. The single-call path keeps its own `reconstruct`; do not add caching across calls (a
stale cached state would answer a *different* question than asked, which is INV-7's exact
prohibition).

**Tests.** `core_indexer` unit tests: (a) `source_refs_at` and N× `entity_source_ref_at` return
identical refs for the same (ids, T) — this is the anti-divergence pin for the extracted helper;
(b) ids absent at T are absent from the map; (c) `predates_recorded_history` matches
`lookup_entities_at`'s for a pre-floor T; (d) one fold per call, asserted by a counter or by timing
ratio against the N-call path.

**Done when.** A 490-id, 12-generation sweep costs 12 folds, measured, and the parity test (a)
passes. *(Met 2026-10-28: parity pins green; fold-count measured 23x on a 30-id fixture sweep
— one fold vs thirty — rather than the full 490-id run. See §12.)*

---

### P1 — `verify_history`: make the gap a number  (DR-36)

**Goal.** Per recorded generation, what fraction of the files the ledger describes can have their
bytes reconstructed. Already prototyped; this phase is about shipping it on P0 instead of a log
replay.

**Required.**

1. `storage.rs` — `pub fn recorded_generations(&self, from: Option<&str>, to: Option<&str>, branch: &str) -> macrame::Result<Vec<String>>`:
   `SELECT DISTINCT recorded_at FROM transaction_log WHERE branch_id = ? [AND window] ORDER BY recorded_at`,
   through `open_diagnostic_conn`. This is a listing, not a fold, and has direct precedent in
   `current_seq_anchor` (`storage.rs:1575`) with the same stated reason: folding to read a scalar
   costs seconds. Document it as such, or the next reader will think INV-6 was broken.
2. `storage.rs` — `pub fn module_concept_ids(&self) -> macrame::Result<Vec<String>>`, or reuse
   `live_concept_ids` (`storage.rs:1365`) plus a suffix filter. Note the subtlety: the sweep needs
   ids that existed *ever*, not ids live *now*, so a `retired = 0` filter is wrong here. State which
   you chose and why in the docstring.
3. `lib.rs` — `verify_history(from, to, generations, all, branch)` → the report dict, or expose the
   two primitives and build the report in Python. **Prefer Python**: the counting is not hot, and
   the report shape will change more often than the engine should.
4. `py_agent/src/coderadar/ops.py` — `verify_history(...) -> dict`, added to `__all__`
   (`ops.py:30-60`), raising `InvalidRequest` for a bad window and `NoIndex` without a store, like
   its neighbours.
5. `py_agent/src/coderadar/render.py` — a text renderer; the per-generation table from the
   prototype is the right shape and is already legible.
6. CLI command `coderadar verify-history` following the `as-of` pattern (`cli.py:590-605`):
   `_enter_project()`, `_op(fmt, …)`, `--format json`.
7. MCP: **in** the first cut — Q-4 decided both ops get one, so
   `coderadar_verify_history` ships with the op: sampling on by default
   (`generations=25`) and a scan cap (`max_generations=200`) because a
   blind agent must not be handed a 6,442-fold bill.
8. `scripts/verify_history.py` — keep, and amend its docstring to point at the shipped op and to say
   its log replay is now redundant. It stays useful as the independent check on the op (two
   implementations, one of which is allowed to be wrong, is a test; two shipped ones is F-6).

**Report shape** (the prototype's, as the contract):

```
{store, root, branch, window: [lo, hi], total_generations, sampled,
 generations: [{timestamp, modules, counts: {ok, no_digest, unresolvable,
                digest_mismatch, digest_malformed}, offenders: {...}}],
 totals: {...}, byte_coverage_pct, distinct_digests}
```

One fetch-and-re-hash per **distinct digest**, not per occurrence: the address is the identity.

**Hazards.** `digest_malformed` must be counted separately from `unresolvable` — a malformed digest
means the bytes were reclaimable (INV-2), which is a different incident from bytes that went
missing. Sampling must always include both window ends, or a reader will conclude coverage is
uniform when it is a step function (it is a step function on this store).

**Tests.** New `tests/test_verify_history.py`: a fixture store with one blob-backed and one
blob-less generation reports exactly 1 and 0; a store whose blob row is deleted reports
`unresolvable` and not `no_digest`; a doctored digest reports `digest_mismatch`; a pre-blob
generation reports `no_digest` for every file; `--all` and sampling agree on a store with ≤ N
generations. `tests/test_retention.py` gains: coverage is unchanged by an archive session, and
`unresolvable` stays 0 across it.

**Done when.** `coderadar verify-history` reproduces the §0 numbers on the self-host store in
seconds rather than minutes, and the archive test proves retention does not move coverage.

---

### P2 — `changes_between`: entity diff with no byte reads  (DR-37)

**Goal.** Added / removed / modified / moved per entity between two instants, from hashes already
recorded. No schema change, no blob reads.

**Required.**

1. Fold at T1 and T2 (`lookup_entities_at`, or P0's call where the digest is also wanted), join by
   entity id, compare `extra.coderadar.content_hash` (`storage.rs:1737`).
2. Classification: present only in T2 = **added**; only in T1 = **removed**; both with different
   hash = **modified**; both with equal hash but different span = **moved**. "Moved" is worth its
   own class because it is the common churn and the least interesting.
3. `ops.changes_between(t1, t2, scope=None) -> dict` + `__all__`, renderer, CLI
   `coderadar changes-between`. Optional `scope` = file path or id prefix.
4. Docstring must carry F-8 verbatim-in-spirit: equal hash is *evidence* of no change, not proof;
   `Snapshot.read_bytes` is one call away when proof is wanted.

**Hazards.** The two folds are two separate reconstructs and an entity can vanish between them if
writes are concurrent; answer as absent rather than erroring (the precedent is
`__init__.py:1559`, "vanished between the two folds; answer as absent"). There is no source text in
the ledger to compare instead (§0, `types.rs:634-639`), so the hash is not merely the cheap path —
it is the only path that does not require blob coverage, which on this store is 0%. That is why P2
reports hash-based classes and points at `Snapshot.read_bytes` for proof rather than claiming to
have diffed text.

The second hazard is **H-11**, measured while capturing P1's baseline: entity ids changed spelling
on 2026-10-03 (`.\core_indexer\…` → `core_indexer/…`) and the old concepts are retired rather than
rewritten, so a window straddling that date yields ~6.3k spurious removals and ~6.3k spurious
additions. `changes_between` must therefore either normalise ids before set-differencing, or detect
the condition and say so in the report. A silent mass-change answer is worse than a refusal.

**Tests.** `tests/test_changes_between.py`: a fixture with one body edit, one rename, one added
function, one deleted file across two generations yields exactly the four classes; a pure
whitespace-only move yields `moved`; equal T1 and T2 yields all-empty.

**Done when.** The four classes are right on a fixture, and the op makes exactly two folds
(asserted).

---

### P3 — The `content_version` spine  (DR-38)

**Goal.** Make "every content this file ever had" an edge walk with intervals, instead of a guessing
game over timestamps.

**Shape.**

```
concept  {path}@{digest}      kind = content_version        (timeless, never retired)
         extra.coderadar: {format, kind, source_blob, size, line_count, parent_digest}
edge     {path}::module  --HAD_CONTENT-->  {path}@{digest}   [valid_from … valid_to]
```

Naming: `content_version`, **not** `file_version` — module content JSON already carries an
unrelated `file_version` integer (`storage.rs:2013`, `types.rs:919`), and reusing the name makes two
different things grep identically.

**Required.**

1. `storage.rs:40-49` — add `pub const HAD_CONTENT: &str = "HAD_CONTENT";` to `edge_type`.
2. `storage.rs` — build the spine concept and the edge in `build_v2_concepts_all`
   (`storage.rs:2213`) and `build_v2_concepts_for_file` (`storage.rs:2249`), both of which already
   receive `&SourceBlobDigests`. Idempotence: the id is derived from the digest, so re-indexing
   unchanged content re-upserts an identical concept and re-asserts an open edge — which the
   existing open-triple skip already absorbs. **One concept + one edge per actual content change,
   nothing for unchanged files**, and that is the property to test.
3. `parent_digest` = the digest this file had immediately before, or null for the first. Read it
   from the current open `HAD_CONTENT` edge before asserting the new one. This is the diff spine and
   it must live in the ledger payload, not be inferred at read time (INV-3 keeps the bytes alive
   through it).
4. Close the previous edge explicitly: `valid_to = now` on the old `HAD_CONTENT` when a new digest
   arrives. See H-2 — nothing sweeps it for you.
5. Exclude spine concepts from the stale sweep. See H-1 — this is the phase's main hazard, not a
   detail.

**Hazards (all three are live traps in today's code).**

- **H-1 — the stale sweep would retire the spine.** `live_concept_ids_for_file` collects ids by
  `extra.coderadar.file_path` (`storage.rs:1333`) *and* by the `{file}::` id-prefix range
  (`storage.rs:1347`). `{path}@{digest}` escapes the prefix range but **not** the `file_path` match,
  so a spine concept carrying `file_path` in its extra — which it wants, for queryability — is
  retired as stale on the next index of that file. Fix: exclude `kind = 'content_version'` in the
  `extra` query, and pin it with a test that indexes twice and asserts the spine survives.
  Per INV-9 the concept is timeless; what ends is the edge.
- **H-2 — nothing closes the edge.** `retire_stale_edges` sweeps a fixed list of 8 structural types
  (`storage.rs:1437-1446`); `HAD_CONTENT` is not among them, and adding it there would be wrong
  (the sweep retires what is absent from the fresh projection, and *all* historical contents are
  absent by definition — it would close the whole chain on every index). Close the previous edge
  explicitly in the writer.
- **H-3 — the framework hook would pair-retire it.** `open_synthetic_triples` selects everything
  `NOT IN ('CALLS','IMPORTS','EXTENDS','OVERRIDES')` (`storage.rs:1271`), so `HAD_CONTENT` lands in
  the §1.3 scoped pair-retire as if it were a framework edge. Add it to that exclusion list. An
  exclusion-based query is a trap for every future edge type; consider inverting it to an
  inclusion list in the same change (§11 Q-2).

**Reads unlocked.** `history_of(file)` → the chain newest-first with intervals;
`content_at(file, T)` without a span lookup; cross-branch content comparison after P4; and P2 gains
a cheap pre-filter (did the module's digest change at all?).

**Tests.** `tests/test_content_spine.py`: index twice unchanged → zero new spine concepts, zero new
edges, spine survives (H-1); edit one file → exactly one new concept, one closed edge, one open
edge, `parent_digest` points at the previous digest; delete the file → chain intact and queryable
at an earlier T; a framework-resolver pass does not retire `HAD_CONTENT` (H-3). Rust-side: the
`edge_type` const is used, not shadowed by a literal.

**Done when.** `history_of` returns the full chain for a file edited three times, after an
`analyze → edit → update → edit → update` sequence, and the unchanged-reindex test shows zero spine
growth.

---

### ~~P3b — Decide the `content.source` duplication~~  (DR-39 — **withdrawn 2026-10-28**)

**This phase does not exist.** It was raised on a false premise and is retracted rather than
deleted, because the premise had consequences elsewhere in this plan and a reader of the merged
analysis doc will have seen it.

**The premise, and why it was wrong.** The phase claimed that `v2_function` writes function body
text into `concepts.content`, duplicating bytes the blob already holds. It does write a `source`
key (`storage.rs:2121`, and `2090`, `2202`, `2219` for classes, constants and type aliases) — but
the field's type is `SourceType` (`types.rs:634-639`), a two-variant enum serialised as `"impl"` or
`"stub"`. It records whether an entity has a body, not what the body is. `module_content` emits no
`source` key at all. **No concept writer stores program text.** `concepts.content` holds metadata
and byte spans: name, kind, path, lines, docstring, decorators, parameters, metrics, resolved calls,
the three xxHash64 digests, and `span`/`name_span`/`body_span`/`params_span`/`decorators_span`.

**What replaces it — a constraint, not a decision.** The blob is the sole custodian of program text
in the store, at every granularity. A body read is necessarily: resolve the generation's digest,
read the blob, slice by the stored span — which is what `entity_source_ref_at` (`storage.rs:953`)
and `_read_raw` (`__init__.py:1539`) already do. Three places in this plan inherit that:

- **§2.5's 0% coverage is total, not partial.** For 6,441 of 6,442 generations there is no
  recoverable text of any kind. The spans survive; they point into bytes nobody kept.
- **P2 must stay hash-based** and must not promise textual diffs (§5 P2, F-8).
- **The scan-cost objection to the spine is void.** `blob_scan_bytes` (D-287) scales with
  metadata-sized payloads, and a spine concept's payload — path, digest, parent digest — is smaller
  than every concept already written. P3 loses its only quantitative objection.

**Required.** Nothing to build. DR-39 is recorded as withdrawn with this reasoning, and no `format`
bump, schema change, read-compat path or P5-coupled migration is needed for it — all four were
consequences of the error.

---

### P4 — Branch-aware reads  (DR-40)

**Goal.** Walk lineages, not just time. Macrame is ready; the facade has no branch parameter
anywhere.

**Required.**

1. Thread a lineage through the fold: `reconstruct_on` (`Macrame src/connection.rs:3570`) beside
   `reconstruct` in `storage.rs:1563`, and a `branch` parameter on `source_refs_at`,
   `entity_source_ref_at`, `lookup_entit{y,ies}_at`, defaulting to the trunk.
2. Python: `branch=` on `Snapshot`, `ops.as_of`, `ops.verify_history`, `ops.changes_between`;
   `ops.branches()` listing (`Macrame src/connection.rs:3049`).
3. Order of operations, per INV-4: resolve the digest **through the reader's lineage**, then
   `blob_get` the branch-global bytes. Never the reverse, and never a per-branch blob lookup.
4. Cross-branch content diff builds on P3's spine. Macrame's own `diff(a, b)`
   (`Macrame src/connection.rs:3074`) is edge-level and deliberately has no instant parameter — it cannot report
   a divergence that is *about* an instant having passed — so it answers a different question and is
   not a substitute.

**Hazards.** A branch-unaware reader over a branched ledger answers whole-ledger belief where the
caller meant one lineage's view. That is a wrong answer, not a missing feature, which is why P4
comes before P5 rather than after. Never add per-branch blob copies (F-5).

**Tests.** Fork, write divergent content on each lineage, assert: each lineage reads its own bytes
at T; the trunk is unaffected; one blob row backs content both lineages reached; `blob_get` is
lineage-independent.

**Done when.** Two lineages with different content for one path each read their own bytes, off one
shared blob table.

---

### P5 — Commit-driven ingestion  (DR-41)

**Goal.** Close gap 3. Make valid time mean the code's own history.

**Shape.** One Macrame branch per git branch, forked at the merge-base. Per commit: `valid_from =`
commit time, `recorded_at =` ingest time, blobs per file-at-commit. Then `as_of_valid` and
`as_of_recorded`/`reconstruct` (D-174) stop being the same question asked twice, and backfilling an
old branch becomes a legitimate retroactive correction rather than a lie told in valid time.

**Why it is the only route to the measured gap.** The 6,441 byte-less generations on this store
describe content that exists in no blob, no payload and no working tree. A full re-analyze fixes
the present and never the past. A commit-driven ingest can *reconstruct* that history from git,
because git still has the bytes. This is the strongest argument available for P5 and it only became
visible once P1 measured the store.

**Required (sketch — this phase needs its own plan before code).**

1. A commit walker: per branch, per commit, the changed file set and their bytes at that commit.
2. Fork mapping: git branch ↔ Macrame lineage, forked at merge-base, recorded once.
3. Stamping: `valid_from` = commit time (committer, not author — state which and why),
   `recorded_at` = engine-stamped ingest time. INV-8 and F-7 govern.
4. A per-store mode flag, because mixed stamping is F-7. Migration of an index-now store is a
   decision, not a default: either re-ingest from git into a fresh store, or keep the old store as
   frozen history and start a new one. Recommend the former; record it.
5. Cost: one write per commit per branch carrying its file set. The blob table absorbs the content
   **union** of history, not the sum of the checkouts — dedup is what makes this affordable, and
   §0's "one row per distinct content" is the measurement that supports the claim.

**Hazards.** The 1 MiB watcher cap vs 8 MiB blob cap pairing (§0, last row) multiplies per commit.
Merge commits must not double-count a content. Shallow clones and rewritten history (rebase, force
push) mean the git side is not append-only while the ledger is — decide what an ingest does when a
previously-ingested commit disappears (recommend: nothing; the ledger keeps what it was told, which
is exactly what bitemporality is for).

**Done when.** A store ingested from git answers "what did this file look like at commit X on
branch Y" with bytes, and `verify_history` reports coverage at or near 100% for the ingested range.

---

## 6. Hazard register

Traps found in today's code while planning. H-1 to H-3 are detailed in P3.

| # | Hazard | Where | Bites in |
|---|---|---|---|
| H-1 | Stale sweep retires spine concepts matched by `extra.coderadar.file_path` | `storage.rs:1333` | P3 |
| H-2 | Nothing closes a `HAD_CONTENT` edge; the 8-type sweep both misses it and would over-close it | `storage.rs:1437-1446` | P3 |
| H-3 | Exclusion-based `open_synthetic_triples` captures every new edge type as "synthetic" | `storage.rs:1271` | P3, and every future edge type |
| H-4 | `entity_source_ref_at` folds per call; any new per-entity historical read inherits the cost | `storage.rs:958` | P0, P2 |
| H-5 | `concepts.id` is UNIQUE — no concept history in the hot table, so every historical read is a replay | hot schema | performance of every phase |
| H-6 | Watcher 1 MiB vs blob 8 MiB caps are paired by documentation only | `storage.rs:1601` + comment | P1 reporting, P5 volume |
| H-7 | No source text anywhere but the blob, so a missing blob loses the text irrecoverably — there is no payload copy to fall back on | `types.rs:634-639`, `storage.rs:953` | P1 (what it measures), P5 (why it matters) |
| H-8 | Naive/offset timestamps accepted at the Python boundary, rejected by Macrame `normalize` | philosophy doc mismatch #4 | every new temporal op's argument handling |
| H-9 | `content_hash` is xxHash64: 64-bit, non-cryptographic | `storage.rs:1815` | P2's contract (F-8) |
| H-10 | Hot-only backup is pointers; bytes live in hot **or** cold | `store-and-retention.md` | operational, all phases |
| H-12 | ~~**`persist_edges` is non-atomic and fails silently.**~~ **Cause fixed 2026-10-28** (§12). `INSTANTIATES` was derived from `Function.resolved_calls`, a `Vec` with no dedup, so a function constructing the same class twice emitted two identical assertions sharing one `valid_from` — a UNIQUE violation on `links` that aborted the run *after* earlier batches had committed, reported only as `[diag]` with a zero exit. Measured before the fix: 53 affected functions, and 2,619 of the generation's edges written with `INSTANTIATES`/`REFERENCES`/`OVERRIDES` missing. Now deduped at the flush boundary, and a failure is recorded and reported. **Residual:** the batch flushes are still not one transaction, so a failure from any *other* cause would still leave earlier batches committed — now loudly, not silently | `persistence.rs:161-176`, `lib.rs:1029-1039` | P3's `HAD_CONTENT` writer must use the same flush path, not a new one |
| H-11 | **Entity ids changed spelling mid-history.** On the self-host store, `{file}::module` ids are `.\core_indexer\...` up to 2026-10-03T22:03:05Z and `core_indexer/...` from 2026-10-03T22:02:56Z on; the 6,308 old-spelling concepts are correctly `retired=1`, so nothing is wrong *now* — but a `changes_between` straddling that boundary classifies by id and reports ~6.3k removals plus ~6.3k additions for code that never changed | measured on `.coderadar/store/coderadar.db` | **P2**, and any future id-shape change |

---

## 7. Test plan

New files, and what each one is for. Existing suites that must not regress: `tests/test_blob_reads.py`
(the §3.0 read path), `tests/test_retention.py` (archive behaviour), `tests/test_ops_api.py`
(surface shape).

| File | Pins |
|---|---|
| `core_indexer` unit tests (in `storage.rs` tests module) | P0 parity between `source_refs_at` and the single call; one-fold-per-call; absent-at-T semantics |
| `tests/test_verify_history.py` | the five outcome classes, sampling vs `--all`, coverage unmoved by archive |
| `tests/test_changes_between.py` | the four change classes, two-folds-only, whitespace move |
| `tests/test_content_spine.py` | zero growth on unchanged reindex, `parent_digest` chain, H-1 and H-3 survival |
| `tests/test_branch_reads.py` (P4) | per-lineage bytes off one shared blob table |

Cross-cutting assertions worth their own tests, because they are the invariants most likely to rot:
every digest CodeRadar writes is 64 lowercase hex (INV-2, grep-level test over a fixture store's
`extra`); no code path outside the archive deletes from `blobs` (INV-5, a source-level test is
acceptable here); `blob_put` precedes digest assertion under failure injection (INV-1, already the
intent of the §1.9 tests — extend to the spine writer).

---

## 8. Decision-register rows to add

Numbering continues from DR-35 (`docs/mutation-rollback-design.md`). Each row needs a landing commit
before it counts as closed, per the v0.12 release-gate convention.

| DR | Decision | Status |
|---|---|---|
| DR-36 | Batched `source_refs_at` primitive + `verify_history` integrity op; `recorded_generations` as a documented non-fold listing | P0 landed `ff5fb96`, P1 landed `b964f1b` (2026-10-28) |
| DR-37 | `changes_between` over `extra.coderadar.content_hash`; hashes detect, bytes prove | P2 closed (`92aafcb` on `dev` 2026-10-28) |
| DR-38 | `content_version` spine with `HAD_CONTENT`; spine concepts timeless and sweep-excluded | proposed (P3) |
| DR-39 | ~~`content.source` duplication: lean on it or trim it~~ | **withdrawn** 2026-10-28 — premise false, there is no duplication (P3b) |
| DR-40 | Branch-aware reads: lineage resolves the reference, bytes stay branch-global | proposed (P4) |
| DR-41 | Commit-driven ingestion; per-store stamping mode; migration path | **deferred** 2026-10-28 until P0–P4 ship (Q-1) |
| DR-42 | `open_synthetic_triples` inverted to an explicit inclusion list of framework edge types, as its own change before P3 | DR-42 closed (`600d689` on `dev` 2026-10-28) |

---

## 9. Sequencing

| Order | Phase | Why here | Blocked by |
|---|---|---|---|
| 1 | P0 `source_refs_at` | nothing else is affordable without it | — |
| 2 | P1 `verify_history` | measures the gap before anything is built on assumptions | P0 |
| 3 | P2 `changes_between` | no schema change; independent of the spine | P0 (optional) |
| 4 | P3 spine | makes P5's per-commit writes natural; carries H-1/H-2/H-3 | P2 for shared read shapes |
| 5 | P4 branch reads | must precede P5: a branch-unaware reader over a branched ledger is wrong, not incomplete | P3 |
| 6 | P5 ingestion — **deferred by decision (Q-1)**: revisit after P0–P4 ship, with coverage numbers from a re-analyzed store rather than the current 0% | the semantic change; needs its own plan | P3, P4, DR-41 |

P3b is gone from this table (withdrawn — see §5). P0–P2 are additive, read-only and shippable in one
release. P3 touches the write path. P4 touches every read signature. P5 is a different product
decision and should not ride a point release.

---

## 10. Out of scope, and never

**Out of scope here:** repository-wide reconstruction of non-indexed files (§2.3 of the analysis doc
— blobs exist only for files the walker visits, and that is a contract to state, not a gap to
close); compression of stored bytes (belongs in Macrame); a diff *cache* (compute first, measure,
then cache keyed `(digest_a, digest_b)` if a hot path appears).

MCP exposure is no longer out of scope: Q-4 decided both ops get one, which makes their report
shapes contracts from the first release rather than after they settle. Each phase specifies its
shape for that reason.

**Never:** delta blobs (F-1), stored diffs (F-3), CodeRadar-side blob GC (F-4/INV-5), per-branch
blob copies (F-5), a second fold implementation in shipped code (F-6).

---

## 11. Open questions for the maintainer

- **Q-1 — P5: adopt or decline? — DECIDED 2026-10-28: deferred until P0–P4 ship.** P5 is neither
  adopted nor declined. P0–P4 are built on the current index-now ingestion; the decision is then
  retaken against `verify_history` numbers from a re-analyzed store instead of the present 0%.
  Accepted cost: the spine may need a second migration if P5 is later adopted, and P3's writer must
  not assume per-commit stamping. The one thing this does *not* leave open is DR-39, which was
  withdrawn on its own merits, not deferred with P5.
- **Q-2 — Invert `open_synthetic_triples` to an inclusion list? — DECIDED 2026-10-28: yes, as its
  own change, before P3.** The exclusion form (H-3) traps every future edge type, so it is fixed at
  the form rather than worked around per type. It lands as a standalone change with its own tests
  because its blast radius is the framework-resolver path (§1.3) and not P3's write path — a P3
  revert must not take the fix with it. **Required:** enumerate the framework edge types from their
  writers (an audit, not a guess) and state the list's membership rule in the code, so the next
  edge type's author knows which side of it they are on. An inclusion list under-sweeps visibly; the
  exclusion list over-sweeps silently, which is the whole reason for the change. New DR row: DR-42.
- **Q-3 — Upstream `recorded_generations` into Macrame? — DECIDED 2026-10-28: implement locally,
  upstream later.** It lands in CodeRadar via `open_diagnostic_conn`, on the `current_seq_anchor`
  precedent (`storage.rs:1575`), so P1 does not block on a Macrame release. **Required:** the
  schema dependency is recorded explicitly at the call site and in DR-36, and it is upstreamed the
  next time a Macrame change is being cut for other reasons. The coupling is acceptable because it
  breaks loudly — a renamed `transaction_log` column fails the query — rather than returning a
  plausible wrong answer.
- **Q-4 — Do the new ops belong on MCP? — DECIDED 2026-10-28: both, `verify_history` and
  `changes_between`.** An agent reasoning over history should be able to ask whether that history
  can be trusted, which is exactly what `verify_history` answers. **Required, because this is the
  option with the real costs:** (a) both report shapes are treated as contracts from the first
  release and specified in their phase sections, since MCP freezes them earlier than a CLI would;
  (b) `verify_history` on MCP must carry bounded defaults — generation sampling on by default and a
  cap on the scan — because its cost scales with generation count and an agent will call it
  without knowing that. Removes the "MCP exposure of the new ops" entry from §10.
- **Q-5 — Does a full analyze get run on the self-host store now? — DECIDED 2026-10-28: capture a
  baseline, then run it.** The pre-analyze `verify_history --json` report is committed as a fixture
  (`tests/fixtures/verify_history_baseline.json`) so the 0%-coverage evidence survives as a
  regression baseline and as P1's no-coverage test input; the store is then re-analyzed. The 6,441
  historical generations stay byte-less either way — only P5 could change that, and P5 is deferred.

---

## Judgment

The architecture is already right where it is hardest to change — content addressing, retention by
reference scan, put-then-assert, branch-global bytes — and §0 confirms it empirically: zero
unresolvable digests, zero mismatches, across 6,442 generations. What is missing is cheap and
additive: one batched primitive, two read APIs, one spine of digests, and a parameter for lineage.

The one expensive thing is P5, and it is expensive because it is a semantic change rather than a
feature: it decides whether CodeRadar's timeline is a record of its own activity or a record of the
code's history. The measurement in §0 is what makes that question concrete — a store with six
thousand generations and one file's bytes is an index of structure that cannot show the code it
describes.

## 12. Progress log

Build P0–P2 first regardless of that decision. They are small, they are honest about what they
cannot answer, and they turn every remaining claim in this document into something that can be
checked rather than argued — which is what §12 now records.

### 2026-10-28 — P0 `source_refs_at` landed (`ff5fb96` on `dev`)

What shipped: `source_ref_from_state` extracted in `storage.rs` (the single implementation
behind both paths); `CodeGraphStore::source_refs_at` (one `reconstruct`, absent ids omitted
from the map, `predates_recorded_history` mirrored); the `#[pyfunction]` binding with
snapshot-then-release locking and `py.allow_threads`, registered beside `entity_source_ref_at`;
`source_ref_to_dict` as the one dict-shape builder, with the single-call path refactored onto it.
No `ops` surface, per the plan — P1/P2 are the only callers.

Tests: 2 Rust unit tests (batch-vs-single parity over present/module/absent ids; predates with
empty map) + `tests/test_source_refs_at.py` (5 FFI tests: t0/t1 parity on a two-generation store,
omission-never-None, predates, garbage-ts `ValueError`). Full gates at landing: Rust lib 499
passed + 1 ignored, `test_blob_reads` + new file 21 passed, `cargo fmt --check` / `clippy
--all-targets -D warnings` / `ruff` clean, rebuilt `_core.pyd`.

Deviations from §5, all deliberate:

- **Corrupt concept fails the batch.** The plan did not specify; propagating (like the single
  call) won over skipping, because a sweep must surface corruption, not hide it (INV-7).
- **Fold-count test (d) not CI-pinned.** No instrumentation hook exists to count folds, and a
  timing-ratio assertion would be flaky CI. Measured manually instead: 30-id fixture sweep, 30
  singles 0.02 s vs batch <0.001 s (23x — one fold vs thirty). The win scales with log size; the
  0.36 s/call figure in §0 was measured on the 40k-log self-host store.
- **`predates` fixture subtlety.** A stamp captured *before* the upsert predates the recording
  (recorded_at lands later), so the Rust fixture re-stamps after the write. Worth remembering for
  every future temporal test.
- **Rebuild needed two `mcp serve` kills.** The repo `_core.pyd` was DLL-locked by stale
  `coderadar.exe mcp serve` processes — same precedent as the v0.13 session. Restart them before
  trusting MCP answers against the new binary.

### 2026-10-28 — P1 `verify_history` landed (`b964f1b` on `dev`)

What shipped: `recorded_generations` + `module_concept_ids` in `storage.rs` (both through
`open_diagnostic_conn`, Q-3 local; ever-ids with both H-11 spellings, branch-scoped, schema
dependency stated) + FFI bindings with snapshot-then-release locking; `ops.verify_history`
(one P0 batch per sampled generation, verdicts cached per distinct digest, the prototype's
report shape as the contract, ends-always-included sampling, `max_generations` cap) + `render`,
`CodeGraph.verify_history`, CLI `verify-history`, MCP `coderadar_verify_history` (25-gen
default, 200-scan cap per Q-4) with server-instructions bullet; both skills updated (28 tools);
prototype docstring amended to shipped-but-independent-check.

Tests: 2 Rust unit tests (windowed listing, retired-included ever-ids) + 15 Python tests
including op-vs-replay agreement on a live store, and a retention pin that an archive session
moves no coverage. Full gates at landing: Rust lib 503 passed + 1 ignored, full Python 1182
passed + 1 skipped, fmt/clippy/ruff clean, rebuilt `_core.pyd`.
Self-host reproduction: 25 sampled of 6,583 generations in 6.8 s, totals ok=274 / no_digest=8274
(3.21% — the §0 step function, now a number); latest generation 275 modules / 274 ok, consistent
with the post-re-analyze fixture plus one added module.

Deviations from §5, all deliberate:

- **MCP in the first cut.** Item 7 predated Q-4; the decision says both ops get one with bounded
  defaults, so the tool ships with P1 rather than waiting. The report shapes were already the
  contract — freezing them at the tool instead of later changes nothing.
- **`digest_malformed` untestable live.** Hot blob bytes are trigger-guarded (content addressing:
  only `put_at` may change), so corruption is reachable only through the cold file. The mismatch
  and unresolvable pins go archive-then-tamper-cold, mirroring `test_retention`'s cold-loss
  pattern; malformed has no live path and stays a classifier branch.
- **Op drops the prototype's `--crosscheck`.** The check compared the replay against the engine;
  the op has no replay (F-6), and singles would defeat P0. The shipped equivalent is the
  `test_op_matches_independent_replay` test: prototype-vs-op totals agreement on a fixture store.
- **Rebuild needed another `mcp serve` kill.** A restarted server process held the repo `_core.pyd`
  again (os error 32). The other editor's b44ad63 hit the same wall. This is now the third
  strike: any rebuild procedure should list `mcp serve` holders first.

### 2026-10-28 — questionnaire decisions and a retraction

Q-1..Q-5 answered (§11). One of them exposed an error of mine: the question "what is actually
stored in `concepts.content`?" showed that **P3b/DR-39's premise was false**. The `source` key the
concept writers emit is `SourceType` (`types.rs:634-639`), an `impl`/`stub` enum — not body text.
No concept writer stores program text at all, so there was never any duplication to trim.

Consequences, now carried through both docs: the blob is the sole custodian of program text at
every granularity; §2.5's 0% coverage means **no** recoverable text rather than a partial loss; the
scan-cost objection to the P3 spine is void; P2 stays hash-based out of necessity, not preference.
Retracted in place rather than deleted (§5 P3b, §8 DR-39, §0, H-7), because the merged analysis
doc stated the error and four of this plan's conclusions followed from it.

Also decided: DR-41 deferred until P0–P4 ship; DR-42 added (invert `open_synthetic_triples` to an
inclusion list, as its own change before P3); `recorded_generations` lands locally on the
`current_seq_anchor` precedent and is upstreamed opportunistically; both new ops get an MCP
surface, which makes their report shapes contracts from release one and requires bounded defaults
on `verify_history`.

### 2026-10-28 — P1 baseline captured (Q-5), and one new hazard

`tests/fixtures/temporal/verify_history_baseline.json` is the pre-analyze report on the self-host
store: 6,442 generations in the window, 25 sampled, `ok=1`, `no_digest=8659`, byte coverage 0.01%,
1 distinct digest, 485 distinct files with no digest. Committed as P1's regression baseline and its
no-coverage test input, so the re-analyze can proceed without destroying the evidence.

Two fixes to the prototype fell out of capturing it. `--offenders` capped only the human render, so
the JSON report was ~570 KB of entity ids (useless as a fixture); it now caps both, the cap is
recorded in the report as `offenders_per_outcome`, and `offender_distinct` carries the uncapped
count so the render's "N distinct files" stays true. Two pre-existing ISC004 lint errors in
`render` fixed in passing.

**H-11, new and measured:** entity ids changed spelling mid-history. `{file}::module` ids are
`.\core_indexer\…` up to 2026-10-03T22:03:05Z and `core_indexer/…` from 2026-10-03T22:02:56Z on.
The 6,308 old-spelling concepts are correctly `retired=1`, so the present state is sound — but P2
classifies by id, so a window straddling that date would report ~6.3k removals and ~6.3k additions
for code that never changed. P2 must normalise ids or detect and report the condition.

### 2026-10-28 — full re-analyze run (Q-5), and a write-path defect it exposed

`reindex --full` (which routes to `coderadar.analyze`, `ops.py:1650`) on the self-host store.
**The blob write path works.** Report: 273 blobs stored, 3,240,623 bytes, 0 oversize, 1 excluded.
`verify_history` on the new generation: 274 files, 273 `ok`, 1 `no_digest`, **99.6% coverage**, 272
distinct digests resolved against disk with 0 unresolvable and 0 mismatches. Across the whole
window coverage is 3.19%, because the 6,441 historical generations stay byte-less — as expected,
and the reason P5 exists. Crosscheck now meaningful rather than trivially true (before, engine and
replay both answered `None` everywhere): **20/20 agreement**.

After-report committed beside the baseline as `verify_history_after_reanalyze.json`.

The one remaining `no_digest` is correct by design: `core_indexer/src/scaffold/secrets.rs` matches
the built-in `*secret*` pattern in `SECRET_BLOB_EXCLUDE_DEFAULTS` (`graph/config.rs:39-54`). Worth
knowing that the secret guard is **name**-based, so a source file that merely *implements* secret
detection is permanently unreconstructible, silently. Not a bug; a policy cost to state.

Second prototype fix: SQLite `LIKE` is case-insensitive for ASCII, so the `%::module` filter also
matched `types.rs::Module` — a struct named `Module`, counted as a module with no digest. The fold
now adds a case-sensitive `GLOB`, which is what moved the new generation from 99.3% to 99.6%.

**H-12, the real find.** The run printed `[diag] persist_edges failed: UNIQUE constraint failed:
links.source_id, links.target_id, links.edge_type, links.valid_from, links.recorded_at,
links.branch_id` — and still exited 0 with a clean JSON report. Traced to the end:

- `INSTANTIATES` is derived from `Function.resolved_calls`, a `Vec`, with no dedup
  (`persistence.rs:316-345`). A function constructing the same class twice emits two identical
  assertions, and every assertion in a run shares one `valid_from`, so the pair collides on the
  `links` UNIQUE key. Measured on this repo: **53 functions** with duplicate `Constructor` targets
  (e.g. `CodeGraph.apply` constructs `CodeRadarError` and `MutationResult` more than once each).
- Each 200-row `assert_edges_bulk` is flushed with `?`, so the abort leaves earlier batches
  committed: a **partial** edge write, not a failed one. 2,619 link rows landed on 2026-10-28.
  The loops before `INSTANTIATES` (CALLS, IMPORTS, EXTENDS, IMPLEMENTS, DECORATES) persisted;
  `INSTANTIATES`, `REFERENCES` and `OVERRIDES` were partly or wholly lost.
- The caller only prints `[diag]` (`lib.rs:1022-1023`), so nothing in the exit code or the report
  says the store's edge state is incomplete.

`resolved_refs` has the same Vec-without-dedup shape but no duplicates on this repo (0 functions),
so it is a latent instance of the same defect. The fix is one dedup set covering every loop, which
is also the guard future edge types need — and P3's `HAD_CONTENT` would inherit the non-atomicity
as it stands.

### 2026-10-28 — H-12 fixed: edge dedup, and the failure made visible

Two halves, because H-12 was two defects wearing one error message.

**The cause.** Deduped at the flush boundary (`persistence.rs:161-176`): a run-level `queued` set
of `edge_key` triples filters each batch before `assert_edges_bulk`, and all ten flush sites now go
through it. Deliberately not fixed per loop — one guard covers the nine current loops and every
edge type added later, which matters because the two offending loops (`INSTANTIATES` over
`resolved_calls`, `REFERENCES` over `resolved_refs`) are the only ones walking `Vec`s today and a
new one would reintroduce the bug. `edge_count` is untouched: it reports what the projection holds,
"whether newly asserted or already open", so a repeated projection entry still counts twice.

**The silence.** `EDGE_PERSIST_ERROR` in `storage.rs`, on the same terms as `BLOB_STATS`:
process-global, reset at the analyze/load boundary (both call sites), because the cheap reindex path
persists edges through `update_file` and per-call threading would miss it. Surfaced as the
`edge_persist_error()` pyfunction → `coderadar.edge_persist_error()` → `ops.reindex`'s report key
(present only when it happened, so a clean report is unchanged) → a `render.reindex` block that says
the edges are INCOMPLETE and quotes the engine. The `[diag]` line became a `[coderadar] WARNING`
naming the consequence (`lib.rs:1029-1039`).

**Residual, stated rather than fixed:** the batch flushes are still not one transaction. A failure
from some other cause would still leave the batches before it committed — but no longer silently.
P3's `HAD_CONTENT` writer must use this flush path rather than adding its own.

Tests. Rust: `test_persist_edges_dedups_repeated_constructor_calls` builds the two-construction
fixture, asserts the duplicate is really in the projection (or it would pass for the wrong reason),
then persists. Verified to fail without the fix with the exact production error —
`UNIQUE constraint failed: links.source_id, ...`. Plus
`test_persist_edges_is_idempotent_across_runs`, since the new guard sits next to the open-interval
check. Python: `tests/test_edge_persist_reporting.py` (5) covers a clean report omitting the key, a
multi-batch duplicate-heavy tree, and both render branches.

Gates: `cargo fmt --check` clean, `clippy --all-targets -D warnings` clean, Rust lib **501 passed**
+ 1 ignored, `ruff` clean, related Python suites 30 passed.

**Re-analyze with the fix:** no abort, no warning, exit 0, 274 blobs. The run wrote the 743 edges
the aborted one had left out, and all seven structural types are now open in the store:
CALLS 8998, IMPORTS 1046, INSTANTIATES 576, OVERRIDES 138, REFERENCES 96, EXTENDS 77, DECORATES 77.

**Installed and re-verified (same day).** The first pass could not replace the repo's `_core.pyd` —
`coderadar mcp serve` held it, the precedent noted above — so the tests and the re-analyze ran
against the freshly built `core_indexer.dll` staged on `PYTHONPATH`. The servers have since been
stopped and `maturin develop` run, and everything was re-run against the **installed** extension
with no staging: `coderadar.edge_persist_error()` resolves through `_core` and answers `None`;
related suites 35 passed; **full Python suite 1,160 passed** (excluding `tests/precision` and
`tests/cr_edit_tests`); `reindex --full` renders a clean report with no warning block and nothing on
stderr. Three `mcp serve` processes were killed to get there (one parent, two children) — restart
them if they are wanted.

### 2026-10-28 — P2 `changes_between` landed (`92aafcb` on `dev`)

What shipped: `all_concept_ids` in `storage.rs` (ever-not-live, no kind filter, branch-scoped,
both H-11 spellings) + `canonical_id` + `content_hashes_at` FFI (one fold serves N ids:
resolved id, `content_hash`, line span; `None` hash/span where the projection carries none) with
snapshot-then-release locking; `ops.changes_between` (canonical grouping, any-present
presence, T2 display spelling, leftover hash-pairing only with no rival claimant, `uncompared`
class for hash-less kinds) + `render`, `CodeGraph.changes_between`, CLI `changes-between`, MCP
`coderadar_changes_between` per Q-4 (no scan cap needed: cost is two folds, ~1 s on the
self-host store); both skills updated (29 tools); count pins 28 → 29 in battery, project-path,
README, ARCHITECTURE, server comment.

Tests: 1 Rust unit test (full ever-universe incl. a retired module's function; modules-only is
a strict subset) + 19 Python tests. Full gates at landing: Rust lib 504 passed + 1 ignored,
full Python 1201 passed + 1 skipped, fmt/clippy/ruff clean, rebuilt `_core.pyd` (no lock fight
this time — no `mcp serve` was running).

Self-host H-11 proof: `changes_between(2026-10-03T21:50, 2026-10-03T22:10)` — the twenty minutes
straddling the respelling — in 0.8 s: `normalized_ids: true`, 5,235 merged groups, 3,116
unmodified, modified 231, moved 101 (same hash, shifted spans — genuine moves through the
respelling), added 126 / removed 115 (real churn incl. a test rename), uncompared 1685. No
phantom 6.3k × 2. An earlier attempt with T1 = 2026-10-01 returned all-added: most old
concepts have `valid_from` 2026-10-03 (asserted in the migration-era re-analyze, live ~20 min),
so at 10-01 they genuinely did not exist — the diff was honest, the window was wrong.

Findings, all read-path-relevant:

- **Old-spelling-at-old-T resolved to `None` (shared lookup bug, fixed).** `entities_at`
  canonicalized before looking up, so an old spelling at a T when only it was live missed
  (canonical = new spelling, absent from the old projection), while the same spelling at a
  new T hit. Both `entities_at` and `content_hashes_at` now try raw-exact first, then
  canonical, then the dotted fallback — strictly more correct (miss → hit only; live-T ids
  are already canonical so nothing else changes). This also fixes `as_of`/`Snapshot.find`
  for pre-respelling spellings. `source_ref_from_state` stays exact-only: P1's sweep passes
  the ever-universe raw, so both spellings hit whichever is live; unifying P0 singles with
  lookup semantics is a follow-up, not P2.
- **Same-id `moved` is nearly unreachable via `update_file`.** The update path rewrites a
  function only on signature/body change (`projection_ops` needs-insert gate), so a pure
  reorder mints no row and the ledger keeps write-time spans: on a `d.py` reorder fixture
  both functions read unmodified. Pinned as `test_pure_reorder_is_invisible_to_the_update_path`.
  Same-id moved surfaces after full re-analyze rewrites (the self-host window shows 101) —
  the read side is ready; whether the write path should mint span-only rows is a DR-worthy
  decision (write amplification vs span history), explicitly not P2 (read-only, no schema
  change). The spec fixture's whitespace-move yields `moved` only through that future path;
  the test drives it through crafted folds instead.
- **Rename reads as removed + added, by hash construction.** `content_hash` covers the full
  span including the name, so `beta` → `beta2` with an identical body still hashes
  differently — leftover pairing correctly refuses to merge them. Cross-id `moved` is real
  file moves with byte-identical text (proven by `e.py::eco` → `f.py::eco`).
- **`uncompared` is a fifth class, deliberately.** Imports, constants, type aliases and routes
  carry no `content_hash` — presence is all the ledger can say. Both-present reads as
  `uncompared`, never silently as unmodified. File-level recall is preserved regardless:
  any byte change still modifies the module hash.
- **Line-anchored import ids shift as removed + added** (`import@390` → `import@406` across
  the H-11 window). Honest under presence-only semantics (R2-17 ids are line-stable by
  construction); pairing them by line-proximity would be guessing.
- **Concurrent-write hazard vacuous for immutable history.** Each fold is deterministic at
  its T; a vanished lookup answers absent per the `__init__.py:1559` precedent rather than
  erroring. Recorded in the op docstring.
- **Deviations from §5:** MCP ships with P2 (Q-4 decided both ops get one — item 3 never
  excluded it, unlike P1's item 7); `uncompared` extends the four-class contract for the
  reason above; same-id `moved` tested via crafted folds, not the live fixture.

### 2026-10-28 — DR-42 closed (`600d689` on `dev`)

Q-2's standalone prerequisite for P3. The audit: ledger edge writers are (1) tree-sitter extraction — the 8 uppercase structural kinds, owned by `retire_stale_edges` (which was already an inclusion list); (2) the framework hook — `synthetic:{resolver}:{edge}` via the single choke point `framework.py::_ledger_synthetic_edge_kind` (13 resolvers, edge parts observed: handles, registers, navigation, callback, depends_on, association); (3) the direct `register_synthetic_edge(s)` path, which preserves caller-chosen kinds verbatim (tests use `DEPENDS_ON`, `HANDLES`). The old `open_synthetic_triples` exclusion list swept classes (2) and (3) plus the four structural kinds it did not exclude — so a 9th structural kind, or any new synthetic writer, would have landed in the hook's retire scope silently (H-3). The hook now sweeps `edge_type LIKE 'synthetic:%'`; the membership rule is stated at the choke point, the query, and the `FRAMEWORK_SYNTHETIC_EDGE_PREFIX` const. A new resolver is covered automatically through the namespace; a kind outside it is visibly not retired — that is the point. No behavior change in any existing test (the hook's scope filter only ever matched route-id sources, which carry only hook-written edges); pinned by a Rust test seeding all three classes plus the retire round-trip, and a Python guard proving route removal still retires while a same-file CALLS survives. Full gates: Rust 505 passed + 1 ignored, Python 1202 passed + 1 skipped, fmt/clippy/ruff clean, rebuilt `_core.pyd` with no lock fight.
