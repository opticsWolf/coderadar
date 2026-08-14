"""Golden fixture: excessive-returns (6 return statements).

`return_count` counts named `return_statement` nodes; 6 returns is above the
threshold of 5 → severity Medium (>= 8 would be High).
"""


def many_returns(x):
    if x == 0:
        return 0
    if x == 1:
        return 1
    if x == 2:
        return 2
    if x == 3:
        return 3
    if x == 4:
        return 4
    return 5
