"""CodeRadar embedding package: dedup helpers and the agreed settings."""

from __future__ import annotations

from pathlib import Path

from ..config import EmbeddingConfig, load_config
from .dedup import (
    PREPROCESS_VERSION,
    EmbedTarget,
    EmbeddingDedup,
    EmbeddingUnavailable,
    compute_content_hash,
    embed_body,
    embed_cache_key,
    stored_key_model,
)


def embedding_settings() -> tuple[str, int]:
    """The (model_name, dimension) the whole process must agree on.

    Read from `.coderadar.toml` when the project sets `[embedding]`, else the
    package default. Index-time and query-time both call this: naming
    different models produces confident nonsense rather than an error.
    """
    try:
        cfg = load_config(str(Path.cwd())).embedding
    except Exception:  # noqa: BLE001 - broken config falls back to the default
        cfg = EmbeddingConfig()
    return cfg.model, cfg.dimension


__all__ = [
    "PREPROCESS_VERSION",
    "EmbedTarget",
    "EmbeddingDedup",
    "EmbeddingUnavailable",
    "compute_content_hash",
    "embed_body",
    "embed_cache_key",
    "embedding_settings",
    "stored_key_model",
]
