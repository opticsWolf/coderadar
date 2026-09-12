"""CodeRadar MCP Server — §26 Agent Interface Design

Exposes the four-tool MCP surface over stdio using MCP v2's
MCPServer decorator API.
"""

from .server import SERVER_INSTRUCTIONS, create_server, serve

__all__ = [
    "SERVER_INSTRUCTIONS",
    "create_server",
    "serve",
]
