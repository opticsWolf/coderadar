"""Compare current cargo check warnings against the fingerprinted baseline.
Usage: python3 warndiff.py  (reads ../.harness/check_now.txt + warn_baseline.txt)
Exit code 0 if identical, 1 otherwise."""
import sys

def parse(path):
    lines = open(path, encoding='utf-8').read().splitlines()
    out = []
    for i, l in enumerate(lines):
        if l.startswith('warning:') and 'generated' not in l:
            msg = l[len('warning:'):].strip()
            loc = ''
            for j in range(i + 1, min(i + 4, len(lines))):
                if '-->' in lines[j]:
                    loc = lines[j].split('-->')[1].strip().split(':')[0].replace('\\', '/')
                    break
            out.append((msg, loc))
    return out

now = parse('check_now.txt')
base = []
for x in open('warn_baseline.txt', encoding='utf-8').read().splitlines():
    if not x.strip():
        continue
    m, l = (x.split(' @ ', 1) + [''])[:2] if ' @ ' in x else (x, '')
    base.append((m, l))
from collections import Counter
cn, cb = Counter(now), Counter(base)
added = list((cn - cb).elements())
removed = list((cb - cn).elements())
for m, l in added:
    print(f'ADDED:   {m}  @ {l}')
for m, l in removed:
    print(f'REMOVED: {m}  @ {l}')
print(f'total now={len(now)} baseline={len(base)} added={len(added)} removed={len(removed)}')
sys.exit(0 if not added and len(now) == len(base) else 1)
