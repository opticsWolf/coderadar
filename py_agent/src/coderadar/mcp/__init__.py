"""CodeRadar MCP Server — §26 Agent Interface Design

Exposes the four-tool MCP surface over stdio transport.
"""

from .server import create_server, serve, SERVER_INSTRUCTIONS
from .budget import ExploreBudget, get_explore_budget

__all__ = [
    "create_server",
    "serve",
    "SERVER_INSTRUCTIONS",
    "ExploreBudget",
    "get_explore_budget",
]
