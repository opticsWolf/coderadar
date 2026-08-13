"""Fixture with known architectural smells for the native smell engine.

- BigDataClass: 11 fields, no logic → too-many-fields + data-class
- too_many_params: 5 params → long-parameter-list
- complex_logic: 10 ifs + 1 for → cyclomatic 12 → high-cyclomatic-complexity
  (+ long-method via the cyclomatic branch)
"""


class BigDataClass:
    a = 1
    b = 2
    c = 3
    d = 4
    e = 5
    f = 6
    g = 7
    h = 8
    i = 9
    j = 10
    k = 11


def too_many_params(a, b, c, d, e):
    return a + b + c + d + e


def complex_logic(x):
    result = 0
    if x > 0:
        result += 1
    if x > 1:
        result += 2
    if x > 2:
        result += 3
    if x > 3:
        result += 4
    if x > 4:
        result += 5
    if x > 5:
        result += 6
    if x > 6:
        result += 7
    if x > 7:
        result += 8
    if x > 8:
        result += 9
    if x > 9:
        result += 10
    for i in range(3):
        result += i
    return result
