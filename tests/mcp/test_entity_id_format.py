"""v0.8 E7: entity IDs are copy-paste friendly in shells.

The graph stores entity IDs as ``.<sep>path::{name}`` with OS-native
separators (on Windows that is ``.\\path\\file.rs::name``). Backslashes are
escape characters in a shell, so an ID read off a tool result was awkward to
paste back into a command. Two things fix that without touching storage:

* ``_friendly_entity_id`` renders a stored ID with forward slashes and the
  redundant ``./`` / ``.\\`` prefix dropped — a pure presentation helper.
* ``_canonical_entity_id`` keeps accepting the friendly form (and every
  separator/prefix variant) so the pasted ID still resolves to the stored FK
  key. The stored key and its FK references never change.
"""

from __future__ import annotations

import pytest

from coderadar.mcp import server as server_mod

try:
    from coderadar._core import analyze as _analyze
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")

# The stored Windows key: ".\proj\src\lib.rs::name" (dot, backslash, ...).
STORED_WINDOWS = r".\proj\src\lib.rs::name"
FRIENDLY = "proj/src/lib.rs::name"


class TestFriendlyEntityId:
    def test_windows_stored_becomes_forward_slash_bare(self):
        assert server_mod._friendly_entity_id(STORED_WINDOWS) == FRIENDLY

    def test_posix_stored_is_unchanged(self):
        assert server_mod._friendly_entity_id("proj/src/lib.rs::name") == "proj/src/lib.rs::name"

    def test_dot_slash_prefix_is_dropped(self):
        assert server_mod._friendly_entity_id("./proj/src/lib.rs::name") == FRIENDLY

    def test_is_idempotent(self):
        once = server_mod._friendly_entity_id(STORED_WINDOWS)
        twice = server_mod._friendly_entity_id(once)
        assert once == twice == FRIENDLY

    def test_module_id_without_path_prefix(self):
        # A bare "./app.py::module" normalises the same way.
        assert server_mod._friendly_entity_id("./app.py::module") == "app.py::module"


class TestCanonicalEntityIdResolution:
    def _patch(self, monkeypatch, stored):
        monkeypatch.setattr(
            "coderadar._core.lookup_entity",
            lambda key: stored.get(key),
        )

    def test_friendly_input_resolves_to_stored_fk(self, monkeypatch):
        self._patch(monkeypatch, {STORED_WINDOWS: {"id": "1"}})
        assert server_mod._canonical_entity_id(FRIENDLY) == STORED_WINDOWS

    def test_exact_stored_form_passes_through(self, monkeypatch):
        self._patch(monkeypatch, {STORED_WINDOWS: {"id": "1"}})
        assert server_mod._canonical_entity_id(STORED_WINDOWS) == STORED_WINDOWS

    def test_mixed_separators_resolve_to_stored(self, monkeypatch):
        self._patch(monkeypatch, {STORED_WINDOWS: {"id": "1"}})
        # ".\proj/src/lib.rs::name" (backslash prefix, slash body) is mixed
        # and still resolves to the all-backslash stored key.
        mixed = r".\proj/src/lib.rs::name"
        assert server_mod._canonical_entity_id(mixed) == STORED_WINDOWS

    def test_unmatched_id_is_returned_unchanged(self, monkeypatch):
        self._patch(monkeypatch, {})
        # Nothing matched — the caller's "not found" path relies on the
        # original string being returned, not a fabricated one.
        assert server_mod._canonical_entity_id("does/not/exist::nope") == "does/not/exist::nope"
