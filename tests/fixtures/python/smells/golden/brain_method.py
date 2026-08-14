"""Golden fixture: brain-method (max method cyclomatic >= 15, WMC >= 20).

`BrainMethod.brain` has 14 `if` decision points → cyclomatic 15.
`BrainMethod.helper` has 4 → cyclomatic 5. WMC = 15 + 5 = 20, and
max_method_cyclomatic = 15 → severity High (>= 30 would be Critical).
"""


class BrainMethod:
    def brain(self, x):
        r = 0
        if x > 0:
            r += 1
        if x > 1:
            r += 1
        if x > 2:
            r += 1
        if x > 3:
            r += 1
        if x > 4:
            r += 1
        if x > 5:
            r += 1
        if x > 6:
            r += 1
        if x > 7:
            r += 1
        if x > 8:
            r += 1
        if x > 9:
            r += 1
        if x > 10:
            r += 1
        if x > 11:
            r += 1
        if x > 12:
            r += 1
        if x > 13:
            r += 1
        return r

    def helper(self, x):
        r = 0
        if x > 0:
            r += 1
        if x > 1:
            r += 1
        if x > 2:
            r += 1
        if x > 3:
            r += 1
        return r
