"""CodeRadar v3.6 — Embedding Package"""
from .dedup import (
    EmbeddingDedup, EmbedTarget, EmbeddingUnavailable, compute_content_hash,
)

__all__ = ["EmbeddingDedup", "EmbedTarget", "EmbeddingUnavailable",
           "compute_content_hash", "embedding_settings"]


def embedding_settings():
    """The (model_name, dimension) the whole process must agree on.

    Read from `.coderadar.toml` when the project sets `[embedding]`, else the
    package default. Index-time and query-time both call this: they used to
    name different models with different dimensions, which produces a search
    that returns confident nonsense rather than an error.
    """
    from coderadar.config import EmbeddingConfig, load_config
    from pathlib import Path
    try:
        cfg = load_config(Path.cwd()).embedding
    except Exception:
        cfg = EmbeddingConfig()
    return cfg.model, cfg.dimension
