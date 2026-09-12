"""CodeRadar Framework Resolvers — §28

Framework-specific graph enrichment for web frameworks.
"""

from .actix import RustActixResolver
from .aspnet import AspNetResolver
from .base import (
    FrameworkExtraction,
    FrameworkResolver,
    SyntheticEdge,
    SyntheticNode,
)
from .django import DjangoResolver
from .express import ExpressResolver
from .fastapi import FastAPIResolver
from .flask import FlaskResolver
from .go import GoResolver
from .laravel import LaravelResolver
from .nestjs import NestJSResolver
from .rails import RailsResolver
from .reactrouter import ReactRouterResolver
from .springboot import SpringBootResolver
from .vuerouter import VueRouterResolver

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
    "ALL_RESOLVERS",
    "AspNetResolver",
    "DjangoResolver",
    "ExpressResolver",
    "FastAPIResolver",
    "FlaskResolver",
    "FrameworkExtraction",
    "FrameworkResolver",
    "GoResolver",
    "LaravelResolver",
    "NestJSResolver",
    "RailsResolver",
    "ReactRouterResolver",
    "RustActixResolver",
    "SpringBootResolver",
    "SyntheticEdge",
    "SyntheticNode",
    "VueRouterResolver",
]
