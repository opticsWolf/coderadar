# CodeRadar Improvement Plan — Algorithms Ported from fossil-mcp

**Status:** Proposal / working plan
**Source of findings:** In-depth analysis of `fossil-mcp` v0.1.8 (Rust, MIT OR Apache-2.0, ~56k LOC)
**CodeRadar version targeted:** post-v0.8 (`core_indexer` Rust core + `py_agent` Python MCP layer)
**Relationship to other plans:** This is the successor programme to
[`v0.8-improvement-plan.md`](v0.8-improvement-plan.md) and **depends on v0.8 Phase 1**
(cold start / `load_snapshot`). See [§3.1](#31-dependency-on-the-v08-plan) for the precise
dependency analysis and [§14](#14-sequencing-effort-and-milestones) for the merged roadmap.
This plan adopts the v0.7/v0.8 doctrine wholesale: honest refusals over fabricated answers,
wire-or-cut, and parity tests written *before* the code they guard.
**License note:** fossil-mcp is dual MIT/Apache-2.0. Algorithm *implementations* may be ported with
attribution; algorithm *ideas* are unencumbered. Prefer re-derivation against CodeRadar types with
attribution comments pointing at the fossil-mcp source file.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Evidence: What fossil-mcp Does Well](#2-evidence-what-fossil-mcp-does-well)
3. [Gap Analysis](#3-gap-analysis)
   - [3.1 Dependency on the v0.8 plan](#31-dependency-on-the-v08-plan)
   - [3.2 Field-defect triage (CODERADAR_BUGS_QUIRKS)](#32-field-report-defect-triage-coderadar_bugs_quirksmd)
4. [Architecture Principles for the Port](#4-architecture-principles-for-the-port)(#4-architecture-principles-for-the-port)
5. [Stage 0 — Foundations (scoring, analysis cache, EvalContext extension)](#5-stage-0--foundations)
6. [Stage 1 — Dead-Code Detection via Reachability](#6-stage-1--dead-code-detection-via-reachability)
7. [Stage 2 — Token-Level Clone Detection (MinHash + LSH + SimHash)](#7-stage-2--token-level-clone-detection)
8. [Stage 3 — AI-Scaffolding & Secrets Scanner](#8-stage-3--ai-scaffolding--secrets-scanner)
9. [Stage 4 — CFG Upgrade of the Smells Engine](#9-stage-4--cfg-upgrade-of-the-smells-engine)
10. [Stage 5 — Centrality Metrics](#10-stage-5--centrality-metrics)
11. [Stage 6 — Advanced (APTED, Dead Branches, RTA-lite)](#11-stage-6--advanced)
12. [Python-Side Tool Exposure](#12-python-side-tool-exposure)
13. [Testing Strategy](#13-testing-strategy)
14. [Sequencing, Effort, and Milestones](#14-sequencing-effort-and-milestones)
15. [Risks and Mitigations](#15-risks-and-mitigations)

---

## 1. Executive Summary

fossil-mcp is a static-analysis CLI/MCP toolkit purpose-built around three detectors — dead code,
code clones (Type 1–4), and AI scaffolding artifacts — on top of a research-grade graph layer
(CFG, PDG, SDG, slicing, RTA/VTA). CodeRadar already owns the hardest prerequisite for most of this:
a resolved, bidirectional call graph (`callers_by_callee` / `callees_by_caller`), a class hierarchy
with MRO, embeddings, an incremental update pipeline, and a bitemporal ledger.

The porting opportunity is asymmetric:

| | fossil-mcp has | CodeRadar has |
|---|---|---|
| Call graph traversal | ✅ both directions | ✅ both directions (adjacency maps) |
| **Forward reachability / dead code** | ✅ full detector chain | ❌ only upstream `affected` |
| **Clone detection** | ✅ Merkle/MinHash/LSH/SimHash/APTED | ❌ embedding dedup only |
| **CFG-based metrics** | ✅ basic blocks + dominators | ⚠️ raw AST walks |
| **Centrality** | ✅ | ❌ |
| **Scaffolding/secrets scan** | ✅ | ❌ |
| Embeddings / semantic search | ❌ | ✅ fastembed, cached on `Function.embedding` |
| Bitemporal history | ❌ | ✅ unique |
| Mutation tools | ❌ | ✅ unique |
| Cold start from ledger | ❌ full rescan per process | 🔧 v0.8 Phase 1 delivers |
| Bitemporal `as_of` traversal | ❌ | ⚠️ in/out pending v0.8 Phase 2.2 |

This plan ports five capability clusters in six stages, ordered so each stage ships as an
independently demoable increment and later stages reuse earlier foundations.

---

## 2. Evidence: What fossil-mcp Does Well

Measured during the analysis (CodeRadar smell engine run over fossil-mcp itself):
135 files, 56,301 LOC Rust, 116 source files, ~963 test functions, `#![forbid(unsafe_code)]`.

Algorithms worth taking, with their fossil-mcp source locations:

| Algorithm | fossil-mcp file(s) | Why it matters |
|---|---|---|
| Entry-point detection → reachability → classification | `src/dead_code/{entry_points,detector,classifier}.rs` (2,170 + 1,926 LOC) | The "vibe coding" killer feature; complements blast radius exactly |
| MinHash + LSH candidate pairing | `src/clones/{minhash,lsh_index}.rs` | Sub-linear clone search; scales where O(n²) dies |
| SimHash over normalized tokens | `src/clones/simhash.rs`, `ir_tokenizer.rs` | Type-2/3 clones that embeddings miss |
| Merkle hashing over ASTs | `src/clones/merkle.rs` | Exact Type-1 clones; incremental change detection |
| APTED tree edit distance | `src/clones/apted.rs` (1,181 LOC, tested vs Zhang-Shasha) | Structural verification of clone candidates |
| Framework-aware entry-point heuristics | `src/dead_code/entry_points.rs` | Spring/FastAPI/package.json awareness reduces false "dead" positives |
| Confidence scoring model | `src/core/scoring.rs` | Comparable confidence tiers across rules |
| SIEVE cache + incremental pipeline | `src/analysis/{sieve_cache,incremental,pipeline}.rs` | Pattern for caching expensive analyses |
| Centrality metrics | `src/graph/centrality.rs` | Ranking signal for god-class / brain-method triage |
| Basic blocks + dominators + const-prop + dead branches | `src/graph/{cfg,constant_prop,expr_evaluator}.rs` | Upgrades smell metrics from AST approximation to real CFG math |

Known weaknesses we deliberately do **not** copy: god-files (`scaffolding.rs` at 3,891 LOC,
`extractor.rs` at 2,403 LOC), Critical-complexity functions (`interactive_repl` cyclo=41,
`node_to_label` cyclo=43), and brain-method classes. Every stage below includes explicit
file-size and complexity budgets to avoid importing the disease with the cure.

---

## 3. Gap Analysis

Capability matrix, CodeRadar symbols verified against `core_indexer/src`:

```
CallGraph            core_indexer/src/graph/call_graph.rs
  ├─ find_callers()          upstream BFS            ✅ exists
  ├─ find_call_chain()       BFS w/ parent map       ✅ exists
  └─ forward reachability    downstream closure      ❌ MISSING  ← Stage 1
ProjectedGraph        core_indexer/src/types.rs:1179
  ├─ callers_by_callee / callees_by_caller            ✅ both directions present
  ├─ subclasses / overridden_by / overrides_base      ✅ (RTA-lite substrate)
  └─ importers / imports_by_importer                  ✅
SmellEngine          core_indexer/src/smells/engine.rs
  ├─ SmellRule trait {id, scope, signals_needed, evaluate}   ✅ extensible
  ├─ EvalContext {entity_id, entity_name, metrics, graph}    ⚠️ no room for precomputed
  │                                                          graph analyses  ← Stage 0
  └─ 9 rules, metrics from AST walks (metrics.rs)            ⚠️ ← Stage 4
Function             core_indexer/src/types.rs:774
  ├─ body_span / body_hash / content_hash / signature_hash   ✅ clone fingerprint keys
  └─ embedding: EmbeddingVec                                 ✅ Type-4 fusion substrate
py_agent             py_agent/src/coderadar/
  └─ mcp tools: affected, explore, node, search …            ⚠️ add dead_code/clones/scaffold tools
```

---

### 3.1 Dependency on the v0.8 plan

The two plans are complementary but not independent. Mapping every dependency:

| v0.8 item | Effect on this plan if missing | Verdict |
|---|---|---|
| **Phase 1.1 `load_snapshot`** (cold start from ledger) | Every new MCP tool (`dead_code`, `find_clones`, `find_scaffolding`) pays the measured ~46 s re-analysis per invocation — unusable by agents | **Hard prerequisite for Stages 1–5 tool exposure.** Stage algorithm work can proceed, but nothing ships until this lands |
| **Phase 1.3 retire `_ensure_graph` re-index** | Same cost path through CLI commands | Follows 1.1 automatically |
| **Phase 2.1 `explore()` → Rust traverse (all four edge kinds)** | Dead-code liveness explanations and clone-group navigation want `imports`/`inherits` traversal, not just calls | Soft dependency; sequence after 2.1 so demos don't refuse mid-walk |
| **Phase 2.2 `as_of` upstream/bidirectional** | Unlocks the temporal variants in Stage 6.4 (dead-code-as-of, clone evolution) — the features fossil-mcp *cannot* copy because it has no ledger | Enabler, not blocker |
| **Phase 2.3 `IMPLEMENTS` edge kind** | Interface implementations folded into `extends` blurs subclass closures for Java/Go/TS/C# — degrades RTA-lite precision (Stage 6.3) and override-aware liveness (§6.2) on those languages | Sequence RTA-lite after 2.3, or accept Python-only scope initially |
| **Phase 4.2 three-way ingest parity test** | The guard rail for any new projection-consuming code. Every stage here adds consumers of `ProjectedGraph`; they must not become a fourth drift source | Mandatory gate per stage (Appendix B updated accordingly) |
| **Phase 4.3 plausible-output audit** | Found that only 4 smell rules are golden-tested. All nine new rules this plan introduces must land golden-tested or we recreate the `god-class` pattern | Adopted into §13 |
| **Phase 4.4 lint backlog / blocking CI** | Ordering doctrine: mechanical diffs last. Our stages are large diffs — they slot *after* 4.4 or between 4.4 and 5, never before Phase 1 | Sequencing constraint |

**What this plan gives back to v0.8:** Stage 1's entry-point detector and Stage 5's centrality are
themselves candidates for the §4.3 audit sweep (framework-resolver-style fixtures), and the
scaffolding scanner (Stage 3) is exactly the kind of surface that produces plausible output with
exit 0 unless golden-tested from day one.

---

### 3.2 Field-report defect triage (CODERADAR_BUGS_QUIRKS.md)

The bug & quirks report (`CODERADAR_BUGS_QUIRKS.md`, 2026-08-24 session on `stitch_crawler`)
changes this plan in two ways: four defects are **hard gates** for stages below, and the rest get
routed into the merged roadmap as **Track H** (integrity hotfixes, ~4 days) or absorbed by
v0.8 phases. Routing table:

| # | Defect | Routed to | Why it matters to this plan |
|---|---|---|---|
| 1 | `replace_body` dedents body on apply → corrupts file (**Critical**) | **Track H1** — ship-blocker | The detection→mutation synergy (scaffold cleanup §8, dead-code removal demos) is unsafe until written files are syntax-verified with rollback. A demo that corrupts the user's file ends the programme |
| 2 | Mutation failure returns three contradictory signals | **Track H2** | Honesty doctrine violation at the API's most dangerous moment |
| 3 | Config changes invisible until project re-switch | **v0.8 P1** (fold into activation path) | Stage 0's revision-keyed analysis cache must invalidate against the *effective* config; a stale `[project].exclude` poisons every downstream count |
| 4 | Docstring treated as body, silently deleted | **Track H3** | Scaffold cleanup would mass-strip documentation — exactly the plausible-output disaster v0.7 spent itself correcting |
| 5 | Entity ID format undocumented, exact-match-or-fail | **v0.8 P2.1-adjacent** (ID normalization near traverse work) | Agents consuming `dead_code()`/`find_clones()` output will feed IDs straight into `affected`/`node`; silent not-found breaks the whole tool chain |
| 6 | Pest grammar rejects its own documented examples | **v0.8 P4.3** audit sweep | Already listed there as "the query language" candidate |
| 7 | Error message recommends forbidden workaround | **Track H4** (one string) | Trivial, but it actively misleads agents |
| 8 | Background indexing indistinguishable from no results | **Gate for Stages 0–2 tools** + v0.8 P1 staleness surfacing | This is §4 principle 7 in the wild: my tools must return a typed `Indexing { progress }` refusal, never empty success. Fix belongs to core; the *obligation* lands on every tool here |
| 9 | Duplicate smell findings (stale + fresh entity versions coexist) | **Track H5 — hard blocker for Stage 0.4 goldens** | Duplicates break set-inclusion monotonicity assertions (`strict ⊇ normal ⊇ loose`), finding counts, and dead-code dedup. Must land before any new rule registers |
| 10 | mcpScript tool-name prefixing undocumented | Docs-only (H4 bundle) | No code change |

**Standing rule adopted from the report's own recommendation:** every Track H fix lands with its
repro from the report pinned as a regression test — these are documented, reproducible defects,
which makes them the cheapest tests in the suite.

Default-excluding build directories (`target/**`, `node_modules/`, `dist/`) — the report's closing
note — rides along with H5 as a one-line config default, since it was the root cause of two of the
ten findings.

#### Track H implementation notes

**H1 — written-file validation with rollback** (mutation/write_guard.rs). The report's key insight:
the dry-run renders correctly while the apply path corrupts, so validating the *body string* is
insufficient — validate what actually landed on disk:

```rust
// core_indexer/src/mutation/write_guard.rs (conceptual)
pub enum MutationOutcome {
    Applied { file_written: bool, graph_updated: bool },
    RejectedPolicy { reason: String },                       // file NOT touched
    RejectedSyntax { errors: Vec<SyntaxError>, rolled_back: bool },
}

pub fn apply_and_verify(edit: Edit) -> Result<MutationOutcome, io::Error> {
    let original = fs::read_to_string(&edit.path)?;
    fs::write(&edit.path, &edit.new_content)?;

    // Verify THE FILE, not the body string — this is what bug #1 proved.
    if let Err(errors) = syntax_check(&edit.path, edit.language) {
        fs::write(&edit.path, &original)?;                    // roll back
        return Ok(MutationOutcome::RejectedSyntax { errors, rolled_back: true });
    }
    Ok(MutationOutcome::Applied { file_written: true, graph_updated: true })
}
```

Python-side double-check costs one line and catches grammar gaps Rust-side validation misses:
`ast.parse(content)` before returning success.

**H2 — one status enum, three fields, no prose contradictions.** The reported response
("Mutation Applied" / "RejectedPolicy" / "Graph has been updated") becomes impossible by
construction: the header is derived from the enum, and `graph_updated` is `false` whenever the
outcome is not `Applied`.

**H5 — findings keyed by current entity version only.** Dedupe at finding-emission time (latest
revision wins), plus a store-level invariant test: after `update_file`, no superseded entity
version may contribute findings. G0-B pins exactly the report's repro shape — one function,
one rule, one finding — across an edit cycle.

---

## 4. Architecture Principles for the Port

1. **Reuse the resolved graph, never re-parse.** fossil-mcp re-parses per tool call in places;
   CodeRadar's single-pass extraction + `ProjectedGraph` is strictly better. All new analyses read
   from `ProjectedGraph` or cached spans.
2. **Everything is incremental.** Any new index must key off `Function.content_hash` /
   `signature_hash` so `update_file` invalidates only what changed (mirrors fossil's
   `analysis/incremental.rs` but on our ledger).
3. **Confidence-weighted everything.** Python dynamic dispatch means call edges are probabilistic.
   `ResolutionMethod` + `confidence: f32` already exist on edges — every new detector must consume
   and emit confidence rather than booleans.
4. **Rules stay small.** Hard budget: no new file > 800 LOC, no function > cyclomatic 20,
   enforced by running CodeRadar's own smell engine on itself in CI (eat your own dog food).
5. **Rust computes, Python exposes.** New algorithms land in `core_indexer`; `py_agent` gets thin
   MCP tool wrappers consistent with existing `affected`/`explore` tools.
6. **Feature-flag rollout.** Each stage lands behind a config flag (`[analysis] dead_code = true`)
   so behavior changes are opt-in until validated on real repos.
7. **Honest refusals only.** (Adopted from v0.7/v0.8.) A new tool that cannot answer — graph too
   stale, language unsupported, confidence below floor — returns a loud, typed refusal, never an
   empty success. No new `explore()`-style single-kind limitation ships silently; interim states
   get README lines per v0.8 §5.2.
8. **Wire-or-cut on day one.** (Adopted from v0.8 Phase 3.) Nothing lands "for later": every new
   module ships with its production caller wired (engine registration, pyfunction, MCP schema) in
   the same PR, or it does not merge. This is how `tool_router.py`-grade dead code stays out.
9. **Derived data is recomputable, never authoritative.** Reachability sets, centrality scores,
   clone fingerprints, CFG caches may be cached (keyed by ledger revision / `content_hash`) but
   are never persisted as truth. The ledger stays the single source of truth, preserving the
   round-trip identity bar v0.8 Phase 1 sets. Parity tests guard every consumer.

---

## 5. Stage 0 — Foundations

**Goal:** shared infrastructure every later stage needs. No user-visible features yet.

### 0.1 Confidence/severity scoring module

Port the concept of fossil's `core/scoring.rs` (e.g., its `clone_confidence`: similarity bands ×
size factor, clamped) as a pure, table-driven module.

```rust
// core_indexer/src/scoring/mod.rs
//! Shared confidence scoring for derived analyses (dead code, clones, dispatch).
//! Adapted conceptually from fossil-mcp src/core/scoring.rs; re-derived for
//! CodeRadar's f32-confidence edge model.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Speculative, // 0.0–0.4 : report only behind a flag
    Low,         // 0.4–0.6
    Medium,      // 0.6–0.8
    High,        // 0.8–0.95
    Certain,     // 0.95+  : e.g. exact Type-1 clone, zero incoming edges + entry-point proof
}

/// Combine independent evidence multiplicatively (naive Bayes style).
/// Each input must be in (0, 1]; callers clamp before calling.
pub fn combine(evidence: &[f32]) -> f32 {
    evidence.iter().product::<f32>().clamp(0.0, 1.0)
}

pub fn tier_of(score: f32) -> Tier {
    match score {
        s if s >= 0.95 => Tier::Certain,
        s if s >= 0.80 => Tier::High,
        s if s >= 0.60 => Tier::Medium,
        s if s >= 0.40 => Tier::Low,
        _ => Tier::Speculative,
    }
}

/// Clone confidence: similarity band × size factor (fossil-mcp scoring.rs analog).
pub fn clone_confidence(similarity: f64, lines: usize) -> f32 {
    let base = if similarity > 0.95 { 1.0 }
        else if similarity > 0.80 { 0.8 }
        else if similarity > 0.60 { 0.6 }
        else { 0.4 };
    let size_factor = if lines > 50 { 1.1 }
        else if lines > 20 { 1.0 }
        else if lines > 10 { 0.9 }
        else { 0.7 };
    ((base * size_factor).min(1.0)) as f32
}
```

**Acceptance:** unit tests pinning band boundaries; used by Stage 1 & 2 outputs.

### 0.2 Extend `EvalContext` with precomputed graph analyses

Today rules see only per-entity metrics plus the raw graph. Dead-code and centrality are
*whole-graph* properties — computing them inside `evaluate()` would be O(rules × V+E).
Compute once in `SmellEngine::run`, pass by reference. Backward compatible because it's a new field
with a `Default`.

```rust
// core_indexer/src/smells/types.rs (additions)

/// Whole-graph analyses computed once per engine run and shared by all rules.
/// Fields are `Option` so cheap runs can skip expensive analyses.
#[derive(Default)]
pub struct GraphAnalyses<'a> {
    /// EntityIds reachable from any production entry point (Stage 1).
    pub reachable: Option<&'a std::collections::HashSet<crate::types::EntityId>>,
    /// Entry points detected for the current run (Stage 1).
    pub entry_points: Option<&'a std::collections::HashSet<crate::types::EntityId>>,
    /// PageRank-style centrality scores, normalized 0..=1 (Stage 5).
    pub centrality: Option<&'a std::collections::HashMap<crate::types::EntityId, f64>>,
}

pub struct EvalContext<'a> {
    pub entity_id: &'a str,
    pub entity_name: &'a str,
    pub metrics: &'a HashMap<String, f64>,
    pub graph: &'a ProjectedGraph,
    /// NEW: precomputed whole-graph analyses. Default empty.
    pub analyses: GraphAnalyses<'a>,
}
```

Update the two construction sites in `engine.rs::run` (method scope, class scope) to thread the
same `GraphAnalyses` through. File scope unchanged.

### 0.3 Analysis cache slot

Reserve a `DashMap<&'static str, Arc<dyn Any + Send + Sync>>` on whatever struct owns the engine
run (or reuse the projection cache), so Stage 1's reachability set and Stage 5's centrality map are
computed once per projection and reused across MCP calls until the ledger bumps the revision.

**Effort:** ~3 days. **Risk:** none (additive).

> **⚠ Prerequisite — v0.8 Phase 1.** The analysis cache must key off the ledger revision exposed
> by `load_snapshot` (not process-lifetime globals) so that cold-started projections get the same
> cache hits as freshly analyzed ones, and so staleness invalidation matches the MCP banner's
> `indexed_at` logic. Implement against the v0.8 Phase 1.4 benchmark harness: the cold-start leg
> must show cache reuse across a fresh-interpreter `load_snapshot`, not just within one process.

### 0.4 Strictness profiles for the smell engine

**Goal:** let callers choose detection sensitivity — `strict` / `normal` / `loose` — as a closed
enum of threshold profiles, evaluated *inside* the rules rather than post-filtered on severity.

**Why this shape:** every rule currently hardcodes one threshold pair that must serve four very
different callers — MCP agents (want precision only), CI gates (zero tolerance on new violations),
humans exploring unfamiliar repos (want weak signals too), legacy triage (wants ranking). fossil-mcp
validates the demand two ways (`min_confidence` params + a whole `config/presets.rs`), but its free-
form config is also a cautionary tale: preset sprawl. The design constraints here come straight from
the v0.7/v0.8 doctrine: no knob explosion, no decorative strictness, deterministic and documentable.

```rust
// core_indexer/src/smells/profile.rs  (target: < 80 LOC)
/// Closed enum — deliberately NOT free-form tuning. Each level needs golden
/// fixtures × all rules; more levels double maintenance for marginal value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Strictness {
    /// Catch more: thresholds drop ~40%. For CI gates and careful audits.
    Strict,
    #[default]
    /// The current hardcoded behavior, unchanged.
    Normal,
    /// Only egregious cases: thresholds rise ~80%. For triage over legacy code.
    Loose,
}

impl Strictness {
    /// Multiplier applied to each rule's baseline thresholds.
    /// Strict lowers them (catches more); Loose raises them.
    pub fn factor(self) -> f64 {
        match self {
            Strictness::Strict => 0.6,
            Strictness::Normal => 1.0,
            Strictness::Loose => 1.8,
        }
    }

    /// Parse from the MCP string parameter; unknown values are a loud typed
    /// error, never silently coerced to Normal (§4 principle 7).
    pub fn parse(s: &str) -> Result<Self, crate::core::Error> {
        match s {
            "strict" => Ok(Self::Strict),
            "normal" | "" => Ok(Self::Normal),
            "loose" => Ok(Self::Loose),
            other => Err(crate::core::Error::analysis(format!(
                "unknown strictness '{other}' (expected: strict | normal | loose)"
            ))),
        }
    }
}
```

Threaded through the same Stage-0 plumbing as `GraphAnalyses`:

```rust
// core_indexer/src/smells/types.rs (EvalContext addition)
pub struct EvalContext<'a> {
    pub entity_id: &'a str,
    pub entity_name: &'a str,
    pub metrics: &'a HashMap<String, f64>,
    pub graph: &'a ProjectedGraph,
    pub analyses: GraphAnalyses<'a>,
    /// NEW: sensitivity profile for this run. Default Normal = today's numbers.
    pub strictness: Strictness,
}
```

Rules multiply their baselines — one source of truth per rule, so new rules inherit all three
levels automatically:

```rust
// example: rules/long_method.rs (same pattern applies to every threshold rule)
fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
    let limit = (BASE_LOC_LIMIT as f64 * ctx.strictness.factor()) as usize;
    let loc = ctx.metrics["loc"] as usize;
    if loc <= limit { return None; }
    // ... unchanged ...
}
```

Graph-scope rules participate identically (`god_class` scales WMC/CBO limits;
`brain_method` scales its max-cyclomatic signal). **Dead-code composes via an alias layer** once
Stage 1 lands — strictness maps onto the confidence floor rather than inventing a second axis:

```rust
// graph/dead_code.rs
impl Strictness {
    pub fn confidence_floor(self) -> f32 {
        match self {
            Strictness::Strict => 0.40, // include Low tier
            Strictness::Normal => 0.60,
            Strictness::Loose => 0.80,  // High + Certain only
        }
    }
}
```

Explicitly rejected alternatives (recorded so they stay rejected):
- **Per-rule user overrides** → knob explosion; the next hundred dead knobs.
- **Severity post-filtering instead of rule-level evaluation** → cosmetic; agents would see
  identical output across levels on small repos, which is a plausible-output lie (v0.8 §4.3).
- **Free-form numeric thresholds in the API** → config surface without a caller (wire-or-cut).
- **More than three levels** → fixture maintenance × rules grows linearly with levels.

Golden-fixture contract: for each fixture repo, assert the exact finding set at all three levels —
in particular that `strict ⊇ normal ⊇ loose` holds as set inclusion. That monotonicity property is
the whole correctness story of the feature and is cheap to pin with `hypothesis` over random
metric draws.

MCP surface (all analysis tools gain one optional parameter):

```
smells(project_path, strictness="strict")
dead_code(project_path, strictness="loose")     # after Stage 1
codegraph_get_smells(entity_id?, rule_id?)       # existing tool gains it server-side too
```

**Effort update:** Stage 0 total moves from ~3 to **~4 days** including profile goldens.

---

## 6. Stage 1 — Dead-Code Detection via Reachability

**Goal:** the flagship feature. "What can be deleted?" as the mirror image of the existing
"What breaks if I touch this?" (`affected`).

Fossil reference: `src/dead_code/detector.rs` ("orchestrates entry point detection, reachability,
and classification") — but its implementation re-collects functions from source; ours starts from
the already-resolved `ProjectedGraph`, which removes the entire fragile name-extraction layer that
accounts for most of fossil's `entry_points.rs` complexity (their file is 2,170 LOC partly because
they re-parse; ours should land under ~400).

### 6.1 Entry-point detection

Roots = entities that external callers (runtime/framework/build) invoke without a resolvable
in-repo call edge. Heuristic ladder, cheapest first:

```rust
// core_indexer/src/graph/entry_points.rs
use crate::types::{EntityId, ProjectedGraph};
use std::collections::HashSet;

/// Entities considered live roots without requiring an inbound call edge.
pub fn detect_entry_points(graph: &ProjectedGraph) -> HashSet<EntityId> {
    let mut roots = HashSet::new();

    for (id, f) in &graph.functions {
        // 1. Conventional mains per language.
        if matches!(f.name.as_str(), "main" | "__main__" | "_start")
            && f.parent_class.is_none()
        {
            roots.insert(id.clone());
            continue;
        }

        // 2. Decorator-driven framework entries. Fossil checks Spring
        //    @RequestMapping et al.; the decorator list is already extracted
        //    into Function.decorators, so this is one linear scan.
        //    Keep the list data-driven (see ENTRY_DECORATORS below) — do NOT
        //    hand-roll per-framework logic like fossil's entry_points.rs.
        if f.decorators.iter().any(|d| is_framework_decorator(d)) {
            roots.insert(id.clone());
            continue;
        }

        // 3. Dunder protocol methods are invoked by the runtime.
        if f.name.starts_with("__") && f.name.ends_with("__") {
            roots.insert(id.clone());
        }
    }

    // 4. Modules never imported by anyone export public API: their top-level
    //    public functions are roots unless a config says the crate is a binary.
    //    (Mirrors fossil's visibility heuristic, minus re-parsing.)
    let imported_modules: HashSet<_> = graph.imports.values()
        .filter_map(|i| i.resolution.target_id())
        .collect();
    for (mid, module) in &graph.modules {
        if imported_modules.contains(mid) || !module.is_root_module { continue; }
        for (fid, f) in &graph.functions {
            if &f.parent_module == mid && f.parent_class.is_none() && is_public(f) {
                roots.insert(fid.clone());
            }
        }
    }
    roots
}

fn is_framework_decorator(d: &str) -> bool {
    // One flat table; extend via config, not code branches.
    const ENTRY_DECORATORS: &[&str] = &[
        "app.route", "router.route", "get", "post", "put", "delete", "patch", // FastAPI/Flask
        "app.command", "click.command", "argparse",                            // CLIs
        "RequestMapping", "GetMapping", "PostMapping",                         // Spring
        "EventHandler", "Subscribe", "Listener",
        "pytest.fixture", "fixture", "given",                                  // tests-as-entries
    ];
    let d = d.trim_start_matches('@');
    ENTRY_DECORATORS.iter().any(|pat| d.contains(pat))
}
```

Design rule learned from fossil: keep heuristics in **data tables**, not nested `matches!` trees —
that is precisely how their `entry_points.rs` grew to cyclomatic 28+ (`collect_defs`) and became a
brain-method generator.

### 6.2 Forward reachability

`callees_by_caller` already gives us downstream adjacency. Add the missing closure:

```rust
// core_indexer/src/graph/reachability.rs
use crate::types::{EntityId, ProjectedGraph};
use std::collections::{HashSet, VecDeque};

pub struct Reachability {
    /// Everything transitively callable from the roots.
    pub reachable: HashSet<EntityId>,
    /// The roots themselves (useful for reporting "why is this live?").
    pub entry_points: HashSet<EntityId>,
}

/// Edge-confidence-aware BFS. Edges below `min_confidence` are ignored so a
/// speculative `Embedding`-resolved call cannot keep genuinely dead code alive.
/// (Fossil's version is unweighted; ours exploits ResolutionMethod confidence.)
pub fn compute_reachable(
    graph: &ProjectedGraph,
    roots: &HashSet<EntityId>,
    min_confidence: f32,
) -> Reachability {
    let mut seen = roots.clone();
    let mut queue: VecDeque<EntityId> = roots.iter().cloned().collect();

    while let Some(current) = queue.pop_front() {
        if let Some(callees) = graph.callees_by_caller.get(&current) {
            for callee in callees {
                // If you need per-edge confidence here, consult the CallGraph
                // edge weights instead of the boolean adjacency maps.
                if seen.insert(callee.clone()) {
                    queue.push_back(callee.clone());
                }
            }
        }
        // Virtual dispatch: a call to a base method can reach any override.
        if let Some(subs) = graph.overridden_by.get(&current) {
            for sub in subs {
                if seen.insert(sub.clone()) {
                    queue.push_back(sub.clone());
                }
            }
        }
    }
    Reachability { reachable: seen, entry_points: roots.clone() }
}
```

Two details matter more than they look:

- **Overrides must extend liveness.** Calling `base.draw()` reaches `Circle.draw`. fossil handles
  this inside RTA; for us it's a two-line loop because `overridden_by` already exists. Skipping it
  produces the classic false positive "this method is dead" that destroys user trust.
- **Test-only liveness.** Like fossil's `include_test_reachable` parameter, compute a second pass
  with test files' functions added as roots; entities live *only* from tests get a separate
  classification (`DeadButTestOnly`) rather than being silently omitted.

### 6.3 Classification and finding emission

```rust
// core_indexer/src/graph/dead_code.rs
use super::reachability::{compute_reachable, Reachability};
use crate::scoring::{combine, tier_of, Tier};

pub enum DeadKind {
    /// No path from any entry point.
    Unreachable,
    /// Reached only from other dead code (a dead chain — fossil's "3-level dead chain").
    TransitivelyDead,
    /// Reached only from test code.
    TestOnly,
}

pub struct DeadFinding {
    pub entity_id: EntityId,
    pub kind: DeadKind,
    pub tier: Tier,
    pub score: f32,
    /// Number of lines removable — drives severity, like fossil's RemovalImpact.
    pub removable_lines: usize,
}

pub fn detect_dead(graph: &ProjectedGraph, include_test_only: bool) -> Vec<DeadFinding> {
    let roots = super::entry_points::detect_entry_points(graph);
    let live = compute_reachable(graph, &roots, 0.5);
    let mut out = Vec::new();

    for (id, f) in &graph.functions {
        if live.reachable.contains(id) { continue; }
        let kind = classify(&live, id);          // transitive vs root-dead vs test-only
        if matches!(kind, DeadKind::TestOnly) && !include_test_only { continue; }

        // Evidence combination: isolation strength × size × parse quality.
        let isolation = if matches!(kind, DeadKind::Unreachable) { 0.9 } else { 0.65 };
        let size_boost = (f.body_span.len() as f32 / 200.0).min(1.0).max(0.3);
        let quality = match f.parse_quality { good => 1.0, partial => 0.8 };
        let score = combine(&[isolation, size_boost, quality]);

        out.push(DeadFinding {
            entity_id: id.clone(),
            kind,
            tier: tier_of(score),
            score,
            removable_lines: f.exit_line.saturating_sub(f.line),
        });
    }
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    out
}
```

### 6.4 Exposure as a smell rule + MCP tool

Register a `SmellRule` reading the precomputed set (Stage 0 plumbing):

```rust
// core_indexer/src/smells/rules/dead_code.rs
pub struct DeadCode;

impl SmellRule for DeadCode {
    fn id(&self) -> &'static str { "dead-code" }
    fn scope(&self) -> Scope { Scope::Method }
    fn signals_needed(&self) -> &'static [&'static str] { &[] } // reads ctx.analyses

    fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
        let reachable = ctx.analyses.reachable?;
        if reachable.contains(ctx.entity_id) { return None; }
        Some(Finding {
            rule_id: self.id().into(),
            entity_id: ctx.entity_id.into(),
            severity: Severity::High,
            message: format!("'{}' is unreachable from any entry point", ctx.entity_name),
            signals: HashMap::from([("reachable".into(), 0.0)]),
        })
    }
}
```

And the Python MCP tool (see §12) exposing `dead_code(project_path, min_confidence,
include_test_reachable=false, max_findings)` — mirroring fossil's `analyze_dead_code` schema so
agents written against fossil migrate trivially.

**Tests to port from fossil's suite** (`tests/end_to_end.rs`, 1,600+ lines of exactly the right
regression cases): multi-line `impl` blocks not flagged dead; Spring `@RequestMapping` treated as
entry; dead chains reported transitively. Re-express them against `ProjectedGraph` fixtures.

Per the v0.8 §4.3 doctrine, `dead-code` must be **golden-tested on day one**, not added to the
"covered by a test that never asserts the rule fires" pile: fixture repos with inline expected
finding sets, asserted exactly.

**Effort:** ~1 week. **Demo:** `dead_code()` returns ranked deletable functions on any repo,
answered from a cold-started snapshot in well under a second (the v0.8 Phase 1.4 number).

---

## 7. Stage 2 — Token-Level Clone Detection

**Goal:** syntactic clone detection (Types 1–3) that scales sub-linearly via LSH, complementing
the existing embedding dedup (which approximates Type 4).

Fossil reference: `src/clones/{minhash,lsh_index,simhash,merkle,ngram_index,ir_tokenizer}.rs`.
Their `detector.rs` docstring states the design: *"unified clone detector combining Merkle hashing,
MinHash+LSH, and SimHash"* — adopt the same three-layer funnel:

```
Layer A  Merkle hash of body tokens      → exact Type-1 groups          O(n)
Layer B  SimHash over normalized tokens  → near-duplicate candidates    O(n)
Layer C  MinHash signature + LSH bands   → recall-oriented candidates   O(n) + O(candidates)
Then     verify survivors pairwise (normalized token Jaccard; APTED in Stage 6)
```

### 7.1 Normalized token stream (Type-2 normalization)

fossil's `ir_tokenizer.rs` replaces identifiers/literals with kind placeholders. We already hold
tree-sitter trees during extraction — hook there, but keep it a standalone pass so it can run over
cached bodies too:

```rust
// core_indexer/src/clones/tokens.rs
/// Normalize a function body into clone-comparable tokens.
/// Identifiers -> ID, literals -> LIT(kind); keywords/punctuation kept verbatim.
/// This is what makes `getUserEmail(u)` ≡ `fetchPhoneNumber(x)`.
pub fn normalize_body(body: &str, lang: Language) -> Vec<u32> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&super::super::CodeGraph::ts_language(&lang).expect("grammar")).unwrap();
    let tree = parser.parse(body, None).unwrap();
    let mut out = Vec::with_capacity(body.len() / 4);
    walk(tree.root_node(), body.as_bytes(), &mut out);
    out
}

fn walk(node: tree_sitter::Node, src: &[u8], out: &mut Vec<u32>) {
    use std::collections::hash_map::DefaultHasher; // token vocabulary built lazily
    let kind = node.kind();
    if node.child_count() == 0 {
        let tok: u32 = match kind {
            k if k.contains("identifier") => TOK_IDENT,          // 1
            "string" | "string_content"    => TOK_STR,           // 2
            "number" | "integer" | "float" => TOK_NUM,           // 3
            "comment"                      => return,            // dropped entirely
            _ => hash_kind_text(kind, node.utf8_text(src).unwrap_or("")), // punctuation verbatim
        };
        out.push(tok);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) { walk(child, src, out); }
}
```

Keep the vocabulary as `const u32` sentinels + xxh3 of `(kind, text)` for leaf tokens — no
allocation-heavy string vectors.

### 7.2 MinHash + banded LSH

```rust
// core_indexer/src/clones/minhash.rs
/// 128-row MinHash signature using xxh3 with seeded keys (no RNG dep needed).
pub struct MinHash { pub rows: [u64; 128] }

impl MinHash {
    pub fn of(shingles: impl Iterator<Item = u64>) -> Self {
        let mut rows = [u64::MAX; 128];
        for sh in shingles {
            for (i, r) in rows.iter_mut().enumerate() {
                let h = xxhash_rust::xxh3::xxh3_64(&sh.to_le_bytes())
                    ^ super::SEEDS[i];               // SEEDS: [u64; 128] constants
                *r = r.min(h);
            }
        }
        Self { rows }
    }
}

/// k-token shingles over the u32 token stream, packed into u64s.
pub fn shingles(tokens: &[u32], k: usize) -> impl Iterator<Item = u64> + '_ {
    tokens.windows(k).enumerate()
        .map(move |(i, w)| xxhash_rust::xxh3::xxh3_64(bytemuck::cast_slice::<u32, u8>(w)) ^ (i as u64))
}
```

```rust
// core_indexer/src/clones/lsh_index.rs
/// Banded LSH: 16 bands × 8 rows → candidate pairs collide when Jaccard ≳ 0.72.
/// (Standard (b, r) tradeoff; expose as config like fossil's select_lsh_params.)
pub struct LshIndex {
    bands: Vec<HashMap<u64, Vec<u32>>>,   // band_hash -> fingerprint ids
    sigs: Vec<MinHash>,
}

impl LshIndex {
    pub fn insert(&mut self, id: u32, sig: MinHash) {
        for (b, chunk) in sig.rows.chunks(8).enumerate() {
            let key = xxhash_rust::xxh3::xxh3_64(bytemuck::cast_slice::<u64, u8>(chunk));
            self.bands[b].entry(key).or_default().push(id);
        }
        self.sigs.push(sig);
    }

    /// Candidate pairs only — never compare all pairs.
    pub fn candidate_pairs(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.bands.iter().flat_map(|band| band.values())
            .flat_map(|bucket| {
                let mut pairs = Vec::new();
                for i in 0..bucket.len() {
                    for j in i + 1..bucket.len() {
                        pairs.push((bucket[i], bucket[j]));
                    }
                }
                pairs
            })
            .collect::<Vec<_>>().into_iter()
    }
}
```

### 7.3 Incremental integration (our advantage over fossil)

fossil fingerprints the whole tree per run; we already have per-function hashes. Key the fingerprint
store by `Function.content_hash`:

```rust
// core_indexer/src/clones/store.rs
/// content_hash -> (fingerprint id). On update_file, only re-fingerprint
/// bodies whose content_hash changed — identical flow to embedding caching.
pub struct FingerprintStore {
    by_content: HashMap<u64, u32>,
    removed_at_revision: DashMap<u32, u64>, // ledger integration
}
```

This makes `detect_clones` after a single-file edit cost one fingerprint + one LSH insert, versus
fossil's full rescan — a concrete, demonstrable performance win.

Storage note per §4 principle 9: the fingerprint store is a *cache* keyed by `content_hash`, held
in memory and rebuilt lazily from body spans after a cold `load_snapshot`. It must never enter the
ledger — otherwise it becomes a second source of truth and the round-trip identity test loses its
meaning.

### 7.4 Output model

```rust
pub struct CloneGroup {
    pub clone_type: CloneType,           // Type1 | Type2 | Type3
    pub similarity: f64,
    pub instances: Vec<CloneInstance>,   // entity_id + ByteSpan from Function.body_span
    pub confidence_tier: Tier,           // scoring::clone_confidence
}
pub enum CloneType { Type1, Type2, Type3 }  // Type4 stays with embeddings; fuse later
```

Expose as MCP tool `find_clones(project_path, min_lines=10, min_similarity=0.8, languages=None)`
plus a `clone-groups` smell scope addition (new `Scope::CloneGroup` variant, or emit findings
against the first instance's entity — prefer the latter initially to avoid Scope enum churn).

**Effort:** ~2 weeks. **Budget check:** keep `detector.rs` orchestration < 300 LOC by following
the funnel structure above; fossil's equivalent sprawls across 900+.

---

## 8. Stage 3 — AI-Scaffolding & Secrets Scanner

**Goal:** detect phase markers, TODO density, placeholder bodies, temp-file naming, and hardcoded
secrets. Zero graph dependency — highest value-to-effort ratio in the plan.

Fossil reference: `src/mcp/tools/scaffolding.rs` — **do not port its structure** (3,891 LOC, the
worst god-file in their codebase). Port the *rule tables* only, expressed as data:

```rust
// core_indexer/src/scaffold/mod.rs  (target: < 400 LOC total)
use serde::Deserialize;

/// Data-driven scanner config — loaded from fossil-style TOML or defaults.
#[derive(Deserialize)]
pub struct ScaffoldConfig {
    #[serde(default = "default_markers")]
    pub comment_patterns: Vec<String>,      // regex: "Phase \\d", "Step \\d", "TODO", "FIXME"
    #[serde(default)]
    pub placeholder_bodies: bool,           // pass / todo!() / NotImplementedError / ...
    #[serde(default)]
    pub temp_file_globs: Vec<String>,       // "temp_*", "backup_*", "old_*", "phase_*"
    #[serde(default)]
    pub include_secrets: bool,
    #[serde(default)]
    pub max_comment_density: f64,
}
```

Secret patterns with mandatory redaction (port the *idea*, rewrite the patterns):

```rust
// core_indexer/src/scaffold/secrets.rs
pub struct SecretPattern {
    pub name: &'static str,        // "openai_api_key"
    pub regex: once_cell::sync::Lazy<regex::Regex>,
}

pub static SECRET_PATTERNS: &[SecretPattern] = &[ /* AWS, GH, Stripe, Slack hooks, PEM… */ ];

/// Redact: keep first 8 chars + "***". Never emit raw secrets in findings —
/// findings go over MCP to agents; treat them as hostile output channels.
pub fn redact(matched: &str) -> String {
    let mut out: String = matched.chars().take(8).collect();
    out.push_str("***");
    out
}

/// Placeholder-body detection rides the existing extraction pass: a function
/// whose body_span trims to only `pass`/`...`/`todo!()`/`panic!("not implemented")`.
pub fn is_placeholder_body(body: &str) -> bool {
    let t = body.trim();
    t == "pass" || t == "..." || t == "todo!()" || t == "unimplemented!()"
        || t == "raise NotImplementedError" || t == "throw new Error(\"TODO\")"
}
```

Because `Function.body_span` and `body_hash` exist, placeholder detection is a map over
`graph.functions` — no file walking needed for that rule. Only comment/temp-file scanning touches
raw files (use the `ignore` crate for walker + gitignore compliance).

MCP tool: `find_scaffolding(project_path, include_secrets=false, max_findings=100)`.

**Effort:** ~4 days. **Note:** pair with a mutation follow-up — "clean scaffolding" is a natural
demo of CodeRadar's write-guarded edits, which fossil cannot do at all.

---

## 9. Stage 4 — CFG Upgrade of the Smells Engine

**Goal:** replace AST-walk approximations in `smells/metrics.rs` with real control-flow graphs;
unlock accurate cyclomatic, dominators, and (Stage 6) dead branches.

Current state: `cyclomatic_complexity` counts decision-point node kinds — a documented
approximation ("deterministic approximation of McCabe"). It miscounts short-circuit operators,
comprehension guards, and pattern-match fallthrough. Fossil's `graph/cfg.rs` builds proper blocks.

### 9.1 CFG construction

```rust
// core_indexer/src/graph/cfg.rs
use petgraph::graph::DiGraph;

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub func_id: crate::types::EntityId,
    /// Source ranges covered, in order (multi-range when a block absorbs splits).
    pub spans: Vec<crate::types::ByteSpan>,
    pub is_entry: bool,
    pub is_exit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind { Fallthrough, TrueBranch, FalseBranch, LoopBack, Exception, Switch(usize) }

pub struct ControlFlowGraph {
    pub blocks: DiGraph<BasicBlock, EdgeKind>,
    pub entry: petgraph::graph::NodeIndex,
    pub exit: petgraph::graph::NodeIndex,
}

impl ControlFlowGraph {
    /// Build from a parsed function body. Language-agnostic: branch on
    /// tree-sitter kinds already enumerated in smells/metrics.rs
    /// (is_decision_point / is_nesting_node) — same tables, different consumer.
    pub fn build(func_id: &crate::types::EntityId, body: tree_sitter::Node, src: &[u8])
        -> Result<Self, CfgError>
    { /* statement-level partitioning; see plan appendix */ }
}

impl ControlFlowGraph {
    /// McCabe on the CFG: M = E − N + 2P. Matches textbook exactly,
    /// unlike AST-kind counting.
    pub fn cyclomatic(&self) -> usize {
        self.blocks.edge_count() - self.blocks.node_count() + 2
    }

    /// Blocks unreachable from entry (intra-procedural dead code —
    /// statements after `return` in the same function).
    pub fn unreachable_blocks(&self) -> Vec<petgraph::graph::NodeIndex> { /* reverse BFS */ }
}
```

### 9.2 Migration strategy — strangler pattern, not big-bang

Keep `metrics.rs` untouched for extraction-time metrics (it's cheap and always available). Add
CFG as a *lazily computed refinement*:

```rust
// core_indexer/src/smells/engine.rs (run method, method-scope section)
let metrics = metrics_for_function(f);
// Refine with CFG math when the body is available; fall back silently otherwise.
if let Some(cfg) = cfg_cache.get_or_build(f) {
    metrics.cyclomatic = cfg.cyclomatic();   // overrides AST estimate
    signals.insert("unreachable_blocks", cfg.unreachable_blocks().len() as f64);
}
```

Cache CFGs keyed by `body_hash` in the Stage 0 analysis cache. Languages with unreliable
tree-sitter block structure degrade gracefully to AST numbers — the existing thresholds (10/20/47/50)
were chosen coarse enough for exactly this reason, per the `metrics.rs` header comment.

New smell rule enabled by CFGs:

```rust
// rules/intra_dead_statements.rs — "statements after return in same function"
fn evaluate(&self, ctx: &EvalContext) -> Option<Finding> {
    let n = ctx.metrics.get("unreachable_blocks")? as usize;
    (n > 0).then(|| Finding {
        rule_id: "intra-dead-statements".into(),
        entity_id: ctx.entity_id.into(),
        severity: Severity::Medium,
        message: format!("{n} unreachable basic block(s) — code after return/raise?"),
        signals: HashMap::from([("unreachable_blocks".into(), n as f64)]),
    })
}
```

**Effort:** ~2 weeks including tests (CFG builders are test-heavy; port fossil's
`test_cfg_basic_structure` / `test_cfg_if_else` / `test_cfg_loop` cases).

---

## 10. Stage 5 — Centrality Metrics

**Goal:** rank entities by structural importance; feed triage ordering for god-class/brain-method
findings and prioritize blast-radius output.

Fossil reference: `src/graph/centrality.rs`. Implementation on our side is straightforward because
`callers_by_callee` is materialized:

```rust
// core_indexer/src/graph/centrality.rs
use std::collections::HashMap;
use crate::types::{EntityId, ProjectedGraph};

/// Weighted degree + harmonic-centrality hybrid. Full PageRank is available
/// later via petgraph, but degree-harmonic explains better to users and costs
/// one BFS per node with early cutoff.
pub fn harmonic_centrality(
    graph: &ProjectedGraph,
    max_depth: usize,
    min_edge_confidence: f32,
) -> HashMap<EntityId, f64> {
    let mut out = HashMap::new();
    for id in graph.functions.keys() {
        let mut score = 0.0;
        for depth in 1..=max_depth {
            let frontier = frontier_at(graph, id, depth, min_edge_confidence);
            if frontier.is_empty() { break; }
            score += frontier.len() as f64 / depth as f64; // harmonic weighting
        }
        out.insert(id.clone(), score);
    }
    // Normalize to 0..=1 for stable thresholds.
    let max = out.values().cloned().fold(f64::MIN, f64::max);
    if max > 0.0 { out.values_mut().for_each(|v| *v /= max); }
    out
}
```

Consumers:
1. **Triage boost:** in god-class/brain-method findings, append `"central"` signal; UI sorts
   `severity × centrality`.
2. **Blast-radius ranking:** `affected` results ordered by centrality, not BFS order — answers
   "of these 40 affected functions, which 3 actually matter?"
3. Stored in `EvalContext.analyses.centrality` (Stage 0) for any future rule.

**Effort:** ~3 days.

---

## 11. Stage 6 — Advanced

Ordered by dependency; none block Stages 1–5.

### 11.1 APTED verification for clone candidates (≈1 week)

Port fossil's `apted.rs` concept (they validate theirs against Zhang-Shasha ground truth — keep
those tests). Apply only to Layer-C survivor pairs above similarity 0.85, capped by subtree size,
to upgrade `similarity` estimates and disambiguate Type2 vs Type3. Budget: persistent stack, no
recursion (their recursive version hits stack limits on deep ASTs; their
`compute_subtree_distance_inline` nests 6 deep — learn from it, don't copy it).

### 11.2 Constant propagation + dead-branch detection (≈2 weeks, depends on Stage 4)

Fossil: `constant_prop.rs` (1,924 LOC) + `expr_evaluator.rs`. Scope down for v1: evaluate only
literal comparisons and boolean literals on condition edges (`if DEBUG:`-style), flag
always-true/false branches as a Medium finding. Skip interprocedural const-prop entirely for now —
their 31-cyclomatic brain method is a warning, not an inspiration.

### 11.3 RTA-lite for Python call-graph sharpening (≈2–3 weeks, highest strategic value)

Problem: `Embedding`/`SignatureMatch` resolution leaves Python dispatch fuzzy; fuzzy edges either
hide dead code (Stage 1 false negatives) or inflate blast radius. We own the substrate fossil
lacks: `subclasses`, `overrides_base`, `ClassHierarchy`, MRO.

```rust
// core_indexer/src/graph/rta_lite.rs
/// Rapid-Type-Analysis-lite: given an instantiation site `x = Circle()`,
/// constrain `x.method(...)` resolutions to Circle ∪ strict subclasses.
/// Re-score affected ResolvedEdges: raise confidence for constrained-in
/// targets, demote (below Stage-1's min_confidence) targets excluded by every
/// instantiated type flowing to that variable.
pub fn refine_dispatch(graph: &mut ProjectedGraph, instantiations: &[(EntityId, EntityId)]) {
    /* per-variable type lattice seeded from instantiations;
       intersect with subclass closures from ClassHierarchy */
}
```

Even a conservative version (only refine when exactly one concrete instantiation dominates)
measurably improves both dead-code precision and `affected` precision — the two headline features.

**Interface languages:** RTA-lite's subclass closure inherits the `IMPLEMENTS` ambiguity described
in v0.8 Phase 2.3 — `class C implements Serializable` currently rides an `extends` edge. Until 2.3
lands, scope RTA-lite claims to Python (where MRO is exact) and mark Java/Go/TS/C# results with a
documented precision caveat rather than shipping silent over-approximation.

### 11.4 Temporal analyses — the features fossil-mcp cannot copy (≈1 week, depends on v0.8 Phase 2.2)

Once `as_of` supports bidirectional traversal over reconstructed projections, the ledger turns
this plan's detectors into history queries — the one capability class fossil-mcp structurally
cannot match, since it has no temporal storage:

```rust
// core_indexer/src/graph/temporal.rs
/// "What was dead at ts, and what died since?"
pub fn dead_as_of(graph_now: &ProjectedGraph, ts: &str) -> Result<DeadDiff, Error> {
    let historical = /* reconstruct(ts) -> MaterializedState -> ProjectedGraph */;
    let then = detect_dead(&historical, false);
    let now = detect_dead(graph_now, false);
    Ok(DeadDiff {
        newly_dead: now.difference(&then),   // AI session artifacts surfacing
        resurrected: then.difference(&now),  // code brought back to life
        persistent: now.intersection(&then), // chronic dead weight
    })
}
```

Two products fall out:
- **Clone evolution** (fossil's `clones/evolution.rs`, but real): track a clone group's similarity
  drift across ledger revisions — when did `utils_v2.py` diverge from `utils_v1.py`, and by how much.
- **Scaffolding velocity**: scaffolding-marker count per week from `git` + ledger timestamps —
  measures whether a team's vibe-coding debt is growing.

MCP tools: `dead_code_as_of(project_path, timestamp)` and `clone_evolution(project_path,
since=timestamp)`. Both must obey the v0.8 §5.2 rule: if Phase 2.2 has not shipped, these tools do
not appear in `tools/list` at all — no stub schemas advertising what cannot answer.

### Explicitly NOT porting
- **SDG/PDG/full slicing** — months of work, needs real dataflow foundation, low agent-facing ROI today. Revisit post-Stage 6.
- **VTA** — subsumed by RTA-lite for our purposes.
- **Interprocedural const-prop** — see 11.2.
- **Self-update / weekly-report machinery** — product surface, not algorithm; CodeRadar's lifecycle differs.

---

## 12. Python-Side Tool Exposure

Follow the existing tool pattern in `py_agent/src/coderadar/mcp/` (as `affected` does). Example
for Stage 1:

```python
# py_agent/src/coderadar/mcp/tools/dead_code.py
from ..server import mcp_tool
from ... import _core  # PyO3 bindings exposed via maturin

@mcp_tool(
    name="dead_code",
    description=(
        "Detect functions unreachable from any entry point "
        "(mirror of affected(): what can be safely deleted?). "
        "Requires a prior scan/index of the project."
    ),
)
async def dead_code(
    project_path: str,
    min_confidence: float = 0.6,        # Speculative|Low tiers filtered by default
    strictness: str = "normal",          # strict | normal | loose — maps to
                                         # confidence floor (Stage 0.4); pass
                                         # EITHER this or min_confidence, not both
    include_test_reachable: bool = False,
    language_filter: str | None = None,  # comma-separated: rust,python,go...
    max_findings: int = 100,
) -> dict:
    """Thin wrapper: heavy lifting happens in core_indexer::graph::dead_code."""
    result = await _core.dead_code_scan(
        project_path,
        min_confidence=min_confidence,
        include_test_reachable=include_test_reachable,
    )
    return {
        "findings": result[:max_findings],
        "summary": {"total": len(result),
                    "removable_lines": sum(f["removable_lines"] for f in result)},
        "note": "Verify with affected(entity_id) before deleting.",
    }
```

Tool-schema compatibility note: mirror fossil-mcp's parameter names (`project_path`,
`min_confidence`, `max_findings`) so prompts written for fossil's `analyze_dead_code` work
unchanged against CodeRadar — free ecosystem compatibility.

Rust↔Python boundary: add the corresponding `#[pyfunction]` next to the existing `with_graph`
pyfunctions; return JSON-ready dicts, not Rust structs.

---

## 13. Testing Strategy

Integrated with the v0.8 §4 testing doctrine — the plans share one philosophy: **a surface that
produces plausible output and exits 0 under a green suite is the enemy.**

1. **Golden fixtures.** Create `fixtures/clones/` and `fixtures/dead-code/` modeled on fossil's
   layout (`utils_v1.py` vs `utils_v2.py`, live-class-with-dead-methods). Each fixture documents
   expected findings inline; tests assert exact sets. This directly closes the gap v0.8 §4.3
   identified (only 4 of 13 rules golden-tested): **all nine rules this plan introduces ship
   golden-tested in the same PR as the rule.**
2. **Ingest parity, extended.** v0.8 Phase 4.2 establishes the three-way property test
   (`analyze` ≡ `analyze + update_file` ≡ `analyze → fresh → load_snapshot`). Every stage here adds
   a *consumer* of the projection; each must extend the assertion tail with its own invariant —
   e.g., reachability sets and centrality scores must be identical across all three paths. Derived
   data differing between ingest paths would be a fourth copy-drift bug class (0.3/0.3b/0.4b
   redux), caught the same way. The parity harness also carries the **strictness-monotonicity
   property** from §5 Stage 0.4: for any projection and any rule, findings(Strict) ⊇ findings(Normal)
   ⊇ findings(Loose), pinned by golden sets per level and randomized metric draws via `hypothesis`.
3. **Port fossil's regression cases.** Their `tests/end_to_end.rs` encodes years of
   false-positive fixes (multi-line `impl`, decorator entries, dead chains). Re-express ~30 highest-
   value cases against ProjectedGraph fixtures.
4. **Property tests** (repo already uses `hypothesis` on the Python side): LSH must never miss a
   pair with true Jaccard ≥ threshold + ε (recall property over random synthetic token streams);
   CFG invariants (every non-flagged block reachable from entry; E−N+2 ≥ 1) over random snippets.
5. **Plausible-output audits as part of each stage's DoD.** Per v0.8 §4.3's candidate list, the
   new tools are themselves high-trust surfaces: `dead_code` returning `{}` on a stale snapshot
   must be distinguishable from `{}` on clean code (return a `stale: true` marker or refuse —
   never silent emptiness); framework-entry decorators must resolve on at least one fixture each,
   exactly like the resolver audit.
6. **Self-hosting gate.** Run the extended smell engine on `core_indexer` itself in CI; new stages
   must not introduce Critical findings in their own code (enforces §4 principle 4). Schedule this
   after v0.8 Phase 4.4 flips CI blocking, so the gate is enforceable rather than decorative.
7. **Differential testing vs fossil-mcp.** Where semantics overlap, run both tools on the same
   fixtures and diff findings — divergence review meetings, not necessarily parity.

---

## 14. Sequencing, Effort, and Milestones

### 14.1 Merged master roadmap

This plan runs **after** Track H (integrity hotfixes) and the v0.8 release train. The programmes
share one timeline with clear handoffs; v0.8's own ordering constraints (lint-last,
parity-before-trust) are preserved.

```
═══════════ TRACK H — integrity hotfixes (from CODERADAR_BUGS_QUIRKS.md) ══ ~4 days
H1      replace_body: validate WRITTEN file parses; auto-rollback on failure (#1)
        ▸ GATE G0-A: mutation repro pinned as regression test
H2      Single-sourced mutation status enum; explicit file_written/graph_updated (#2)
H3      Preserve leading docstrings on replace_body, or flag removal in diff (#4)
H4      Error-guidance string fix (#7) + mcpScript prefixing docs (#10)
        + default-exclude build dirs (target/, node_modules/, dist/)
H5      Dedupe smell findings by current entity version only (#9)
        ▸ GATE G0-B: golden fixture asserts one finding per entity per rule

═══════════ v0.8 (docs/v0.8-improvement-plan.md) ═══════════════════════ ~10 days
v0.8 P1     Cold start: load_snapshot + retire _ensure_graph + benchmark
            ▸ GATE G1: round-trip parity green; cold start < 4.6 s
v0.8 P2.1   explore() → Rust traverse (all four kinds)
v0.8 P4.1/2 Tier-1 signature matrix + THREE-WAY ingest parity property test
            ▸ GATE G2: parity test exists BEFORE this plan's consumers land
v0.8 P3     Wire-or-cut lsp/, agent/, tool_router
v0.8 P2.2   as_of upstream/bidirectional        ── enables Stage 6.4
v0.8 P2.3   IMPLEMENTS edge kind                ── enables Stage 6.3 full scope
v0.8 P4.3   Plausible-output audit sweep
v0.8 P4.4   Lint backlog + blocking CI          ── mechanical diffs land here, last
v0.8 P5     CHANGELOG + interim-state docs

═══════════ THIS PLAN (fossil-mcp ports) ════════════════════════════ ~12 weeks
Week 1      Stage 0  Foundations (scoring, GraphAnalyses, revision-keyed caches)
                     requires G1 + G2
Week 2–3    Stage 1  Dead code (entry points → reachability → classifier → MCP tool)
                     ▸ MILESTONE A: demo "what can I delete?" on a real repo,
                       answered from a cold-started snapshot
Week 4–5    Stage 2  Clones part 1 (tokens, MinHash, LSH, store, Type-1/2)
Week 6      Stage 3  Scaffolding scanner (+ secrets) — parallelizable, pure Rust
                     ▸ MILESTONE B: find_clones + find_scaffolding tools live
Week 7–8    Stage 2  Clones part 2 (SimHash, Type-3, incremental store polish)
Week 9–10   Stage 4  CFG build + metrics strangler + intra-dead-statements rule
                     ▸ MILESTONE C: CFG-backed metrics behind feature flag
Week 11     Stage 5  Centrality + triage integration
Week 12+    Stage 6  APTED → dead branches → RTA-lite → temporal analyses
            (RTA-lite after v0.8 P2.3; temporal after v0.8 P2.2)
                     ▸ MILESTONE D: RTA-lite precision report on Python corpus
                     ▸ MILESTONE E: dead_code_as_of + clone_evolution demos
```

Total: ~10 days of v0.8 + ~12 weeks for Stages 0–C with one engineer. If v0.8 must stay minimal,
its own minimum-viable cut (**P1 → P2.1 → P5.1**) is sufficient for this plan to begin — G1 and G2
are the only hard gates. **Track H runs first regardless**: it is 4 days, it fixes a critical
corruption bug, and G0-B is a precondition for every golden test this plan relies on.

### 14.2 Cross-plan parallelization slots

Mirroring v0.8's own observation that its Phase 3 can run beside Phase 1, the reverse slot exists
too: **this plan's Stage 3 (scaffolding scanner)** touches neither `lib.rs`, `storage.rs`, nor the
projection at all. It is the designated parallel work item whenever v0.8 Phase 1 stalls on parity-
test reconciliation — which v0.8 itself flags as the most likely overrun.

### 14.3 Dependency graph (merged)

```
v0.8 P1 ──► G1 ──► Stage 0 ──► Stage 1 ──────────────► Stage 6.4 (temporal) ◄── v0.8 P2.2
                        │
                        ├──► Stage 2 ──► Stage 6.1 (APTED)
                        ├──► Stage 3 (independent; v0.8-stall filler)
                        └──► Stage 5
Stage 4 ──► Stage 6.2 (dead branches)
v0.8 P2.3 ──► Stage 6.3 (RTA-lite, full-language scope)
```

---

## 15. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| **v0.8 Phase 1.1 parity reconciliation overruns** (flagged there as the likely overrun), delaying every gate this plan waits on | High | High | Stage 3 pre-positioned as the parallel filler (§14.2); Stage 0's scoring module is graph-independent and can start immediately |
| Cold-started projections subtly differ → dead-code results differ by ingest path | Medium | Critical | Extend the three-way parity test with derived-data assertions *before* Stage 1 merges (§13.2); feature flag keeps `analyze()` path default |
| Dead-code false positives destroy trust | High | High | Confidence tiers; `Speculative` hidden by default; "verify with `affected()`" guidance in tool output; test-only classification; stale-snapshot refusal, never silent `{}` |
| Dynamic dispatch hides liveness (Python) | High | Medium | Overrides-extend-liveness rule (§6.2); RTA-lite in Stage 6.3; configurable entry decorators |
| `IMPLEMENTS` deferral (v0.8 P2.3 slip) weakens interface-language liveness | Medium | Medium | Document precision caveat per §6.2/§11.3; scope RTA-lite claims to Python until 2.3 lands |
| LSH memory growth on monorepos | Medium | Medium | Band count tunable; in-memory cache pruned at ledger revision boundaries (never persisted, §4.9) |
| CFG builder correctness across 16 grammars | Medium | High | Strangler migration keeps AST fallback; property-test CFG invariants (every block reachable from entry except flagged ones; E−N+2 ≥ 1) |
| Importing fossil's complexity debt | Medium | Medium | §4 budgets (<800 LOC/file, cyclo ≤ 20) + CI self-hosting gate once v0.8 P4.4 makes CI blocking |
| PyO3 surface churn | Low | Low | All new APIs behind `await _core.<tool>` async wrappers; version the boundary |
| Scope creep toward SDG/slicing | Medium | Schedule | Explicitly deferred in §11; revisit gate = RTA-lite shipped and measured |
| Track H slips ("we'll fix it during v0.8") → goldens built on duplicate-prone findings, mutation demos unsafe | Medium | Critical | Track H is sequenced before everything and gated (G0-A/B); it is deliberately small (~4 days) so it cannot become a programme of its own |
| New MCP tools hit the #8 empty-vs-warming ambiguity on slow indexes | High (large repos) | High | Every tool here returns typed `Indexing` / `Stale` refusals per §4.7; DoD includes an explicit refusal-path test, not just happy path |

---

## Appendix A — fossil-mcp source map (for implementers)

```
Dead code      src/dead_code/entry_points.rs   framework/visibility/config entry heuristics
               src/dead_code/detector.rs       orchestration: entries → reachability → classify
               src/dead_code/classifier.rs     DeadCodeFinding {confidence, severity, removal_impact}
Clones         src/clones/detector.rs          funnel docstring: Merkle + MinHash/LSH + SimHash
               src/clones/minhash.rs           signature construction
               src/clones/lsh_index.rs         banded index, select_lsh_params()
               src/clones/simhash.rs           normalize_for_simhash(), token_weight()
               src/clones/ir_tokenizer.rs      Type-2 identifier/literal normalization
               src/clones/apted.rs             APTED + Zhang-Shasha cross-validation tests
Scoring        src/core/scoring.rs             clone_confidence(), Confidence tiers
CFG/dataflow   src/graph/cfg.rs                BasicBlock, dominators
               src/graph/constant_prop.rs      DeadBranch {block_id, condition, always_value}
               src/graph/centrality.rs         centrality metrics
Scaffolding    src/mcp/tools/scaffolding.rs    RULE TABLES ONLY (structure: do not imitate)
Incremental    src/analysis/incremental.rs     change-set driven re-analysis pattern
```

## Appendix B — Definition-of-Done checklist (per stage)

- [ ] Gates G0-A/G0-B (mutation integrity + findings dedupe) green on the branch
- [ ] Gates G1/G2 (v0.8 Phase 1 parity + three-way ingest test) green on the branch
- [ ] Feature flag in `[analysis]` config; default off until Milestone validation
- [ ] Unit tests ≥ 90% line coverage on new modules; golden fixtures committed
- [ ] **Golden tests assert the new rule/tool fires AND doesn't fire on negative fixtures**
      (closes the v0.8 §4.3 god-class coverage pattern for every new rule)
- [ ] Derived-data parity assertions added to the three-way ingest test (§13.2)
- [ ] Wire-or-cut satisfied: engine registration + pyfunction + MCP schema in the same PR (§4.8)
- [ ] Honest-refusal check: stale graph, unsupported language, below-floor confidence → typed
      refusal or explicit marker, never silent empty success (§4.7, §13.5)
- [ ] **Strictness profiles: golden finding sets asserted at strict/normal/loose; monotonicity
      strict ⊇ normal ⊇ loose verified** (Stage 0.4; per-stage for rules added later)
- [ ] MCP tool registered with schema mirroring fossil parameter names
- [ ] Self-hosting CI gate passes (no new Critical smells in own code) — once v0.8 P4.4 blocks
- [ ] CHANGELOG entry (v0.8 §5.1 convention starts here) + docs snippet in
      `docs/traversal-matrix.md` if traversal-related
- [ ] Performance note: p95 latency measured on a ≥1k-file corpus **via the cold-start path**, not
      just warm-process
