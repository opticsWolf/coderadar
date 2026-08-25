"""Stage 6.3 golden tests: RTA-lite dispatch sharpening for dead code.

Virtual-dispatch liveness (a reachable base method keeps every override
alive) over-approximates: an override on a class that is never constructed
anywhere in the indexed root is a dead-code false negative. RTA-lite re-flags
exactly those overrides with the weakest evidence tier — it never demotes
anything the base detector calls live.

Construction evidence is conservative: a class counts as instantiated when a
resolved Constructor edge exists OR any raw call site matches its simple
name. Name collisions only suppress findings — the safe direction.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, find_dead_code
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

BASES = """class Base:
    def __init__(self):
        self._setup()

    def _setup(self):
        self.value = 1

class Derived(Base):
    def _setup(self):
        self.value = 2

def main():
    b = Base()
    return b
"""


def run(src: str):
    import tempfile

    d = tempfile.mkdtemp()
    Path(d, "bases.py").write_text(src, encoding="utf-8")
    analyze(d)
    return find_dead_code(0.0)


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestRtaLite:
    def test_uninstantiated_override_flagged_rta_dead(self):
        findings = run(BASES)
        hits = [f for f in findings if f["kind"] == "rta-dead"]
        assert len(hits) == 1, "Derived._setup must be the single RTA finding"
        assert hits[0]["entity_id"].endswith("Derived._setup")
        # Weakest evidence class: never reaches confident tiers.
        assert hits[0]["score"] < 0.8

    def test_constructed_class_suppresses_rta_finding(self):
        src = BASES.replace(
            "def main():\n    b = Base()\n    return b\n",
            "def main():\n    d = Derived()\n    return d\n",
        )
        findings = run(src)
        assert not [
            f for f in findings if "Derived" in f["entity_id"]
        ], "constructing Derived keeps its override honestly live"

    def test_directly_called_override_not_flagged(self):
        # Base._setup is reached by a direct call edge from __init__, so it
        # must never be an RTA finding even though no instance exists either.
        findings = run(BASES)
        base_setup = [
            f for f in findings if f["entity_id"].endswith("Base._setup")
        ]
        assert base_setup == []
