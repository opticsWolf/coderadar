"""CodeRadar v3.6 — Embedding Deduplication (§13.1)

Content-addressed embedding deduplication: before embedding an entity,
compute xxHash of its body. If unchanged since last embedding, reuse
the stored vector from LadybugDB.
"""

from __future__ import annotations

import hashlib
import struct
from typing import Any, Dict, List, Optional, Tuple

import structlog

logger = structlog.get_logger(__name__)


class EmbeddingUnavailable(RuntimeError):
    """The embedding model cannot be loaded, so no vector can be produced.

    Raised rather than returning a placeholder vector: a stored placeholder is
    indistinguishable from a real embedding downstream.
    """


class EmbeddingDedup:
    """Content-addressed embedding cache.

    In steady state, >85% of entity bodies are unchanged between edits,
    so most updates touch only graph edges, not the model.
    """

    def __init__(self, model_name: str = "jinaai/jina-code-embeddings-0.5b",
                 dimension: int = 896, truncated_dimension: int = 64,
                 max_body_tokens: int = 2000, batch_size: int = 32):
        self.model_name = model_name
        self.dimension = dimension
        self.truncated_dimension = truncated_dimension
        self.max_body_tokens = max_body_tokens
        self.batch_size = batch_size
        self._model = None  # Lazy-loaded fastembed model
        self.metrics: Dict[str, int] = {
            "cache_hit": 0,
            "cache_miss": 0,
            "generated": 0,
        }

    def embed_batch(
        self,
        to_embed: List[EmbedTarget],
        db: Any,
    ) -> List[Optional[List[float]]]:
        """Embed a batch with deduplication.

        Returns list where each element is:
          - vector (List[float]) for new embeddings to store
          - None for hash-cached entities (skip)
        """
        CACHE_HIT = object()  # unique sentinel
        out: List[Any] = []

        for entity in to_embed:
            if self._get_cached(db, entity.id, entity.content_hash):
                self.metrics["cache_hit"] += 1
                out.append(CACHE_HIT)
            else:
                self.metrics["cache_miss"] += 1
                out.append(None)

        # Find indices needing actual embedding (None, not CACHE_HIT)
        miss_indices = [i for i, v in enumerate(out) if v is None]
        if miss_indices:
            bodies = [to_embed[i].body[:self.max_body_tokens * 4]
                      for i in miss_indices]
            vectors = self._model_embed(bodies)
            for i, vec in zip(miss_indices, vectors):
                out[i] = vec
                self.metrics["generated"] += 1

        # Convert CACHE_HIT → None (caller skips None entries)
        return [None if v is CACHE_HIT else v for v in out]

    def _model_embed(self, texts: List[str]) -> List[List[float]]:
        """Run the embedding model on a batch of texts."""
        if self._model is None:
            try:
                from fastembed import TextEmbedding
                self._model = TextEmbedding(model_name=self.model_name)
            except ImportError as exc:
                # Zero vectors used to be returned here. They are stored with a
                # valid content hash, so `has_embedding` goes true and the dedup
                # cache treats them as fresh for ever — semantic search is then
                # silently and permanently poisoned. Fail loudly instead.
                logger.error("fastembed not installed; cannot embed",
                             model=self.model_name)
                raise EmbeddingUnavailable(
                    f"fastembed is not installed, so {self.model_name!r} cannot "
                    "embed anything. Install it (`uv add fastembed`) or skip the "
                    "embedding step; storing placeholder vectors would poison the "
                    "dedup cache."
                ) from exc

        result = list(self._model.embed(texts, batch_size=self.batch_size))
        return [r.tolist() for r in result]

    def _get_cached(self, db: Any, entity_id: str, content_hash: str) -> bool:
        """Check if embedding is cached AND hash matches (not stale)."""
        try:
            from coderadar._core import lookup_entity
            entity = lookup_entity(entity_id)
            if entity and entity.get("has_embedding"):
                stored_hash = entity.get("embedding_hash", "")
                return stored_hash == content_hash
        except (ImportError, RuntimeError):
            pass
        return False

    def cache_hit_rate(self) -> float:
        """Return the current cache hit rate."""
        total = self.metrics["cache_hit"] + self.metrics["cache_miss"]
        if total == 0:
            return 0.0
        return self.metrics["cache_hit"] / total



class EmbedTarget:
    """A single entity to embed."""
    __slots__ = ("id", "body", "content_hash", "kind")

    def __init__(self, entity_id: str, body: str, content_hash: str,
                 kind: str = "function"):
        self.id = entity_id
        self.body = body
        self.content_hash = content_hash
        self.kind = kind


def compute_content_hash(data: bytes) -> str:
    """Compute xxHash of content bytes for dedup."""
    # Use xxhash via hashlib if available, otherwise SHA-256 fallback
    try:
        import xxhash  # type: ignore
        return xxhash.xxh3_64(data).hexdigest()
    except ImportError:
        return hashlib.sha256(data).hexdigest()[:16]
