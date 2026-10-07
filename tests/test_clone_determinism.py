"""§0.2 clone determinism (DR-12): three fresh-process runs over a fixture
with exact ties and a `max_groups` truncation give byte-identical JSON.

Each run gets its own fixture copy and its own PYTHONHASHSEED. (Rust
`HashMap`s seed from OS entropy, so fresh processes already vary the
iteration order the fix removes; the seed differences are belt.)
"""

import json
import os
import shutil
import subprocess
import sys

BODY_X = """def {name}():
    a = 1
    b = 2
    c = a + b
    d = c * 2
    e = d - a
    f = e + b
    g = f * c
    return g
"""

BODY_Y = """def {name}():
    items = [10, 20, 30]
    total = 0
    for it in items:
        total = total + it
    avg = total // 3
    rem = total % 3
    out = avg + rem
    return out
"""

# Same normalized stream as BODY_X (identifiers differ, literals match) —
# a Type-2 pair with a different RAW hash.
BODY_X_RENAMED = """def {name}():
    p = 1
    q = 2
    r = p + q
    s = r * 2
    t = s - p
    u = t + q
    v = u * r
    return v
"""


def _write_fixture(root):
    root.mkdir(parents=True, exist_ok=True)
    # Byte-identical duplicates across files (the realistic clone shape):
    # same RAW hash → Type-1. A/B tie on (size, similarity, type) by design.
    (root / "aa.py").write_text(BODY_X.format(name="alpha"))
    (root / "ab.py").write_text(BODY_X.format(name="alpha"))
    (root / "ba.py").write_text(BODY_Y.format(name="gamma"))
    (root / "bb.py").write_text(BODY_Y.format(name="gamma"))
    (root / "c1.py").write_text(BODY_X.format(name="e1"))
    (root / "c2.py").write_text(BODY_X_RENAMED.format(name="e2"))


_PROBE = """\
import json, os, sys
sys.path.insert(0, {src!r})
os.chdir({fix!r})
from coderadar import analyze
g = analyze(".", create_store=True)
res = g.find_clones(min_lines=6, min_similarity=0.5, max_groups=2)
print(json.dumps(res, sort_keys=True))
"""


def _run_once(fixture_src, seed):
    work = fixture_src.parent / f"run{seed}"
    if work.exists():
        shutil.rmtree(work)
    shutil.copytree(fixture_src, work)
    src = os.path.join(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__))), "py_agent", "src")
    env = dict(os.environ, PYTHONHASHSEED=str(seed))
    proc = subprocess.run(
        [sys.executable, "-c", _PROBE.format(src=src, fix=str(work))],
        capture_output=True, text=True, timeout=600, env=env, check=False,
    )
    assert proc.returncode == 0, f"seed {seed} failed: {proc.stderr[-2000:]}"
    return proc.stdout


def test_three_runs_byte_identical(tmp_path):
    fixture = tmp_path / "fixture"
    _write_fixture(fixture)
    outs = [_run_once(fixture, seed) for seed in (0, 1, 42)]
    assert outs[0] == outs[1] == outs[2], "clone output varies run to run"
    groups = json.loads(outs[0])
    # The truncation must keep something: two Type-1 ties + the Type-2 pair
    # make three groups; max_groups=2 keeps the first two deterministically.
    assert len(groups) == 2, f"expected the truncated pair, got {len(groups)}"
    assert all(g["clone_type"] == "type-1" for g in groups)
    first_ids = [[i["entity_id"] for i in g["instances"]] for g in groups]
    assert first_ids == sorted(first_ids), f"tie order not canonical: {first_ids}"
