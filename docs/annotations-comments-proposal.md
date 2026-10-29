# Annotations & comments proposal (P6 programme sketch)

**Status (2026-10-29): proposal only — no code, no schema change, no DR filed.**
Answers the maintainer's "annotate every type/edge with structured markdown,
store comments in the database, store commit messages separately" as structure,
architecture, and ideas. Intended reader: whoever scopes the next programme
after P5 (suggested name: **P6 annotations**).

## 1. The one idea

CodeRadar already stores non-code concepts: the P3 spine (`content_version`),
P4 pointers (`branch_content`), P5a commit markers (`commit:{sha}`). A comment
is the fourth: **`kind = 'comment'`**, a concept whose payload is markdown and
whose history is the ledger itself (edit = supersede, delete = retire — the
"who checked what when" comes free from `valid_from`/`recorded_at`, the same
way `history_of` gets file history for free).

Commit messages are the fifth thing that already half-exists: P5a.3 commit
concepts carry author time, identities, and `stamp_nudged`, but **not the
message**. The message belongs in the commit concept's payload — no new kind,
no new id scheme, just the rest of P5-D2.

## 2. What "annotatable" means

Every *type* is a concept id (`a.py::module`, `a.py::g`, routes,
`{path}@{digest}`, `commit:{sha}`, `branch:{b}:content:{path}`).
Every *edge* is a `(source_id, target_id, edge_type)` triple in the link
ledger, open over a `valid_from → valid_to` interval (edges retire, never
delete). The traversable registry (`ALL_EDGE_KINDS`, `lib.rs`) is exactly
this — all addressable as strings:

| edge kind | meaning (one line) | derived by |
|---|---|---|
| `calls` | function → function it invokes | call resolution cascade |
| `imports` | module → module it depends on | import graph |
| `extends` | class → base class | MRO / base unfold (§2.6: by target declaration) |
| `implements` | class → interface | same partition, interface side |
| `decorates` | function → its decorator | unique-hits-only (§2.6 slice C) |
| `instantiates` | function → class it constructs | overlaps `calls`, kept separate (slice D) |
| `overrides` | method → parent method it overrides | inheritance analysis |
| `references` | entity → entity it names without calling | overlap-free with `calls` (slice E) |
| `synthetic:{resolver}:{kind}` | framework associations (`handles`, `registers`, `navigation`, `callback`, `depends_on`, … — 13 resolvers) | `framework.py` hook; swept by namespace (DR-42) |

Not edges, though they read like them: `external::*` is a pseudo-target
marker for callees outside the index (unresolved/builtin), not a kind —
there is no row to annotate. Killed and gone: `HAS_CONTENT` (P4 considered
it for branch pointers, retention evidence killed it — pointers are
concepts). And per this proposal, no annotation edges either (§2, below).

Proposal: **comments target concept ids only, never edges.** Rationale, three
lessons already paid for:

- P3's lesson: where a concept suffices, no edge is minted (the `HAD_CONTENT`
  edge died on retention evidence).
- D-225: edges are relations with cross-lineage semantics; annotations are
  metadata and must not inherit relation machinery (sweep, retire, triggers).
- Comments must outlive their target (a deleted function's rationale is the
  most valuable comment in the store). An edge to a retired target dangles;
  a payload reference (`about: <id>`) reads forever.

So the reference lives **in the comment payload**, and "all comments on X" is
a read-time lookup, not a traversal. If edge annotation is ever needed, it is
a second programme with its own edge kind — not a property of this one.

**Which code entities are addressable** (read off `types.rs` / `walker.rs`,
2026-10-29 — the `about` field can name any of these):

| entity | concept? | id shape / note |
|---|---|---|
| module (**the file's row** — there is no `file` kind; annotating "the file" means `about: a.py::module`) | yes (`module`) | `a.py::module` |
| class | yes (`class`) | `a.py::C` |
| function | yes (`function`) | `a.py::f` |
| method | yes — **as `function`** | `a.py::C.m`; `FunctionKind::Method/...` + `parent_class` in payload, not a separate kind |
| import / constant / type_alias / field | yes | extractor-minted ids |
| route | yes (`route`) | resolver-minted ids |
| spine / commit / pointer | yes | history reads stay pure (§6) — annotatable but pointless; not recommended |
| **parameter** | **no** | `Vec<Parameter>` inside the `Function` payload — serialized into the function's concept JSON, never a standalone row. Annotate the function; a parameter concern is a checklist item (§4), not an id. Promoting parameters to concepts (new kind, new ids, sweep/retire/classifier/embeddings blast radius) is rejected for v1 — re-open only with a consumer that outgrows items. |

"Hard" is therefore exactly this: **a row in the concepts table with a stable
id, a kind, and a lifecycle** (open → superseded → optionally retired). Files
qualify *through* their module row — one indexed file is exactly one live
`{path}::module` concept. What has no row is not annotatable: `external::*`
markers, edges (addressable only as retiring triples), paths that never
minted rows (excluded paths, non-indexable languages — the walker filters
them before extraction), and groupings that never existed as rows
(directories, the repo itself). v1 does not invent rows for any of these.

## 3. Identity and lookup

```
id:      comment:{about}:{n}        # e.g. comment:a.py::g:1
about:   <target concept id>        # payload field, verbatim
kind:    comment
format:  checklist                  # v1: the only format
```

Opaque counters were considered (`comment:{ulid}` + payload `about`) and
rejected: prefix-scannable ids give "all comments on X" as one
`LIKE 'comment:{about}:%'` — the exact pattern `history_of` already uses for
the spine chain. `n` is a per-target sequence (max-scan or a `kv` counter;
decide at build time — both exist). No `file_path` annotation: comments are
not files, and the stale sweep must pass over them exactly like it passes
over commit concepts and pointers (same exclusion, fourth member).

## 4. The checklist grammar (the "force" question)

V1 forces it: **every comment body parses as one or more `- [ ]` / `- [x]`
items, flat, nothing else.** Write-boundary validation refuses anything else
with the offending line cited — the same posture as P5a.2's Zulu boundary
(refuse loudly, never silently coerce) and the route ghost refusals.

- Canonical form: the writer normalizes to canonical markdown (like Zulu
  micros for stamps — one shape in, one shape stored).
- Toggle = new version with one item flipped (`comment(id, item)`), so the
  checklist's own history is queryable.
- Nesting, freeform `note` format, rich markdown: explicitly deferred. One
  shape keeps "which items are still open on this subtree?" a real query
  instead of a parsing project.

Whether the force stands forever is the maintainer's call (§7, Q-A) — but v1
forces it, because a mixed corpus cannot be aggregated and aggregation is the
point (see §6).

## 5. Commit messages: the same ledger, a different citizen

| | commit concept | comment concept |
|---|---|---|
| id | `commit:{sha}` (exists, P5a.3) | `comment:{about}:{n}` (new) |
| writer | ingest only | user / agent op |
| mutability | **frozen** (INV-10: history never rewrites) | **versioned** (supersede on edit, retire on delete) |
| payload | subject + body + P5-D2 metadata | checklist markdown + `about` |
| read path | commit metadata, blame archaeology | annotation queries (§6) |
| edges | none (P3's rule, unchanged) | none (§2) |

The message lands in `commit_concept` at ingest (additive field, same bulk —
no new flush, no new generation). Commits already ingested without messages
(the scratch migration) get them from a **`backfill_commit_messages`** op:
reads `git log`, restates each commit row with the message fields, trunk
lineage (legal restate — same mint lineage, §P5a.5 precedent), idempotent
(`filter_unchanged` absorbs repeats). Old message-less commits without a
backfill are an honest gap, not a silent one.

## 6. Reads (the payoff)

- `comments_on(target)` → versions newest-first (same shape contract as
  `history_of`: `{count, versions: [{items, valid_from, valid_to, open}]}`).
- `open_items(scope)` → every unchecked item under a file/prefix/branch —
  the aggregate the force (§4) buys.
- `comment_history(id)` → the checklist's own chain (who checked what when).
- File history stays pure: `history_of` does **not** join comments (opt-in
  join later, never by default — the chain is code).
- Search: comments **are** embed targets. Today's vectors answer *what*;
  comment vectors answer *why* — the highest-value embedding content in the
  store, and there are hundreds of comments, not millions of generations,
  so H-19's cost reasoning permits it. Current version only, re-embedded
  on toggle (keyed-dedup absorbs the unchanged).
- MCP: **reads yes, writes no.** Q-4's reasoning (an agent should ask what
  it can trust) covers `comments_on`/`open_items`; H-13 covers the refusal
  of MCP writes. Same split as P5a.3/P5a.5.

## 7. Integration rules (new invariants)

- **INV-13 — annotations never gate code reads.** Comments are excluded from
  every coverage universe (`module_concept_ids` family), from
  `changes_between` (no hash → not `uncompared`, simply absent), from the
  sweep candidate sets, and from embeddings' entity counts. A store with a
  thousand comments verifies exactly like one with none.
- **INV-14 — checklist grammar at the write boundary.** The parser lives at
  the one place that writes a comment; no read path repairs, no fallback
  rendering. (P5a.2's Zulu boundary is the template.)
- **Comment stamps are present-tense.** Comments always take the clock,
  mode-independent: they annotate from the present, never rewrite history,
  so INV-10/INV-11 do not apply to them. Ordering is safe by construction —
  `now` sorts after every commit stamp, and comment ids never join code
  chains.
- **Lineage: trunk-global by default.** A comment's `about` names an id;
  reads resolve it through the reader's branch (the `source_refs` fallback
  pattern). Branch-local comments are deferred — no `on_branch` on v1
  comment writes, and the question is recorded, not solved.
- **Deletion is retire-first.** Soft retire by default (audit trail);
  hard delete only through the archive-session discipline that already
  governs physical removal. No comment path invents its own deleter
  (INV-5's spirit: one removal discipline, not two).

## 8. Phasing (each independently shippable)

- **P6a — kind + writes + reads.** `comment` kind, `ConceptUpsert` shape,
  checklist validator at the write boundary, `comments_on` /
  `comment_history` + `open_items`, ops + CLI, MCP reads only, exclusion
  pins (INV-13 tests: verify/retention/sweep suites green with comments
  present).
- **P6b — aggregation + vectors.** `open_items` across scopes, comments as
  embed targets (current version), search-side join ("why" alongside
  "what").
- **P6c — commit messages.** Message fields in `commit_concept` for new
  ingests + `backfill_commit_messages` for existing stores.
- **Deferred:** nesting, freeform `note`, branch-local comments, edge
  annotation (separate programme if ever), MCP writes (same bar as Q-10).

## 9. Open questions for the maintainer

- **Q-A — does the force stand?** Checklist-only forever, or is v1 strict
  with `note` arriving in P6b? (Recommendation: force stands until
  `open_items` has a consumer asking for prose.)
- **Q-B — versioned-forever or editable?** Versions are free (ledger-native),
  but the UI may want "current only". (Recommendation: store versions, read
  current by default — `history_of`'s exact contract.)
- **Q-C — embed comments?** (§6 argues yes; cost is small but nonzero.)
- **Q-D — DR rows:** `DR-44` (comment kind + checklist enforcement),
  `DR-45` (commit message storage)? File when P6a scopes, not before.
