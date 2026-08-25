"""Clone golden fixture.

clone_a / clone_b  — identical bodies            → Type-1 group
clone_c            — renamed vars + new literal  → Type-2 (same normalized stream)
unrelated          — different structure         → must not be grouped
"""


def clone_a(x):
    total = 0
    for i in range(20):
        total += i * x
    return total


def clone_b(x):
    total = 0
    for i in range(20):
        total += i * x
    return total


def clone_c(y):
    acc = 0
    for j in range(99):
        acc += j * y
    return acc


def unrelated(q):
    return sorted({k: v for k, v in q.items()}, reverse=True)[0]
