"""Language-server pool, backed by the Rust resolver."""

from dataclasses import dataclass, field
from pathlib import Path

from .. import lsp_definition, lsp_override_edges, lsp_status


@dataclass
class LSPOverride:
    """One low-confidence edge and the definition the server found."""

    entity_id: str
    file: str
    line: int
    column: int
    target: str | None = None


@dataclass
class ManagedServer:
    """A configured language server (the engine spawns it per request)."""

    language: str
    command: str
    root: Path = field(default_factory=Path.cwd)


class LSPPool:
    """Query the configured servers; the engine owns their lifecycle."""

    def __init__(self, root=None):
        self.root = Path(root) if root is not None else Path.cwd()

    def status(self):
        """The engine's LSP settings."""
        return lsp_status()

    def definition(self, language, path, line, column):
        """Ask for the definition at a position."""
        return lsp_definition(language, str(path), line, column, str(self.root))

    def override(self, edges):
        """Apply the override threshold to edge rows."""
        return lsp_override_edges(edges, str(self.root))


__all__ = ["LSPOverride", "LSPPool", "ManagedServer"]
