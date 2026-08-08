"""CodeRadar MCP Server — §26 Agent Interface Design

Exposes the four-tool MCP surface over stdio using MCP v2's
MCPServer decorator API.
"""

from .server import create_server, serve, SERVER_INSTRUCTIONS

__all__ = [
    "create_server",
    "serve",
    "SERVER_INSTRUCTIONS",
]
