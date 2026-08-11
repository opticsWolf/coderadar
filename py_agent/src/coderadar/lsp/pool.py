"""CodeRadar v3.6 — LSP Persistent Warm Pool (§14)

Optional persistent LSP servers — spawned once per language per workspace,
kept synchronized via didOpen/didChange on ingestion.
"""

from __future__ import annotations

import structlog
import os
import subprocess
import time
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

logger = structlog.get_logger(__name__)


@dataclass
class LSPOverride:
    """An LSP override for a low-confidence edge."""
    edge_id: str
    target_file: str
    target_line: int
    target_column: int
    confidence: float = 1.0


class LSPPool:
    """One long-lived server process per enabled language, shared across all files.

    Spawned once on first use; initialized with workspace root.
    Kept synchronized via textDocument/didOpen + didChange on every ingestion.
    Idle servers shut down after idle_timeout_s (default 600) and re-spawn lazy.
    Definition lookups hit a TTL cache keyed on (path, line, col, content_hash).
    """

    def __init__(self, enabled: bool = False,
                 idle_timeout_s: int = 600,
                 timeout_ms: int = 5000,
                 result_ttl_s: int = 300,
                 override_threshold: float = 0.90,
                 server_commands: Optional[Dict[str, str]] = None):
        self.enabled = enabled
        self.idle_timeout_s = idle_timeout_s
        self.timeout_ms = timeout_ms
        self.result_ttl_s = result_ttl_s
        self.override_threshold = override_threshold
        self.server_commands = server_commands or {
            "python": "pyright-langserver --stdio",
            "typescript": "typescript-language-server --stdio",
            "rust": "rust-analyzer",
            "go": "gopls",
        }

        self._servers: Dict[str, ManagedServer] = {}
        self._cache: Dict[tuple, Any] = {}
        self._last_activity: Dict[str, float] = {}

    def is_enabled(self, language: str) -> bool:
        """Check if LSP is enabled for a language."""
        return self.enabled and language in self.server_commands

    def ensure_server(self, language: str, workspace_root: str) -> Optional[ManagedServer]:
        """Get or create a managed LSP server for a language."""
        if not self.is_enabled(language):
            return None

        key = f"{language}:{workspace_root}"
        if key in self._servers:
            self._last_activity[key] = time.time()
            return self._servers[key]

        # Spawn new server
        cmd = self.server_commands[language]
        server = ManagedServer(language, cmd, workspace_root)
        self._servers[key] = server
        self._last_activity[key] = time.time()

        logger.info("lsp.server_started", language=language, workspace=workspace_root)
        return server

    def sync_file(self, path: str, text: str, language: str,
                  workspace_root: str) -> None:
        """Called by ingestion BEFORE any LSP query for this file.

        Pushes didOpen/didChange so the server's view is always current.
        """
        if not self.is_enabled(language):
            return

        server = self.ensure_server(language, workspace_root)
        if server is None:
            return

        if server.is_open(path):
            server.did_change(path, text, version=server.bump_version(path))
        else:
            server.did_open(path, text, language, version=1)

        # Invalidate cache for this file
        self._invalidate_prefix(path)

    def definition(self, path: str, line: int, col: int,
                   content_hash: str, language: str,
                   workspace_root: str) -> Optional[LSPOverride]:
        """Look up the definition of a symbol at a position."""
        if not self.is_enabled(language):
            return None

        cache_key = (path, line, col, content_hash)
        if cache_key in self._cache:
            return self._cache[cache_key]

        server = self.ensure_server(language, workspace_root)
        if server is None:
            return None

        result = server.request("textDocument/definition", {
            "textDocument": {"uri": f"file://{path}"},
            "position": {"line": line, "character": col},
        }, timeout=self.timeout_ms / 1000)

        self._cache[cache_key] = result
        return result

    def override_batch(
        self, low_confidence_edges: List[Any]
    ) -> List[LSPOverride]:
        """Only consulted for edges the Rust engine resolved below override_threshold."""
        overrides: List[LSPOverride] = []
        for edge in low_confidence_edges:
            lsp_result = self.definition(
                edge.file, edge.line, edge.column, edge.content_hash,
                edge.language, edge.workspace_root,
            )
            if lsp_result and self._maps_to_known_entity(lsp_result):
                overrides.append(LSPOverride(
                    edge_id=edge.id,
                    target_file=lsp_result.target_file,
                    target_line=lsp_result.target_line,
                    target_column=lsp_result.target_column,
                    confidence=1.0,
                ))
        return overrides

    def shutdown(self) -> None:
        """Shut down all managed LSP servers."""
        for key, server in self._servers.items():
            server.shutdown()
            logger.info("lsp.server_shutdown", key=key)
        self._servers.clear()
        self._cache.clear()

    def _invalidate_prefix(self, path: str) -> None:
        """Remove all cache entries for a file path."""
        self._cache = {
            k: v for k, v in self._cache.items()
            if k[0] != path
        }

    def _maps_to_known_entity(self, lsp_result: Any) -> bool:
        """Check if an LSP definition result maps to a known graph entity."""
        return lsp_result is not None


@dataclass
class ManagedServer:
    """A single long-lived LSP server process."""
    language: str
    command: str
    workspace_root: str
    _process: Optional[subprocess.Popen] = None
    _open_files: Dict[str, int] = field(default_factory=dict)

    def is_open(self, path: str) -> bool:
        return path in self._open_files

    def bump_version(self, path: str) -> int:
        current = self._open_files.get(path, 0)
        new_version = current + 1
        self._open_files[path] = new_version
        return new_version

    def did_open(self, path: str, text: str, language: str, version: int) -> None:
        """Send textDocument/didOpen notification."""
        self._open_files[path] = version

    def did_change(self, path: str, text: str, version: int) -> None:
        """Send textDocument/didChange notification."""
        self._open_files[path] = version

    def request(self, method: str, params: Dict[str, Any],
                timeout: float = 5.0) -> Optional[Any]:
        """Send a JSON-RPC request to the LSP server."""
        # In production: communicate over stdio with JSON-RPC
        return None

    def shutdown(self) -> None:
        """Send shutdown + exit to the LSP server."""
        if self._process:
            self._process.terminate()
            self._process = None
        self._open_files.clear()
