"""Ratchet on the call-graph benchmark: numbers may rise, never fall.

Refresh the recorded numbers after an intentional improvement with
``PRECISION_UPDATE=1 pytest tests/precision -s``.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from .harness import (
    dangling_targets,
    dead_code_report,
    format_dead_report,
    format_report,
    measure,
)

try:
    from coderadar import _core
except ImportError:  # pragma: no cover
    _core = None

pytestmark = pytest.mark.skipif(
    _core is None or not hasattr(_core, "call_sites"), reason="needs the built _core extension")

HERE = Path(__file__).parent
ROOT = HERE.parent.parent
BASELINE = HERE / "baseline.json"
DEAD_GOLDEN = HERE / "deadcode_golden.json"
SLACK = 0.005  # half a point of noise

CORPORA = {
    "fixtures": HERE / "fixtures" / "py_shapes",
    "self": ROOT / "py_agent" / "src",
}
METRICS = ("extraction_recall", "in_repo_rate", "resolution_precision", "resolution_recall")


def _load() -> dict:
    return json.loads(BASELINE.read_text("utf-8")) if BASELINE.exists() else {}


def _golden() -> dict:
    return json.loads(DEAD_GOLDEN.read_text("utf-8"))


def test_dead_code_precision_on_the_labelled_sample():
    """§7.2 gate: a hand-labelled `alive` entity must never come back as a
    High or Medium finding — those are the ones a reviewer acts on."""
    m = dead_code_report(CORPORA["self"], _golden())
    print("\n" + format_dead_report("self", m))
    for line in m["false_positives"]:
        print("    false positive:", line)
    assert m["false_positives"] == [], (
        "hand-verified live code reported as High/Medium dead"
    )
    assert m["precision"] == 1.0


def test_dead_code_recall_keeps_the_verified_findings():
    m = dead_code_report(CORPORA["self"], _golden())
    assert m["missed"] == [], "verified dead code is no longer reported"


def test_dead_code_finding_budget_only_shrinks():
    """§7.2 ratchet: findings per function may fall, never rise."""
    golden = _golden()
    m = dead_code_report(CORPORA["self"], golden)
    budget = golden["budget"]
    assert m["high_medium_findings"] <= budget["high_medium_findings"], (
        f"High+Medium findings rose to {m['high_medium_findings']} "
        f"(budget {budget['high_medium_findings']})"
    )
    assert m["findings"] <= budget["findings"], (
        f"findings rose to {m['findings']} (budget {budget['findings']})"
    )


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
