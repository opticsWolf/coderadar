"""Item 7: one exclusion matcher, every pass.

Covers the Python side of first-class exclusion: the shared predicate and
file iterator honor the baseline + user config, and the `exclude`
command group round-trips patterns through `.coderadar.toml`.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from click.testing import CliRunner

from coderadar.cli import main
from coderadar.excludes import FALLBACK_BASELINE, is_excluded, iter_project_files

try:
    import coderadar._core  # noqa: F401
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


def _project(tmp_path: Path) -> Path:
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "a.py").write_text("def f():\n    pass\n")
    (tmp_path / ".venv" / "lib").mkdir(parents=True)
    (tmp_path / ".venv" / "lib" / "junk.py").write_text("x = 1\n")
    (tmp_path / "target").mkdir()
    (tmp_path / "target" / "gen.py").write_text("y = 2\n")
    return tmp_path


def test_baseline_excludes_build_and_venv_dirs(tmp_path):
    root = _project(tmp_path)
    assert is_excluded(root / ".venv" / "lib" / "junk.py", root)
    assert is_excluded(root / "target" / "gen.py", root)
    assert not is_excluded(root / "src" / "a.py", root)


def test_iter_project_files_never_descends_into_excludes(tmp_path):
    root = _project(tmp_path)
    seen = {p.name for p in iter_project_files(root, suffixes=(".py",))}
    assert seen == {"a.py"}


def test_user_config_exclude_is_honored(tmp_path):
    root = _project(tmp_path)
    (tmp_path / "scratch").mkdir()
    (tmp_path / "scratch" / "b.py").write_text("z = 3\n")
    assert not is_excluded(root / "scratch" / "b.py", root)
    (tmp_path / ".coderadar.toml").write_text('[project]\nexclude = ["scratch/"]\n')
    # The shared matcher reads the active config; the CLI-equivalent push
    # is what `exclude add` + `_activate` do — emulate it here.
    from coderadar.config import activate_config
    activate_config(root)
    try:
        assert is_excluded(root / "scratch" / "b.py", root)
        assert not is_excluded(root / "src" / "a.py", root)
        seen = {p.name for p in iter_project_files(root, suffixes=(".py",))}
        assert "b.py" not in seen
        assert "a.py" in seen
    finally:
        activate_config(Path("."))


def test_fallback_baseline_covers_rust_defaults():
    rust_defaults = {
        ".venv/", "venv/", "node_modules/", "site-packages/", "target/",
        "dist/", "build/", "__pycache__/", ".mypy_cache/", ".tox/",
        ".git/", ".hg/", ".svn/", ".coderadar/", ".pytest_cache/",
    }
    assert rust_defaults <= set(FALLBACK_BASELINE), (
        "fallback copy drifted from Rust DEFAULT_EXCLUDES — keep in sync"
    )


def test_exclude_add_list_remove_round_trip(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    runner = CliRunner()
    out = runner.invoke(main, ["exclude", "add", "scratch/"])
    assert out.exit_code == 0, out.output
    assert (tmp_path / ".coderadar.toml").exists()
    out = runner.invoke(main, ["exclude", "list"])
    assert out.exit_code == 0, out.output
    assert "scratch/" in out.output
    assert ".venv/" in out.output  # baseline visible, not implicit
    out = runner.invoke(main, ["exclude", "add", "scratch/"])
    assert "Already excluded" in out.output
    out = runner.invoke(main, ["exclude", "remove", "scratch/"])
    assert out.exit_code == 0, out.output
    out = runner.invoke(main, ["exclude", "remove", "scratch/"])
    assert "Not excluded" in out.output


def test_exclude_list_shows_gitignore_layer_and_effect(tmp_path, monkeypatch):
    # R1§6-13 follow-up: the .gitignore layer used to be invisible and the
    # stack had no effect numbers — list shows layers plus engine effect.
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".gitignore").write_text("*.log\n# comment\n\n")
    (tmp_path / "a.py").write_text("x = 1\n")
    (tmp_path / "b.log").write_text("noise\n")
    runner = CliRunner()
    out = runner.invoke(main, ["exclude", "list"])
    assert out.exit_code == 0, out.output
    assert "gitignore" in out.output
    assert "*.log" in out.output
    assert "comment" not in out.output  # comments are not patterns
    assert "Effect on" in out.output
    assert "3 file(s)" in out.output  # .gitignore + a.py + b.log walked
