"""A tiny LRU around the engine's query planner."""

from collections import OrderedDict

from .. import query as _query


class QueryCache:
    """Cache query results; the engine owns parsing and execution."""

    def __init__(self, size=64):
        self.size = size
        self._rows = OrderedDict()

    def get(self, text):
        """Run `text`, reusing a cached result when present."""
        if text in self._rows:
            self._rows.move_to_end(text)
            return self._rows[text]
        rows = _query(text)
        self._rows[text] = rows
        if len(self._rows) > self.size:
            self._rows.popitem(last=False)
        return rows

    def clear(self):
        """Drop every cached result."""
        self._rows.clear()


__all__ = ["QueryCache"]
