"""Small math helpers."""

from .sibling import SIBLING_NAME


def add(a, b):
    return a + b


def multiply(a, b):
    return add(a, b) * 2 + len(SIBLING_NAME)
