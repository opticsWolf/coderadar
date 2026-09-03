"""Fixture dirs must stay store-free.

Background: fixture dirs once accumulated 300MB+ of gitignored
`.coderadar` databases from repeated test runs (every analyze on a warm
store appends new concept versions even when nothing changed — see
docs/macrame-0.15-upgrade-notes.md). Every analyze then opened those
stores, and the per-analyze ledger-revision fold cost seconds.
Nothing in the suite needs warm fixture stores (verified: full suite
green with the dirs deleted, nothing re-plants them), so fail loudly if
one reappears instead of slowly getting slower again.
"""

from __future__ import annotations

from pathlib import Path

FIXTURES = Path(__file__).parent / "fixtures"


def test_no_coderadar_dirs_under_fixtures():
    offenders = sorted(
        str(p.parent) for p in FIXTURES.rglob(".coderadar") if p.is_dir()
    )
    assert not offenders, (
        "store directories planted under tests/fixtures (they grow a new "
        "ledger version per analyze and slow the whole suite):\n"
        + "\n".join(f"  - {o}" for o in offenders)
        + "\nDelete them; if a test needs a warm store, point it at tmp_path."
    )
