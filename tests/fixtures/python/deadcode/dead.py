"""Dead-code golden fixture.

Live chain:      main -> run_pipeline -> _load / _transform
Dead:            _orphan (no callers), _chain_a -> _chain_b (dead chain)
Runtime-live:    Widget.__repr__ (dunder protocol, no in-repo callers)

The module is never imported, but underscore-prefixed helpers are NOT
public API, so being caller-less means dead here. Public names are live
by the unimported-module export rule.
"""


def main():
    return run_pipeline()


def run_pipeline():
    return _transform(_load())


def _load():
    return [1, 2, 3]


def _transform(items):
    return [x * 2 for x in items]


class Widget:
    def __repr__(self):  # runtime-invoked protocol method
        return "Widget()"


def _orphan():
    return 1


def _chain_a():
    return _chain_b()


def _chain_b():
    return 2
