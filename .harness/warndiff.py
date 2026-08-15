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

NOW = sys.argv[1] if len(sys.argv) > 1 else 'check_now.txt'
BASE = sys.argv[2] if len(sys.argv) > 2 else 'warn_baseline.txt'
raw = open(NOW, encoding='utf-8').read()
err_lines = [l for l in raw.splitlines() if l.startswith('error[') or l.startswith('error:')]
if err_lines:
    print('COMPILE ERRORS PRESENT (warning comparison unreliable):')
    for l in err_lines[:5]:
        print('  ' + l)
now = parse(NOW)
base = []
for x in open(BASE, encoding='utf-8').read().splitlines():
    if not x.strip():
        continue
    m, l = (x.split(' @ ', 1) + [''])[:2] if ' @ ' in x else (x, '')
    base.append((m, l))
# set semantics: the same (msg, loc) can be emitted once per target
# (lib + lib-test), which is noise, not a delta.
cn, cb = set(now), set(base)
added = sorted(cn - cb)
removed = sorted(cb - cn)
for m, l in added:
    print(f'ADDED:   {m}  @ {l}')
for m, l in removed:
    print(f'REMOVED: {m}  @ {l}')
print(f'unique now={len(cn)} baseline={len(cb)} added={len(added)} removed={len(removed)}')
bad = bool(err_lines) or bool(added) or bool(removed)
sys.exit(0 if not bad else 1)
