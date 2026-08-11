"""CodeRadar v3.6 — Configuration Models (§15)

Pydantic models for .coderadar.toml and .harness/config.toml.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Literal, Optional

try:
    import tomllib
except ImportError:
    import tomli as tomllib  # type: ignore

from pydantic import BaseModel, Field


# ── .coderadar.toml ─────────────────────────────────────────────────────────

class ProjectConfig(BaseModel):
    languages: List[str] = ["python"]
    roots: List[str] = ["src/", "tests/"]
    exclude: List[str] = ["**/migrations/**", "**/__pycache__/**", "**/.venv/**"]


class PythonConfig(BaseModel):
    sys_path: List[str] = ["src/"]
    follow_type_checking_imports: bool = False
    strict_wildcard_imports: bool = True
    extra_known_decorators: List[Dict[str, str]] = Field(default_factory=list)


class StackGraphConfig(BaseModel):
    rules_dir: str = ""
    max_path_depth: int = 10
    incremental: bool = True


class ImportGraphConfig(BaseModel):
    max_import_depth: int = 3
    include_same_package: bool = True


class SignatureConfig(BaseModel):
    min_score: float = 0.5
    name_weight: float = 0.4
    arity_weight: float = 0.3
    proximity_weight: float = 0.3
    ambiguous_name_ceiling: int = 500  # v3.6: O(K²) guard from CodeGraph's name-matcher.ts


class LSPConfig(BaseModel):
    enabled: bool = False
    result_ttl_s: int = 300
    idle_timeout_s: int = 600
    timeout_ms: int = 5000
    override_threshold: float = 0.90
    servers: Dict[str, str] = Field(default_factory=lambda: {
        "python": "pyright-langserver --stdio",
        "typescript": "typescript-language-server --stdio",
        "rust": "rust-analyzer",
        "go": "gopls",
    })


class ResolutionConfig(BaseModel):
    min_confidence: float = 0.3
    stack_graph: StackGraphConfig = Field(default_factory=StackGraphConfig)
    import_graph: ImportGraphConfig = Field(default_factory=ImportGraphConfig)
    signature: SignatureConfig = Field(default_factory=SignatureConfig)
    lsp: LSPConfig = Field(default_factory=LSPConfig)


class EmbeddingConfig(BaseModel):
    model: str = "jinaai/jina-code-embeddings-0.5b"
    dimension: int = 896
    truncated_dimension: int = 64
    max_body_tokens: int = 2000
    batch_size: int = 32
    workers: int = 2


class DatabaseConfig(BaseModel):
    """Storage configuration — Macrame embedded bitemporal graph.

    Macrame stores entities as Concepts with JSON metadata in content,
    edges as EdgeAssertions with properties. A single .db file contains
    the full bitemporal ledger.
    """
    path: str = ".coderadar/coderadar.db"
    sync_mode: str = "full"  # "full" | "normal" | "off"
    wal_autocheckpoint: int = 1000


class IngestionConfig(BaseModel):
    batch_chunk_size: int = 200
    embedding_budget_ms: int = 2000
    defer_low_priority_below: float = 0.6


class MemoryConfig(BaseModel):
    stack_graph_mb: int = 60
    call_graph_mb: int = 40
    resolution_cache_mb: int = 20
    spill_compression: str = "zstd"


class MutationConfig(BaseModel):
    enabled: bool = True
    default_dry_run: bool = True
    max_files_per_plan: int = 100
    max_edits_per_plan: int = 500
    max_body_tokens: int = 4000
    backup_dir: str = ".harness/backups"
    backup_retention_hours: int = 24
    post_verify: bool = True
    max_repair_attempts: int = 3
    require_clean_git: bool = False
    allow: List[str] = Field(default_factory=lambda: ["src/", "lib/", "tests/", "scripts/"])
    deny: List[str] = Field(default_factory=lambda: [".git/", ".harness/", "/migrations/", "/*.lock", "/generated/"])
    audit_retention_days: int = 30
    audit_max_entries: int = 10000
    audit_summarize_after_days: int = 7


class QueryConfig(BaseModel):
    max_depth: int = 5
    default_top_k: int = 10
    cache_ttl_seconds: int = 300
    cache_max_size: int = 256
    use_rust_graph_for_traversal: bool = True


class GitConfig(BaseModel):
    enabled: bool = True
    reindex_on_branch_switch: bool = True


class LLMConfig(BaseModel):
    provider: str = "openai"
    model: str = "gpt-4o"
    max_context_tokens: int = 8192
    context_strategy: Literal["signatures_only", "structural", "full"] = "structural"
    temperature: float = 0.1
    api_key_env: str = "OPENAI_API_KEY"


class PerformanceConfig(BaseModel):
    worker_threads: int = 4
    debounce_ms: int = 50
    query_check_interval: int = 64


class OutputConfig(BaseModel):
    snapshot_path: str = "./.coderadar/snapshot.bin"
    journal_path: str = "./.coderadar/wal.log"


class CodeRadarConfig(BaseModel):
    """Root configuration model for .coderadar.toml."""
    project: ProjectConfig = Field(default_factory=ProjectConfig)
    python: PythonConfig = Field(default_factory=PythonConfig)
    resolution: ResolutionConfig = Field(default_factory=ResolutionConfig)
    embedding: EmbeddingConfig = Field(default_factory=EmbeddingConfig)
    database: DatabaseConfig = Field(default_factory=DatabaseConfig)
    ingestion: IngestionConfig = Field(default_factory=IngestionConfig)
    memory: MemoryConfig = Field(default_factory=MemoryConfig)
    mutation: MutationConfig = Field(default_factory=MutationConfig)
    query: QueryConfig = Field(default_factory=QueryConfig)
    git: GitConfig = Field(default_factory=GitConfig)
    llm: LLMConfig = Field(default_factory=LLMConfig)
    performance: PerformanceConfig = Field(default_factory=PerformanceConfig)
    output: OutputConfig = Field(default_factory=OutputConfig)


# ── .harness/config.toml ────────────────────────────────────────────────────

class LanguageConfig(BaseModel):
    extensions: List[str]
    parser: str
    tags_query: str = "tags.scm"
    import_patterns: List[str] = Field(default_factory=list)
    function_patterns: List[str] = Field(default_factory=list)
    method_self_param: Optional[str] = None
    lsp_command: Optional[str] = None


class HarnessGeneralConfig(BaseModel):
    watch_paths: List[str] = Field(default_factory=lambda: ["src/", "tests/"])
    exclude_patterns: List[str] = Field(default_factory=list)
    debounce_ms: int = 500
    max_file_size_bytes: int = 1_048_576
    log_level: str = "info"


class HarnessConfig(BaseModel):
    """Root configuration model for .harness/config.toml."""
    general: HarnessGeneralConfig = Field(default_factory=HarnessGeneralConfig)
    languages: Dict[str, LanguageConfig] = Field(default_factory=dict)


# ── Config Loading ─────────────────────────────────────────────────────────

def load_config(project_root: Path) -> CodeRadarConfig:
    """Load .coderadar.toml from the project root."""
    config_path = project_root / ".coderadar.toml"
    if config_path.exists():
        raw = tomllib.loads(config_path.read_text())
        return CodeRadarConfig(**raw)
    return CodeRadarConfig()


def load_harness_config(project_root: Path) -> HarnessConfig:
    """Load .harness/config.toml from the project root."""
    config_path = project_root / ".harness" / "config.toml"
    if config_path.exists():
        raw = tomllib.loads(config_path.read_text())
        return HarnessConfig(**raw)
    return HarnessConfig()
