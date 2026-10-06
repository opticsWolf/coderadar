# Vector embedding integration: findings, gaps, and next steps

**Status:** review only; no embedding-persistence implementation is included here. Findings are based on CodeRadar v0.9.4 and its `macrame-db` 0.18.0 dependency.

## Summary

- **FastEmbed is run by CodeRadar, not by Macrame.** CodeRadar uses FastEmbed to generate both indexed-entity vectors and query vectors. Macrame receives caller-supplied vectors; it does not load or invoke an embedding model.
- **Macrame 0.18 already has a dedicated persistent vector API.** It supports per-model vector tables, model registration, bulk and incremental vector upserts, and vector search backed by a DiskANN index.
- **CodeRadar does not currently use that API.** Its vectors are held only on entities in the process-local `ProjectedGraph`. Macrame persists CodeRadar concepts and edges, but not their embedding vectors.
- Adding persistence could reduce repeated embedding inference after a restart and improve search scaling on large projects. It requires explicit model identity, cache invalidation, update/retirement handling, result-score conversion, and a decision about bulk-index downtime and vector cleanup.

## Current CodeRadar data flow

1. `EmbeddingConfig` defaults to `BAAI/bge-small-en-v1.5` with dimension 384. The MCP query path lazily loads `fastembed.TextEmbedding` using that configured model.
2. `CodeGraph.compute_embeddings()` enumerates functions, classes, modules, imports, constants, and type aliases. It currently embeds an entity's `signature`, falling back to its `name`; it does not embed the full source body. FastEmbed returns Python vectors, which are passed through the native extension as `Vec<f64>`.
3. `EmbeddingDedup` hashes the selected text and checks the current native graph for an existing vector with the same hash. New vectors are stored in `ProjectedGraph` as `EmbeddingVec { vec: Vec<f64>, hash }` using a bulk projection update.
4. Query text is embedded with FastEmbed and passed to `_core.search_similar`. Rust scans all in-memory entity maps, computes cosine similarity, sorts descending, and returns the top results. This is an exhaustive search, not a database vector-index query.
5. Cold-start reconstruction creates entities with `EmbeddingVec::default()`. Thus the vector values and their content hashes do not survive process restart, even though the concepts do.

Relevant CodeRadar files:

- `py_agent/src/coderadar/config.py`
- `py_agent/src/coderadar/embedding/dedup.py`
- `py_agent/src/coderadar/__init__.py` (`compute_embeddings` and `search_similar`)
- `py_agent/src/coderadar/mcp/server.py` (query embedding and MCP search path)
- `core_indexer/src/types.rs` (`EmbeddingVec`)
- `core_indexer/src/graph/embeddings.rs` (projection storage and invalidation)
- `core_indexer/src/graph/cold_start.rs` (cold-start defaults)
- `core_indexer/src/lib.rs` (`search_similar` and cosine scoring)

## Macrame 0.18 vector capability

The installed Macrame 0.18 API has the pieces needed for a persistent-vector prototype:

- `register_model(model, dimension)` creates a per-model `F32_BLOB(dimension)` table and its DiskANN vector index. Registration is idempotent at the same dimension and rejects a conflicting dimension.
- `upsert_embeddings` writes/replaces vectors in chunks while maintaining the index. `bulk_embeddings` is designed for initial/backfill loads: it temporarily drops the index, writes the vectors, then rebuilds the index. The latter makes the model's vectors unavailable to search during the rebuild window.
- `search_vector` queries the registered DiskANN index and returns concept IDs with **cosine distance** (lower is closer), whereas CodeRadar's current search returns **cosine similarity** (higher is closer). A bridge must preserve ordering and convert the score contract before exposing results through the existing API.
- The model identifier is a validated SQL-safe name: lowercase ASCII, starts with a letter, then lowercase letters/digits/underscores, maximum 48 characters. The configured FastEmbed name `BAAI/bge-small-en-v1.5` cannot be used directly; CodeRadar needs a stable alias, for example `bge_small_en_v1_5`, mapped to the actual FastEmbed name and dimension.
- Vectors are derived side data rather than bitemporal ledger assertions. Macrame does not run FastEmbed, and its vector rows do not provide historical embedding versions. Do not treat persisted vectors as a historical record of what a past model or source revision produced.

Macrame's own source documents show the index-maintenance cost can be significant: on its reference machine, 2,000 vectors at dimension 256 took about 31.0 seconds through indexed upserts versus 19.7 seconds through `bulk_embeddings`. This is an upstream reference measurement, not a CodeRadar benchmark; it does not predict performance for CodeRadar's 384-dimensional vectors or other hardware.

## Gaps and integration risks

### 1. No durable vector lifecycle in CodeRadar

There are no current CodeRadar calls to Macrame's model registration, vector upsert, or vector search APIs. Persisting vectors alone would not make the present `_core.search_similar` use them: that function only scans vectors attached to the in-memory projection. An integration must choose either to hydrate vectors into the projection on load or, preferably for larger projects, query Macrame and resolve returned concept IDs to CodeRadar entities.

### 2. Cache identity does not include model identity

The in-memory `EmbeddingVec` stores a content hash but no model name/version. `EmbeddingDedup` treats a matching content hash as a cache hit. Calling `compute_embeddings()` with a different model in the same process can therefore reuse a vector made by the previous model. A durable cache key should include at least:

- stable model identifier and model/revision provenance;
- expected dimension;
- a version for the text-selection/preprocessing recipe;
- a hash of the exact text sent to FastEmbed.

The existing `concepts.extra.coderadar.content_hash` should not automatically be treated as this fingerprint: the embedding input currently is a signature or name, while concept content metadata serves CodeRadar's persistence/cold-start format.

### 3. Dimension errors are weakly surfaced today

Macrame's registered table can enforce the declared dimension on writes and queries. The current Rust cosine function instead returns `0.0` for unequal vector lengths, making a model/dimension mismatch look like poor search results rather than a clear error. Validate query and indexed vector dimensions before search and fail with a diagnostic.

### 4. Model-name and precision conversion

The FastEmbed model name must be separated from Macrame's safe table identifier. In addition, CodeRadar currently carries `f64` vectors while Macrame stores `f32`; the integration needs an explicit, tested conversion and should check for non-finite values and the expected dimension at the boundary.

### 5. Search contract and entity hydration

Macrame returns concept IDs and cosine distances. CodeRadar returns entity payloads with cosine similarity. The bridge must convert `similarity = 1 - distance`, preserve top-k ordering, map IDs back to active projected entities, and define behavior for a hit whose entity is missing from the current projection. Add parity tests against the existing exhaustive cosine search; separately measure recall because DiskANN is an indexed nearest-neighbor path rather than the current full scan.

### 6. Incremental updates, retirement, and deletion

`clear_embeddings_for_file()` only clears vectors in memory. A persistent path must update vectors after file edits and ensure old vectors cannot appear as live results. Macrame's vector search filters retired concepts, but a retired concept row can remain in the ledger with its vector row. I did not find a documented actor-safe per-concept vector-delete API in Macrame 0.18. This leaves a cleanup/storage-growth question: determine whether to add a supported deletion operation, rebuild/compact model tables periodically, or accept hidden retired rows with a defined bound.

### 7. Bulk indexing has an availability window

Macrame's `bulk_embeddings` is more efficient for a full load but drops the DiskANN index during the load/rebuild. Searches for that model are unavailable during this interval. Its API is designed to attempt index reconstruction even when the chunked load fails, but the integration still needs cancellation/failure tests and an operational policy for concurrent searches. Incremental `upsert_embeddings` avoids that indexless window but pays index-maintenance cost per chunk.

### 8. Historical and snapshot expectations

Embedding rows are derivative and are not reconstructed as part of CodeRadar's current `MaterializedState` cold start. A persistent integration can make them available across process restarts by querying the vector table directly, but it must not imply that Macrame's ledger snapshots contain model-versioned embeddings. Transaction-time vector search is not available from the current one-vector-per-concept design.

### 9. Current embedding coverage and quality should be made explicit

- `compute_embeddings()` asks `search_entities` for up to 10,000 entities **per kind** and has no paging loop. Validate that this is sufficient or add pagination before claiming complete indexing for very large projects.
- Current input is signature/name, not full implementation text or docstrings. Decide whether this is the desired semantic-search representation before building a persistent cache; changing it later requires re-embedding.
- `EmbeddingConfig.truncated_dimension` is not applied in the current embedding path. `max_body_tokens` and the config's batch size are not fully plumbed through `embedding_settings()` into `EmbeddingDedup` (which uses its own defaults/arguments). Align the effective configuration and document whether length limiting is token-based or a character approximation.
- The MCP search handler's auto-compute fallback catches `RuntimeError`, while the native search returns an empty list when a loaded graph has no vectors. In that case, automatic embedding may not run; test this path and use an explicit no-embeddings check if auto-compute is intended.
- `EmbeddingDedup._get_cached()` reads the current native graph via `lookup_entity`; its “LadybugDB” wording is stale and does not describe a persistent cache.

## Recommended next steps

### Phase 1 — settle the contract and fix existing cache correctness

1. Decide whether vectors are a durable derived cache (recommended) or intentionally ephemeral. State clearly that FastEmbed generates vectors and Macrame only stores/indexes/searches them.
2. Define a stable model registry, e.g. Macrame alias `bge_small_en_v1_5` → FastEmbed model `BAAI/bge-small-en-v1.5`, dimension 384, plus model/preprocessing version.
3. Fix the in-memory dedup key to include model identity and preprocessing version; add dimension checks and explicit errors for mismatches.
4. Decide the embedding text (signature/name versus richer source text) and the invalidation fingerprint before persisting any cache entries.
5. Resolve the 10,000-per-kind coverage limit, config plumbing, and the empty-index auto-compute behavior.

### Phase 2 — build a small Macrame-backed prototype

1. Register the model when opening the CodeRadar store.
2. Add an actor-safe Rust/Python bridge to batch vectors through Macrame's public APIs; convert to `f32` at the boundary. Do not bypass the Macrame write actor with raw connection writes.
3. Use `bulk_embeddings` for an initial backfill in a controlled/maintenance window. Use `upsert_embeddings` for incremental batches after file changes.
4. Implement vector search through Macrame, convert distance to the existing similarity contract, and hydrate result IDs from the active CodeRadar graph.
5. Specify behavior for retired/removed concepts, partial writes, cancellation, model changes, and a cold start. Investigate the missing per-concept vector deletion/compaction path before relying on long-lived stores.

### Phase 3 — validate before making it the default

- Test persistence after close/reopen, cold start, file edits, entity retirement, model/dimension changes, partial bulk failure, and query during bulk rebuild.
- Compare Macrame top-k results and scores with the current exhaustive search on a representative corpus; measure ANN recall as well as latency.
- Benchmark end-to-end embedding time, startup time, query latency, incremental update cost, database/index size, and memory on small and large repositories. Include the current in-memory mode as the baseline.
- Keep the exhaustive in-memory path as a fallback until parity, invalidation, and recovery behavior are established. Choose any size-based switch only from measured CodeRadar results.
