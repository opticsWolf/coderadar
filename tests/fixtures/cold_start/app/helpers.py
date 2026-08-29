"""Helpers used across the app.

Absolute cross-package import (`app.models`) and a relative import
(`.utils.mathx`) so a cold load must restore IMPORTS edges in both forms,
and call indices rebuilt from `resolved_calls`.
"""

from app.models import Derived, STATUS
from .utils.mathx import add


def helper():
    return "x"


def combine(a, b):
    return add(a, b) + helper() + STATUS + Derived().describe()
