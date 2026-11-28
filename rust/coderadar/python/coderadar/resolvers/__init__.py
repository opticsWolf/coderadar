"""Rust-backed framework resolvers with the original Python API shape."""

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode
from .frameworks import (
    AspNetResolver,
    DjangoResolver,
    ExpressResolver,
    FastAPIResolver,
    FlaskResolver,
    GoResolver,
    LaravelResolver,
    NestJSResolver,
    RailsResolver,
    ReactRouterResolver,
    RustActixResolver,
    RustFrameworkResolver,
    SpringBootResolver,
    VueRouterResolver,
)
from .resolution import (
    canonical_route_pattern,
    prefer_in_dir,
    resolve_reference,
    resolve_route,
    route_patterns_match,
)

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
    "RustFrameworkResolver",
    "SpringBootResolver",
    "SyntheticEdge",
    "SyntheticNode",
    "VueRouterResolver",
    "canonical_route_pattern",
    "prefer_in_dir",
    "resolve_reference",
    "resolve_route",
    "route_patterns_match",
]
