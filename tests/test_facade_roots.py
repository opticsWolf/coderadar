"""MCP root ladder through the Rust bridge (S7 unit).

Mirrors the reference suite (`tests/mcp/test_root_resolution.py`): the ladder
order, the walk-up, the confirmed-beats-unconfirmed rule, URI decoding and the
selector for files/subdirectories. Everything but `client_roots` is Rust.
"""

from __future__ import annotations

import importlib
from pathlib import Path

from tests._rust_bridge import rust_bridge


def roots_module():
    rust_bridge()
    return importlib.import_module("coderadar_rs.mcp.roots")


def project(root: Path, marker: str = ".coderadar") -> Path:
    if marker == ".coderadar":
        (root / marker / "store").mkdir(parents=True)
    else:
        (root / marker).write_text("[project]\n", encoding="utf-8")
    nested = root / "src" / "deep" / "nested"
    nested.mkdir(parents=True)
    return nested


def test_a_marker_is_found_from_deep_inside(tmp_path: Path) -> None:
    roots = roots_module()
    nested = project(tmp_path)
    assert roots.find_marker(nested) == tmp_path / ".coderadar"
    assert roots.find_marker(tmp_path / ".coderadar") == tmp_path / ".coderadar"


def test_a_config_file_counts_as_a_marker(tmp_path: Path) -> None:
    roots = roots_module()
    nested = project(tmp_path, ".coderadar.toml")
    assert roots.find_marker(nested) == tmp_path / ".coderadar.toml"


def test_an_uninitialised_tree_finds_nothing(tmp_path: Path) -> None:
    roots = roots_module()
    nested = tmp_path / "a" / "b"
    nested.mkdir(parents=True)
    assert roots.find_marker(nested) is None


def test_the_client_root_outranks_the_flag_and_the_cwd(tmp_path: Path) -> None:
    roots = roots_module()
    client = tmp_path / "client"
    flagged = tmp_path / "flagged"
    cwd = tmp_path / "cwd"
    for directory in (client, flagged, cwd):
        project(directory)
    uri = "file:///" + str(client).replace("\\", "/").lstrip("/")
    resolved = roots.resolve_project_root([uri], str(flagged), str(cwd))
    assert resolved.path == client
    assert resolved.source == roots.CLIENT_ROOT
    assert resolved.confirmed


def test_the_flag_outranks_the_cwd_and_a_confirmed_root_beats_an_unconfirmed_one(
    tmp_path: Path,
) -> None:
    roots = roots_module()
    flagged = tmp_path / "flagged"
    cwd = tmp_path / "cwd"
    project(flagged)
    bare = cwd / "src"
    bare.mkdir(parents=True)
    resolved = roots.resolve_project_root(None, str(flagged), str(bare))
    assert resolved.path == flagged
    assert resolved.source == roots.PATH_FLAG


def test_the_walk_up_wins_over_the_candidate_itself(tmp_path: Path) -> None:
    roots = roots_module()
    nested = project(tmp_path)
    resolved = roots.resolve_project_root(cwd=str(nested))
    assert resolved.path == tmp_path
    assert resolved.confirmed


def test_nothing_confirmed_still_answers_and_says_so(tmp_path: Path) -> None:
    roots = roots_module()
    bare = tmp_path / "bare"
    bare.mkdir()
    resolved = roots.resolve_project_root(cwd=str(bare))
    assert resolved.path == bare
    assert resolved.source == roots.CWD
    assert not resolved.confirmed
    assert "no .coderadar marker" in roots.describe(resolved)


def test_uri_decoding_matches_the_reference() -> None:
    roots = roots_module()
    assert roots.uri_to_path("") is None
    assert roots.uri_to_path("https://example.com/x") is None
    assert roots.uri_to_path("file:///tmp/a%20b") == Path("/tmp/a b")
    windows = roots.uri_to_path("file:///C:/x%20y/z")
    assert windows is not None
    assert "C:" in str(windows) and "x y" in str(windows)


def test_selector_resolves_files_and_subdirectories_to_the_root(tmp_path: Path) -> None:
    roots = roots_module()
    nested = project(tmp_path)
    source = nested / "m.py"
    source.write_text("x = 1\n", encoding="utf-8")
    for probe in (source, nested, tmp_path):
        resolved = roots.resolve_selector(str(probe))
        assert resolved is not None
        assert resolved.path == tmp_path
        assert resolved.source == roots.PROJECT_PATH
        assert resolved.confirmed
    # A name below a real directory is read as a file of that directory; a
    # path whose every component is missing is a caller error (Python agrees).
    tail = roots.resolve_selector(str(tmp_path / "missing"))
    assert tail is not None
    assert tail.path == tmp_path
    assert tail.confirmed
    assert roots.resolve_selector(str(tmp_path / "missing" / "deeper")) is None


def test_adopt_project_root_moves_the_process_and_reports_the_old_cwd(
    tmp_path: Path, monkeypatch
) -> None:
    roots = roots_module()
    nested = project(tmp_path)
    previous = Path.cwd()
    monkeypatch.chdir(nested)
    resolved = roots.resolve_project_root(cwd=str(nested))
    try:
        old = roots.adopt_project_root(resolved)
        assert Path.cwd().resolve() == tmp_path
        assert old.resolve() == nested
    finally:
        monkeypatch.chdir(previous)
