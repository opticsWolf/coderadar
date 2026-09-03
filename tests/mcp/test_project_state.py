"""v0.8 P2-3: project persistence across sessions
(guide: docs/v0.8-p2-agent-ux-guide.md).

The client launches the server from a fixed, project-independent directory;
the agent's `set_project` choice for that launch directory is recorded in
`~/.coderadar/mcp/last_projects.json` and becomes a ladder rung on the next
launch — between `--path` and cwd.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from coderadar import project_state
from coderadar.mcp import lazy, roots, server as server_mod, startup


@pytest.fixture
def home(tmp_path, monkeypatch):
    """A private home so tests never touch the real ~/.coderadar."""
    fake = tmp_path / "home"
    fake.mkdir()
    monkeypatch.setenv("HOME", str(fake))
    monkeypatch.setenv("USERPROFILE", str(fake))
    return fake


@pytest.fixture(autouse=True)
def _clean_handles(tmp_path):
    lazy.configure(None)
    startup.configure(None)
    previous = Path(os.getcwd())
    yield
    os.chdir(previous)
    lazy.configure(None)
    startup.configure(None)


def test_record_and_read_round_trip(home, tmp_path):
    launch = tmp_path / "launch"
    launch.mkdir()
    project = tmp_path / "proj"
    project.mkdir()

    project_state.record_project(launch, project)
    assert project_state.last_project_for(launch) == project

    # Another launch directory has no record.
    other = tmp_path / "elsewhere"
    other.mkdir()
    assert project_state.last_project_for(other) is None


def test_stale_record_reads_as_nothing(home, tmp_path):
    launch = tmp_path / "launch"
    launch.mkdir()
    gone = tmp_path / "deleted"
    gone.mkdir()
    project_state.record_project(launch, gone)
    gone.rmdir()

    assert project_state.last_project_for(launch) is None


def test_corrupt_record_reads_as_nothing_and_is_repairable(home, tmp_path):
    launch = tmp_path / "launch"
    launch.mkdir()
    project = tmp_path / "proj"
    project.mkdir()

    f = project_state.state_file()
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text("{not json", encoding="utf-8")
    assert project_state.last_project_for(launch) is None

    # A record over a corrupt file rewrites it cleanly.
    project_state.record_project(launch, project)
    data = json.loads(f.read_text(encoding="utf-8"))
    assert list(data.values()) == [str(project)]


def test_record_never_raises(home, tmp_path):
    # Best-effort by contract: recording into a fresh launch directory must
    # not raise, and a directory that was never recorded reads as nothing.
    project_state.record_project(tmp_path / "x", tmp_path / "y")  # no raise
    assert project_state.last_project_for(tmp_path / "x") is None


def test_windows_key_normalisation(home, tmp_path):
    launch = tmp_path / "launch"
    launch.mkdir()
    project = tmp_path / "proj"
    project.mkdir()
    if os.name != "nt":
        pytest.skip("case-insensitivity is a Windows concern")
    project_state.record_project(launch, project)
    assert project_state.last_project_for(Path(str(launch).upper())) == project


# ── The ladder rung ─────────────────────────────────────────────────────────


def test_ladder_uses_the_recorded_project(home, tmp_path):
    launch = tmp_path / "launch"
    launch.mkdir()
    project = tmp_path / "proj"
    (project / ".coderadar" / "store").mkdir(parents=True)
    project_state.record_project(launch, project)

    resolved = roots.resolve_project_root(
        cwd=str(launch), launch_cwd=launch)
    assert resolved.path == project
    assert resolved.source == roots.PREVIOUS_SESSION
    assert resolved.confirmed


def test_ladder_without_launch_cwd_ignores_the_record(home, tmp_path, monkeypatch):
    launch = tmp_path / "launch"
    launch.mkdir()
    project = tmp_path / "proj"
    (project / ".coderadar" / "store").mkdir(parents=True)
    project_state.record_project(launch, project)

    resolved = roots.resolve_project_root(cwd=str(launch))
    assert resolved.source == roots.CWD


def test_path_flag_beats_the_record(home, tmp_path):
    launch = tmp_path / "launch"
    launch.mkdir()
    recorded = tmp_path / "recorded"
    (recorded / ".coderadar" / "store").mkdir(parents=True)
    explicit = tmp_path / "explicit"
    (explicit / ".coderadar" / "store").mkdir(parents=True)
    project_state.record_project(launch, recorded)

    resolved = roots.resolve_project_root(
        path_flag=str(explicit), cwd=str(launch), launch_cwd=launch)
    assert resolved.path == explicit
    assert resolved.source == roots.PATH_FLAG


# ── set_project writes the record ──────────────────────────────────────────


def test_set_project_rewrites_the_record(home, tmp_path):
    launch = tmp_path / "launch"
    launch.mkdir()
    target = tmp_path / "proj"
    (target / ".coderadar" / "store").mkdir(parents=True)
    (target / "a.py").write_text("def a():\n    return 1\n", encoding="utf-8")

    server_mod.set_launch_cwd(launch)
    try:
        out = server_mod._set_project(str(target), confirm=False)
    finally:
        server_mod.set_launch_cwd(None)

    assert "Switched" in out or "Already serving" in out
    # chdir happened (adopt_project_root) — restore before asserting.
    os.chdir(launch)
    assert project_state.last_project_for(launch) == target


# ── Subprocess smoke: the real `mcp serve` resumes the recorded project ─────


def test_serve_subprocess_resumes_recorded_project(home, tmp_path):
    project = tmp_path / "proj_a"
    (project / ".coderadar" / "store").mkdir(parents=True)
    (project / "a.py").write_text("def f():\n    return 1\n", encoding="utf-8")
    launch = tmp_path / "launch"
    launch.mkdir()
    project_state.record_project(launch, project)

    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    proc = subprocess.run(
        [sys.executable, "-c",
         "import sys; sys.argv = ['coderadar', 'mcp', 'serve']; "
         "from coderadar.cli import main; main()"],
        cwd=str(launch), env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True, text=True, timeout=120,
    )
    log = proc.stderr + proc.stdout
    assert "previous session" in log, log
    assert str(project).lower() in log.lower(), log
