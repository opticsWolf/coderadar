"""CodeRadar Framework Resolvers — §28

Framework-specific graph enrichment for web frameworks.
"""

from .base import (
    FrameworkResolver,
    FrameworkExtraction,
    SyntheticNode,
    SyntheticEdge,
)
from .django import DjangoResolver
from .flask import FlaskResolver
from .fastapi import FastAPIResolver

# Registry of all available resolvers
ALL_RESOLVERS: list[type[FrameworkResolver]] = [
    DjangoResolver,
    FlaskResolver,
    FastAPIResolver,
]

__all__ = [
    "FrameworkResolver",
    "FrameworkExtraction",
    "SyntheticNode",
    "SyntheticEdge",
    "DjangoResolver",
    "FlaskResolver",
    "FastAPIResolver",
    "ALL_RESOLVERS",
]
