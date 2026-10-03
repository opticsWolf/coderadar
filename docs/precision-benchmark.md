# Precision benchmark & CI gate (v0.10, Phase 7)

Nothing in the 0.10 plan stays fixed without a number guarding it. This is
the number, where it comes from, and how it is enforced.

## What is measured

`tests/precision/harness.py` indexes a corpus and compares the result against
two independent sources of truth:

| Metric | Truth | Definition |
|---|---|---|
| extraction recall | Python `ast` | extracted call refs / `ast.Call` nodes inside functions, broken down by call shape |
| resolution precision | `# -> Target` annotations | edges agreeing with the annotation / edges emitted |
| resolution recall | `# -> Target` annotations | edges agreeing / annotated sites |
| dead-code precision | hand-labelled golden | High+Medium findings not in the `alive` list / labelled findings |

Resolution truth is annotation-based rather than `jedi`: annotations are
committed with the fixture, run in CI without a second interpreter, and name
the *qualified* target (so a wrong-but-plausible edge fails, which is the
case that matters). `dangling_targets()` separately asserts that every
resolved edge names an entity the index actually holds.

## Corpora

| Corpus | What it covers |
|---|---|
| `tests/precision/fixtures/py_shapes/` | one file per call shape and binding pattern (§1) |
| `py_agent/src` (this repo) | dogfood: façade/registry patterns, decorator factories, untyped receivers |
| any root via `measure(path)` | e.g. Lace @ a pinned commit — run locally, not in CI |

## Current numbers

`tests/precision/baseline.json` (ratchet — improvements are recorded with
`PRECISION_UPDATE=1 pytest tests/precision -s`):

| Corpus | extraction recall | in-repo resolution rate | precision | recall |
|---|---|---|---|---|
| fixtures | 100 % | 76.7 % | 100 % | 100 % |
| self | 99.97 % | 23.0 % | 100 % | 100 % |

`in_repo_rate` is the share of call sites bound to an entity in the corpus;
the rest are stdlib/third-party calls, which is most of a Python program.
It rose from 21.1 % to 23.0 % with the Phase 7 import fixes below.

## Dead code on the dogfood corpus

Running `find_dead_code` over `py_agent/src` before and after the Phase 6–7
work, hand-verifying every High finding:

| | before | after |
|---|---|---|
| findings | 269 (of 449 functions, 60 %) | 121 (27 %) |
| High | 93 | 2 |
| Medium | 75 | 0 |

The two remaining High findings are true positives and stay in the report:
`QueryCache.invalidate` and `QueryCache.prune_expired` have no call site
anywhere in the package (`cached_query` only uses `get`/`set`). They are
listed in `tests/precision/deadcode_golden.json` under `dead`, so a future
change that stops reporting them fails the recall gate.

The false positives that were fixed, each now covered by a regression test in
`tests/test_phase7_precision.py`:

| False positive | Rule added |
|---|---|
| 24 `@mcp.tool(...)` handlers | a decorator that is an attribute *call* is registration |
| `requires_index` | a definition used as a decorator is called by the decoration machinery |
| `CodeGraph.query`, `watch` | a package façade's public functions and public-class methods are API |
| `LSPPool.shutdown`, `ToolRouter.route` | a module a package re-exports is surface, transitively |
| `_resolve_version`, `_env_seconds` | module-level calls (import-time initializers) run at import |
| `install`, `make_middleware` | relative imports resolve, aliases resolve to the original symbol |
| `set_indexed_root`, `resolve_entity_path` | `from x import a as b` binds `b`, calls still find `a` |
| `_index_is_empty`, `_leave` | value references (defaults, `or` operands) are liveness |
| `search_text`, `shutdown`, `classify` | public method of an instantiated class caps at Low, not High |
| `_fire`, `_parent_is_alive` | an unresolved `x.foo()` call site caps same-named methods at Low |

Findings that could go either way are now *reported at the weakest tier*
rather than either silenced or claimed at 0.9 — `WEAK_SURFACE_CAP = 0.45`,
with the reason in `evidence`.

## The gate

`tests/precision/test_precision.py` runs as part of the normal suite, which
CI runs whole (`uv run pytest -q`, `testpaths = ["tests"]`):

* extraction recall ≥ baseline, resolution precision/recall ≥ baseline − 0.5 pt;
* no hand-verified `alive` entity is a High or Medium finding;
* every hand-verified `dead` entity is still reported;
* the finding budget may shrink, never grow.

A change that loses recall or precision fails CI instead of shipping.
