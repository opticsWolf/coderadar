# RTA-Lite Precision Report — Milestone D

**Deliverable of:** fossil-mcp improvement plan §11.3 / §14.1 Milestone D
**Scope:** Stage 6.3 `rta-dead` findings evaluated against manually verified ground
truth on three real Python corpora (Pallets ecosystem).
**CodeRadar version:** v0.7.20 · **Date:** 2026-08-26

---

## 1. Corpus and method

| Repo | Commit | LOC class | Why chosen |
|---|---|---|---|
| flask | master (shallow) | ~25k | Plugin registry pattern (`JSONTag` hierarchy) |
| click | master (shallow) | ~30k | Deep `Command`/`Group`/`Option` hierarchies |
| werkzeug | master (shallow) | ~45k | Exception hierarchy + reloader strategy classes |

Each repo was indexed standalone (`analyze(repo)`), then
`find_dead_code(min_confidence=0.0, include_test_reachable=False,
max_findings=10_000)` was run. Every `rta-dead` finding was then verified
manually: is the defining class really never instantiated inside the repo,
and if not, why did the instantiation signal miss it?

## 2. Results

### 2.1 Dead-code detector overall (context)

| Repo | unreachable | transitively-dead | rta-dead | total |
|---|---|---|---|---|
| flask | 173 | 77 | 9 | 259 |
| click | 209 | 136 | 3 | 348 |
| werkzeug | 446 | 136 | 3 | 585 |

(The `unreachable`/`transitively-dead` numbers were not audited for this
report; they are listed as context only.)

### 2.2 RTA-lite findings and verdicts

**flask — 9 findings, 0 true positives**

| Finding | Verdict | Cause |
|---|---|---|
| `json/tag.py::{TagDict,PassDict,PassList,TagBytes,TagDateTime,TagMarkup,TagTuple,TagUUID}.to_json` (8×) | **FP** | Dynamic constructor dispatch: tags are built via `tag = tag_class(self)` (`tag.py:275`) from a registry populated by the `@register` decorator. The raw-call-name scan only sees literal names. |
| `test_json_tag.py::test_custom_tag.TagFoo.to_json` | **FP** | Same registry path (`app.json.register(TagFoo)` → dynamic construction). |

**click — 3 findings, 0 true positives**

| Finding | Verdict | Cause |
|---|---|---|
| `core.py::CommandCollection.get_command` | **FP-by-caveat** | Zero construction sites anywhere in the repo — `CommandCollection` is public API meant for out-of-tree use. This is exactly the "instances could come from outside the indexed root" limitation documented at ship time. |
| `tests/test_context.py::NonExitingOption.__init__`, `DebugLoggerOption.__init__` (2×) | **FP** | Constructed via `@click.option(..., cls=NonExitingOption)` — click instantiates from the `cls` parameter dynamically. |

**werkzeug — 3 findings, 0 true positives**

| Finding | Verdict | Cause |
|---|---|---|
| `exceptions.py::_RetryAfter.get_headers` | **FP-by-caveat** | `_RetryAfter` itself is never constructed literally; its concrete subclasses (`TooManyRequests`, `ServiceUnavailable`) are exported API raised by applications. Ancestor classes inherit the construction blindness of their children's external users. |
| `_reloader.py::StatReloaderLoop.run_step` | **FP** | Dict-dispatch construction: `reloader_loops[reloader_type](...)` (`_reloader.py:390`). |
| `local.py::_ProxyIOp.__init__.bind_f` | **Artifact** | Entity name suggests an extraction quirk rather than a real method boundary; flagged for follow-up in the ingest-parity work (v0.8 P4.2), not an RTA issue per se. |

## 3. Scorecard

| Metric | Value |
|---|---|
| rta-dead findings across corpus | **15** |
| True positives (in-repo verifiable) | **0** |
| False positives — dynamic constructor dispatch | 11 |
| False positives — external-construction caveat (by design) | 3 |
| Extraction artifacts | 1 |
| User-facing false positives at default settings | **0** |

The last row is the operative one: every rta-dead finding scored between
0.135 and 0.450, below the default `min_confidence=0.6`, so none of these
false positives reach a default-configuration user. They surface only when
an agent deliberately lowers the threshold — at which point each finding
carries its kind label and the documented external-construction caveat.

## 4. What the report changes

1. **The design bet held.** RTA-lite was shipped as the weakest evidence
   tier precisely because Python construction is frequently dynamic.
   Milestone D confirms that call: without the tier discipline these 15
   findings would all be user-facing false positives on flagship repos.
2. **The v0.8-routed fix is the right fix.** Root cause #1 (dynamic
   dispatch, 11/15 FPs) cannot be closed by better name scanning — it needs
   real `ResolvedCall::Constructor` resolution flowing through the resolver
   cascade (already routed to v0.8 P2.3-adjacent work; see the discovery
   note in `graph/rta_lite.rs`). Registry/factory patterns will resolve
   correctly once constructor calls propagate through type inference.
3. **Cheap improvement identified but deferred:** closing
   `instantiated_classes` under subclass→ancestor edges would clear cases
   where literal subclasses exist but parents do not (none in this corpus —
   both werkzeug `_RetryAfter` subclasses are themselves externally built —
   so it buys nothing today). Recorded for when constructor resolution
   lands.
4. **One extraction artifact** (`_ProxyIOp.__init__.bind_f`) handed to the
   v0.8 P4.2 parity-test backlog.

## 5. Recommendation

Keep `rta-dead` exactly as shipped: Speculative-tier, below the default
confidence floor, distinct kind label. Do not promote it to a stronger
tier until constructor resolution exists and this report's measurement is
re-run with precision > 0. Re-measure after v0.8 P2.3.
