"""CodeRadar v3.6 — Query Cache (§7.3)

LRU cache for Macrame query results, keyed on (method, params, graph_epoch).
Invalidated on every write.
"""

from __future__ import annotations

import hashlib
import json
import time
from functools import wraps
from typing import Any, Callable, Dict, Optional


class QueryCache:
    """Thread-safe LRU cache for Macrame query results.

    Cache entries keyed on (method, params, graph_epoch).
    TTL and max-size bounded. Invalidated on every graph write.
    """

    def __init__(self, max_size: int = 256, ttl_seconds: int = 300):
        self._max_size = max_size
        self._ttl_seconds = ttl_seconds
        self._cache: Dict[str, _CacheEntry] = {}

    def get(self, key: str) -> Optional[Any]:
        entry = self._cache.get(key)
        if entry is None:
            return None
        # >=, not >: a ttl of 0 means "never valid", and on Windows two
        # monotonic() reads around a set() can be equal — a strict compare
        # made ttl=0 entries immortal there while Linux timers masked it.
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
        # Same >= as get(): ttl=0 expires everything, deterministically.
        expired = [
            k for k, v in self._cache.items()
            if now - v.timestamp >= self._ttl_seconds
        ]
        for k in expired:
            del self._cache[k]
        return len(expired)

    @staticmethod
    def make_key(method: str, params: Dict[str, Any],
                 graph_epoch: int) -> str:
        param_str = json.dumps(params, sort_keys=True)
        raw = f"{method}|{param_str}|{graph_epoch}"
        return hashlib.sha256(raw.encode()).hexdigest()[:32]

    def __len__(self) -> int:
        return len(self._cache)


class _CacheEntry:
    __slots__ = ("value", "timestamp")

    def __init__(self, value: Any, timestamp: float):
        self.value = value
        self.timestamp = timestamp


_default_cache = QueryCache()


def cached_query(func: Callable) -> Callable:
    """Decorator that caches Macrame query results."""

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
