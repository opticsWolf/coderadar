"""Switching projects at runtime, end to end.

`codegraph_set_project` re-runs the whole startup sequence against a new
root — config in, process moved, index restarted — from inside a tool call.
These drive it through the real `analyze`, the real background index and
the real config loader, because the interesting failures are exactly the
ones a stub would hide: ids prefixed with the wrong walk, config from the
old project still in force, a graph that answers with half of each project.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from coderadar.mcp import lazy, server as server_mod, startup
from coderadar.mcp.lazy import LazyRootRetry
from coderadar.mcp.roots import ResolvedRoot
from coderadar.mcp.server import _set_project


@pytest.fixture(autouse=True)
def _handles(tmp_path, monkeypatch):
    """A clean handle set per test, and the cwd restored afterwards."""
    lazy.configure(None)
    startup.configure(None)
    previous = Path(os.getcwd())
    yield
    os.chdir(previous)
    lazy.configure(None)
    startup.configure(None)


def _make_project(root: Path, fn_name: str, marker: bool = True,
                  config: str | None = None) -> Path:
    if marker:
        (root / ".coderadar" / "store").mkdir(parents=True)
    else:
        root.mkdir(parents=True)
    (root / "m.py").write_text(
        f"def {fn_name}():\n    return 1\n", encoding="utf-8")
    if config is not None:
        (root / ".coderadar.toml").write_text(config, encoding="utf-8")
    return root


@pytest.fixture
def proj_a(tmp_path) -> Path:
    return _make_project(tmp_path / "proj_a", "alpha_fn")


@pytest.fixture
def proj_b(tmp_path) -> Path:
    # A distinctive valid knob so the config-follows test can see it move.
    return _make_project(
        tmp_path / "proj_b", "beta_fn",
        config="[resolution]\nmin_confidence = 0.75\n")


def _serve_from(root: Path) -> None:
    """Pretend startup chose `root`: chdir + install both handles."""
    os.chdir(root)
    lazy.configure(LazyRootRetry(
        ResolvedRoot(path=root.resolve(), source="--path",
                     marker=root / ".coderadar"),
        startup.BackgroundIndex(),
    ))
    startup.configure(startup.BackgroundIndex())


def _wait_ready() -> None:
    outcome = startup.current().wait(timeout=60)
    assert outcome.ready, f"index failed: {outcome.error}"


class TestSwitching:
    def test_reads_answer_from_the_new_project_only(self, proj_a, proj_b):
        import coderadar

        _serve_from(proj_a)
        report = _set_project(str(proj_b))
        assert f"Switched to `{proj_b.resolve()}`" in report
        _wait_ready()

        names = [row.get("name")
                 for row in coderadar.CodeGraph().query("functions")]
        assert "beta_fn" in names
        assert "alpha_fn" not in names

    def test_a_file_and_a_subdirectory_select_the_project(self, proj_a, proj_b):
        import coderadar

        _serve_from(proj_a)
        (proj_b / "src").mkdir()

        assert "Switched" in _set_project(str(proj_b / "src"))
        _wait_ready()
        assert "beta_fn" in [r.get("name") for r in
                             coderadar.CodeGraph().query("functions")]

    def test_switching_back_restores_the_first_project(self, proj_a, proj_b):
        import coderadar

        _serve_from(proj_a)
        _set_project(str(proj_b))
        _wait_ready()
        _set_project(str(proj_a))
        _wait_ready()

        names = [row.get("name")
                 for row in coderadar.CodeGraph().query("functions")]
        assert "alpha_fn" in names
        assert "beta_fn" not in names


class TestConfirmation:
    def test_an_unmarked_root_is_refused_until_confirmed(self, tmp_path, proj_a):
        bare = _make_project(tmp_path / "bare", "gamma_fn", marker=False)
        _serve_from(proj_a)

        refused = _set_project(str(bare))
        assert "confirm=true" in refused
        assert Path(os.getcwd()).resolve() == proj_a.resolve()

    def test_confirm_serves_the_unmarked_root_and_says_so(self, tmp_path, proj_a):
        bare = _make_project(tmp_path / "bare", "gamma_fn", marker=False)
        _serve_from(proj_a)

        report = _set_project(str(bare), confirm=True)
        assert "unconfirmed" in report
        assert Path(os.getcwd()).resolve() == bare.resolve()


class TestIdempotenceAndConfig:
    def test_switching_to_the_current_root_changes_nothing(self, proj_a):
        _serve_from(proj_a)
        report = _set_project(str(proj_a))
        assert "Already serving" in report

    def test_the_new_projects_config_takes_effect(self, proj_a, proj_b):
        from coderadar._core import get_config

        _serve_from(proj_a)
        _set_project(str(proj_b))

        cfg = get_config()
        assert cfg.get("resolution", {}).get("min_confidence") == pytest.approx(0.75)

    def test_a_broken_config_warns_but_still_switches(self, proj_a, proj_b):
        (proj_b / ".coderadar.toml").write_text("[resolution\n", encoding="utf-8")
        _serve_from(proj_a)

        report = _set_project(str(proj_b))

        assert "WARNING: config not applied" in report
        # The switch itself went ahead — an unreadable config must not leave
        # the server stranded on the old project.
        assert Path(os.getcwd()).resolve() == proj_b.resolve()


class TestLazyRetryRetirement:
    def test_an_explicit_switch_ends_the_roots_list_dance(self, proj_a, proj_b):
        _serve_from(proj_a)
        _set_project(str(proj_b))

        retry = lazy.current()
        assert retry is not None
        assert not retry.should_ask()
        assert retry.resolved.path == proj_b.resolve()


class TestMutationStaysConfined:
    def test_an_escape_path_into_the_old_project_never_writes(self, proj_a, proj_b):
        """After switching to B, a mutation naming A's file must not land.

        Whatever rejects it — the policy confining writes to the indexed
        root, or the planner refusing a path outside the graph — the file on
        disk must come out untouched. That is the property the whole
        one-writable-project story rests on.
        """
        before = (proj_a / "m.py").read_text(encoding="utf-8")

        _serve_from(proj_a)
        _set_project(str(proj_b))
        _wait_ready()

        import coderadar
        answer = server_mod._replace_body(
            coderadar.CodeGraph(),
            entity_id=f"{proj_a / 'm.py'}::alpha_fn",
            new_body="    return 999\n",
            expected_hash=None,
            dry_run=False,
        )

        assert (proj_a / "m.py").read_text(encoding="utf-8") == before
        # And the answer must not pretend it worked.
        assert "**Applied**" not in answer


class TestInFlightHonesty:
    def test_a_call_racing_the_switch_never_answers_with_the_old_project(self,
                                                                         proj_a,
                                                                         proj_b):
        """Right after a switch, a read either waits or reports progress —

        it never answers from the old project's graph."""
        _serve_from(proj_a)
        report = _set_project(str(proj_b))
        assert "background" in report

        answer = server_mod._search(None, query="fn", kind=None, top_k=10)
        # Either the fresh graph (beta) or an honest wait/progress notice —
        # never stale alpha results presented as current.
        assert "alpha_fn" not in answer or "beta_fn" in answer
