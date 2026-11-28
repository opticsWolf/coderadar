"""Thin config objects over the engine's active configuration.

The Rust engine owns parsing, validation and defaults; these dataclass-shaped
objects only expose the live values to Python callers.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from . import activate_config as _activate
from . import config_json


@dataclass
class ProjectConfig:
    roots: list[str] = field(default_factory=list)
    exclude: list[str] = field(default_factory=list)


@dataclass
class ImportGraphConfig:
    enabled: bool = True


@dataclass
class SignatureConfig:
    enabled: bool = True


@dataclass
class LSPConfig:
    enabled: bool = False
    timeout_ms: int = 5000
    override_threshold: float = 0.9
    servers: dict[str, str] = field(default_factory=dict)


@dataclass
class ResolutionConfig:
    lsp: LSPConfig = field(default_factory=LSPConfig)


@dataclass
class EmbeddingConfig:
    model: str = "BAAI/bge-small-en-v1.5"
    dimension: int = 384


@dataclass
class DatabaseConfig:
    path: str = ".coderadar/store/coderadar.db"


@dataclass
class RetentionConfig:
    enabled: bool = True


@dataclass
class MutationConfig:
    enabled: bool = True


@dataclass
class QueryConfig:
    max_results: int = 100


@dataclass
class WatchConfig:
    debounce_ms: int = 500


@dataclass
class CodeRadarConfig:
    project: ProjectConfig = field(default_factory=ProjectConfig)
    import_graph: ImportGraphConfig = field(default_factory=ImportGraphConfig)
    signature: SignatureConfig = field(default_factory=SignatureConfig)
    resolution: ResolutionConfig = field(default_factory=ResolutionConfig)
    embedding: EmbeddingConfig = field(default_factory=EmbeddingConfig)
    database: DatabaseConfig = field(default_factory=DatabaseConfig)
    retention: RetentionConfig = field(default_factory=RetentionConfig)
    mutation: MutationConfig = field(default_factory=MutationConfig)
    query: QueryConfig = field(default_factory=QueryConfig)
    watch: WatchConfig = field(default_factory=WatchConfig)

    def model_dump(self, **_kwargs: Any) -> dict[str, Any]:
        """The pydantic-compatible spelling callers already use."""
        return _dump(self)


@dataclass
class ActivatedConfig:
    config: CodeRadarConfig
    applied: dict[str, Any] = field(default_factory=dict)
    ignored: list[str] = field(default_factory=list)


def _dump(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: _dump(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_dump(item) for item in value]
    if hasattr(value, "__dataclass_fields__"):
        return {
            name: _dump(getattr(value, name))
            for name in value.__dataclass_fields__
        }
    return value


def _build(raw: dict[str, Any]) -> CodeRadarConfig:
    resolution = raw.get("resolution") or {}
    lsp = resolution.get("lsp") or {}
    return CodeRadarConfig(
        project=ProjectConfig(**(raw.get("project") or {})),
        import_graph=ImportGraphConfig(**(raw.get("import_graph") or {})),
        signature=SignatureConfig(**(raw.get("signature") or {})),
        resolution=ResolutionConfig(
            lsp=LSPConfig(
                enabled=bool(lsp.get("enabled", False)),
                timeout_ms=int(lsp.get("timeout_ms", 5000)),
                override_threshold=float(lsp.get("override_threshold", 0.9)),
                servers=dict(lsp.get("servers") or {}),
            )
        ),
        embedding=EmbeddingConfig(**(raw.get("embedding") or {})),
        database=DatabaseConfig(**(raw.get("database") or {})),
        retention=RetentionConfig(**(raw.get("retention") or {})),
        mutation=MutationConfig(**(raw.get("mutation") or {})),
        query=QueryConfig(**(raw.get("query") or {})),
        watch=WatchConfig(**(raw.get("watch") or {})),
    )


def load_config(project_root: str = ".") -> CodeRadarConfig:
    """Load a project's config by activating it in the engine."""
    _activate(str(project_root))
    return _build(config_json())


def activate_config(project_root: str = ".") -> ActivatedConfig:
    """Activate `project_root/.coderadar.toml` and report what took effect."""
    report = _activate(str(project_root)) or {}
    return ActivatedConfig(
        config=_build(config_json()),
        applied=dict(report.get("applied") or {}),
        ignored=list(report.get("ignored") or []),
    )


__all__ = [
    "ActivatedConfig", "CodeRadarConfig", "DatabaseConfig", "EmbeddingConfig",
    "ImportGraphConfig", "LSPConfig", "MutationConfig", "ProjectConfig",
    "QueryConfig", "ResolutionConfig", "RetentionConfig", "SignatureConfig",
    "WatchConfig", "activate_config", "load_config",
]
