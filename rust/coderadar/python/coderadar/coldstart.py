"""Cold-start helpers over the Rust engine."""

from pathlib import Path

from . import coldstart_build_graph


def build_graph(root):
    """Load the store or index the tree, returning the engine report."""
    return coldstart_build_graph(str(root))


__all__ = ["build_graph"]
