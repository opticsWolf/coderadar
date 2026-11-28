"""Parity fixture: routes, classes, constants and calls in one file."""

from __future__ import annotations

from util import helper

MAX_ITEMS = 10
Item = dict


class Stock:
    """A counted collection."""

    def __init__(self, count: int) -> None:
        self.count = count

    def add(self, n: int) -> int:
        return self.count + n

    def twice(self) -> int:
        return self.add(self.count)


def total(rows: list[int]) -> int:
    """Sum a list through the helper."""
    return helper(len(rows))


def report(stock: Stock) -> dict:
    return {"total": total([stock.count]), "items": []}
