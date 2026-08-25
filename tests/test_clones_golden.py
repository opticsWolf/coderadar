"""Day-one golden tests for Stage 2 clone detection (plan §13.1).

Asserts exact grouping on tests/fixtures/python/clones/ — a Type-1 pair, a
renamed Type-2 body, and a structurally unrelated control that must never be
grouped.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import pytest

try:
    from coderadar._core import analyze, find_clones
    _CORE_AVAILABLE = True
except ImportError:
    _CORE_AVAILABLE = False

FIXTURE = Path(__file__).parent / "fixtures" / "python" / "clones"


@pytest.mark.skipif(not _CORE_AVAILABLE, reason="Rust _core extension not built")
class TestClonesGolden:
    @pytest.fixture(autouse=True)
    def _index(self):
        analyze(str(FIXTURE))

    def _groups(self, **kwargs):
        groups = find_clones(**kwargs)
        # entity name -> the type of the group containing it
        out = {}
        for g in groups:
            for inst in g["instances"]:
                name = inst["entity_id"].split("::")[-1]
                out[name] = g["clone_type"]
        return out, groups

    def test_identical_bodies_are_type1(self):
        mapping, _ = self._groups(min_lines=4)
        assert mapping.get("clone_a") == "type-1"
        assert mapping.get("clone_b") == "type-1"

    def test_renamed_body_is_type2(self):
        mapping, _ = self._groups(min_lines=4)
        assert mapping.get("clone_c") == "type-2"

    def test_unrelated_body_is_never_grouped(self):
        mapping, _ = self._groups(min_lines=4)
        assert "unrelated" not in mapping

    def test_type1_similarity_is_exact(self):
        _, groups = self._groups(min_lines=4)
        t1 = [g for g in groups if g["clone_type"] == "type-1"]
        assert all(g["similarity"] == 1.0 for g in t1)

    def test_min_lines_filters_short_bodies(self):
        _, groups = self._groups(min_lines=10_000)
        assert groups == []


class TestTedVerification:
    """Stage 6.1: strong shingle candidates get exact ordered-TED verification.

    Honest scope note: kind-labeled rename-blind trees make same-kind
    statement reordering cheap for TED, so strong candidates are usually
    CONFIRMED with a refined score rather than rejected; outright rejection
    requires structural divergence big enough that bag-of-shingles rarely
    proposes the pair at all. Score REFINEMENT is the observable win: this
    golden pins a pair whose shingle similarity is a misleading 1.0 being
    corrected to the true structural value.
    """

    @pytest.fixture(autouse=True)
    def _index(self, tmp_path):
        # Eight assignments of alternating shapes (1-arg / 2-arg calls).
        steps = [
            "    alpha = compute_alpha(x)",
            "    beta = adjust_beta(alpha, x)",
            "    gamma = blend(beta, alpha)",
            "    delta = verify_gamma(gamma)",
            "    epsilon = extend(delta, beta)",
            "    zeta = normalize(epsilon)",
            "    eta = project(zeta, x)",
            "    theta = finalize(eta, zeta)",
        ]

        def pipeline(name, order):
            body = "    acc = reset()\n"
            for i in order:
                body += steps[i] + "\n"
            body += "    return seal(acc)\n"
            return f"def {name}(x):\n{body}"

        helpers = "".join(
            f"def {f}(*a):\n    return a\n\n"
            for f in [
                "compute_alpha", "adjust_beta", "blend", "verify_gamma",
                "extend", "normalize", "project", "finalize", "reset", "seal",
            ]
        )
        # The swap exchanges a 1-arg with a 2-arg assignment, so the
        # normalized token streams DIFFER (Types 1-2 cannot claim the pair)
        # while the shingle multiset barely moves — the pair rides into
        # Layer C where TED gives the second opinion.
        src = helpers + pipeline("plain_pipe", list(range(8))) + "\n"
        src += pipeline("swapped_pipe", [1, 0, 3, 2, 4, 5, 6, 7]) + "\n"
        (tmp_path / "pipes.py").write_text(src, encoding="utf-8")
        analyze(str(tmp_path))

    def test_ted_upgrades_group_similarity_to_structural_truth(self):
        groups = find_clones(min_similarity=0.85)
        hits = [
            g
            for g in groups
            if {"plain_pipe", "swapped_pipe"}
            <= {i["entity_id"].split("::")[-1] for i in g["instances"]}
        ]
        assert len(hits) == 1, "LSH must surface the swapped pipe pair"
        g = hits[0]
        assert g["clone_type"] == "type-3"
        # Bag-of-shingles scores this pair at exactly 1.0 (its shingle sets
        # coincide under the swap) - the blind spot that motivated Stage
        # 6.1. Kind-labeled TED sees the two argument-count changes and
        # refines the score below exact equality while keeping the pair.
        assert 0.8 <= g["similarity"] < 1.0, (
            f"TED must refine the shingle estimate down, got {g['similarity']}"
        )

