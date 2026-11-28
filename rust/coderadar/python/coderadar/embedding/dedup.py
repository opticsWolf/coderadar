"""Content-addressed embedding dedup (DR-11 §13.1).

Every decision lives in Rust: the embedded text, the cache-key format, the
hash, the cache lookup and the batch call. This module only names the engine
entry points and keeps the per-instance configuration/metrics the Python
callers read.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any

from .. import (
    content_hash as _content_hash,
    embed_body as _embed_body,
    embed_cache_key as _embed_cache_key,
    embed_preprocess_version as _preprocess_version,
    embedding_batch as _embedding_batch,
    stored_key_model as _stored_key_model,
)

PREPROCESS_VERSION = _preprocess_version()


class EmbeddingUnavailable(RuntimeError):
    """The embedding model cannot be loaded, so no vector can be produced."""


class EmbedTarget:
    """A single entity to embed."""

    __slots__ = ("body", "content_hash", "id", "kind")

    def __init__(
        self,
        entity_id: str,
        body: str,
        content_hash: str,
        kind: str = "function",
    ):
        self.id = entity_id
        self.body = body
        self.content_hash = content_hash
        self.kind = kind


def embed_body(entity: dict, kind: str) -> str:
    """The text that gets embedded for one entity."""
    return _embed_body(json.dumps(entity), kind)


def embed_cache_key(body_hash: str, model_id: str) -> str:
    """The dedup key stored as `embedding_hash`."""
    return _embed_cache_key(body_hash, model_id)


def stored_key_model(stored_hash: str) -> str | None:
    """The model id baked into a stored key, or None for legacy bare hashes."""
    return _stored_key_model(stored_hash)


try:  # Python uses xxhash when installed, else its SHA-256 fallback.
    import xxhash  # noqa: F401

    _HAVE_XXHASH = True
except ImportError:
    _HAVE_XXHASH = False


def compute_content_hash(data: bytes) -> str:
    """Content hash for dedup: xxHash3-64, or SHA-256[:16] without xxhash."""
    if _HAVE_XXHASH:
        return _content_hash(bytes(data))
    return hashlib.sha256(bytes(data)).hexdigest()[:16]


class EmbeddingDedup:
    """Content-addressed embedding cache over the Rust engine."""

    def __init__(
        self,
        model_name: str = "BAAI/bge-small-en-v1.5",
        dimension: int = 384,
        max_body_tokens: int = 2000,
        batch_size: int = 32,
    ):
        self.model_name = model_name
        self.dimension = dimension
        self.max_body_tokens = max_body_tokens
        self.batch_size = batch_size
        self.metrics: dict[str, int] = {
            "cache_hit": 0,
            "cache_miss": 0,
            "generated": 0,
        }

    def embed_batch(self, to_embed: list[EmbedTarget], db: Any = None) -> list[list[float] | None]:
        """Embed a batch, returning `None` for hash-cached entities.

        # Errors
        Raises `EmbeddingUnavailable` when the Rust engine cannot load the
        model, and `RuntimeError` when no graph is loaded.
        """
        _ = db
        targets = list(to_embed)
        try:
            report = _embedding_batch(
                self.model_name,
                [target.id for target in targets],
                [target.body for target in targets],
                [target.content_hash for target in targets],
                self.batch_size,
                self.dimension,
                self.max_body_tokens,
                False,
            )
        except RuntimeError as error:
            message = str(error)
            if "No graph loaded" in message:
                raise
            raise EmbeddingUnavailable(message) from None
        for key, value in report["metrics"].items():
            self.metrics[key] = self.metrics.get(key, 0) + value
        return report["vectors"]

    def cache_hit_rate(self) -> float:
        """Return the current cache hit rate."""
        total = self.metrics["cache_hit"] + self.metrics["cache_miss"]
        if total == 0:
            return 0.0
        return self.metrics["cache_hit"] / total


__all__ = [
    "PREPROCESS_VERSION",
    "EmbedTarget",
    "EmbeddingDedup",
    "EmbeddingUnavailable",
    "compute_content_hash",
    "embed_body",
    "embed_cache_key",
    "stored_key_model",
]
