markdown
Architecture Specification: CodeGraph Engine
A Real-Time Semantic Code Graph Database with Multi-Language Support and AST-Aware Refactoring

Version: 3.2.1 (Consolidated, Restoration + Patches 3.1/3.2)
Date: 2026-07-25
Status: Implementation-Ready — pending cargo check validation gate (§15.5)
Document ID: CG-ARCH-003
Supersedes: v3.0, v3.1, v3.2, Amendment CG-ARCH-003-A1 (all consolidated herein)

Revision History

| Rev | Date | Summary |
|-----|------|---------|
| 3.0 | 2026-07-25 | Base specification: Rust-native Stack Graphs resolution, LadybugDB hybrid graph+vector store, GraphRAG pipeline, optional LSP. |
| 3.1 | 2026-07-25 | Review response (CG-ARCH-003-R1) — all 18 findings resolved: design-pseudocode labeling, StableDiGraph O(1) removal, cycle-safe traversals, persistent warm LSP pool, staged two-phase commit, disjoint confidence bands, corrected Cypher, ::toplevel sentinel, real git blame, schema versioning, cross-boundary trace correlation, memory budgets (register in Appendix C). |
| 3.2 | 2026-07-25 | Amendment A1 incorporated: AST-aware MutationEngine, byte-accurate span model, four LLM refactoring tool calls, WriteGuard, mutation policy, MutationLog audit. |
| 3.2.1 | 2026-07-25 | Restoration pass: §15.4 snapshot table (full tail), §15.5, §16 (§16.2 cross-checked against v3.1 §14.2), Appendix A ParsedFile. Patch 3.1: MutationLog tiered retention (§11.10). Patch 3.2: indent normalization (§11.3) — deterministic failure mode closed, no repair-attempt consumed. |

Executive Summary

CodeGraph Engine is a local, high-performance code analysis daemon that builds and maintains a live semantic graph of any codebase, enabling LLMs and developer tools to both query and safely rewrite code through a unified pipeline.

Near-zero idle CPU via event-driven debouncing and content-addressed embedding deduplication.
Real-time ingestion: single-file save → graph updated in  Rust engine
│       │   └── policy.py               # Allow/deny, budgets, git cleanliness
│       ├── query/  (planner.py, templates.py, executor.py, cache.py)
│       ├── agent/  (graphrag.py, contextbuilder.py, prompts.py)
│       ├── ingestion/  (pipeline.py, batch.py, scheduler.py, commit.py)
│       └── diagnostics/  (metrics.py, health.py, cli.py)
│
├── cli/  (Rust binary: codegraph-cli)
└── tests/  (rust/, python/, fixtures/)

Core Dependencies

3.1 Rust Core

| Crate | Version | Purpose |
|-------|---------|---------|
| pyo3 | 0.23+ | Python extension; #[pyclass] zero-copy structs. |
| notify / notify-debouncer-full | 7.0 / 0.4 | FS events + coalescing. |
| tree-sitter + tree-sitter-language-pack | 0.24 / 0.6+ | Parsing, 300+ grammars. |
| stack-graphs | 0.14+ | Name resolution (12 languages). |
| tree-sitter-graph | 0.12+ | TSG rule execution. |
| petgraph | 0.7+ | StableDiGraph for import/call graphs (stable indices across removals). |
| ropey | 1.6+ | Incremental re-parse and mutation multi-edit application. |
| similar | 2.5+ | Unified diff generation for MutationPlan previews. |
| dashmap | 6.0+ | Concurrent resolution caches. |
| lru | 0.12+ | LRU eviction for graph fragments. |
| rayon | 1.10+ | Parallel per-file resolve/embed prep. |
| xxhash-rust | 0.8 | Content-addressed deduplication (non-cryptographic). |
| git2 | 0.19 | Branch detection + blame. |
| zstd | 0.13+ | Compression for spilled fragments and mutation backups. |
| ulid | 1.1+ | traceid and planid generation. |
| ignore, crossbeam-channel, tracing, serde, smol_str | — | Ignore filtering, event queue, logging, config, identifiers. |

3.2 Python Orchestrator

| Package | Purpose |
|---------|---------|
| ladybug | LadybugDB driver (Kùzu dialect) + HNSW vector extension. |
| fastembed | ONNX embeddings, jinaai/jina-code-embeddings-0.5b (896-d, Matryoshka-truncatable to 64-d). |
| litellm | Unified LLM API + function-calling tool dispatch. |
| grep-ast, ast-grep | Structural context compression for LLM prompts. |
| structlog, click, pydantic | Logging, CLI, config validation. |

3.3 Optional LSP Servers (Disabled by Default)

pyright, typescript-language-server, rust-analyzer, gopls — managed by the persistent warm pool in §10, never spawned per-request.

Semantic Resolution Engine (Rust-Native)

4.1 Pseudocode Notice & Resolution Cascade

All code listings in this document are design pseudocode defining intended structure and API surface. Exact crate APIs (stack-graphs 0.14, tree-sitter-graph 0.12, git2 0.19, petgraph 0.7) must be verified with cargo check during implementation (gate in §15.5). Where a listing conflicts with a real crate API, the crate wins; the listing is then a documentation bug, not a design change.

All primary semantic resolution happens in Rust, before any data crosses the PyO3 boundary:
text
LAYER 1  Stack Graphs            confidence 0.90 - 1.00   (12 languages, .tsg rules)
LAYER 2  Import Graph + Scope    confidence 0.80 - 0.89   (single match 0.89, ambiguous 0.80)
LAYER 3  Signature Matching      confidence 0.40 - 0.79   (name + arity + proximity)
LAYER 4  Embedding Fallback      confidence 0.20 - 0.39   (Python, threshold 0.85 cosine)
LAYER 5  LSP Override            confidence 1.00          (optional, pool-backed, §10)

Bands are disjoint by construction: every resolver clamps its output into its band, so downstream consumers can filter on confidence and resolution_method without ambiguity.

4.2 Stack Graphs Resolver (semantic/stack_graph.rs)
rust
// DESIGN PSEUDOCODE
pub struct StackGraphResolver {
    graph: StackGraph,
    language_rules: HashMap,
    // LRU-bounded: cold fragments spill to .harness/spill/ (zstd), rebuilt on demand
    file_fragments: LruCache>,
    spill_dir: PathBuf,
}

impl StackGraphResolver {
    pub fn indexfile(&mut self, filepath: &str, source: &str,
                      tree: &Tree, language: &str) -> Result {
        let rules = self.language_rules.get(language)
            .ok_or(StackGraphError::UnsupportedLanguage)?;
        self.evictfragment(filepath);                    // incremental: drop old nodes

        let mut functions = Functions::stdlib();
        let mut tsg_graph = TsgGraph::new();
        Pass::from_file(rules)
            .execute(tree, source, &mut functions, &mut tsg_graph)
            .maperr(|e| StackGraphError::TsgExecution(e.tostring()))?;

        let filehandle = self.graph.getorcreatefile(file_path);
        let nodes = self.convertfragment(&tsggraph, file_handle);
        self.filefragments.put(filepath.to_string(), nodes); // LRU may spill oldest
        Ok(())
    }

    pub fn resolvereference(&self, filepath: &str, reference: &ParsedReference) -> Option {
        let refnode = self.findreferencenode(filepath, reference)?;
        let mut best: Option = None;
        for path in self.itercompletepaths(ref_node) {   // forward path stitching
            if let Some(target) = self.pathtodefinition(&path) {
                let confidence = self.score_path(&path).clamp(0.90, 1.00);
                if best.asref().mapor(true, |b| confidence > b.confidence) {
                    best = Some(ResolvedRef { target, confidence,
                        method: ResolutionMethod::StackGraph,
                        kind: reference.kind.clone(), line: reference.line });
                }
            }
        }
        best
    }
}

4.3 Import Graph — O(1) Removal

StableDiGraph keeps NodeIndex values valid across removals, so the path→index map never needs rebuilding.
rust
// DESIGN PSEUDOCODE
use petgraph::stable_graph::{StableDiGraph, NodeIndex};

pub struct ImportGraph {
    graph: StableDiGraph,
    pathtonode: DashMap,  // stable across removals
    nodetopath: DashMap,  // reverse map for O(1) cleanup
    exports: DashMap>,
}

impl ImportGraph {
    /// O(1) removal. No index rebuild — StableDiGraph guarantees stability.
    pub fn removefile(&self, filepath: &str) {
        let key = SmolStr::new(file_path);
        if let Some((, idx)) = self.pathto_node.remove(&key) {
            self.graph.remove_node(idx);
            self.nodetopath.remove(&idx);
        }
        self.exports.remove(&key);
    }

    /// Depth-limited BFS over transitive imports. visited bounds work on
    /// cyclic import graphs (e.g. Python circular imports).
    pub fn transitiveimports(&self, filepath: &str, max_depth: usize) -> Vec {
        let Some(&start) = self.pathtonode.get(&SmolStr::new(file_path)) else { return vec![] };
        let mut visited: HashSet = HashSet::from([start]);
        let mut frontier: VecDeque = VecDeque::from([(start, 0)]);
        let mut result = Vec::new();
        while let Some((node, depth)) = frontier.pop_front() {
            if depth >= max_depth { continue; }
            for next in self.graph.neighbors_directed(node, Direction::Outgoing) {
                if visited.insert(next) {
                    result.push(self.graph[next].clone());
                    frontier.push_back((next, depth + 1));
                }
            }
        }
        result
    }
}

4.4 Call Graph — Cycle-Safe Traversals
rust
// DESIGN PSEUDOCODE
pub struct CallGraph {
    graph: StableDiGraph,
    pathtonode: DashMap,
}

impl CallGraph {
    /// Reverse BFS with explicit visited set + depth cap.
    /// Safe on recursive/mutually-recursive call graphs.
    pub fn findcallers(&self, targetid: &str, max_depth: usize) -> Vec {
        let Some(&target) = self.pathtonode.get(&SmolStr::new(target_id)) else { return vec![] };
        let mut visited: HashSet = HashSet::from([target]);
        let mut queue: VecDeque = VecDeque::from([(target, 0)]);
        let mut result = Vec::new();
        while let Some((node, depth)) = queue.pop_front() {
            if depth > 0 { result.push((self.graph[node].clone(), depth)); }
            if depth >= max_depth { continue; }
            for caller in self.graph.neighbors_directed(node, Direction::Incoming) {
                if visited.insert(caller) { queue.push_back((caller, depth + 1)); }
            }
        }
        result
    }

    /// Shortest call chain. BFS with visited set terminates on cyclic graphs;
    /// max_depth bounds the search horizon.
    pub fn findcallchain(&self, sourceid: &str, targetid: &str,
                           max_depth: usize) -> Option> {
        let source = *self.pathtonode.get(&SmolStr::new(source_id))?;
        let target = *self.pathtonode.get(&SmolStr::new(target_id))?;
        let mut visited: HashSet = HashSet::from([source]);
        let mut parent: HashMap> = HashMap::from([(source, None)]);
        let mut queue: VecDeque = VecDeque::from([(source, 0)]);
        while let Some((current, depth)) = queue.pop_front() {
            if current == target {
                let mut path = Vec::new();
                let mut cursor = Some(current);
                while let Some(n) = cursor { path.push(self.graph[n].clone()); cursor = parent[&n]; }
                path.reverse();
                return Some(path);
            }
            if depth >= max_depth { continue; }
            for next in self.graph.neighbors_directed(current, Direction::Outgoing) {
                if visited.insert(next) {
                    parent.insert(next, Some(current));
                    queue.push_back((next, depth + 1));
                }
            }
        }
        None
    }
}

4.5 Orchestrator — Sentinel Sources + Staged Commit API
rust
// DESIGN PSEUDOCODE
pub const TOPLEVEL: &str = "::toplevel";

impl SemanticEngine {
    /// Stage all in-memory graph mutations for a file WITHOUT applying them.
    /// Python commits or rolls back after its DB transaction resolves.
    pub fn stage_file(&mut self, parsed: &ParsedFile, source: &str, tree: &Tree) -> StagedChange {
        let mut staged = StagedChange::new(&parsed.path);
        staged.stackgraphdelta = self.stackgraph.difffile(&parsed.path, source, tree, &parsed.language);
        staged.importdelta       = self.importgraph.diffimports(&parsed.path, &parsed.imports, &self.extractexports(parsed));
        staged.definitiondelta   = self.diffdefinitions(parsed);
        staged.edges              = self.resolve_all(&parsed.path, parsed);
        staged.calldelta         = CallGraph::difffrom(&staged.edges);
        staged
    }

    pub fn commit_staged(&mut self, staged: StagedChange)   { self.apply(staged); }
    pub fn rollback_staged(&mut self, staged: StagedChange) { drop(staged); }

    /// Every edge gets a valid source. References outside any function attach to
    /// the file's synthetic {path}::toplevel node — no dangling edges.
    fn sourceidfor(reference: &ParsedReference, file_path: &str) -> String {
        reference.enclosing_function.clone()
            .unwraporelse(|| format!("{}{}", file_path, TOPLEVEL))
    }

    fn resolvereference(&self, filepath: &str, reference: &ParsedReference) -> Option {
        let sourceid = Self::sourceidfor(reference, filepath);
        // Layer 1 — Stack Graphs (0.90-1.00)
        if let Some(r) = self.stackgraph.resolvereference(file_path, reference) {
            return Some(ResolvedEdge { source_id, confidence: r.confidence.clamp(0.90, 1.00),
                method: ResolutionMethod::StackGraph, / ... / });
        }
        // Layer 2 — Import-constrained (0.80-0.89)
        let matches = self.importgraph.resolveinimports(filepath, &reference.name, 3);
        match matches.len() {
            1 => return Some(ResolvedEdge { source_id, confidence: 0.89,
                    method: ResolutionMethod::ImportConstrained, / ... / }),
            n if n > 1 => {
                let best = self.rankbyproximity(&matches, file_path);
                return Some(ResolvedEdge { source_id, confidence: 0.80,
                    method: ResolutionMethod::ImportConstrained, / ... / });
            }
            _ => {}
        }
        // Layer 3 — Signature matching (0.40-0.79)
        if let Some(m) = self.signature_matcher.resolve(&reference.name,
                reference.receiver.asderef(), filepath, &self.definitions) {
            if m[0].score > 0.5 {
                return Some(ResolvedEdge { source_id,
                    confidence: (0.40 + m[0].score * 0.39).clamp(0.40, 0.79),
                    method: ResolutionMethod::SignatureMatch, / ... / });
            }
        }
        None // falls through to Python Layer 4/5
    }
}

4.6 TSG Rules — Provenance

Production .tsg rule files are derived from the reference implementations in the stack-graphs project's per-language crates (e.g. stack-graphs-python, stack-graphs-typescript), vendored and pinned at build time, validated by the golden resolution tests in §15.2.

The excerpt below is illustrative pseudocode only — it conveys the AST→stack-graph mapping and is not valid TSG syntax:
text
// ILLUSTRATIVE ONLY — not valid TSG. See vendored reference rules.
(file)                          -> root scope node, exported
(class_definition name: ...)    -> definition node (class name) + new scope
(function_definition name: ...) -> definition node + new scope for body
(call function: (identifier))   -> reference node, resolved through enclosing scopes
(importfromstatement ...)     -> reference node bound to another file's exports
(call function: (attribute ...))-> reference node with receiver hint

4.7 Stack Graph Language Coverage

| Language | Coverage | Language | Coverage |
|----------|----------|----------|----------|
| Python | ~95% | C | ~85% |
| TypeScript | ~93% | C++ | ~83% |
| JavaScript | ~92% | Ruby | ~88% |
| Rust | ~90% | PHP | ~87% |
| Go | ~92% | C# | ~90% |
| Java | ~91% | Kotlin | ~88% |

Languages without vendored TSG rules fall through to Layers 2–3, which are language-agnostic (driven by tags.scm extraction).

Database Schema (LadybugDB)

5.1 Dialect Note & Configuration-Driven DDL

LadybugDB is the community-maintained successor of KùzuDB. It preserves the Kùzu storage format and Kùzu Cypher dialect — CREATE NODE TABLE / CREATE REL TABLE DDL, REL group semantics, and the HNSW vector extension. All DDL and queries in this document are Kùzu-dialect as maintained by LadybugDB.

The schema is generated from configuration to avoid hardcoded dimensions:
python
py_agent/src/config.py
from pydantic import BaseModel

class EmbeddingConfig(BaseModel):
    model: str = "jinaai/jina-code-embeddings-0.5b"
    dimension: int = 896
    truncated_dimension: int = 64   # fast pre-filtering (Matryoshka)
    maxbodytokens: int = 2000
    batch_size: int = 32

class DatabaseConfig(BaseModel):
    path: str = ".harness/semantic.db"
    hnswefconstruction: int = 128
    hnsw_m: int = 16
    hnswefsearch: int = 64

5.2 Node Tables

Entities carry byte spans (consumed by the MutationEngine, §11) in addition to line numbers.
cypher
// PHYSICAL STRUCTURE
CREATE NODE TABLE Module (
    id STRING PRIMARY KEY, name STRING, path STRING, language STRING,
    packagetype STRING, updatedat TIMESTAMP
);
CREATE NODE TABLE File (
    id STRING PRIMARY KEY, path STRING, language STRING, size_bytes INT64,
    linecount INT64, contenthash STRING, last_modified TIMESTAMP,
    gitblameauthor STRING, gitblamecommit STRING, updated_at TIMESTAMP
);

// CODE ENTITIES (Function shown; Method/Class/Variable follow the same pattern)
CREATE NODE TABLE Function (
    id STRING PRIMARY KEY, name STRING, qualified_name STRING, signature STRING,
    body STRING, docstring STRING,
    startline INT64, endline INT64,
    startbyte INT64, endbyte INT64,                 // whole definition span
    namestartbyte INT64, nameendbyte INT64,       // identifier only
    paramsstartbyte INT64, paramsendbyte INT64,   // "(...)" parameter list
    bodystartbyte INT64, bodyendbyte INT64,       // block, signature excluded
    isasync BOOLEAN, isgenerator BOOLEAN, isstatic BOOLEAN, isproperty BOOLEAN,
    is_toplevel BOOLEAN,                              // synthetic module-level sentinel
    visibility STRING, decorators STRING[], content_hash STRING,
    embedding FLOAT[896], embeddingshort FLOAT[64], updatedat TIMESTAMP
);
CREATE NODE TABLE Class    ( / as Function, minus params_, plus is_abstract, bases */ );
CREATE NODE TABLE Method   ( / as Function, plus parent_class, is_class_method, is_abstract / );
CREATE NODE TABLE Variable ( /* id, name, typeannotation, isglobal, is_constant,
                                isexported, startline, startbyte, endbyte,
                                namestartbyte, nameendbyte, content_hash */ );
CREATE NODE TABLE Parameter (
    id STRING PRIMARY KEY, name STRING, type_annotation STRING, position INT64,
    hasdefault BOOLEAN, defaultvalue STRING, isvariadic BOOLEAN, iskeyword_only BOOLEAN
);
CREATE NODE TABLE Import (
    id STRING PRIMARY KEY, modulepath STRING, symbolname STRING, alias STRING,
    iswildcard BOOLEAN, isrelative BOOLEAN, start_line INT64,
    namestartbyte INT64, nameendbyte INT64        // symbol slot, for rename rewriting
);

// METADATA
CREATE NODE TABLE ChangeEvent (
    id STRING PRIMARY KEY, entityid STRING, entitytype STRING, change_type STRING,
    timestamp TIMESTAMP, oldhash STRING, newhash STRING, trigger STRING
);
CREATE NODE TABLE IndexMetadata (
    id STRING PRIMARY KEY, schemaversion INT64, lastfull_index TIMESTAMP,
    lastincrementalindex TIMESTAMP, totalfiles INT64, totalentities INT64,
    embeddingmodel STRING, embeddingdimension INT64
);

// MUTATION AUDIT — retention is tiered; see §11.10
CREATE NODE TABLE MutationLog (
    id STRING PRIMARY KEY,                            // plan ULID
    tool STRING, entityid STRING, planjson STRING,  // nulled after summarize window
    affectedfiles STRING[], editcount INT64,
    status STRING,                                    // applied | rolledback | rejectedstale
    syntaxerrors STRING[], traceid STRING, timestamp TIMESTAMP
);

Span validity rule: all byte columns are trustworthy only while the on-disk file content hashes to the row's content_hash. The MutationEngine enforces this (§11.6 step 3); queries treat spans as display hints. Spans refresh on every reindex.

5.3 Relationship Tables
cypher
// STRUCTURAL (SYNTACTIC) — from Tree-sitter AST
CREATE REL TABLE CONTAINS_MODULE (FROM Module TO Module);
CREATE REL TABLE CONTAINS_FILE (FROM Module TO File);
CREATE REL TABLE DECLARES_CLASS (FROM File TO Class);
CREATE REL TABLE DECLARES_FUNC (FROM File TO Function);
CREATE REL TABLE DECLARES_VAR (FROM File TO Variable);
CREATE REL TABLE HAS_METHOD (FROM Class TO Method);
CREATE REL TABLE HAS_PARAM (FROM Function TO Parameter);
CREATE REL TABLE HAS_PARAM (FROM Method TO Parameter);
CREATE REL TABLE HAS_IMPORT (FROM File TO Import);
CREATE REL TABLE NESTED_IN (FROM Class TO Class);
CREATE REL TABLE NESTED_IN (FROM Function TO Function);

// SEMANTIC — resolved by the Rust semantic engine
CREATE REL TABLE CALLS (
    FROM Function TO Function,
    confidence FLOAT, resolution_method STRING,
    callsiteline INT64, isconditional BOOLEAN, resolvedat TIMESTAMP,
    callsitestartbyte INT64, callsiteendbyte INT64,   // mutation cascade targets
    argsstartbyte INT64, argsendbyte INT64
);
CREATE REL TABLE CALLS (FROM Method TO Method, confidence FLOAT, resolutionmethod STRING, callsiteline INT64, isconditional BOOLEAN, resolvedat TIMESTAMP, callsitestartbyte INT64, callsiteendbyte INT64, argsstartbyte INT64, argsend_byte INT64);
CREATE REL TABLE CALLS (FROM Function TO Method, confidence FLOAT, resolutionmethod STRING, callsiteline INT64, isconditional BOOLEAN, resolvedat TIMESTAMP, callsitestartbyte INT64, callsiteendbyte INT64, argsstartbyte INT64, argsend_byte INT64);
CREATE REL TABLE INSTANTIATES (FROM Function TO Class, confidence FLOAT, resolutionmethod STRING, siteline INT64);
CREATE REL TABLE IMPORTS (FROM File TO File, modulename STRING, symbols STRING[], isrelative BOOLEAN);
CREATE REL TABLE EXTENDS (FROM Class TO Class, confidence FLOAT);
CREATE REL TABLE IMPLEMENTS (FROM Class TO Class, confidence FLOAT);
CREATE REL TABLE OVERRIDES (FROM Method TO Method, confidence FLOAT);
CREATE REL TABLE READS_FROM (FROM Function TO Variable);
CREATE REL TABLE WRITES_TO (FROM Function TO Variable);
CREATE REL TABLE READS_FROM (FROM Method TO Variable);
CREATE REL TABLE WRITES_TO (FROM Method TO Variable);
CREATE REL TABLE RETURNS_TYPE (FROM Function TO Class);
CREATE REL TABLE RETURNS_TYPE (FROM Method TO Class);
CREATE REL TABLE HAS_TYPE (FROM Variable TO Class);
CREATE REL TABLE HAS_TYPE (FROM Parameter TO Class);
CREATE REL TABLE DECORATED_BY (FROM Function TO Function);
CREATE REL TABLE DECORATED_BY (FROM Class TO Function);
CREATE REL TABLE TRIGGERED_BY (FROM MutationLog TO File);

5.4 Vector Indexes
cypher
CALL createhnswindex('funcembeddingidx', 'Function', 'embedding',
    {dimension: 896, metric: 'cosine', ef_construction: 128, m: 16});
CALL createhnswindex('funcembeddingshortidx', 'Function', 'embeddingshort',
    {dimension: 64, metric: 'cosine', ef_construction: 64, m: 8});
CALL createhnswindex('methodembeddingidx', 'Method', 'embedding',
    {dimension: 896, metric: 'cosine', ef_construction: 128, m: 16});
CALL createhnswindex('classembeddingidx', 'Class', 'embedding',
    {dimension: 896, metric: 'cosine', ef_construction: 128, m: 16});

5.5 Schema Versioning & Migration Policy
python
SCHEMA_VERSION = 4

MIGRATIONS: dict[int, Migration] = {
    2: Migration(kind="additive",
        statements=["ALTER TABLE Function ADD is_toplevel BOOLEAN DEFAULT false"]),
    3: Migration(kind="destructive", reason="embedding dimension 768 -> 896",
        action="background_reindex"),
    4: Migration(kind="additive",
        statements=[ /* byte-span columns on entities; CALLS call-site spans;
                        MutationLog + TRIGGERED_BY */ ]),
}

Startup protocol: read IndexMetadata.schema_version; apply pending additive migrations in order; for destructive migrations, build semantic.db.next in the background, verify node/edge counts against the old DB, then atomically swap directories. The old DB is retained for one release cycle.

The Real-Time Ingestion Pipeline

6.1 Flow
text
Watcher -> Debouncer(500ms) -> BatchEvent{trace_id: ULID} -> EventClassifier
  -> IgnoreFilter -> Parser(Tree-sitter) -> DiffEngine(xxHash)
  -> SemanticEngine.stage_file()          [Rust: resolve, stage, apply NOTHING]
        |
        v  PyO3: StagedChange + ParsedFile + Vec
Python:
  -> Embedder (content-addressed dedup: unchanged xxHash -> skip embedding)
  -> Embedding fallback for Rust-unresolved refs (Layer 4)
  -> Optional LSP override via warm pool (Layer 5, §10)
  -> DB transaction (LadybugDB ACID)
       success -> engine.commit_staged(staged)
       failure -> engine.rollback_staged(staged); file re-queued

6.2 Staged Two-Phase Commit
python
py_agent/src/ingestion/pipeline.py  (design pseudocode)
def process_file(self, change: FileChange) -> None:
    source = Path(change.path).read_text()

    # Phase 1 — Rust stages everything, applies nothing
    staged = self.engine.stagefile(change.path, source, traceid=change.trace_id)

    try:
        to_embed = [e for e in staged.entities
                    if not self.embedder.isduplicate(e.id, e.contenthash, self.db)]
        embeddings = self.embedder.embedbatch([e.body for e in toembed])  # may raise

        fallback = self.linker.embedding_fallback(change.path, staged.unresolved, embeddings)
        if self.lsppool.isenabled(staged.language):
            self.lsppool.syncfile(change.path, source)          # didChange first
            staged.applylspoverrides(self.lsppool.overridebatch(staged.low_confidence))

        # Phase 2a — DB transaction
        self.db.sync_file(change.path, staged, embeddings, staged.edges + fallback)

        # Phase 2b — only now do the Rust in-memory graphs mutate
        self.engine.commit_staged(staged)
        self.query_cache.invalidate()

    except (EmbeddingError, DatabaseError) as exc:
        self.engine.rollback_staged(staged)      # Rust graphs stay consistent with DB
        self.metrics.inc("ingestion.rollback")
        self.scheduler.requeue(change, backoff=True)
        log.warning("ingestion_failed", path=change.path, error=str(exc),
                    traceid=change.traceid)

Idempotency guarantee: stage_file is a pure diff against current state, so re-processing a file after a rollback produces the same staged change. Retries are safe without dedup bookkeeping.

6.3 Embedding Backpressure

Each batch carries an embedding time budget (default 2 s); the pool reports elapsed time per sub-batch.
If a batch exceeds its budget it is split: embedded entities proceed to commit; the remainder re-queues as a lower-priority continuation batch.
Non-critical embeddings are deferrable: entities only reachable via Layer-3 edges (confidence 100 queued batches) is retained as the outer governor.

6.4 Performance Impact of Rust-Native Resolution

| Operation | Python-side (legacy) | Rust-native | Speedup |
|-----------|----------------------|-------------|---------|
| Resolve 100 references | ~45 ms | ~2 ms | ~22x |
| Import traversal (depth 3) | ~12 ms | ~0.3 ms | ~40x |
| Call chain (depth 5) | ~25 ms | ~0.5 ms | ~50x |
| PyO3 crossings per file | 5–8 | 1 | 5–8x fewer |

Concurrency Model

7.1 Single-Writer, Multiple-Reader

One ingestion worker holds the write lock; query threads read MVCC snapshots; embedding pool bounded at 2 workers with ONNX intra-op parallelism = 1; watcher on its own thread feeding a crossbeam-channel. The MutationEngine (§11) acquires the same writer lock during apply, so mutations and ingestion never interleave.

7.2 Writer Throughput Target

Target: 1,000 file changes ingested in  200 files commit in 200-file chunks, so readers are never blocked > 2 s and a mid-batch crash loses at most one chunk (re-queued via ChangeEvent reconciliation).
Bulk-load path: codegraph rebuild --full bypasses the watcher and streams chunks directly.

Git Integration (git.rs)

8.1 Branch-Switch Detection
rust
// DESIGN PSEUDOCODE — git2::Diff::foreach takes (file, binary, hunk, line) callbacks
pub fn checkheadchange(&mut self) -> Option> {
    let current = self.repo.head().ok()?.target()?.to_string();
    if self.lasthead.asderef() == Some(&current) { return None; }
    let old = self.last_head.replace(current.clone())?;

    let oldtree = self.repo.revparsesingle(&old)?.peeltotree().ok()?;
    let newtree = self.repo.revparsesingle(&current)?.peeltotree().ok()?;
    let diff = self.repo.difftreetotree(Some(&oldtree), Some(&new_tree), None).ok()?;

    let mut changed = Vec::new();
    diff.foreach(
        &mut |_, delta| {
            if let Some(p) = delta.new_file().path() {
                changed.push(p.tostringlossy().into_owned());
            }
            true
        },
        None, None, None,                 // binary, hunk, line callbacks
    ).ok()?;
    Some(changed)
}

8.2 .gitignore Integration

Via the ignore crate: .gitignore + .harnessignore + built-in defaults (node_modules/, target/, dist/, build/, pycache/, .git/, *.min.js, vendor/, .venv/, .harness/).

8.3 Blame
rust
// DESIGN PSEUDOCODE
pub fn blame_file(&self, path: &str) -> Option {
    let mut opts = git2::BlameOptions::new();
    opts.newest_commit(true);                   // most recent commit only
    let blame = self.repo.blame_file(Path::new(path), Some(&mut opts)).ok()?;
    let hunk = blame.get_index(0)?;
    let sig = hunk.final_signature();
    Some(BlameInfo {
        author: sig.name().to_string(),
        commit: hunk.finalcommitid().to_string(),
        timestamp: sig.when().seconds(),
    })
}

Blame refreshes lazily (at most once per file per HEAD change) and populates File.gitblameauthor / File.gitblamecommit.

GraphRAG Query Execution

9.1 Query Planner

Classifies natural-language intent (scopeexploration, impactanalysis, callchain, similaritysearch, dependencygraph, definitionlookup), extracts parameters, selects a parameterized Cypher template.

9.2 Query Template Library

All relationship-property predicates live in WHERE clauses (Kùzu pattern maps support equality only). All templates are exercised by snapshot tests (§15.4).
cypher
-- Scope Exploration with pre-filtered vector search
MATCH (root:File {path: $root_path})
OPTIONAL MATCH (root)-[:IMPORTS*1..2]->(dep:File)
WITH collect(DISTINCT dep) + [root] AS scope_files
UNWIND scope_files AS sf
MATCH (sf)-[:DECLARES_FUNC]->(target:Function)
WITH collect(DISTINCT target.id) AS candidate_ids
CALL dbsimilaritysearch('funcembeddingidx', $queryembedding, $topk,
                          {filter: {id: candidate_ids}})
YIELD node AS matched, score
OPTIONAL MATCH (matched)(callee:Function)
WHERE rel.confidence > 0.7
RETURN matched.name, matched.signature, matched.body, matched.docstring,
       parent.path AS file_path, score, collect(DISTINCT callee.name) AS calls
ORDER BY score DESC
LIMIT $top_k;
-- Methods are queried analogously against methodembeddingidx and merged
-- in the executor (two searches, one ranked merge).

-- Impact Analysis (reverse dependency)
MATCH (target:Function {id: $target_id})
MATCH (caller:Function)-[:CALLS*1..$depth]->(target)
WITH DISTINCT caller
MATCH (caller)(other:Function)
RETURN caller.name, caller.signature, caller.body, parent_file.path,
       collect(DISTINCT other.name) AS also_calls
ORDER BY parent_file.path;

-- Call Chain
MATCH path = (src:Function {name: $sourcename})-[:CALLS*1..$maxdepth]->(tgt:Function {name: $target_name})
WITH path, length(path) AS depth
ORDER BY depth
LIMIT 5
UNWIND nodes(path) AS node
WITH collect(node.name) AS chain, depth
RETURN chain, depth;

-- Global Similarity Search
CALL dbsimilaritysearch('funcembeddingidx', $queryembedding, $topk)
YIELD node AS matched, score
OPTIONAL MATCH (matched)(callees:Function)
RETURN matched.name, matched.signature, matched.body, matched.docstring,
       parent.path, score, collect(DISTINCT callees.name) AS calls
ORDER BY score DESC;

-- Dependency Graph
MATCH (root:File {path: $root_path})-[imp:IMPORTS*1..$depth]->(dep:File)
OPTIONAL MATCH (dep)-[:DECLARES_CLASS]->(cls:Class)
OPTIONAL MATCH (dep)-[:DECLARES_FUNC]->(func:Function)
RETURN dep.path, dep.language,
       collect(DISTINCT cls.name) AS classes,
       collect(DISTINCT func.name) AS functions,
       length(imp) AS depth
ORDER BY depth, dep.path;

-- Definition Lookup
MATCH (func:Function)
WHERE func.name = $name OR func.qualified_name = $name
OPTIONAL MATCH (func)(param:Parameter)
OPTIONAL MATCH (func)-[:CALLS]->(callees:Function)
OPTIONAL MATCH (callers:Function)-[:CALLS]->(func)
RETURN func.name, func.signature, func.body, func.docstring, parent.path,
       collect(DISTINCT param.name) AS parameters,
       collect(DISTINCT callees.name) AS calls,
       collect(DISTINCT callers.name) AS called_by;

9.3 Cache, Context Builder, Rust-Accelerated Traversal

Query cache: LRU keyed on query + parameters + graph version; invalidated on every write.
Context builder: grep-ast structural compression (signatures_only / structural / full strategies), token-budgeted.
Rust-accelerated traversal: callchain and impactanalysis intents are served from the in-memory CallGraph (findcallchain, findimpactset), then enriched with code bodies from LadybugDB.

LSP — Optional Persistent Warm Pool

Disabled by default. When enabled, LSP servers run as persistent, warm processes — never spawned per request.
python
py_agent/src/lsp/pool.py  (design pseudocode)
class LSPPool:
    """
    One long-lived server process per enabled language, shared across all files.
    - Spawned once on first use; initialized with the workspace root.
    - Kept synchronized via textDocument/didOpen + didChange on every ingestion.
    - Idle servers shut down after idletimeouts (default 600) and re-spawn
      lazily — but never per-reference.
    - Definition lookups hit a TTL cache keyed on (path, line, col, content_hash).
    """
    def ensureserver(self, language: str, workspaceroot: str) -> ManagedServer: ...

    def sync_file(self, path: str, text: str) -> None:
        """Called by ingestion BEFORE any LSP query for this file."""
        lang = detect_language(path)
        if not self.is_enabled(lang): return
        server = self.ensureserver(lang, workspaceroot_of(path))
        if server.is_open(path):
            server.didchange(path, text, version=server.bumpversion(path))
        else:
            server.did_open(path, text, lang, version=1)
        self.cache.invalidate_prefix(path)      # stale results die here

    def definition(self, path, line, col, content_hash):
        key = (path, line, col, content_hash)
        if (hit := self.cache.get(key)) is not None: return hit
        server = self.ensureserver(detectlanguage(path), workspacerootof(path))
        result = server.request("textDocument/definition",
                                position_params(path, line, col),
                                timeouts=self.config.timeoutms / 1000)
        self.cache.put(key, result)
        return result

    def overridebatch(self, lowconfidence_edges):
        """Only consulted for edges the Rust engine resolved below 0.90."""
        for edge in lowconfidenceedges:
            lsp = self.definition(edge.file, edge.line, edge.column, edge.content_hash)
            if lsp and self.mapstoknown_entity(lsp):
                yield LSPOverride(edge, target=lsp, confidence=1.0)

| Naive-spawn concern | Mitigation |
|---------------------|------------|
| 1–10 s cold-start per query | Servers spawn once per language per workspace; steady-state latency is single-digit ms. |
| Stale results without didChange | Ingestion pushes didOpen/didChange before any query; per-file cache prefix invalidated on every change. |
| Process leak | Idle timeout (600 s) + daemon-shutdown hook. |
| Cost when unused | Pool stays empty until first LSP-eligible edge; default config keeps it off. |

AST-Aware Mutation Engine

The engine extends CodeGraph from a read-only index to a read-write refactoring engine. The LLM decides what to change semantically; the Rust core computes where and how at the byte level. No regex, no search-and-replace, no 50-file context windows.

It reuses three existing mechanisms rather than inventing parallel ones:

| Mechanism | Reused for |
|-----------|------------|
| Staged two-phase commit (§6.2) | Mutation apply/rollback semantics |
| Content-addressed xxHash (§6) | Optimistic concurrency control — every edit carries the expected_hash of the content it was planned against |
| Single-writer lock (§7.1) | Mutations hold the writer lock during apply; WriteGuard suppresses watcher self-triggers |

11.1 Byte-Accurate Entity Model

Tree-sitter tracks byte offsets natively — immune to line-ending (\n vs \r\n) variation. Four spans per entity let mutations target name, signature, or body independently:
rust
// DESIGN PSEUDOCODE — core_indexer/src/types.rs
#[pyclass] #[derive(Clone, Copy)]
pub struct ByteSpan {
    #[pyo3(get)] pub start: usize,
    #[pyo3(get)] pub end: usize,          // exclusive
}

#[pyclass] #[derive(Clone)]
pub struct ParsedFunction {
    // ... all ingestion fields (name, qualified_name, signature, body,
    //     docstring, startline, endline, content_hash, ...) ...
    #[pyo3(get)] pub span: ByteSpan,                  // entire definition
    #[pyo3(get)] pub name_span: ByteSpan,             // identifier bytes only
    #[pyo3(get)] pub params_span: ByteSpan,           // "(...)" parameter list
    #[pyo3(get)] pub body_span: ByteSpan,             // block, signature excluded
    #[pyo3(get)] pub decorators_span: Option,
}
// ParsedMethod, ParsedClass, ParsedVariable gain the same span fields.

#[pyclass] #[derive(Clone)]
pub struct ParsedReference {
    // ... existing fields (name, kind, line, column, enclosing_function, receiver) ...
    #[pyo3(get)] pub name_span: ByteSpan,             // identifier bytes at use site
    #[pyo3(get)] pub args_span: Option,     // "(...)" of a call site
}

Raw &source[..start] panics on non-char boundaries (a real risk with multi-byte identifiers and comments). All span dereferences go through one helper:
rust
// DESIGN PSEUDOCODE — core_indexer/src/mutation/spans.rs
pub fn slice_span(source: &str, span: ByteSpan) -> Result {
    if span.start > span.end || span.end > source.len() {
        return Err(MutationError::SpanOutOfBounds(span));
    }
    if !source.ischarboundary(span.start) || !source.ischarboundary(span.end) {
        return Err(MutationError::NonCharBoundary(span));
    }
    Ok(&source[span.start..span.end])
}

11.2 Rope-Based Multi-Edit Application

Multiple edits per file apply in descending offset order so earlier offsets remain valid:
rust
// DESIGN PSEUDOCODE — core_indexer/src/mutation/edit.rs
use ropey::Rope;

#[pyclass] #[derive(Clone)]
pub struct MutationEdit {
    #[pyo3(get)] pub file: String,
    #[pyo3(get)] pub span: ByteSpan,
    #[pyo3(get)] pub replacement: String,
    #[pyo3(get)] pub expected_hash: String,   // content hash the plan was computed against
}

pub fn applyeditsto_file(source: &str, edits: &[MutationEdit]) -> Result {
    let mut rope = Rope::from_str(source);
    let mut ordered: Vec = edits.iter().collect();
    ordered.sort_by(|a, b| b.span.start.cmp(&a.span.start));   // descending
    for edit in ordered {
        let (s, e) = ropeclampedchar_bounds(&rope, edit.span)?;
        rope.remove(s..e);
        rope.insert(s, &edit.replacement);
    }
    Ok(rope.to_string())
}

11.3 Indent Normalization (Patch 3.2)

LLMs routinely paste code at the wrong indentation level (most commonly column 0). Re-parsing such a body fails deterministically — a recoverable error that nonetheless consumes one of the maxrepairattempts slots and costs an LLM round-trip. The engine therefore normalizes indentation before rope application (§11.2) and parse verification (§11.6 step 6), so repair attempts are reserved for genuinely semantic errors.
rust
// DESIGN PSEUDOCODE — core_indexer/src/mutation/indent.rs
pub struct IndentStyle { pub unit: char / ' ' or '\t' /, pub width: usize }

/// Detect the file's dominant convention: tabs if tab-indented lines outnumber
/// space-indented ones, else spaces with the most common leading-run width.
pub fn detectindentstyle(source: &str) -> IndentStyle;

pub fn normalizeindent(newcode: &str, target: &str, style: IndentStyle,
                        verbatim_spans: &[ByteSpan]) -> String {
    // 1. incoming_base = minimum leading whitespace across non-empty lines
    // 2. Per line: target + (line with incoming_base stripped), preserving
    //    relative depth exactly
    // 3. Convert leading whitespace to style (tabs  spaces)
    // 4. Lines inside verbatim_spans (multi-line string literal interiors,
    //    detected by the standalone parse of new_code) are preserved verbatim —
    //    re-indenting them would change runtime string content
    // 5. Empty lines stay empty (no trailing whitespace introduced)
}

Application points:

replaceentitybody: target = leading whitespace of the first line of body_span (the entity's own level).
create_entity: target = leading whitespace of the anchor entity's first line (sibling level), or the file's base level for position = "top".

Normalization is total and side-effect-free: worst case it is the identity (code already at the correct level). It never fails and never consumes a repair attempt; if the normalized code still does not parse, the error is genuinely semantic and enters the §11.6 repair loop as usual.

11.4 MutationEngine API
rust
// DESIGN PSEUDOCODE — core_indexer/src/mutation/mod.rs
pub struct MutationEngine {
    parser: CodeParser,
    semantic: SemanticEngineHandle,   // read access to call/import/stack graphs
    write_guard: WriteGuard,
    writer_lock: Arc>,      // the §7.1 single-writer lock
    backup_dir: PathBuf,
}

impl MutationEngine {
    // ---- planning (pure, side-effect-free) ----
    pub fn planbodyreplacement(&self, entityid: &str, newbody: &str,
                                 expectedhash: Option, dryrun: bool) -> Result;
    pub fn plansignatureupdate(&self, entityid: &str, newsignature: &str,
                                 callsitevalues: &HashMap,
                                 injectdefaults: bool, dryrun: bool) -> Result;
    pub fn planrename(&self, entityid: &str, new_name: &str,
                       includestrings: bool, dryrun: bool) -> Result;
    pub fn plancreateentity(&self, target_file: &str, anchor: &str,
                              code: &str, dry_run: bool) -> Result;

    // ---- application (atomic, verified; §11.6 pipeline) ----
    pub fn apply(&mut self, plan: &MutationPlan) -> MutationResult;
}

#[pyclass]
pub struct MutationPlan {
    #[pyo3(get)] pub id: String,                    // ULID
    #[pyo3(get)] pub tool: String,
    #[pyo3(get)] pub edits: Vec,
    #[pyo3(get)] pub affected_files: Vec,
    #[pyo3(get)] pub diff_preview: String,          // unified diff (similar crate)
    #[pyo3(get)] pub unverified_sites: Vec,
    #[pyo3(get)] pub warnings: Vec,
}

#[pyclass] #[derive(Clone)]
pub struct UnverifiedSite {
    #[pyo3(get)] pub file: String,
    #[pyo3(get)] pub line: u32,
    #[pyo3(get)] pub snippet: String,
    #[pyo3(get)] pub reason: String,   // "low-confidence edge (0.62)", "dynamic dispatch",
                                       // "inside macro body", "string literal"
}

#[pyclass]
pub struct MutationResult {
    #[pyo3(get)] pub status: MutationStatus,     // Applied | RolledBack | RejectedStale
    #[pyo3(get)] pub files_written: Vec,
    #[pyo3(get)] pub syntax_errors: Vec,
    #[pyo3(get)] pub reindex: ReindexSummary,
    #[pyo3(get)] pub backup_path: Option,
}

#[pyclass] #[derive(Clone)]
pub struct SyntaxDiagnostic {
    #[pyo3(get)] pub file: String,
    #[pyo3(get)] pub line: u32,
    #[pyo3(get)] pub column: u32,
    #[pyo3(get)] pub message: String,
    #[pyo3(get)] pub offending_span: ByteSpan,
}

11.5 WriteGuard — Watcher Self-Write Suppression

When the engine writes mutated files, the watcher would otherwise race the mutation path's own synchronous reindex:
rust
// DESIGN PSEUDOCODE — coreindexer/src/mutation/writeguard.rs
pub struct WriteGuard {
    suppressed: DashMap,  // path -> (expected hash, expiry)
}

impl WriteGuard {
    pub fn suppress(&self, path: &str, postwritehash: String) { / insert, 5s TTL / }

    /// watcher.rs calls this for every debounced event:
    pub fn shoulddrop(&self, path: &str, ondisk_hash: &str) -> bool {
        match self.suppressed.get(path) {
            Some((expected, expiry)) if *expiry > Instant::now() && expected == ondiskhash => true,
            _ => false,
        }
    }
}

The TTL is a safety net: if the synchronous reindex crashes, the entry expires and the watcher re-captures the file normally. No event is ever lost.

11.6 The Mutation Apply Pipeline
text
LLM tool call (dry_run=false)
  │
  ├─ 1. Policy check (§11.9): allow/deny globs, file & edit budgets, git cleanliness
  ├─ 2. Acquire single-writer lock          ← same lock as the ingestion worker
  ├─ 3. Hash guard: every affected file's on-disk xxHash == edit.expected_hash
  │       mismatch → RejectedStale (nothing written; LLM re-queries fresh context)
  ├─ 4. Snapshot originals → .harness/backups/{plan_id}/   (zstd, 24h retention)
  ├─ 5. Per file: indent normalization (§11.3) → Rope apply (descending offsets)
  │       → candidate content
  ├─ 6. VERIFY: re-parse every candidate with Tree-sitter
  │       NEW ERROR nodes → full rollback from snapshot, return SyntaxDiagnostic[]
  │       (Python feeds these verbatim to the LLM; maxrepairattempts = 3)
  │       Note: indentation failures are normalized upfront (§11.3) and never
  │       reach this step, so they never consume a repair attempt.
  ├─ 7. Register paths in WriteGuard (expected post-write hashes)
  ├─ 8. Atomic write: temp file in same directory + rename() per file
  ├─ 9. Synchronous reindex of touched files through the §6 pipeline:
  │       stagefile → DB transaction → commitstaged   (watcher suppressed)
  ├─ 10. Release writer lock; MutationLog entry; metrics
  └─ 11. Return MutationResult (reindex summary included)

Failure taxonomy: step 3 → rejectedstale (LLM must re-read); step 6 → rolledback with diagnostics (LLM repairs); steps 7–9 crash → next startup reconciles (files whose hashes match no known state are reindexed by the watcher after guard TTL expiry; the pending MutationLog entry is marked rolled_back by recovery).

Verification scope: step 6 checks for new ERROR nodes relative to the pre-mutation parse — a file that was already partially broken is not rejected unless the mutation made it worse.

11.7 The Signature Cascade

The flagship operation: one call rewrites the definition and every verified call site; the LLM never sees the 50 files.

Preflight parse. newsignature is parsed standalone with the file's grammar. Parse failure → plan rejected with a SyntaxDiagnostic. If the embedded name differs from the current name → rejected with guidance to use renamesymbol.
Parameter diff. Old vs new parameter lists → added[], removed[], renamed[], reordered[], retyped[].
Definition edit. One edit replacing params_span.
Call-site enumeration. callgraph.findcallers(entityid, depth=1) ∪ stack-graph references of kind Call, filtered to edges with stored argsspan and confidence ≥ 0.8.
Per-site rewrite rules (each site parsed with Tree-sitter; argument spans located in the AST, never by text search):

   | Parameter change | Positional call site | Keyword call site |
   |------------------|----------------------|-------------------|
   | Added, has default, inject_defaults=false | skip | skip |
   | Added, has default, inject_defaults=true | insert expr at index | append name=expr |
   | Added, required | insert callsitevalues[name] at index | append name= |
   | Removed | delete argument at old index | delete by keyword name |
   | Renamed | untouched (positional semantics unchanged) | rewrite keyword old= → new= |
   | Reordered | rewrite argument order to new positions | untouched |

Preflight completeness check. If any required parameter lacks a callsitevalues entry → plan rejected with the full list of affected call sites, so the LLM can supply expressions with full knowledge.
Unverified sites. Call sites with confidence  expression to insert at every call site, e.g. {\"timeout\": \"30\"}."
      },
      "inject_defaults": { "type": "boolean", "default": false },
      "expected_hash":   { "type": "string" },
      "dry_run":         { "type": "boolean", "default": true }
    },
    "required": ["entityid", "newsignature"]
  }
}

rename_symbol
json
{
  "name": "rename_symbol",
  "description": "Renames a function, method, class, or variable and rewrites the definition, all resolved references, and all import statements. Shadowed same-name symbols in unrelated scopes are untouched (Stack Graphs scope resolution). String-literal occurrences are only rewritten when include_strings=true.",
  "parameters": {
    "type": "object",
    "properties": {
      "entity_id":       { "type": "string" },
      "newname":        { "type": "string", "pattern": "^[A-Za-z][A-Za-z0-9_]*$" },
      "include_strings": { "type": "boolean", "default": false },
      "expected_hash":   { "type": "string" },
      "dry_run":         { "type": "boolean", "default": true }
    },
    "required": ["entityid", "newname"]
  }
}

Execution: definition namespan; every Stack-Graph-resolved (Layer 1) and import-constrained (Layer 2) reference edits its namespan — scope-aware by construction, so a shadowing local in another module is untouched; import statements rewrite the symbol slot (from auth import login → login_v2), preserving aliases; qualified usages (auth.login(...)) rewrite the attribute identifier node only.

create_entity
json
{
  "name": "create_entity",
  "description": "Creates a new function or class in a target file, anchored after an existing entity or at file top/end. The code must parse in the file's language; indentation is normalized to the anchor's level; imports it needs are NOT added automatically — inspect the plan's warnings for missing symbols.",
  "parameters": {
    "type": "object",
    "properties": {
      "target_file":      { "type": "string" },
      "anchorentityid": { "type": "string", "description": "Insert immediately after this entity. Omit and use position instead." },
      "position":         { "type": "string", "enum": ["top", "end"] },
      "code":             { "type": "string" },
      "expected_hash":    { "type": "string" },
      "dry_run":          { "type": "boolean", "default": true }
    },
    "required": ["target_file", "code"]
  }
}

The insertion point derives from the anchor's span.end (plus blank-line normalization). New code is parse-checked in context (synthetic file = target + insertion) before the plan is issued and indented to the anchor's sibling level (§11.3); unresolved symbols it references are listed in warnings.

Shared Result Schemas
json
// MutationPlan (dry_run=true, or returned from apply on rejection)
{
  "plan_id": "01J9ZK4T7Q3M5R8W2X6V",
  "tool": "update_signature",
  "affected_files": ["src/auth.py", "src/api/users.py", "src/api/sessions.py"],
  "edit_count": 53,
  "diffpreview": "--- src/auth.py\n+++ src/auth.py\n@@ -41,7 +41,7 @@\n-def validateuser(userid: str):\n+def validateuser(user_id: str, timeout: int = 30):\n...",
  "unverified_sites": [
    { "file": "src/legacy.py", "line": 88, "snippet": "getattr(mod, 'validate_user')(...)",
      "reason": "dynamic dispatch (confidence 0.62)" }
  ],
  "warnings": []
}

// MutationResult (dry_run=false)
{
  "status": "applied",                 // applied | rolledback | rejectedstale
  "files_written": ["src/auth.py", "src/api/users.py", "src/api/sessions.py"],
  "syntaxerrors": [],                 // populated when status=rolledback
  "reindex": { "files": 3, "entitiesupdated": 12, "edgesupdated": 53, "duration_ms": 210 },
  "backup_path": ".harness/backups/01J9ZK4T7Q3M5R8W2X6V/"
}

Routing (pyagent/src/mutation/toolrouter.py)
python
DESIGN PSEUDOCODE
TOOLSCHEMAS = [REPLACEENTITYBODY, UPDATESIGNATURE, RENAMESYMBOL, CREATEENTITY]

class MutationToolRouter:
    def handle(self, tool_call) -> dict:
        name, args = toolcall.function.name, json.loads(toolcall.function.arguments)
        self.policy.check(name, args)                       # raises PolicyViolation
        planner = {
            "replaceentitybody": self.engine.planbodyreplacement,
            "updatesignature":    self.engine.plansignature_update,
            "renamesymbol":       self.engine.planrename,
            "createentity":       self.engine.plancreate_entity,
        }[name]
        plan = planner(args)
        if args.get("dry_run", True):
            return plan.to_dict()
        result = self.engine.apply(plan)                    # atomic, verified
        self.db.log_mutation(plan, result)                  # MutationLog node
        return result.to_dict()

litellm wiring
response = litellm.completion(model=cfg.model, messages=messages,
                              tools=[{"type": "function", "function": s} for s in TOOL_SCHEMAS],
                              tool_choice="auto")

11.9 Safety & Mutation Policy
toml
.harness/config.toml — [mutation]
[mutation]
enabled = true
defaultdryrun = true             # dry_run omitted in a tool call -> true
maxfilesper_plan = 100
maxeditsper_plan = 500
maxbodytokens = 4000             # reject absurdly large newbodycode
backup_dir = ".harness/backups"
backupretentionhours = 24
post_verify = true                 # §11.6 step 6
maxrepairattempts = 3            # per logical mutation in one agent turn
requirecleangit = false          # if true, reject when worktree is dirty
allow = ["src/", "lib/", "tests/", "scripts/"]
deny  = [".git/", ".harness/", "/migrations/", "/*.lock",
         "/package-lock.json", "/generated/"]
Audit retention (patch 3.1; see §11.10)
auditretentiondays = 30
auditmaxentries = 10000
auditsummarizeafter_days = 7

Hard guarantees, independent of config:

Never commits. Mutations only touch the git working tree; the user reviews via git diff and commits themselves.
Never touches deny-listed paths, even if the LLM names them — policy rejection is returned as a tool error the LLM can read.
Stale contexts always rejected (expected_hash), so concurrent human edits are never silently overwritten.
All-or-nothing across files — a 50-file rename cannot half-apply.
Every mutation is auditable — MutationLog + on-disk backup + trace_id correlation.

11.10 MutationLog Retention & Pruning (Patch 3.1)

MutationLog is append-only and grows with every tool call; the plan_json field (which embeds the full unified diff) dominates row size. Left unbounded it is the only steadily-growing table in the schema. Retention is tiered:

| Age | Retained |
|-----|----------|
| 0 – auditsummarizeafterdays (7) | Full row including planjson with diff |
| 7 – auditretentiondays (30) | Summary only: tool, entityid, affectedfiles, editcount, status, syntaxerrors, traceid, timestamp; planjson nulled |
| > 30 days | Pruned |

The auditmaxentries (10,000) cap applies at all times, evicting oldest first regardless of age. Pruning runs at daemon startup and every 24 h, alongside — but independent of — backup expiry: backups serve rollback (backupretentionhours), MutationLog serves audit. Pruning is an additive maintenance operation (no schema migration); deletes run as chunked transactions (500 rows) to respect the 2 s reader blackout (§7.2). The audit-critical fields (what, where, status, trace_id) survive the full retention window even after the diff payload is dropped.

11.11 Closing the Loop — Agent Workflow
text
User: "Update the database connection to use a connection pool."

RETRIEVE   GraphRAG scope_exploration -> db.py::connect body + callers
              (results carry content_hash + byte spans)
PLAN       LLM -> replaceentitybody(dryrun=true, expectedhash=...)
               same call with dry_run=false
APPLY      Rust: hash guard OK -> indent normalize -> rope edit -> re-parse OK
              -> atomic write
VERIFY     parse clean -> synchronous reindex (210 ms) -> graph fresh
   4'. REPAIR (if step 4 found ERROR nodes): rollback + SyntaxDiagnostic[]
              -> LLM fixes newbodycode -> retry (attempt 2/3)
              (indentation errors never reach this loop — normalized in §11.3)
CONFIRM    LLM -> definition_lookup("db.py::connect") reads back the new code;
              impact_analysis shows the caller set is unchanged
AUDIT      MutationLog row; user sees a plain git diff

The syntax-error feedback loop (step 4') is automatic: SyntaxDiagnostic objects are serialized directly into the next LLM message as tool results — file, line, column, offending span — so repair iterations are grounded in the parser's actual complaints, not the LLM's guess.

11.12 Known Limits

| Case | Behavior |
|------|----------|
| Macro bodies (Rust macrorules!, C preprocessor) | Call sites inside macro bodies reported in unverifiedsites, never auto-edited |
| Dynamic dispatch (getattr, JS bracket access, reflection) | Reported in unverified_sites with reason |
| Symbol names inside strings/templates | Untouched unless includestrings=true (renamesymbol), then flagged per site |
| Cross-repository references | Out of scope until v3.3 multi-repo (roadmap §19) |
| Generated files | Deny-listed by default via /generated/ and exclude patterns |

Configuration (.harness/config.toml)
toml
[general]
watch_paths = []
exclude_patterns = [".generated.", ".pb.go", ".g.dart"]
debounce_ms = 500
maxfilesizebytes = 1048_576
log_level = "info"

[embedding]
model = "jinaai/jina-code-embeddings-0.5b"
dimension = 896
truncated_dimension = 64
maxbodytokens = 2000
batch_size = 32
workers = 2

[database]
path = ".harness/semantic.db"
hnswefconstruction = 128
hnsw_m = 16
hnswefsearch = 64

[resolution]
min_confidence = 0.3
[resolution.stack_graph]
rules_dir = ""            # empty = use bundled rules
maxpathdepth = 10
incremental = true
[resolution.import_graph]
maximportdepth = 3
includesamepackage = true
[resolution.signature]
min_score = 0.5
name_weight = 0.4
arity_weight = 0.3
proximity_weight = 0.3
[resolution.lsp]
enabled = false           # opt-in
resultttls = 300
idletimeouts = 600
timeout_ms = 5000
override_threshold = 0.90
[resolution.lsp.servers]
python = "pyright-langserver --stdio"
typescript = "typescript-language-server --stdio"
rust = "rust-analyzer"
go = "gopls"

[ingestion]
batchchunksize = 200          # sub-transaction size
embeddingbudgetms = 2000      # per-batch time budget before splitting
deferlowpriority_below = 0.6

[memory]                        # per-component budgets (LRU + spill)
stackgraphmb = 60
callgraphmb = 40
resolutioncachemb = 20
spill_compression = "zstd"

[mutation]
enabled = true
defaultdryrun = true
maxfilesper_plan = 100
maxeditsper_plan = 500
maxbodytokens = 4000
backup_dir = ".harness/backups"
backupretentionhours = 24
post_verify = true
maxrepairattempts = 3
requirecleangit = false
allow = ["src/", "lib/", "tests/", "scripts/"]
deny  = [".git/", ".harness/", "/migrations/", "/*.lock",
         "/package-lock.json", "/generated/"]
auditretentiondays = 30        # MutationLog pruning horizon
auditmaxentries = 10000        # hard cap, oldest first
auditsummarizeafterdays = 7   # drop planjson diff payload, keep summary

[query]
max_depth = 5
defaulttopk = 10
cachettlseconds = 300
cachemaxsize = 256
userustgraphfortraversal = true

[llm]
provider = "openai"
model = "gpt-4o"
maxcontexttokens = 8192
temperature = 0.1
apikeyenv = "OPENAIAPIKEY"

[git]
enabled = true
reindexonbranch_switch = true

Multi-Language Support

13.1 Language Tiers

| Tier | Languages | Parsing | Semantic Resolution | Coverage |
|------|-----------|---------|---------------------|----------|
| Tier 1 (Full Stack Graphs) | Python, TypeScript, JavaScript, Rust, Go, Java, C, C++, Ruby, PHP, C#, Kotlin | Tree-sitter + tags.scm + Stack Graphs (.tsg) | Stack Graphs → Import Graph → Signature | ~93% resolved natively in Rust |
| Tier 2 (Import + Signature) | Swift, Scala, Lua, Elixir, Erlang, Haskell, OCaml, Zig, Nim, Dart, R, Julia, Perl | Tree-sitter + tags.scm | Import Graph → Signature Match | ~70% |
| Tier 3 (Structural) | Shell, SQL, HTML, CSS, YAML, TOML, JSON, Markdown, + 280 more | Tree-sitter (AST only) | Signature Match (language-agnostic) | ~40% |
| Tier 4 (Optional LSP) | Any Tier 1/2 language | + LSP server | LSP override for edge cases | ~98% (when LSP enabled) |

Mutation support follows resolution tiers: Tier 1 languages get the full tool suite including signature cascade; Tier 2/3 get replaceentitybody and createentity (span-based, language-agnostic) with renamesymbol/update_signature limited to same-file and import-level rewrites.

13.2 Per-Language Configuration
toml
.harness/languages/python.toml
[languages.python]
extensions = [".py", ".pyi"]
parser = "tree-sitter-python"
tags_query = "tags.scm"
lsp_command = "pyright-langserver --stdio"
import_patterns = ["from {module} import {symbol}", "import {module}", "import {module} as {alias}"]
class_patterns = ["class {name}({bases}):"]
function_patterns = ["def {name}({params}):", "async def {name}({params}):"]
methodselfparam = "self"

13.3 Adding a New Stack Graph Language

Write a .tsg rule file defining scope/definition/reference mappings.
Place it in core_indexer/src/semantic/rules/{language}.tsg.
Add test fixtures in tests/fixtures/{language}_project/.
Add golden resolution tests in tests/rust/stackgraphtest.rs.
Rebuild the Rust extension.

No Python changes required. The SemanticEngine auto-discovers .tsg files at startup.

Observability & Diagnostics

14.1 Structured Logging

All components emit structured JSON via tracing (Rust) and structlog (Python):
json
{
  "timestamp": "2026-07-25T14:32:01.123Z",
  "level": "info",
  "component": "ingestion.pipeline",
  "event": "batch_processed",
  "filescount": 3, "entitiescreated": 12, "entities_modified": 5,
  "embeddingsgenerated": 8, "embeddingscached": 9, "edges_created": 23,
  "durationms": 342, "trigger": "filesave", "trace_id": "01J9ZK3M..."
}

14.2 Metrics (.harness/metrics.json)

Ingestion: fileswatched, node/edge totals, embeddingcachehitrate, avgingestionlatencyms, parseerrorslasthour, unresolvedreferencesrate, ingestion.rollback.
Mutation: mutationstotal{tool,status}, mutationeditstotal, mutationrollbackrate, mutationstalerejectionrate, unverifiedsitestotal, repairattemptstotal, mutationlogrows.
System: memoryrssmb, dbsizemb, per-component residency vs budget.

14.3 Health Check & CLI

The daemon listens on .harness/codegraph.sock (ping, stats). The CLI provides codegraph status, codegraph query "...", codegraph rebuild --full, codegraph diagnose --unresolved, codegraph mutations --last 20 (audit trail from MutationLog).

14.4 Cross-Boundary Trace Correlation
text
Watcher (Rust)            PyO3 boundary              Python
  traceid = ULID  ──────>  BatchEvent.traceid  ──>  structlog.bind(trace_id=...)
  tracing::span!(batch, %trace_id)                   every log line for this batch
                                                     carries the same trace_id

Mutations extend the same scheme: MutationLog.trace_id correlates the tool call, the apply pipeline, and the synchronous reindex it triggers. One ingestion or mutation flow is greppable end-to-end with a single ID.

Testing Strategy

15.1 Pyramid

Unit 70% / integration 25% / E2E 5%.

15.2 Rust Semantic Engine Tests
rust
#[test]
fn testpythonimport_resolution() {
    let mut engine = SemanticEngine::newfortest();
    engine.processfilestr("utils.py", "def validate_email(email: str) -> bool:\n    return '@' in email\n");
    let result = engine.processfilestr("main.py",
        "from utils import validateemail\ndef processuser(email: str):\n    if validate_email(email):\n        print('valid')\n");
    let calls: Vec<> = result.resolvededges.iter().filter(|e| e.edge_type == EdgeType::Calls).collect();
    assert_eq!(calls.len(), 1);
    asserteq!(calls[0].targetname, "validate_email");
    assert_eq!(calls[0].method, ResolutionMethod::StackGraph);
    assert!(calls[0].confidence >= 0.90);
}

#[test]
fn testcycliccallgraphterminates() {
    let mut engine = SemanticEngine::newfortest();
    engine.processfilestr("loop.py", "def a(): b()\ndef b(): a()\n");
    let callers = engine.find_callers("loop.py::a", 10);   // must terminate
    assert!(callers.iter().any(|(id, _)| id == "loop.py::b"));
}

#[test]
fn testremovefileiso1andstable() {
    let mut engine = SemanticEngine::newfortest();
    for i in 0..1000 { engine.processfilestr(&format!("f{i}.py"), "def x(): pass"); }
    let before = engine.importgraph.nodecount();
    engine.remove_file("f500.py");
    asserteq!(engine.importgraph.node_count(), before - 1);
    assert!(engine.import_graph.contains("f999.py"));     // surviving indices valid
}

#[test]
fn testtoplevelsentinelformodulelevelrefs() {
    let mut engine = SemanticEngine::newfortest();
    engine.processfilestr("lib.py", "def helper(): pass");
    let staged = engine.stagefilestr("main.py", "from lib import helper\nhelper()");
    assert!(staged.edges.iter().all(|e| !e.sourceid.isempty()));
}

15.3 Rust Mutation Engine Tests
rust
#[test] fn bodyreplacementpreservessignaturedocstring_decorators();
#[test] fn indentnormalizationrebasescolumnzeropastetoentitylevel();   // patch 3.2
#[test] fn indentnormalizationpreservestriplequotedstringinteriors();    // patch 3.2
#[test] fn signaturecascaderewritesallverifiedcallsites();
#[test] fn requiredparamwithoutcallsitevaluefails_preflight();
#[test] fn renameskipsshadowedsamename_symbols();           // stack-graph scope proof
#[test] fn staleexpectedhashrejectsmutation();
#[test] fn syntaxerrortriggersfullmultifile_rollback();
#[test] fn crlffilemutationisbyte_accurate();
#[test] fn multibyteidentifieroffsetsneverpanic();
#[test] fn descendingoffsetapplicationkeepsedits_stable();  // 200 edits, one file
#[test] fn mutationlogpruningrespectsretentionandcap();   // patch 3.1

15.4 Python Integration, Snapshot & Policy Tests
python
def testfullingestionpipeline(tempdb, sample_project):
    pipeline = IngestionPipeline(config=testconfig(), db=tempdb)
    pipeline.processbatch(list(sampleproject.glob("/*.py")))
    assert temp_db.execute("MATCH (f:File) RETURN count(f)")[0][0] == 3
    edges = temp_db.execute(
        "MATCH (a:Function)-[r:CALLS]->(b:Function) "
        "RETURN a.name, b.name, r.confidence, r.resolution_method")
    assert len(edges) > 0
    assert all(0  200 entities/s |
| Steady-state dedup hit rate | > 85% |
| Mutation plan generation (≤100 files, dry run) | ,
    #[pyo3(get)] pub functions: Vec,
    #[pyo3(get)] pub methods: Vec,
    #[pyo3(get)] pub variables: Vec,
    #[pyo3(get)] pub imports: Vec,
    #[pyo3(get)] pub references: Vec,
    #[pyo3(get)] pub error_ratio: f32,
    #[pyo3(get)] pub line_count: u32,
    #[pyo3(get)] pub size_bytes: u64,
}

#[pyclass] #[derive(Clone)]
pub struct ParsedFunction {
    #[pyo3(get)] pub name: String,
    #[pyo3(get)] pub qualified_name: String,
    #[pyo3(get)] pub signature: String,
    #[pyo3(get)] pub body: String,
    #[pyo3(get)] pub docstring: Option,
    #[pyo3(get)] pub start_line: u32,
    #[pyo3(get)] pub end_line: u32,
    #[pyo3(get)] pub span: ByteSpan,
    #[pyo3(get)] pub name_span: ByteSpan,
    #[pyo3(get)] pub params_span: ByteSpan,
    #[pyo3(get)] pub body_span: ByteSpan,
    #[pyo3(get)] pub decorators_span: Option,
    #[pyo3(get)] pub is_async: bool,
    #[pyo3(get)] pub is_generator: bool,
    #[pyo3(get)] pub is_static: bool,
    #[pyo3(get)] pub visibility: String,
    #[pyo3(get)] pub decorators: Vec,
    #[pyo3(get)] pub parameters: Vec,
    #[pyo3(get)] pub content_hash: String,
}
// ParsedClass, ParsedMethod, ParsedVariable follow the same pattern
// (Method adds parentclass, isclassmethod, isabstract; Class adds bases).

#[pyclass] #[derive(Clone)]
pub struct ParsedParameter {
    #[pyo3(get)] pub name: String,
    #[pyo3(get)] pub type_annotation: Option,
    #[pyo3(get)] pub position: u32,
    #[pyo3(get)] pub has_default: bool,
    #[pyo3(get)] pub default_value: Option,
    #[pyo3(get)] pub is_variadic: bool,
    #[pyo3(get)] pub iskeywordonly: bool,
}

#[pyclass] #[derive(Clone)]
pub struct ParsedImport {
    #[pyo3(get)] pub module_path: String,
    #[pyo3(get)] pub symbol_name: Option,
    #[pyo3(get)] pub alias: Option,
    #[pyo3(get)] pub is_wildcard: bool,
    #[pyo3(get)] pub is_relative: bool,
    #[pyo3(get)] pub start_line: u32,
    #[pyo3(get)] pub name_span: ByteSpan,
}

#[pyclass] #[derive(Clone)]
pub struct ParsedReference {
    #[pyo3(get)] pub name: String,
    #[pyo3(get)] pub kind: ReferenceKind,
    #[pyo3(get)] pub line: u32,
    #[pyo3(get)] pub column: u32,
    #[pyo3(get)] pub enclosing_function: Option,
    #[pyo3(get)] pub receiver: Option,
    #[pyo3(get)] pub name_span: ByteSpan,
    #[pyo3(get)] pub args_span: Option,
}

#[pyclass] #[derive(Clone, PartialEq)]
pub enum ReferenceKind { Call, Instantiation, Inheritance, TypeAnnotation, AttributeAccess, Import }

#[pyclass]
pub struct BatchEvent {
    #[pyo3(get)] pub trace_id: String,        // ULID from watcher
    #[pyo3(get)] pub changes: Vec,
    #[pyo3(get)] pub trigger: String,
    #[pyo3(get)] pub timestamp: u64,
}

#[pyclass]
pub struct StagedChange {                     // opaque to Python
    #[pyo3(get)] pub path: String,
    #[pyo3(get)] pub entities: Vec,
    #[pyo3(get)] pub edges: Vec,
    #[pyo3(get)] pub unresolved: Vec,
    #[pyo3(get)] pub language: String,
}

// Mutation types: MutationEdit, MutationPlan, UnverifiedSite, MutationResult,
// SyntaxDiagnostic — defined in §11.2–11.4. IndentStyle / normalize_indent —
// §11.3 (internal to the Rust core, not exposed across the PyO3 boundary).

Appendix B: LadybugDB (Kùzu-Dialect) Vector Search API
cypher
CALL createhnswindex('indexname', 'NodeTable', 'embeddingproperty',
    { dimension: 896, metric: 'cosine', ef_construction: 128, m: 16 });

-- Pre-filtered search: HNSW restricted to a candidate set from graph traversal
CALL dbsimilaritysearch('indexname', $queryvector, $top_k, {filter: {property: $value}})
YIELD node, score;

-- Two-stage: 64-d Matryoshka pre-filter, then 896-d refinement
CALL dbsimilaritysearch('funcembeddingshortidx', $queryshort, 50)
YIELD node AS candidate
WITH collect(candidate.id) AS candidates
CALL dbsimilaritysearch('funcembeddingidx', $query_full, 10, {filter: {id: candidates}})
YIELD node, score
RETURN node.name, node.body, score;

Exact procedure signatures confirmed against the LadybugDB release pinned at implementation time (§15.5 gate).

Appendix C: Review Response Register (CG-ARCH-003-R1)

All 18 findings from review R1 were accepted in v3.1 and remain resolved in this consolidated document.

| # | Finding | Severity | Resolution |
|---|---------|----------|------------|
| 1.1 | Rust listings not compilable | P0 | Labeled design pseudocode; API errors corrected; cargo check gate (§15.5) |
| 1.2 | TSG rule files invalid | P0 | Labeled illustrative; production rules from reference stack-graphs crates (§4.6) |
| 1.3 | LSP on-demand spawning anti-pattern | P0 | Persistent warm pool with didOpen/didChange sync + TTL cache (§10) |
| 1.4 | ImportGraph::remove_file O(N) rebuild | P0 | StableDiGraph + bidirectional maps, O(1) removal (§4.3) |
| 1.5 | Traversals lack cycle detection | P0 | Explicit visited set + depth cap on all traversals (§4.4) |
| 2.1 | LadybugDB vs KùzuDB ambiguity | P1 | Relationship stated: community successor, Kùzu dialect (§5.1) |
| 2.2 | Broken Cypher templates | P1 | Templates rewritten; predicates moved to WHERE (§9.2) |
| 2.3 | Empty source_id dangling edges | P1 | ::toplevel sentinel node per file (§4.5) |
| 2.4 | No rollback on embedding failure | P1 | Staged two-phase commit (§6.2) |
| 2.5 | Backpressure missing on embedding pool | P1 | Per-batch budget + batch splitting (§6.3) |
| 3.1 | Overlapping confidence ranges | P2 | Disjoint bands (§4.1) |
| 3.2 | "cache-bypass via xxHash" misnomer | P2 | Renamed to content-addressed embedding deduplication |
| 3.3 | Git blame stubbed | P2 | Real git2 implementation (§8.3) |
| 3.4 | Python test syntax error | P2 | Fixed (§15.4) |
| 4.1 | In-memory graph memory unbounded | Arch | Per-component budgets, LRU, disk spill (§16.2) |
| 4.2 | Single-writer throughput unquantified | Arch | 1,000 files < 30 s; chunked sub-transactions (§7.2) |
| 4.3 | No schema versioning/migration | Arch | schema_version + migration policy (§5.5) |
| 4.4 | No cross-boundary trace correlation | Arch | trace_id (ULID) across PyO3 (§14.4) |

End of Document — CG-ARCH-003 v3.2.1 (Consolidated, Restoration + Patches 3.1/3.2)