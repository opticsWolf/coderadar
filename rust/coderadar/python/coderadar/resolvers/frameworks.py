"""Framework resolver classes backed by the pure-Rust engine."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from .base import FrameworkExtraction, FrameworkResolver, SyntheticEdge, SyntheticNode
from .. import _native


class RustFrameworkResolver(FrameworkResolver):
    """Base class whose five resolver methods all execute in Rust."""

    FRAMEWORK = ""

    @property
    def name(self) -> str:
        return self.FRAMEWORK

    def detect(self, project_root: Path) -> bool:
        return bool(_native.framework_detect(str(project_root), self.FRAMEWORK))

    def claims_reference(self, name: str) -> bool:
        return bool(_native.framework_claims_reference(name, self.FRAMEWORK))

    def extract(self, file_path: str, source: str) -> FrameworkExtraction:
        raw = _native.framework_extract(str(file_path), source, self.FRAMEWORK)
        return FrameworkExtraction(
            file_path=raw.get("file_path", str(file_path)),
            nodes=[SyntheticNode(**row) for row in raw.get("nodes", [])],
            edges=[SyntheticEdge(**row) for row in raw.get("edges", [])],
        )

    def resolve(
        self, ref_name: str, candidates: list[dict[str, Any]]
    ) -> dict[str, Any] | None:
        result = _native.framework_resolve_one(ref_name, self.FRAMEWORK, candidates)
        return result or None


class DjangoResolver(RustFrameworkResolver):
    FRAMEWORK = "django"


class FlaskResolver(RustFrameworkResolver):
    FRAMEWORK = "flask"


class FastAPIResolver(RustFrameworkResolver):
    FRAMEWORK = "fastapi"


class GoResolver(RustFrameworkResolver):
    FRAMEWORK = "go"


class RustActixResolver(RustFrameworkResolver):
    FRAMEWORK = "actix"


class ExpressResolver(RustFrameworkResolver):
    FRAMEWORK = "express"


class SpringBootResolver(RustFrameworkResolver):
    FRAMEWORK = "springboot"


class LaravelResolver(RustFrameworkResolver):
    FRAMEWORK = "laravel"


class AspNetResolver(RustFrameworkResolver):
    FRAMEWORK = "aspnet"


class RailsResolver(RustFrameworkResolver):
    FRAMEWORK = "rails"


class NestJSResolver(RustFrameworkResolver):
    FRAMEWORK = "nestjs"


class VueRouterResolver(RustFrameworkResolver):
    FRAMEWORK = "vue-router"


class ReactRouterResolver(RustFrameworkResolver):
    FRAMEWORK = "react-router"
