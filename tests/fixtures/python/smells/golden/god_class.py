"""Golden fixture: positive god-class (WMC >= 47 AND CBO >= 5).

- `God` has 4 methods × 12 `if`s = 13 cyclomatic each → WMC = 52.
- `God` inherits from 5 same-file classes → CBO = 5 (resolved_bases).
Severity: WMC >= 47 but < 70 → Medium.
"""


class A:
    pass


class B:
    pass


class C:
    pass


class D:
    pass


class E:
    pass


class God(A, B, C, D, E):
    def m1(self, x):
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
        return r

    def m2(self, x):
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
        return r

    def m3(self, x):
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
        return r

    def m4(self, x):
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
        return r
