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
from .go import GoResolver
from .actix import RustActixResolver
from .express import ExpressResolver
from .springboot import SpringBootResolver
from .laravel import LaravelResolver
from .aspnet import AspNetResolver
from .rails import RailsResolver
from .nestjs import NestJSResolver
from .vuerouter import VueRouterResolver
from .reactrouter import ReactRouterResolver

# Registry of all available resolvers
ALL_RESOLVERS: list[type[FrameworkResolver]] = [
    DjangoResolver,
    FlaskResolver,
    FastAPIResolver,
    GoResolver,
    RustActixResolver,
    ExpressResolver,
    SpringBootResolver,
    LaravelResolver,
    AspNetResolver,
    RailsResolver,
    NestJSResolver,
    VueRouterResolver,
    ReactRouterResolver,
]

__all__ = [
    "FrameworkResolver",
    "FrameworkExtraction",
    "SyntheticNode",
    "SyntheticEdge",
    "DjangoResolver",
    "FlaskResolver",
    "FastAPIResolver",
    "GoResolver",
    "RustActixResolver",
    "ExpressResolver",
    "SpringBootResolver",
    "LaravelResolver",
    "AspNetResolver",
    "RailsResolver",
    "NestJSResolver",
    "VueRouterResolver",
    "ReactRouterResolver",
    "ALL_RESOLVERS",
]
