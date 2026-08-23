"""The server has to find the project before it can serve it.

`rootUri`/`workspaceFolders` do not exist in MCP, so the only signals are the
client's `roots/list`, the `--path` flag, and the cwd — and the cwd is the
weakest of the three, because MCP clients launch servers from wherever they
happen to be. These cover the ladder, the walk-up, and the chdir that makes
cwd and root the same directory.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from coderadar.mcp.roots import (
    CLIENT_ROOT,
    CWD,
    PATH_FLAG,
    ResolvedRoot,
    adopt_project_root,
    describe,
    find_marker,
    resolve_project_root,
    uri_to_path,
)


def _project(root: Path, marker: str = ".coderadar") -> Path:
    """A directory that looks initialised, plus a nested working directory."""
    if marker == ".coderadar":
        (root / marker / "store").mkdir(parents=True)
    else:
        (root / marker).write_text("[project]\n", encoding="utf-8")
    nested = root / "src" / "deep" / "nested"
    nested.mkdir(parents=True)
    return nested


class TestFindMarker:
    def test_a_marker_directory_is_found_from_deep_inside(self, tmp_path):
        nested = _project(tmp_path)
        assert find_marker(nested) == tmp_path / ".coderadar"

    def test_a_config_file_counts_as_a_marker(self, tmp_path):
        nested = _project(tmp_path, ".coderadar.toml")
        assert find_marker(nested) == tmp_path / ".coderadar.toml"

    def test_an_uninitialised_tree_finds_nothing(self, tmp_path):
        nested = tmp_path / "a" / "b"
        nested.mkdir(parents=True)
        # tmp_path is somewhere under the system temp dir, which is not a
        # CodeRadar project; if this ever fails, something planted a marker.
        assert find_marker(nested) is None

    def test_a_file_is_read_as_the_directory_holding_it(self, tmp_path):
        nested = _project(tmp_path)
        source = nested / "m.py"
        source.write_text("x = 1\n", encoding="utf-8")
        assert find_marker(source) == tmp_path / ".coderadar"


class TestTheLadder:
    def test_the_client_root_outranks_the_flag_and_the_cwd(self, tmp_path):
        client = tmp_path / "client"
        flag = tmp_path / "flag"
        cwd = tmp_path / "cwd"
        for d in (client, flag, cwd):
            _project(d)

        resolved = resolve_project_root(
            client_roots=[client.as_uri()], path_flag=str(flag), cwd=str(cwd))
        assert resolved.path == client.resolve()
        assert resolved.source == CLIENT_ROOT

    def test_the_flag_outranks_the_cwd(self, tmp_path):
        flag = tmp_path / "flag"
        cwd = tmp_path / "cwd"
        for d in (flag, cwd):
            _project(d)

        resolved = resolve_project_root(path_flag=str(flag), cwd=str(cwd))
        assert resolved.path == flag.resolve()
        assert resolved.source == PATH_FLAG

    def test_the_cwd_is_the_last_rung(self, tmp_path):
        cwd = tmp_path / "cwd"
        _project(cwd)
        resolved = resolve_project_root(cwd=str(cwd))
        assert resolved.path == cwd.resolve()
        assert resolved.source == CWD

    def test_a_confirmed_lower_rung_beats_an_unconfirmed_higher_one(self, tmp_path):
        """Evidence beats preference.

        A client that declares a root with no marker in it has told us where
        it is, not where the project is. If the flag points at a directory
        that actually carries a marker, that is the better answer.
        """
        client = tmp_path / "client"
        client.mkdir()
        flag = tmp_path / "flag"
        _project(flag)

        resolved = resolve_project_root(
            client_roots=[client.as_uri()], path_flag=str(flag), cwd=str(tmp_path))
        assert resolved.path == flag.resolve()
        assert resolved.source == PATH_FLAG
        assert resolved.confirmed

    def test_the_walk_up_wins_over_the_candidate_itself(self, tmp_path):
        nested = _project(tmp_path)
        resolved = resolve_project_root(cwd=str(nested))
        assert resolved.path == tmp_path.resolve()
        assert resolved.marker == (tmp_path / ".coderadar").resolve()

    def test_nothing_confirmed_anywhere_still_answers_and_says_so(self, tmp_path):
        bare = tmp_path / "bare"
        bare.mkdir()
        resolved = resolve_project_root(path_flag=str(bare), cwd=str(tmp_path))
        assert resolved.path == bare.resolve()
        assert resolved.source == PATH_FLAG
        assert not resolved.confirmed
        assert "no .coderadar marker" in describe(resolved)

    def test_a_nonexistent_candidate_is_skipped_not_fatal(self, tmp_path):
        cwd = tmp_path / "cwd"
        _project(cwd)
        resolved = resolve_project_root(
            path_flag=str(tmp_path / "does-not-exist"), cwd=str(cwd))
        assert resolved.path == cwd.resolve()
        assert resolved.source == CWD

    def test_the_path_is_canonicalised(self, tmp_path):
        _project(tmp_path)
        crooked = tmp_path / "src" / ".." / "src" / "deep" / ".."
        resolved = resolve_project_root(cwd=str(crooked))
        assert resolved.path == tmp_path.resolve()
        assert ".." not in str(resolved.path)


class TestRootUris:
    def test_a_file_uri_becomes_a_path(self, tmp_path):
        assert uri_to_path(tmp_path.as_uri()) == tmp_path

    def test_percent_escapes_are_decoded(self, tmp_path):
        spaced = tmp_path / "my project"
        spaced.mkdir()
        assert "%20" in spaced.as_uri()
        assert uri_to_path(spaced.as_uri()) == spaced

    def test_a_non_file_scheme_is_declined_rather_than_guessed(self):
        # Falling through to the next rung beats misreading a URI we do not
        # understand as a local path.
        assert uri_to_path("https://example.com/repo") is None

    def test_an_unusable_root_does_not_sink_the_ladder(self, tmp_path):
        cwd = tmp_path / "cwd"
        _project(cwd)
        resolved = resolve_project_root(
            client_roots=["https://example.com/repo"], cwd=str(cwd))
        assert resolved.path == cwd.resolve()


class TestAdoptingTheRoot:
    """Root and cwd have to be the same directory.

    Entity ids are prefixed with the path `analyze()` walked, while every read
    helper resolves against the process cwd. Serving one directory from
    another produced ids under one prefix and lookups under another, and the
    agent saw an empty project.
    """

    def test_the_process_moves_onto_the_root(self, tmp_path):
        nested = _project(tmp_path)
        previous = Path(os.getcwd())
        resolved = resolve_project_root(cwd=str(nested))
        try:
            returned = adopt_project_root(resolved)
            assert Path(os.getcwd()).resolve() == tmp_path.resolve()
            assert returned == previous
        finally:
            os.chdir(previous)

    def test_relative_and_absolute_agree_afterwards(self, tmp_path):
        nested = _project(tmp_path)
        previous = Path(os.getcwd())
        try:
            adopt_project_root(resolve_project_root(cwd=str(nested)))
            # This is the property `_reindex`'s analyze('.') depends on.
            assert Path(".").resolve() == tmp_path.resolve()
        finally:
            os.chdir(previous)


class TestTheWalkUpHasABoundary:
    """`~/.coderadar` is a user directory, not a project.

    CodeRadar keeps a config and store under the home directory, and an
    unbounded walk-up would find it from anywhere below and declare the whole
    home directory to be one enormous project. The walk stops before home.
    """

    def test_a_marker_at_home_is_not_adopted(self, tmp_path, monkeypatch):
        import coderadar.mcp.roots as roots

        home = tmp_path / "home"
        (home / ".coderadar").mkdir(parents=True)
        nested = home / "code" / "someproject" / "src"
        nested.mkdir(parents=True)
        monkeypatch.setattr(roots, "_home", lambda: home.resolve())

        assert roots.find_marker(nested) is None

    def test_a_real_project_under_home_is_still_found(self, tmp_path, monkeypatch):
        import coderadar.mcp.roots as roots

        home = tmp_path / "home"
        (home / ".coderadar").mkdir(parents=True)
        project = home / "code" / "someproject"
        (project / ".coderadar").mkdir(parents=True)
        nested = project / "src" / "deep"
        nested.mkdir(parents=True)
        monkeypatch.setattr(roots, "_home", lambda: home.resolve())

        assert roots.find_marker(nested) == project / ".coderadar"

    def test_a_marker_above_home_is_not_adopted_either(self, tmp_path, monkeypatch):
        import coderadar.mcp.roots as roots

        above = tmp_path / "users"
        home = above / "someone"
        home.mkdir(parents=True)
        (above / ".coderadar.toml").write_text("[project]\n", encoding="utf-8")
        nested = home / "work"
        nested.mkdir()
        monkeypatch.setattr(roots, "_home", lambda: home.resolve())

        assert roots.find_marker(nested) is None
