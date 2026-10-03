"""Ratchet on the call-graph benchmark: numbers may rise, never fall.

Refresh the recorded numbers after an intentional improvement with
``PRECISION_UPDATE=1 pytest tests/precision -s``.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from .harness import dangling_targets, format_report, measure

try:
    from coderadar import _core
except ImportError:  # pragma: no cover
    _core = None

pytestmark = pytest.mark.skipif(
    _core is None or not hasattr(_core, "call_sites"), reason="needs the built _core extension")

HERE = Path(__file__).parent
ROOT = HERE.parent.parent
BASELINE = HERE / "baseline.json"
SLACK = 0.005  # half a point of noise

CORPORA = {
    "fixtures": HERE / "fixtures" / "py_shapes",
    "self": ROOT / "py_agent" / "src",
}
METRICS = ("extraction_recall", "in_repo_rate", "resolution_precision", "resolution_recall")


def _load() -> dict:
    return json.loads(BASELINE.read_text("utf-8")) if BASELINE.exists() else {}


@pytest.mark.parametrize("corpus", sorted(CORPORA))
def test_no_dangling_edge_targets(corpus):
    assert dangling_targets(CORPORA[corpus]) == []


@pytest.mark.parametrize("corpus", sorted(CORPORA))
def test_no_regression(corpus):
    result = measure(CORPORA[corpus])
    print("\n" + format_report(corpus, result))
    for line in result["mismatches"][:40]:
        print("   ", line)

    if os.environ.get("PRECISION_UPDATE"):
        data = _load()
        data[corpus] = {k: round(result[k], 4) for k in METRICS}
        BASELINE.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", "utf-8")
        return

    base = _load().get(corpus)
    assert base, f"no baseline for {corpus!r}; run with PRECISION_UPDATE=1"
    for k in METRICS:
        assert result[k] >= base[k] - SLACK, \
            f"{corpus}: {k} fell {base[k]:.1%} -> {result[k]:.1%}"
