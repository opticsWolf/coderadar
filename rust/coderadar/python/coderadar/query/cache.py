"""A host-side LRU for query results, keyed on (method, params, epoch).

Caching is process state, not graph state, so it lives here — the engine owns
parsing and execution (`coderadar.query`), and this only remembers answers.
The semantics match the reference: TTL expiry uses `>=` so `ttl=0` is never
valid (two equal monotonic reads on Windows made a strict compare immortal).
"""

from __future__ import annotations

import hashlib
import json
import time
from collections.abc import Callable
from functools import wraps
from typing import Any


class QueryCache:
    """Thread-safe-enough LRU for query results."""

    def __init__(self, max_size: int = 256, ttl_seconds: int = 300):
        self._max_size = max_size
        self._ttl_seconds = ttl_seconds
        self._cache: dict[str, _CacheEntry] = {}

    def get(self, key: str) -> Any | None:
        entry = self._cache.get(key)
        if entry is None:
            return None
        if time.monotonic() - entry.timestamp >= self._ttl_seconds:
            del self._cache[key]
            return None
        return entry.value

    def set(self, key: str, value: Any) -> None:
        if len(self._cache) >= self._max_size:
            oldest = min(self._cache, key=lambda k: self._cache[k].timestamp)
            del self._cache[oldest]
        self._cache[key] = _CacheEntry(value, time.monotonic())

    def invalidate(self) -> None:
        self._cache.clear()

    def prune_expired(self) -> int:
        now = time.monotonic()
        expired = [
            key for key, entry in self._cache.items()
            if now - entry.timestamp >= self._ttl_seconds
        ]
        for key in expired:
            del self._cache[key]
        return len(expired)

    @staticmethod
    def make_key(method: str, params: dict[str, Any], graph_epoch: int) -> str:
        param_str = json.dumps(params, sort_keys=True)
        raw = f"{method}|{param_str}|{graph_epoch}"
        return hashlib.sha256(raw.encode()).hexdigest()[:32]

    def __len__(self) -> int:
        return len(self._cache)


class _CacheEntry:
    __slots__ = ("timestamp", "value")

    def __init__(self, value: Any, timestamp: float):
        self.value = value
        self.timestamp = timestamp


_default_cache = QueryCache()


def cached_query(func: Callable) -> Callable:
    """Decorator that caches query results on name + arguments + epoch."""

    @wraps(func)
    def wrapper(*args, **kwargs):
        cache = _default_cache
        graph_epoch = kwargs.pop("_epoch", 0)
        key = QueryCache.make_key(
            func.__name__,
            {"args": str(args)[:200], "kwargs": str(kwargs)[:200]},
            graph_epoch,
        )
        cached = cache.get(key)
        if cached is not None:
            return cached
        result = func(*args, **kwargs, _epoch=graph_epoch)
        cache.set(key, result)
        return result

    return wrapper


__all__ = ["QueryCache", "cached_query"]
