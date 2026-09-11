"""Helpers: clone pair + one long smelly function."""


def helper(x):
    return x * 2


def combine(items):
    total = 0
    for i in items:
        total += i
    return total


def combine_copy(items):
    total = 0
    for i in items:
        total = total + i
    return total


def sprawling_report(rows, threshold=10, verbose=False, prefix="", limit=100):
    out = []
    count = 0
    for r in rows:
        if r is None:
            continue
        if isinstance(r, dict):
            v = r.get("v", 0)
        elif isinstance(r, (list, tuple)):
            v = r[0] if r else 0
        else:
            v = r
        if v > threshold:
            if verbose:
                out.append(f"{prefix}big:{v}")
            else:
                out.append(str(v))
            count += 1
        else:
            if verbose:
                out.append(f"{prefix}small:{v}")
            else:
                out.append(".")
        if count >= limit:
            break
    if verbose:
        return "\n".join(out)
    return "".join(out)
