"""MCP front door: `create_server` wraps the Rust ops as tools."""

from .server import SERVER_INSTRUCTIONS, create_server, requires_index

__all__ = ["SERVER_INSTRUCTIONS", "create_server", "requires_index"]
