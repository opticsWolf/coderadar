"""Embedding settings, read from the active config."""

from .. import embedding_settings as _settings

PREPROCESS_VERSION = "1.0"


def embedding_settings():
    """The configured model and dimension."""
    return _settings()


__all__ = ["PREPROCESS_VERSION", "embedding_settings"]
