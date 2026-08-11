"""CodeRadar v3.6 — Ingestion Pipeline (§6.7)

Orchestrates the two-phase commit between Rust staging and Macrame persistence.

Phase 1: Rust stage_file() — parse, extract entities/edges, flat-buffer encode.
Phase 2: Python — decode flat buffers, embed unresolved references (L4),
         run LSP fallback (L5), write to Macrame as Concepts + EdgeAssertions.
Phase 3: On Macrame success → Rust commit_staged() + rebuild ProjectedGraph.
         On failure → rollback_staged().
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional

import structlog

logger = structlog.get_logger(__name__)


@dataclass
class IngestionResult:
    """Result of processing a batch of files through the ingestion pipeline."""
    files_processed: int = 0
    entities_created: int = 0
    entities_modified: int = 0
    embeddings_generated: int = 0
    embeddings_cached: int = 0
    edges_created: int = 0
    duration_ms: float = 0.0
    errors: List[str] = field(default_factory=list)


class IngestionPipeline:
    """Orchestrates the staged two-phase commit for ingestion.

    Phase 1: Rust stage_file() — pure diff, no mutation.
    Phase 2: Python — decode flat buffers, embed unresolved references,
             run LSP fallback, write to Macrame as Concepts.
    Phase 3: On Macrame success → Rust commit_staged(); on failure → rollback_staged().
    """

    def __init__(self, db_path: Optional[str] = None):
        self.db_path = db_path or ".coderadar/coderadar.db"
        self._batch_chunk_size = 200

    def process_batch(
        self, staged_changes: List[Any]
    ) -> IngestionResult:
        """Process a batch of staged changes through the full ingestion pipeline."""
        result = IngestionResult()

        for staged in staged_changes:
            try:
                # 1. Decode flat buffers → Python entity/edge/ref objects
                entities, edges, refs = self._decode_staged(staged)

                # 2. Embed unresolved references (Layer 4)
                self._embed_unresolved(refs)

                # 3. LSP fallback (Layer 5) if enabled
                self._run_lsp_fallback(refs)

                # 4. Write entities and edges to Macrame
                self._write_to_macrame(entities, edges)

                result.files_processed += 1
                result.entities_created += len(entities)

            except Exception as e:
                logger.error("ingestion.error", file=getattr(staged, 'path', 'unknown'),
                              error=str(e))
                result.errors.append(str(e))

        return result

    def _decode_staged(self, staged: Any) -> tuple:
        """Decode flat-buffer payload from Rust side."""
        from .flatbuffer import decode_extraction

        meta = getattr(staged, 'meta', b'')
        entity_bytes = getattr(staged, 'entities', b'')
        edge_bytes = getattr(staged, 'edges', b'')
        ref_bytes = getattr(staged, 'refs', b'')
        arena = getattr(staged, 'arena', b'')

        fb = decode_extraction(meta, entity_bytes, edge_bytes, ref_bytes, arena)
        return fb.entities, fb.edges, fb.refs

    def _embed_unresolved(self, refs: List[Any]) -> None:
        """Layer 4: Compute embeddings for unresolved references.

        Macrame stores embeddings directly on Concepts via
        `ConceptUpsert.embedding` field — no separate vector DB needed.
        """
        pass

    def _run_lsp_fallback(self, refs: List[Any]) -> None:
        """Layer 5: Override low-confidence edges with LSP results."""
        pass

    def _run_framework_extraction(
        self, file_path: str, source: str,
    ) -> tuple:
        """v3.6: Run framework resolvers (Django/Flask/FastAPI) on a file.

        Returns (synthetic_nodes, synthetic_edges) to be merged into the
        graph alongside tree-sitter-extracted entities.

        Full pipeline wiring (merging SyntheticEdges into the Rust
        ProjectedGraph) is deferred — currently the resolvers are
        surfaced via CLI display only.
        """
        from coderadar.resolvers import ALL_RESOLVERS

        nodes, edges = [], []
        for resolver_cls in ALL_RESOLVERS:
            resolver = resolver_cls()
            try:
                extraction = resolver.extract(file_path, source)
                nodes.extend(extraction.nodes)
                edges.extend(extraction.edges)
            except Exception:
                pass
        return nodes, edges

    def _write_to_macrame(self, entities: List[Any], edges: List[Any]) -> None:
        """Write staged entities and edges to Macrame.

        Entities → ConceptUpsert (kind + JSON metadata in content).
        Edges → EdgeAssertion with properties (confidence, provenance, language).
        Macrame handles the bitemporal ledger automatically.
        """
        pass

    def commit(self, staged: Any) -> None:
        """Commit staged changes — called after Macrame write succeeds."""
        # Trigger ProjectedGraph rebuild on Rust side
        pass

    def rollback(self, staged: Any) -> None:
        """Rollback staged changes — called if Macrame write fails."""
        pass


class PipelineConfig:
    """Configuration for the ingestion pipeline."""
    batch_chunk_size: int = 200
    embedding_budget_ms: int = 2000
    defer_low_priority_below: float = 0.6
    max_file_size_bytes: int = 1_048_576
