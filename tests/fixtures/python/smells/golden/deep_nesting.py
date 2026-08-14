"""Golden fixture: deep-nesting (4 levels of nested control flow).

`nesting_depth` is the maximum depth of decision/block nodes in the function
subtree, so four nested `if`s yield depth 4 → severity Medium (>= 6 would be
High). Cyclomatic = 1 + 4 = 5; return_count = 2.
"""


def deeply_nested(x):
    if x > 0:
        if x > 1:
            if x > 2:
                if x > 3:
                    return 1
    return 0
