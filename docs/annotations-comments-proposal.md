# Annotations & comments proposal (P6 programme sketch)

**Status (2026-10-29): proposal only — no code, no schema change, no DR filed.**
Answers the maintainer's "annotate every type/edge with structured markdown,
store comments in the database, store commit messages separately" as structure,
architecture, and ideas. Intended reader: whoever scopes the next programme
after P5 (suggested name: **P6 annotations**).

Maintainer decisions already folded in: annotations enforce md lists and md
checkable lists; comments are freeform md text; **all** md-type inputs get
linted (§4). The old Q-A (force vs free) is therefore decided — what remains
is lint strictness (§9, Q-A2).

## 1. The two ideas (plus the half-built third)

CodeRadar already stores non-code concepts: the P3 spine (`content_version`),
P4 pointers (`branch_content`), P5a commit markers (`commit:{sha}`). Two more:

- An **annotation** is the fourth: **`kind = 'annotation'`**, a concept whose
  payload is a markdown **list** — plain bullets/numbered and/or checkable
  task lists, nothing else. Structure enforced, therefore aggregatable.
- A **comment** is the fifth: **`kind = 'comment'`**, a concept whose payload
  is **freeform markdown text**. No grammar, but linted like everything else.

Both get their history from the ledger itself (edit = supersede, delete =
retire — "who checked what when" comes free from `valid_from`/`recorded_at`,
the same way `history_of` gets file history for free).

Commit messages are the half-built third thing: P5a.3 commit concepts carry
author time, identities, and `stamp_nudged`, but **not the message**. The
message belongs in the commit concept's payload — no new kind, no new id
scheme, just the rest of P5-D2. Messages are *not* markdown-typed and are
*not* linted: they are frozen git history, not authored through our boundary.

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

Proposal: **annotations and comments target concept ids only, never edges.**
Rationale, three lessons already paid for:

- P3's lesson: where a concept suffices, no edge is minted (the `HAD_CONTENT`
  edge died on retention evidence).
- D-225: edges are relations with cross-lineage semantics; annotations are
  metadata and must not inherit relation machinery (sweep, retire, triggers).
- Annotations must outlive their target (a deleted function's rationale is
  the most valuable row in the store). An edge to a retired target dangles;
  a payload reference (`about: <id>`) reads forever.

So the reference lives **in the payload**, and "everything said about X" is
a read-time lookup, not a traversal. If edge annotation is ever needed, it
is a second programme with its own edge kind — not a property of this one.

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
| **parameter** | **no** | `Vec<Parameter>` inside the `Function` payload — serialized into the function's concept JSON, never a standalone row. Annotate the function; a parameter concern is an annotation item (§4), not an id. Promoting parameters to concepts (new kind, new ids, sweep/retire/classifier/embeddings blast radius) is rejected for v1 — re-open only with a consumer that outgrows items. |

"Hard" is therefore exactly this: **a row in the concepts table with a stable
id, a kind, and a lifecycle** (open → superseded → optionally retired). Files
qualify *through* their module row — one indexed file is exactly one live
`{path}::module` concept. What has no row is not annotatable: `external::*`
markers, edges (addressable only as retiring triples), paths that never
minted rows (excluded paths, non-indexable languages — the walker filters
them before extraction), and groupings that never existed as rows
(directories, the repo itself). v1 does not invent rows for any of these.

## 3. Identity and lookup (two kinds, one pattern)

```
annotation:  id: annotation:{about}:{n}   # e.g. annotation:a.py::g:1
comment:     id: comment:{about}:{n}       # e.g. comment:a.py::g:1
about:       <target concept id>           # payload field, verbatim, both kinds
```

Opaque counters were considered and rejected: prefix-scannable ids give "all
annotations (or comments) on X" as one `LIKE '{kind}:{about}:%'` — the exact
pattern `history_of` already uses for the spine chain. `n` is a per-target,
per-kind sequence (max-scan or a `kv` counter; decide at build time — both
exist). Separate sequences per kind: the scans never mix, and neither kind
renumbers the other. No `file_path` annotation on either: they are not files,
and the stale sweep must pass over both exactly like it passes over commit
concepts and pointers (same exclusion, now six members).

## 4. Grammars and lint (the decided split)

Two gates at the one write boundary, parser-based (a real markdown parser,
never regex — the Zulu-boundary posture: refuse loudly, never silently
coerce; the exact parser is a build-time choice, not a proposal decision):

- **Gate 1 — lint, both kinds: repair-or-refuse.** The body must parse as
  markdown. Defects with a deterministic, meaning-preserving fix are
  repaired by the linter itself (whitespace, fence closure, list
  marker/indent canonicalization, blank-line insertion) and the *repaired
  canonical form* is what gets stored — one shape in, one shape stored
  (Zulu micros for stamps is the template). Refusal is the last resort,
  not the first: only when the parser cannot produce a meaning-preserving
  canonical form (non-UTF8 bytes, unparseable constructs) is the input
  refused, with the offending span cited. Repair-then-store is the generous
  direction; silent coercion never happens because the repair is
  deterministic and the canonical form is what every read sees.
  Why lint freeform comments at all: it guarantees renderability and
  embeddability (`embed_body` assumes sane text), and it means a future
  aggregation never meets garbage. Cheap at write, expensive never.
- **Gate 2 — list grammar, annotations only.** After repair, the document
  must consist solely of list blocks: bullets, numbered, and/or checkable
  task lists (`- [ ]` / `- [x]`). Repair runs *first* (a missing space after
  `-` gets fixed, then passes); what is still not a list after repair —
  headings, paragraphs, code blocks — is refused. Flat v1 (nesting deferred,
  not forbidden forever).

Consequences, stated upfront:

- Toggle = new version with one item flipped (`annotate(id, item)`), so the
  checklist's own history is queryable — "who checked what when" is the
  annotation's `history_of`.
- `open_items` aggregates **annotations only**. Freeform comments cannot be
  aggregated and are not tried — that is the price of free, paid knowingly.
- Commit messages pass through **neither** gate (frozen history, §1).

## 5. Commit messages: the same ledger, a different citizen

| | commit concept | annotation concept | comment concept |
|---|---|---|---|
| id | `commit:{sha}` (exists, P5a.3) | `annotation:{about}:{n}` (new) | `comment:{about}:{n}` (new) |
| writer | ingest only | user / agent op | user / agent op |
| mutability | **frozen** (INV-10: history never rewrites) | **versioned** (supersede / retire) | **versioned** (supersede / retire) |
| payload | subject + body + P5-D2 metadata | list-only markdown + `about` | freeform markdown + `about` |
| input gates | none (git text is git text) | lint + list grammar | lint |
| read path | commit metadata, blame archaeology | aggregation queries (§6) | rationale reads (§6) |
| edges | none (P3's rule, unchanged) | none (§2) | none (§2) |

The message lands in `commit_concept` at ingest (additive field, same bulk —
no new flush, no new generation). Commits already ingested without messages
(the scratch migration) get them from a **`backfill_commit_messages`** op:
reads `git log`, restates each commit row with the message fields, trunk
lineage (legal restate — same mint lineage, §P5a.5 precedent), idempotent
(`filter_unchanged` absorbs repeats). **When it runs: only on explicit
operator invocation, never automatically and never lazily on read** (reads
don't write; and the repo must be present — a store can outlive its
checkout, shallow clones may lack old messages). The concrete triggers are:
(1) once per store created before the body field existed; (2) after a
migrate that *resumed* such a store (new rows carry bodies, old rows get
healed). Mixed-version resumes are the reason it exists as an op rather
than a migrate step. Old message-less commits without a backfill are an
honest gap, not a silent one.

## 6. Reads (the payoff)

- `annotations_on(target)` / `comments_on(target)` → versions newest-first
  (same shape contract as `history_of`:
  `{count, versions: [{body, valid_from, valid_to, open}]}` — `open` is the
  unchecked-item count, meaningful for annotations, always 0 for comments).
- `open_items(scope)` → every unchecked item under a file/prefix/branch —
  the aggregate the list force (§4) buys. Annotations only, by construction.
- `annotation_history(id)` / `comment_history(id)` → each row's own chain.
- File history stays pure: `history_of` does **not** join either kind
  (opt-in join later, never by default — the chain is code).
- Search: **both kinds are embed targets.** Today's vectors answer *what*;
  annotation/comment vectors answer *why* — and there are hundreds of rows,
  not millions of generations, so H-19's cost reasoning permits it. Current
  version only, re-embedded on edit (keyed-dedup absorbs the unchanged).
- MCP: **reads yes, writes no.** Q-4's reasoning (an agent should ask what
  it can trust) covers the four read ops; H-13 covers the refusal of MCP
  writes. Same split as P5a.3/P5a.5.

## 7. Integration rules (new invariants)

- **INV-13 — annotations never gate code reads.** Both kinds are excluded
  from every coverage universe (`module_concept_ids` family), from
  `changes_between` (no hash → not `uncompared`, simply absent), from the
  sweep candidate sets, and from embeddings' entity counts. A store with ten
  thousand annotations verifies exactly like one with none.
- **INV-14 — repair-or-refuse at the write boundary.** One linter, two
  gates (§4), living at the one place that writes each kind: repair when
  deterministic, refuse only when repair is impossible (both kinds) or the
  repaired form still fails the list grammar (annotations). No read path
  repairs, no fallback rendering. (P5a.2's Zulu boundary is the template.)
- **Annotation/comment stamps are present-tense.** Both always take the
  clock, mode-independent: they annotate from the present, never rewrite
  history, so INV-10/INV-11 do not apply to them. Ordering is safe by
  construction — `now` sorts after every commit stamp, and neither kind's
  ids join code chains.
- **Lineage: trunk-global by default.** An `about` names an id; reads resolve
  it through the reader's branch (the `source_refs` fallback pattern).
  Branch-local rows are deferred — no `on_branch` on v1 writes, and the
  question is recorded, not solved.
- **Deletion is retire-first.** Soft retire by default (audit trail);
  hard delete only through the archive-session discipline that already
  governs physical removal. Neither kind invents its own deleter
  (INV-5's spirit: one removal discipline, not two).

## 8. Phasing (each independently shippable)

- **P6a — kinds + writes + reads.** `annotation` + `comment` kinds,
  `ConceptUpsert` shapes, the linter with both gates, `annotations_on` /
  `comments_on` + histories, ops + CLI, MCP reads only, exclusion pins
  (INV-13 tests: verify/retention/sweep suites green with both kinds
  present).
- **P6b — aggregation + vectors.** `open_items` over annotations, both
  kinds as embed targets (current version), search-side join ("why"
  alongside "what").
- **P6c — commit messages.** Message fields in `commit_concept` for new
  ingests + `backfill_commit_messages` for existing stores.
- **Deferred:** list nesting, branch-local rows, edge annotation (separate
  programme if ever), MCP writes (same bar as Q-10).

## 9. Open questions for the maintainer

- **Q-A — decided 2026-10-29:** annotations enforce lists + checkable lists,
  comments are freeform md. (Was: force vs free.)
- **Q-A2 — decided 2026-10-29: repair-or-refuse.** Repair when the fix is
  deterministic and meaning-preserving (both kinds); refuse only when no
  such repair exists, or when the repaired form still fails the annotation
  list grammar. (Was: refuse vs warn-and-store.)
- **Q-B — versioned-forever or editable?** Versions are free (ledger-native),
  but the UI may want "current only". (Recommendation: store versions, read
  current by default — `history_of`'s exact contract.)
- **Q-C — embed both kinds?** (§6 argues yes; cost is small but nonzero.)
- **Q-D — DR rows:** `DR-44` (annotation/comment kinds + gates), `DR-45`
  (commit message storage)? File when P6a scopes, not before.
