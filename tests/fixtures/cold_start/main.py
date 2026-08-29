"""Entry point.

Absolute cross-package imports plus a call into a cross-module function, so
a cold load must restore the IMPORTS edge and the CALLS index rebuilt from
`resolved_calls`.
"""

from app.models import Base, Derived, STATUS
from app import combine, helper


def run():
    value = combine(1, 2)
    return Derived().describe() + value + helper() + STATUS


if __name__ == "__main__":
    print(run())
