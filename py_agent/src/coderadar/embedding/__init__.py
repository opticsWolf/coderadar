"""CodeRadar v3.3 — Embedding Package"""
from .dedup import EmbeddingDedup, EmbedTarget, compute_content_hash

__all__ = ["EmbeddingDedup", "EmbedTarget", "compute_content_hash"]
