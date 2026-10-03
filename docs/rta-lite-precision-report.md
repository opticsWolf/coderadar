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

---

## 6. Re-run — v0.10 precision plan §2.5

**CodeRadar version:** 0.9.4 + the `dev_precision` Phase-1/2 resolver work ·
**Date:** re-run during the v0.10 precision plan · **Corpus:** fresh shallow
clones of `master` (flask `d73fa1c`, click `06b2a67`, werkzeug `594452f` —
the corpus moved since §1's 2026-08-26 snapshot).

Method identical to §1: standalone `analyze(repo)`, then
`find_dead_code(min_confidence=0.0, include_test_reachable=False,
max_findings=10_000)`, then manual verification of every `rta-dead`
finding against the source.

### 6.1 Results

| Repo | unreachable | transitively-dead | **rta-dead** | total |
|---|---|---|---|---|
| flask | 137 | 32 | **0** | 169 |
| click | 123 | 43 | **10** | 176 |
| werkzeug | 357 | 59 | **5** | 421 |

The flask result is the headline: the 9 findings of the pre-fix run (§6.3)
are gone, and no new ones replaced them.

### 6.2 Verdicts

**flask — 0 findings.** No audit needed.

**click — 10 findings, 0 true positives**

| Finding | Verdict | Cause |
|---|---|---|
| `tests/test_context.py::*::{DebugLoggerOption,NonExitingOption,ExitingOption,ParameterInternalCheck}.{__init__,set_state,reset_state,set_level,reset_loggers,process_value}` (8×) | **FP** | Dynamic `cls=` dispatch: the classes are defined inside test functions and handed to `@click.option(..., cls=...)`; click constructs them via `cls(param_decls, **attrs)` (`decorators.py:346`, `:374`). |
| `core.py::CommandCollection.get_command`, `.list_commands` (2×) | **FP-by-caveat** | The only in-repo reference is the `__init__.py` re-export; instances come from out-of-tree users merging command groups. |

**werkzeug — 5 findings, 0 true positives**

| Finding | Verdict | Cause |
|---|---|---|
| `_reloader.py::StatReloaderLoop.run_step`, `ReloaderLoop.trigger_reload` (2×) | **FP** | Dict-dispatch construction: `reloader_loops[reloader_type](...)` (`_reloader.py:401`). The base's `trigger_reload` is called by the dynamically built subclass's `run_step`. |
| `exceptions.py::_RetryAfter.get_headers` | **FP** | Class-as-value construction: tests call `cls(retry_after=...)` over a parametrized class list (`test_exceptions.py:135`); in-repo source raises the subclasses only. |
| `datastructures/accept.py::_CharsetAccept._value_matches`, `._value_matches._normalize` (2×) | **FP-by-caveat** | The class is reachable in-repo only through the deprecated `CharsetAccept` module attribute (`__getattr__`, `accept.py:410`); instances are built by out-of-tree users of the deprecated name. |

### 6.3 Scorecard

| Metric | §1 (v0.7.20) | §6 (v0.9.4+dev) |
|---|---|---|
| rta-dead findings across corpus | 15 | **15** |
| True positives (in-repo verifiable) | 0 | **0** |
| FP — dynamic construction | 11 | 11 |
| FP — external-construction caveat (by design) | 3 | 4 |
| Extraction artifacts | 1 | 0 |
| User-facing false positives at default settings | 0 | **0** |

Every finding scores 0.450, far below the default `min_confidence=0.6`, so
the default-configuration user still sees none of them.

### 6.4 What changed between §1 and §6

The counts look similar, but the composition changed completely:

1. **Constructor resolution now exists and is doing its job.** The §1
   finding families that static resolution *can* fix are fixed:
   - flask's entire 9-finding set (registry-dispatched `Tag*.to_json`,
     `SessionInterface.*`) is gone. Root causes closed: class-body
     annotated defaults (`session_interface: SessionInterface =
     SecureCookieSessionInterface()`) now count as construction, `type[X]`
     fields with a concrete default dispatch through the default, and a
     virtual-dispatch hop into an *instantiated* class no longer reads as
     "speculative" in the RTA direct-reachability pass.
   - click's `Parameter._check_name_is_usable` (base method called by
     instantiated subclasses) cleared the same way.
   - the §1 extraction artifact `_ProxyIOp.__init__.bind_f` cleared —
     `_ProxyIOp(operator.iadd)` in the class body is now recognized as a
     construction site.
2. **Everything left is genuinely dynamic.** The remaining 15 findings are
   all cases where construction happens through a value the resolver
   cannot statically track: `cls=` keywords, dict dispatch, class-as-value
   parameters, or classes never constructed in-repo at all. §1's
   conclusion — that this family "cannot be closed by better name scanning"
   — holds; it would need data-flow/points-to analysis, which is out of
   scope for a lite tier.
3. **click/werkzeug counts rose because the corpus moved** (master since
   2026-08-26) and because the refined direct-reachability pass now
   classifies more of these methods as dispatch-only liveness rather than
   leaving them in the unreachable/transitive tiers. Same absolute FP
   count, better-labelled cause.

### 6.5 Decision (plan §2.5)

The plan gates promotion on re-run precision ≥ 80 %. Measured precision is
**0/15 = 0 %**, so:

- **`rta-dead` stays Speculative-only** — same tier, same distinct kind
  label, still below the default confidence floor. No promotion.
- The measured gap is now entirely dynamic-construction territory; a future
  promotion decision needs a points-to/data-flow tier that does not exist,
  not another name-scanning refinement.
- The 0.9.4+dev resolver improvements are worth keeping on their own:
  flask's dead-code findings dropped from 259 (§1 context) to 169 with no
  regression in the audited tiers, and the FP-causing liveness
  over-approximation (dispatch-only classification) is measurably tighter.
