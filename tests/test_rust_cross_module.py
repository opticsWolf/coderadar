"""DR-13 §0.3: Rust cross-module call resolution.

The Python L2-L3 import cascade was never ported: Rust `use` items were
recorded as raw text, `a::b()` callees were dropped (no `scoped_identifier`
arm), and `crate::`/`super::`/`self::` had no module mapping — so 1 of 6
cross-module edges resolved and the rest went `external::` or missing.

This pins the two-module precision project: every in-repo call shape
resolves, nothing leaks to `external::`.
"""

import pytest
from coderadar import analyze, ops

MAIN_RS = """mod alpha;
mod beta;

use alpha::helper as assist;

fn main() {
    assist();
    beta::run();
}
"""

ALPHA_RS = """pub fn helper() {}

fn local_caller() {
    helper();
    super::beta::run();
}
"""

BETA_RS = """use super::alpha::helper;

pub fn run() {
    helper();
    crate::alpha::helper();
}
"""


@pytest.fixture()
def project(tmp_path):
    (tmp_path / "main.rs").write_text(MAIN_RS, encoding="utf-8")
    (tmp_path / "alpha.rs").write_text(ALPHA_RS, encoding="utf-8")
    (tmp_path / "beta.rs").write_text(BETA_RS, encoding="utf-8")
    analyze(str(tmp_path))
    return tmp_path


def _callees(fid):
    return sorted(c["id"] for c in ops.callees(fid))


def test_use_alias_bare_call_resolves(project):
    # `use alpha::helper as assist;` + `assist()` — the DR-13 baseline
    # externalized this as `external::assist`.
    assert _callees("main.rs::main") == ["alpha.rs::helper", "beta.rs::run"]


def test_bare_crate_relative_path_resolves(project):
    # `mod beta;` makes `beta::run` callable with no `use` at all.
    assert "beta.rs::run" in _callees("main.rs::main")


def test_super_path_resolves(project):
    # `super::beta::run()` from alpha.rs — dropped entirely pre-fix.
    assert _callees("alpha.rs::local_caller") == [
        "alpha.rs::helper",
        "beta.rs::run",
    ]


def test_use_bound_sibling_and_crate_root_resolve(project):
    # `use super::alpha::helper;` + `helper()` AND `crate::alpha::helper()`.
    assert _callees("beta.rs::run") == ["alpha.rs::helper"]


def test_no_external_leaks_from_fixture(project):
    for fid in [
        "main.rs::main",
        "alpha.rs::local_caller",
        "alpha.rs::helper",
        "beta.rs::run",
    ]:
        assert not any(
            c.startswith("external::") for c in _callees(fid)
        ), f"{fid} leaks external"
    # And the reverse direction: helper has three in-repo callers.
    assert sorted(c["id"] for c in ops.callers("alpha.rs::helper")) == [
        "alpha.rs::local_caller",
        "beta.rs::run",
        "main.rs::main",
    ]
