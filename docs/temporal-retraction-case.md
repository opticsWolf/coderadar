# The retraction verb: does P5c need "this lineage absent from T on"?

**Status:** filed 2026-11-14 — CodeRadar's answer to Macrame's v0.23.0 plan §7 Q5 (the ask carried
from D-315's amendment). Written during M1/U-2, before U-3's design freezes and before P5c starts.
**Answer: no CodeRadar workflow produces the case. The verb is not a requirement; it should not be
scheduled, and it is not a gate on U-2 or U-3.**
**Baseline:** CodeRadar 0.16.0, `macrame-db` 0.22.0, store schema v29.
**Reproduction:** `scripts/retraction_probe.py` — a ledger probe run against the Macrame Python
wheel, not a CodeRadar test (it imports `macrame`, which lives in Macrame's venv).

## 1. The question, as Macrame's plan states it

> The retraction verb (carried from D-315's amendment): "this lineage absent from T onward" where
> the lineage holds an own row starting past T still has no name in today's verbs. Gated on a real
> consumer naming the need; if CodeRadar's P5c becomes that consumer, it files the ask and the
> answer is that act — not a retirement that silently stops short.

The plan's own §3.1 L3 is the reason U-3 will not close this: **"the reader's own rows are
uncapped. The reader's lineage is `dist 0` with no cutoff on either axis; caps bound inheritance
only."** U-3's valid cut (L7/L8) bounds what a child *inherits* from its ancestors. A lineage's own
rows are, by design, outside every cap. So if P5c needed this, it would still need it after 0.23.0.

This note does the six things the reviewer asked for: the ledger state, the requested act, the reads
at T2/T4/T6 before and after, why today's acts cannot express it, which P5c workflow or test needs
it, and what a caller does today instead. The last of those is the one that matters, and it is the
answer.

## 2. The ledger state and the act

A branch lineage `feat` (D-6 c: a branch with commits of its own), forked at T3, holds its own
concept version of `x.py::module` twice:

* `[T2, T5)` — content `a`, written at T2, superseded at T5;
* `[T5, ∞)` — content `b`, the lineage's open belief.

The caller asks for **"`feat` is absent from T3 on"**: not "the T2 belief ended at T3" (a
retirement says that), but "this lineage holds no belief about this id at any instant ≥ T3,
including the instants its own rows cover".

| instant | before the act | the request |
|---|---|---|
| T2 | `a` | `a` (the request speaks from T3 on) |
| T4 | `a` | absent |
| T6 | `b` | absent |

The difference from a retirement is visible in the last column pair: a retirement at T3 *ends* the
T2 belief at T3 and leaves the T5 belief alone, so T4 answers absent and T6 still answers `b`.

## 3. What today's acts do (measured on 0.22.0)

Every probe builds the ledger above with `fork("feat", "main", at=T3)`,
`upsert_concept(ConceptUpsert(ID, valid_from=T2, branch="feat", content="a"))` and a supersede at
T5, then reads `concepts_by_id([ID], branch="feat", as_of_valid=…)` — the per-lineage concept read
at a valid instant, i.e. the surface U-2's fold will widen. Results:

| act | outcome |
|---|---|
| `upsert_concept(retired@T3, supersede=True)` with the T5 row present | **refused**: `ConceptSingleOpenError` — "a second open interval from T3 … one from T5 is already recorded" |
| `write_concepts([retired@T3])` (the engine's bulk) with the T5 row present | **refused**: `OverlappingConceptError` |
| CodeRadar's `upsert_concepts_bulk` over the same state | **refused earlier**, by its own decision: "only a closed version that ends at or before T5 may backfill (P5b.5)" |
| a zero-width tombstone at T3 (`valid_to = T3`, CodeRadar's trunk retirement shape) written *before* the T5 row exists | lands; T2 `a`, T4 absent, T6 absent — **then a later T5 write via the bulk re-opens it**: T6 `b` |
| an open-ended retired row at T3 (the branch-shadow shape, `retired=true`, no `valid_to`), same order | lands; T4/T6 absent — **then a later supersede at T5 lands and re-opens it**: T5/T6 `b` |
| a same-start retired rewrite at T5 via `write_concepts` | lands; T5/T6 absent — the first half of a multi-act workaround |

Two conclusions, both structural:

1. **Against an existing later own row the act is refused, not approximated.** All three layers —
   single write, engine bulk, CodeRadar's decision — refuse before anything lands. The plan's
   phrase "a retirement that silently stops short" describes the *other* order (act first, belief
   later), where the retirement lands and a later write re-opens.
2. **No existing shape binds the future, and that is the write law, not a gap in it.** A
   zero-width tombstone is a point close. An open-ended retired row holds the future only until a
   later act supersedes it (D-312's one-write law: the newer belief closes whatever stood). "Absent
   from T on" as an unconditional claim is not a row shape at all — it would be a **policy**
   ("refuse later writes on this id on this lineage") or a **fold parameter** ("a read at ≥ T
   ignores later rows"). Neither is a verb, and both are new decisions.

The multi-act workaround exists on paper — write every later start's retirement first (latest
first, zero-width), then the retirement at T3 — but it is not a verb: the caller must enumerate the
later starts it knows, a crash between acts leaves T6 answering `b`, and it cannot reach a later row
written by an actor the caller does not control.

## 4. Why U-3 does not close it

The valid cut is an *inheritance* bound: an ancestor row whose `valid_from` is past the child's
composed cut is not state on the child (L7), and the ancestor's ends past the cut read as open (L8).
Both clauses bind rows the child did **not** write. L3 keeps the child's own rows uncapped in the
same plan, and the plan says so explicitly ("a cap on the reader would mean the reader stops seeing
itself, which no ask names and no doctrine supports").

So the retraction case survives U-2 and U-3 untouched. Filing it now, rather than after both,
means the answer is known before P5c commits to a design that assumes a verb which will not exist.

## 5. The search: no CodeRadar workflow produces the state

Every place P5c (or anything near it) writes a lineage's own rows, and whether it asks for
retroactive absence from them:

| workflow | own row later than the wanted absence? | verdict |
|---|---|---|
| A-5, the fork at the merge-base on the valid axis (U-3) | no — the fork bounds **inheritance**; the child's own rows are new acts at their own stamps | the cut is the answer; no retraction |
| A-6, merges assert merged files on the receiving lineage (D-315 adoption) | no — the assertion is stamped at the merge commit's time, forward; nothing asks the trunk to be absent *below* the merge | assertion, not retraction |
| D-5, a rebased or force-pushed branch | **by design no** — D-5 chose a new generation (`feat` → `feat#2`) over alternative (c) "retract the old commits"; the new generation holds no row at the old commit's stamp, so absence is automatic and the old generation stays readable | generations, not retraction |
| A-8/P5-D8, "nothing recorded is ever retracted" | — | CodeRadar's own law; a vel sim. act would overturn it first |
| A-9, the store is git-derived | — | "make it not have been recorded" is answered by **rewrite git, rebuild, re-import**, not by an in-place act |
| P5d, observed edits | no — a discarded edit is recorded as the reverted content (another observation) | P5-D8, again |
| P5b.5, backfill windows | no — a window writes below a belief, at the commits' stamps; a tombstone is written only where an id's life ended before the tip, at its own stamp | forward-shaped by construction |
| P5b.7, renames | no — the old id is retired at the rename commit, which is forward by construction | forward |
| `migrate-to-commit-time`, `rebuild-store` | no — fresh stores written from git; no in-place absence | rebuild |
| P5c's future tests (F3/F4 flips, fork/drift pins) | — | no fixture has "own row at T5, absent from T3" |

If CodeRadar cannot produce the case, that is the answer: the plan's "P5c-gated" phrase was a
placeholder, and this note is the artifact that resolves it.

## 6. The answer

**The verb is not a requirement.** No CodeRadar workflow asks a single lineage to un-assert its own
row from an instant earlier than that row's start. The cases that look like it are one of two
things, both already answered:

* **inheritance** — the child should stop seeing an ancestor's state from T on: U-3's valid cut
  (L7/L8), which is the correct act and the one P5c ships against;
* **"the ledger should not have recorded this"** — a rebuild from rewritten git (A-9), which is
  CodeRadar's stated answer to exactly that question.

What a caller does today instead, in order of preference: (1) the truthful act at the instant it
can name — a retirement at the later start, or a supersede at T3 when the lineage has no later
open belief; (2) report the ledger as it stands (a later belief is a fact, and `at=<sha>` reads say
which commit seen it); (3) rebuild from git when the history itself is wrong.

**Reopen trigger.** A named workflow in which a *single lineage's own row* must be invisible from an
instant earlier than that row's start, and neither the truthful act at a nameable instant nor a
rebuild answers it. That would be a decision on CodeRadar's side first — it would have to amend
P5-D8 — and a Macrame decision second; it would not be a parameter on U-3's cut. Nothing in P5c's
scope, or in any phase now planned, is that workflow.

**What this note is not.** It is not a request to design or schedule the verb in 0.23.0; it does not
gate U-2 or U-3; and it does not ask Macrame to change the plan. It closes the plan's Q5 with the
consumer's answer: *filed, no case, not scheduling*.
