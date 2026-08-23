"""CodeRadar — Configuration Models

Pydantic models for `.coderadar.toml`, the one configuration file. They are
loaded by `load_config` and pushed into the Rust core by `activate_config`,
which the CLI and the MCP server call at startup.

`.harness/config.toml` and its models are gone: the file was deleted in
105762e and nothing had read the loader since.
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
    # `languages` is gone: extraction picks the grammar from the file
    # extension, so a list here decided nothing.
    # Empty walks the whole project root. A non-empty default would narrow
    # indexing for every project that does not set `roots` — the file is
    # loaded for real now, so a wrong default is a wrong index.
    roots: List[str] = []
    exclude: List[str] = ["**/migrations/**", "**/__pycache__/**", "**/.venv/**"]


class ImportGraphConfig(BaseModel):
    max_import_depth: int = 3
    include_same_package: bool = True


class SignatureConfig(BaseModel):
    """Carried, not read on any live path — see `CodeRadarConfig`.

    signature.rs is reached only from `resolve_file`/`resolve_reference`,
    which nothing outside tests calls; the live cascade is `resolve_calls`.
    """
    min_score: float = 0.5
    name_weight: float = 0.4
    arity_weight: float = 0.3
    proximity_weight: float = 0.3
    ambiguous_name_ceiling: int = 500  # v3.6: O(K²) guard from CodeGraph's name-matcher.ts


class LSPConfig(BaseModel):
    """Carried, not read on any live path — see `CodeRadarConfig`.

    `lsp/pool.py` defines the pool these settings describe, and nothing
    constructs it.
    """
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
    import_graph: ImportGraphConfig = Field(default_factory=ImportGraphConfig)
    signature: SignatureConfig = Field(default_factory=SignatureConfig)
    lsp: LSPConfig = Field(default_factory=LSPConfig)


class EmbeddingConfig(BaseModel):
    # One model, one dimension. Index-time and query-time used to disagree
    # (jina/896 here, BAAI/384 in compute_embeddings and the MCP search path),
    # which is a broken search however carefully each half behaves. BAAI is
    # the pair that already worked end to end.
    model: str = "BAAI/bge-small-en-v1.5"
    dimension: int = 384
    truncated_dimension: int = 64
    max_body_tokens: int = 2000
    batch_size: int = 32


class DatabaseConfig(BaseModel):
    """Storage configuration — Macrame embedded bitemporal graph.

    Macrame stores entities as Concepts with JSON metadata in content,
    edges as EdgeAssertions with properties. A single .db file contains
    the full bitemporal ledger.
    """
    # Relative to the project root unless absolute. Read by `analyze`.
    path: str = ".coderadar/store/coderadar.db"


class MutationConfig(BaseModel):
    enabled: bool = True
    default_dry_run: bool = True
    max_files_per_plan: int = 100
    max_edits_per_plan: int = 500
    max_body_tokens: int = 4000
    backup_retention_hours: int = 24
    post_verify: bool = True
    max_repair_attempts: int = 3
    require_clean_git: bool = False
    allow: List[str] = Field(default_factory=lambda: ["src/", "lib/", "tests/", "scripts/"])
    deny: List[str] = Field(default_factory=lambda: [".git/", ".coderadar/", "/migrations/", "/*.lock", "/generated/"])


class QueryConfig(BaseModel):
    """Carried, not read on any live path — see `CodeRadarConfig`."""
    max_depth: int = 5
    default_top_k: int = 10
    cache_ttl_seconds: int = 300
    cache_max_size: int = 256
    use_rust_graph_for_traversal: bool = True


class WatchConfig(BaseModel):
    """What the file watcher does. Read by `coderadar.Watcher`."""
    debounce_ms: int = 100
    max_file_size_bytes: int = 1_048_576


class CodeRadarConfig(BaseModel):
    """Root configuration model for .coderadar.toml.

    Sections with a consumer today: `project` and `database` (the walk and
    the store, read by `analyze`), `mutation` (the policy gate),
    `resolution.import_graph` (the resolver), `embedding` and `watch` (read
    on this side).

    Sections with none were deleted rather than left documented and inert —
    [llm], [output], [ingestion], [memory], [git], [python], [performance]'s
    thread counts, [database]'s hnsw_*, [mutation]'s audit_* — along with
    `.harness/config.toml`, whose file was deleted in 105762e while its
    loader stayed behind.

    Three are carried but not yet read, each marked on its own model:
    `resolution.signature`, `resolution.lsp` and `query`. They wait on the
    decision about the code that would read them (signature.rs, lsp/pool.py)
    rather than being cut ahead of it. `resolution.stack_graph` went with
    stack_graph.rs, which resolved nothing.
    """
    project: ProjectConfig = Field(default_factory=ProjectConfig)
    resolution: ResolutionConfig = Field(default_factory=ResolutionConfig)
    embedding: EmbeddingConfig = Field(default_factory=EmbeddingConfig)
    database: DatabaseConfig = Field(default_factory=DatabaseConfig)
    mutation: MutationConfig = Field(default_factory=MutationConfig)
    query: QueryConfig = Field(default_factory=QueryConfig)
    watch: WatchConfig = Field(default_factory=WatchConfig)


# ── Config Loading ─────────────────────────────────────────────────────────

def load_config(project_root: Path) -> CodeRadarConfig:
    """Load .coderadar.toml from the project root."""
    config_path = project_root / ".coderadar.toml"
    if config_path.exists():
        raw = tomllib.loads(config_path.read_text())
        return CodeRadarConfig(**raw)
    return CodeRadarConfig()


def activate_config(project_root: Path) -> "ActivatedConfig":
    """Load `project_root/.coderadar.toml` and push it into the Rust core.

    Loading alone changed nothing before: every consumer built its own
    defaults, so `load_config` was called from one test and nowhere else.
    This is the step that makes the file take effect, and it is what the CLI
    and the MCP server call at startup.

    Only keys the file actually sets are sent (`exclude_unset`), so a project
    that omits a section keeps the core's defaults rather than being handed
    pydantic's — which matters for `[project] roots`, where the model default
    would otherwise silently narrow indexing to `src/` and `tests/`.

    The core reports back which keys it could use; `ignored` names those that
    map to nothing yet. Callers may surface it — silence is what let a
    hundred inert knobs look load-bearing for so long.
    """
    cfg = load_config(project_root)
    payload = cfg.model_dump(exclude_unset=True, mode="json")
    ignored: List[str] = []
    try:
        from coderadar._core import set_config
    except ImportError:
        return ActivatedConfig(config=cfg, ignored=ignored, applied={})
    report = set_config(payload)
    # The core reports what *it* could not map. Two sections are read on this
    # side instead — listing them as ignored would be a false alarm.
    python_side = ("embedding.", "watch.")
    ignored = [k for k in report.get("ignored", [])
               if not k.startswith(python_side)]
    return ActivatedConfig(
        config=cfg,
        ignored=ignored,
        applied=dict(report.get("applied", {})),
    )


class ActivatedConfig(BaseModel):
    """The configuration in force, plus what the core made of it."""
    config: CodeRadarConfig
    applied: Dict[str, Any] = Field(default_factory=dict)
    ignored: List[str] = Field(default_factory=list)
