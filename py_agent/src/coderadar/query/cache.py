"""CodeRadar v3.3 — Query Cache (§7.3)

LRU cache for query results, keyed on (template_id, params, graph_epoch).
Invalidated on every write.
"""

from __future__ import annotations

import hashlib
import json
import time
from functools import wraps
from typing import Any, Callable, Dict, List, Optional


class QueryCache:
    """Thread-safe LRU cache for query results.

    Cache entries keyed on (template_id, params, graph_epoch).
    TTL and max-size bounded. Invalidated on every graph write.
    """

    def __init__(self, max_size: int = 256, ttl_seconds: int = 300):
        self._max_size = max_size
        self._ttl_seconds = ttl_seconds
        self._cache: Dict[str, _CacheEntry] = {}

    def get(self, key: str) -> Optional[Any]:
        """Get a cached result. Returns None if expired or missing."""
        entry = self._cache.get(key)
        if entry is None:
            return None
        if time.monotonic() - entry.timestamp > self._ttl_seconds:
            del self._cache[key]
            return None
        return entry.value

    def set(self, key: str, value: Any) -> None:
        """Store a result in the cache."""
        if len(self._cache) >= self._max_size:
            # Evict the oldest entry
            oldest = min(self._cache, key=lambda k: self._cache[k].timestamp)
            del self._cache[oldest]
        self._cache[key] = _CacheEntry(value, time.monotonic())

    def invalidate(self) -> None:
        """Clear all cached results."""
        self._cache.clear()

    def prune_expired(self) -> int:
        """Remove all expired entries. Returns count removed."""
        now = time.monotonic()
        expired = [
            k for k, v in self._cache.items()
            if now - v.timestamp > self._ttl_seconds
        ]
        for k in expired:
            del self._cache[k]
        return len(expired)

    @staticmethod
    def make_key(template_id: str, params: Dict[str, Any],
                 graph_epoch: int) -> str:
        """Create a deterministic cache key."""
        param_str = json.dumps(params, sort_keys=True)
        raw = f"{template_id}|{param_str}|{graph_epoch}"
        return hashlib.sha256(raw.encode()).hexdigest()[:32]

    def __len__(self) -> int:
        return len(self._cache)


class _CacheEntry:
    """A single cache entry with timestamp."""
    __slots__ = ("value", "timestamp")

    def __init__(self, value: Any, timestamp: float):
        self.value = value
        self.timestamp = timestamp


# Global query cache instance
_default_cache = QueryCache()


def cached_query(func: Callable) -> Callable:
    """Decorator that caches query results based on function arguments.

    Cache key = (function_name, args, kwargs, graph_epoch).
    """

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
