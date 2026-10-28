# Temporal code reconstruction: walking time and branches over recorded bytes

> **Date:** 2026-10-27 · **Baseline:** CodeRadar v0.13.0 (`e68c443`, `main`) on `macrame-db 0.19.1`
> **Status:** design proposal — nothing here is implemented. No decision register row yet.
> §6 has a working prototype (`scripts/verify_history.py`) and §2.5 reports what it measured.
> **Question answered:** can the bitemporal ledger carry *code* history (not just structural
> history), is one content-addressed blob per file enough, and what is the lightweight diffable
> structure that makes "how did this change between T1 and T2" a query rather than a byte fetch.
> **Builds on:** [`code-storage-vs-macrame-philosophy.md`](code-storage-vs-macrame-philosophy.md)
> (doctrine conformance of the blob design — its verdict stands unchanged) and
> [`store-and-retention.md`](store-and-retention.md) (hot/cold, backup pair).
> **CodeRadar refs:** `core_indexer/src/storage.rs:196,953,1601,1719-1753,1815,2085`,
> `lib.rs:949-963`, `graph/projection_ops.rs:828`, `py_agent/src/coderadar/__init__.py:1481-1600`.
> **Macrame refs:** `src/blob.rs`, `src/temporal/archive.rs:1095`, `src/connection.rs:3074,3545,3570`,
> `src/branch.rs`; D-281/D-287/D-288, D-213/D-214/D-228.

## TL;DR

The blob design is not the weak link and needs no replacement. Two things are missing, and one
of them is already half-built:

1. **Sampling frequency.** `valid_from = now_iso8601()` at index time makes the ledger's timeline
   *"when CodeRadar looked"*, not *"when the code changed"*. Contents that existed between two
   runs exist nowhere. No blob scheme fixes this; only commit-driven ingestion does.
2. **No queryable version spine.** Answering "every content this file ever had" means folding at
   candidate timestamps and comparing `extra`. It should be a graph walk.

The lightweight diffable structure is mostly present already: every entity at every generation
carries a `content_hash` (xxHash64 hex of its body) in both its canonical `content` JSON
(`storage.rs:1815`) and the mirrored `extra.coderadar` the fold already hands back
(`coderadar_extra`, `storage.rs:1737`). Entity-level change detection across time therefore needs
**no byte reads and no schema change** — it needs an API. Expose that first, add a version-spine
node second, and keep diffs as a read-time computation cached by `(digest_a, digest_b)` — never as
the record of truth.

One finding came out of this pass that is not in either prior doc: **function body text is already
stored per version in the ledger** (`v["source"]`, `storage.rs:2085`, carried in `concepts.content`).
That makes body-level diffing free today and makes ledger payloads carry the bytes twice. §3.4
states the choice that creates.

**Measured, not argued (§2.5):** on this repo's own store the byte coverage of recorded history is
**0.0%** — one file-generation of 5,079 sampled across 6,442 generations. The structural history is
intact and the bytes are absent, because 6,441 of those generations predate the blob write path and
no analyze has run since it landed. That makes §2.2 the live problem on real data, not a
hypothetical.

Two things not to do: **delta blobs** (the reference scan cannot see a base digest that lives
inside another blob's bytes — D-287) and **compression inside CodeRadar** (the address *is*
`sha256(stored bytes)`; compressing above the addressing layer breaks both the address and the
hot-file `size` CHECK).

---

## 1. What exists today, stated precisely

### 1.1 One blob per distinct content, not per file and not per generation

`put_source_blob` (`storage.rs:196`) writes the whole file's bytes on every full analyze
(`lib.rs:956`) **and** every incremental update (`graph/projection_ops.rs:828`). Macrame addresses
the bytes by SHA-256 in lowercase hex (`blob.rs`, D-281 amendment 3) and a re-put of held bytes
updates `put_at` only, never the row's bytes.

Consequences, all of them load-bearing:

- An unchanged file re-put across 500 generations is **one** row whose `put_at` advances. A file
  that changed 50 times is 50 rows.
- The storage ceiling is **Σ(distinct contents × size)**, not generations × repo size. For a repo
  where a minority of files churn, this is already close to optimal — it is git's loose-object
  behaviour before packing.
- A re-put is the application announcing "I still want this", which is exactly what the archive's
  age guard reads (D-281 amendment 1). Analyze's unconditional re-put is therefore not waste; it is
  the retention signal.

### 1.2 The reference, not the bytes, is what is versioned

The digest lands in `extra.coderadar.source_blob` on the file's module concept, and `extra` is
carried per concept version. So the *pointer* is bitemporal and the *bytes* are timeless and
branch-global. This is the shape Macrame's schema was built for (`kv_store`'s neighbour: outside the
ledger, yet an archive participant because past ledger states name it) and the philosophy doc's
doctrine-VI finding covers it.

### 1.3 Reconstruction at T already works

`Snapshot._read_raw` (`__init__.py:1539`) folds the entity at T, reads
`entity_source_ref_at` (`storage.rs:953`) for `(digest, span, flags)`, `blob_get`s the bytes, and
slices. It distinguishes four outcomes honestly — not in the graph at T (`None`), no blob recorded,
digest unresolvable, span exceeding blob length — and the last of those is a ledger/blob
disagreement detector that already exists per read (`__init__.py:1597`).

`read_bytes` is exact (CRLF and non-ASCII preserved); `read_source` decodes lossily for display.
Module ids read the whole blob, which is the only read that is whole-file exact — entity spans are
tree-sitter node extents, so a trailing line break belongs to no entity.

### 1.4 Retention is safer than it looks

A blob leaves the hot file only inside an archive session, only when last put before the cutoff,
**and** only when no hot log entry names its digest (`archive.rs:1095`, D-287/D-288). A cold-only
blob that a hot entry names is copied back. There is no refcount and no second reclamation path —
which is precisely why CodeRadar must never introduce its own blob GC (D-281 gate 3).

The one real failure mode is operational: **a hot-only backup is pointers**. `store-and-retention.md`
already pins the quiesce → `wal_checkpoint(TRUNCATE)` → copy-the-pair drill.

### 1.5 Put-then-assert is the crash-safe direction

`lib.rs:949` states it as a contract: bytes hit the blob store before the digest is asserted, so a
crash leaves orphan bytes (bounded, aged out by the scan) and **never a live concept naming absent
bytes**. The reverse ordering would strand history. This is not an optimisation target.

---

## 2. The gaps, ranked by how much they hurt "reconstruct the code at T"

### 2.1 Sampling frequency — the one that matters

`now_iso8601()` (`storage.rs:68-79`) is used as `valid_from` for concepts and edges alike. The
comment there is about microsecond width, which is correct and load-bearing for
`recorded_at <= ts` folding; the *semantic* consequence is recorded one layer up, as mismatch #3 in
the philosophy doc ("`valid_from = index-now` conflates world time with learn time"). That entry
understates it for the purposes of this document.

The ledger's timeline is a sequence of **indexing events**. Between two analyze runs a file may
have passed through ten states; nine of them have no blob, no concept version, and no trace
anywhere. Watch mode samples finer but still samples, and a `git checkout` between two runs is
invisible — the ledger will show one jump that corresponds to no single edit anyone made.

This is the gap that no blob-side change can close. §5 is the answer to it.

### 2.2 Write-path skips leave structure without bytes

Four ways a module concept exists at T with no `source_blob`:

| Cause | Where | Visible as |
|---|---|---|
| `store_source_blobs = false` | `storage.rs:198` | `BlobOutcome::Disabled` |
| `blob_exclude` match (secrets defaults) | `storage.rs:204` | `SkippedExcluded` |
| `> 8 MiB` (`BLOB_MAX_BYTES`, `storage.rs:1601`) | `storage.rs:217` | `SkippedOversize` |
| `blob_put` error | `lib.rs:963` | diag line, **indexing continues** |

Each surfaces at read time as `ContentUnavailable` with a message naming the cause, which is the
right behaviour (doctrine VIII: fidelity is a parameter, never silent). What is missing is the
aggregate: `blob_stats()` reports the *current* session's puts and skips, so there is no way to ask
**"what fraction of generation 7 is byte-recoverable?"** §6 fixes that.

### 2.3 Only indexed files have bytes

Blobs exist for files the indexer walks. "Reconstruct this file at T" works. "Reconstruct the
repository at T" does not, and will not under this model — non-code files, lockfiles, assets and
anything behind the walker's filters have no recorded bytes at any T. Worth stating in the API
contract rather than discovering.

### 2.4 Cold-file loss

Covered in §1.4: degrades honestly, never silently, and is an operations problem rather than an
architecture one. Listed here only so the ranking is complete.

### 2.5 What this is on the repo's own store, measured

Run of `scripts/verify_history.py` (the §6 prototype) against
`.coderadar/store/coderadar.db` on 2026-10-27:

```
window           2026-09-10T18:14:13Z .. 2026-10-26T16:58:19Z
generations      15 sampled of 6442
totals (file-generations)  ok=1  no_digest=5078  unresolvable=0  digest_mismatch=0
byte coverage              0.0%
distinct digests checked   1
```

The `blobs` table holds **one row, 30 bytes**, named by `m.py::module` — a scratch file, not repo
source. Across a 114 MB store, 6,442 recorded generations and 490 known modules, that is the whole
of the recorded code bytes.

**Why, exactly.** The write path landed in `6065b0a` (2026-10-06, "feat: DR-25 §1.9 put-on-index").
The store's generations fall on 2026-09-10 (184), 10-03 (3,908), 10-04 (826), 10-06 (1,519) and
10-26 (**1**). Even the latest 10-06 generations carry no digest, so no analyze has run through the
new path; the single 10-26 write was an incremental update touching one file, and that file's bytes
are the one blob.

Three things follow, and the third is the one that matters:

- **Nothing is corrupt and nothing was lost to archival.** `unresolvable=0` and
  `digest_mismatch=0`: the retention machinery (§1.4) is not implicated at all. There was never
  anything there to lose.
- **The gap is write-path coverage, exactly as §2.2 describes** — not blob design, not archival,
  not the cold file.
- **A full re-analyze fixes the present and not the past.** It puts bytes for *current* content
  only. The 6,441 historical generations describe content that exists in no blob, no payload and no
  working tree, so they are **permanently byte-less**. §2.2's "pre-blob generation" case is not a
  footnote on this store; it is all of it. This is also the sharpest available argument for §5: a
  commit-driven ingest can *reconstruct* that history from git, whereas analyze-driven indexing
  cannot recover a single byte of it.

The measurement is worth repeating after any full analyze, and after any archive session — the
first to confirm coverage went up, the second to confirm `unresolvable` stayed at zero.

### 2.6 The read API cannot sweep history: it folds per call

Found while building the §6 prototype, and it shapes §6's implementation.

`entity_source_ref_at(id, T)` calls `self.reconstruct(timestamp)` on every invocation
(`storage.rs:957`) and then reads one concept out of the resulting `MaterializedState`. Measured on
this store (40k log entries, 12,857 concepts): **~0.36 s per call**, and `lookup_entities_at`
likewise folds once per call (`lib.rs:1752`) but returns projection dicts that carry no `extra`,
so the digest is not in them.

That is correct for the API's purpose — one entity, one instant, one read — and unusable for a
sweep: one 490-module generation costs ~3 minutes, a 12-generation sample ~35. There is no
batched route from Python today, because the one call that would amortise the fold drops the field
the sweep needs.

Consequence for §6: the shipped op needs one new primitive,
**`source_refs_at(entity_ids, T) -> {id: SourceRef}`** — one `reconstruct`, N refs out of the same
state. It is a small, local addition beside `entity_source_ref_at` and it turns a sweep from
O(files × generations) folds into O(generations). The prototype works around its absence by
replaying `transaction_log` payloads itself, which is a doctrine-I violation acceptable in a
measurement script and not in shipped code — see §6.

---

## 3. The lightweight diffable structure

Three layers, cheapest first. Each is useful alone; each is a prerequisite for the next only in the
sense that it makes the next cheaper.

### 3.1 Layer 1 — entity-level change detection with no byte reads (no schema change)

Every entity already carries a body hash, xxHash64 rendered as hex, in two places: the canonical
`content` JSON (`meta[annotation::CONTENT_HASH]` from `f.body_hash` and friends, `storage.rs:1815`,
with the per-kind writers at `storage.rs:2014-2091`) **and** the `extra.coderadar.content_hash`
mirror that `coderadar_extra` writes for exactly this class of maintenance query
(`storage.rs:1737`). The mirror is the one to read: `extra` comes back with the folded concept, so
no `content` parse is needed.

So `changes_between(T1, T2)` is: fold twice, join entities by id, compare `content_hash`. That
yields **added / removed / modified / moved** (moved = same hash, different span) across the whole
graph by comparing 16-character strings, with zero `blob_get` calls and no new storage.

Caveat to write into the docstring: xxHash64 is non-cryptographic and 64-bit. It is adequate for
change *detection* and for ranking what to look at; it must never decide whether to skip a write
whose correctness matters, and it must never be presented as proof two bodies are identical. When
proof is wanted, the bytes are one `read_bytes` away.

Note also that a 16-hex-character hash is **invisible to the archive's reference scan** (which
matches 64-char lowercase-hex runs). That is correct here — `content_hash` names no blob — but it is
the reason §3.5's warning exists.

### 3.2 Layer 2 — a version spine, so "when did this change" is a graph walk

Promote the content pointer from an `extra` field to a first-class node:

```
concept  {path}@{digest}        kind = content_version
         extra.coderadar: { source_blob, size, line_count, parent_digest }
edge     {path}::module  --had_content-->  {path}@{digest}   [valid interval]
```

(`content_version`, not `file_version`: module concepts already carry an unrelated `file_version`
integer in their content JSON, `storage.rs:2013`/`types.rs:919`, and reusing the name would make
two different things grep identically.)

Properties worth having:

- **One concept + one edge per actual content change.** Unchanged files add nothing, because the
  id is derived from the digest and the upsert is idempotent — the same skip-if-open discipline
  `persist_edges` already uses for open edge triples.
- **The id is content-addressed**, so the node is immutable by construction and two branches that
  reach the same content share the node.
- **`parent_digest` is the diff spine** — a DAG of digests with bytes on the side. Walking "every
  content of this file, newest first" becomes an edge traversal with the valid interval telling you
  *when* each content was true.
- **The digest appears in a ledger payload**, so the reference scan sees it (§3.5).

The existing `extra.coderadar.source_blob` on the module concept stays — it is what
`entity_source_ref_at` reads, and removing it would break every at-T read for no gain. The spine is
additive.

### 3.3 Layer 3 — diffs computed on read, cached by content pair

Never store a diff as the record of truth. Cache key is `(digest_a, digest_b)`: two immutable
addresses, so the entry is immutable and discardable at any time without losing history. The
philosophy doc states the rule already under doctrine VII ("key cached diffs by
`(digest_a, digest_b)` if ever needed, never by mutable id"); this document only confirms it
survives contact with the version spine.

Where the cache lives is a later decision. `kv_store` is the obvious candidate (outside the ledger,
operational, branch-global). A cache entry stored as a *blob* would need its key published in the
ledger for the same reason §3.5 gives.

### 3.4 The bytes are already in the ledger twice — decide on purpose

`v2_function` writes `v["source"] = f.source` (`storage.rs:2085`), and the v2 JSON is the concept's
canonical `content` (`build_concept`, `storage.rs:1757`). So a function's body text is stored, per
version, inside a ledger payload — alongside the blob that holds the same bytes as part of the
whole file.

Three consequences, none of them currently written down:

- **Body diffing needs no blobs at all today.** Fold at T1 and T2, read `content.source`, diff. For
  functions and classes this is a complete answer already; §3.1's hash tells you *whether* to
  bother.
- **Log payloads carry source text.** The archive's reference scan reads hot log payloads to decide
  blob liveness, and `blob_scan_bytes` is the number it reports (D-287). Source text in payloads
  makes that scan proportionally more expensive, and makes the hot file grow with body churn
  independently of the blob table's dedup.
- **It is duplication with different lifetimes.** The blob is reclaimable by the scan; the payload
  copy is not reclaimable at all until the row archives. Doctrine V — no physical deletion in hot —
  applies to the payload copy and not to the bytes it duplicates.

The choice to make deliberately: **lean on it** (declare `content.source` the body-diff path, keep
blobs for whole-file exactness and non-function bytes) or **trim it** (store spans only and slice
bodies from the blob on read, which is what `entity_source_ref_at` already does). Trimming is the
cleaner architecture and the cheaper hot file; it is also a `content` schema change with a `format`
bump and a read-compat path for existing generations, so it is not free. This document does not
decide it — it records that the duplication is a decision, not an accident, and that §5's
per-commit ingestion multiplies whichever way it goes.

### 3.5 Two things not to do

**No delta blobs.** (Same scan, different trap from §3.4's duplication.) A blob survives only while some hot log entry names its digest in a payload
(D-287). A delta blob's base digest would live *inside another blob's bytes*, and the scan never
reads blob contents — only log payloads. The base would therefore become archive-eligible, and
eventually cold-file-loss-eligible, while a live delta depended on it. The failure is silent and
arrives one archive session late.

If deltas are ever unavoidable: the base digest **must** be published in a ledger concept's `extra`
(the version spine is the natural place), and a full materialisation must be written every N links
so chain depth is bounded. The recommendation stands at *don't*: content-addressed dedup plus the
spine captures most of the saving at none of the fragility.

**No compression inside CodeRadar.** The address *is* `sha256(stored bytes)`, and the hot `blobs`
table ties `size` to the byte length by CHECK. Storing zstd bytes under a plaintext digest breaks
verification, the address, and that constraint at once; keeping a second plaintext digest in `extra`
alongside a compressed blob address breaks `blob_get`'s meaning instead. Compression belongs below
the addressing layer, i.e. inside Macrame, as its own decision.

---

## 4. Branches

Macrame is ready. CodeRadar is not using it.

**Macrame side (no work, and no changes wanted).** Blobs and `kv_store` carry no `branch_id`
deliberately: bytes and operational state are branch-global, and what is versioned per lineage is
the *reference* (D-213/D-214). `fork`, `branches()`, `reconstruct_on` (`connection.rs:3570`) and
`diff(a, b)` (`connection.rs:3074`) exist. `diff` is edge-level and has no instant parameter by
design — a diff filtered to an instant cannot report the divergence that is *about* an instant
having passed — so cross-branch **content** diffing is CodeRadar's to build on §3.2's spine.

**CodeRadar side.** There is no branch parameter anywhere in the facade: `Snapshot`, `ops.as_of`,
and `entity_source_ref_at` are trunk-only. Branch-aware reading means threading a lineage through
the fold (`reconstruct_on`/ancestry-bounded) and *then* `blob_get`-ing branch-global bytes — the
order matters and is the philosophy doc's consequence (b).

**Invariants to carry forward so a future branch feature cannot regress history:** never add
per-branch blob copies; never add branch-scoped blob GC; `put_at` refresh stays lineage-independent
(correct, because eligibility also requires "no hot log entry names it" in the archiving lineage's
scan).

---

## 5. Commit-driven ingestion: the only thing that closes §2.1

If the goal is to walk time *and* branches and see how the code actually changed, the decisive
change is not about blobs. It is about what drives ingestion:

- one Macrame branch per git branch, forked at the merge-base;
- per commit: `valid_from = commit time`, `recorded_at = ingest time`;
- blobs per file-at-commit — far cheaper than it sounds, because content addressing means unchanged
  files across commits cost nothing beyond a `put_at` touch.

Then bitemporality means something real: valid time is the code's own history, recorded time is when
CodeRadar learned it, branches are branches, and **backfilling an old branch becomes a legitimate
retroactive correction** rather than a lie told in valid time. This is the only configuration in
which "reconstruct the file at T" has no sampling gap.

It also retires mismatch #3 in the philosophy doc rather than documenting around it, and it is the
point at which `as_of_valid` vs `as_of_recorded`/`reconstruct` (D-174) stop being the same question
asked twice.

Cost to be honest about: a full history ingest is one write per commit per branch, each carrying its
file set; the blob table absorbs the content union of the history, not the sum of the checkouts.

---

## 6. Do this first: make the gap measurable

Before any of §3–§5, add an integrity op — `verify_history(from, to)` — that folds every recorded
generation in the window and reports, per generation:

- module concepts with no `source_blob`;
- recorded digests that do not resolve (hot or cold);
- blobs whose recomputed SHA-256 disagrees with their address;
- recorded spans exceeding the blob's byte length.

It is cheap, it is read-only, and it converts "there might be a gap between the structural history
and the bytes" into a number per generation. §2.5 is that number for this repo, and it decided the
question: §2.2 is not theoretical here, it is the whole store.

**Prototype.** `scripts/verify_history.py` does this today — sampled or `--all` generations, the
four outcomes above plus `digest_malformed`, one fetch-and-re-hash per distinct digest, and
`--crosscheck N` which compares its answers against the engine's (25/25 agreement on the run in
§2.5). Read its docstring before quoting its numbers: it replays `transaction_log` payloads to get
all digests at T in one query, because of §2.6.

**What the shipped op needs that the prototype fakes.** One batched primitive,
`source_refs_at(entity_ids, T)` (§2.6). With it, the op is a loop over generations with no fold of
its own — the engine folds once per generation and the Python side only counts, which is the
division of labour doctrine I asks for. Without it the op would either re-implement folding
(forbidden) or take ~35 minutes for twelve generations (useless). Everything else it needs already
exists: `reconstruct`, `blob_get`, and the span check already written for single reads
(`__init__.py:1597`).

---

## 7. Suggested order

| Step | What | Schema change | Closes |
|---|---|---|---|
| 0 | `source_refs_at(ids, T)` batched primitive (§2.6) | none | makes step 1 affordable |
| 1 | `verify_history(from, to)` — prototype exists | none | measures §2.2/§2.3 |
| 2 | `changes_between(T1, T2)` (Layer 1) | none | entity-level diffability |
| 3 | Version spine + `had_content` (Layer 2) | additive | "when did this change" as a walk |
| 3b | Decide the `content.source` duplication (§3.4) | none, or `format` bump if trimmed | hot-file growth, scan cost |
| 4 | Branch-aware reads (`reconstruct_on` through the facade) | none in Macrame | §4 |
| 5 | Commit-driven ingestion | ingestion rewrite | §2.1 |

Step 0 exists only because of §2.6 and is a prerequisite for step 1 alone. Step 2 is independent of
everything else. Step 3 makes step 5's
per-commit writes natural rather than awkward. Step 4 is worth doing before step 5, because a
branch-unaware reader over a branched ledger answers whole-ledger belief where the caller meant one
lineage's view — and that is a wrong answer, not a missing feature.

## Judgment

Keep the blob design exactly as the philosophy doc approved it: content-addressed whole-file bytes,
digest in versioned `extra`, retention by the reference scan, put-before-assert, hot+cold backup
pair. Add a version spine and two read APIs on top; compute diffs on read and cache them by content
pair. Resist delta chains and in-CodeRadar compression, both of which trade a bounded storage win
for an unbounded correctness risk against D-287's scan.

The honest framing of the original worry, in three parts. There is **no gap between Macrame's
structural history and the bytes it names** — put-then-assert makes that direction impossible, and
the scan makes the retention direction safe; §2.5 confirms it on real data with zero unresolvable
digests and zero mismatches. There *is* a gap between **what the ledger describes and what it
recorded bytes for**, and on this store it is total (§2.5) — a write-path coverage problem, fixed
going forward by a full analyze and never fixed backwards. And underneath both there is the gap
between **the code's history and the ledger's samples of it**, which is closed by changing what
triggers a write (§5), not by changing how bytes are stored.
