// ── Graph Config (§15) ──────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct GraphConfig {
    pub project: ProjectConfig,
    pub database: DatabaseConfig,
    pub resolution: ResolutionConfig,
    pub import_graph: ImportGraphConfig,
    pub signature: SignatureConfig,
    pub mutation: MutationConfig,
    pub query: QueryConfig,
    /// Stage 4 strangler flag: refine smell metrics with CFG math when the
    /// body parses. Default OFF until validated (Milestone C); the AST
    /// numbers remain the fallback.
    pub analysis: AnalysisConfig,
}

/// What `analyze` walks.
#[derive(Clone, Debug, Default)]
pub struct ProjectConfig {
    /// Subdirectories to index, relative to the project root. Empty (the
    /// default) walks the whole root, which is what every caller got before
    /// the config was wired and what a project without `[project] roots`
    /// keeps getting.
    pub roots: Vec<String>,
    /// Glob patterns to skip, on top of the `.gitignore` rules `ignore`
    /// already applies.
    pub exclude: Vec<String>,
}

/// Built-in secret patterns for `[database] blob_exclude` (§1.9, DR-30).
///
/// Belt over the trust-boundary argument: the store lives beside the
/// sources, but backups and copies travel — secret-bearing files get graph
/// coverage WITHOUT a blob. Gitignore syntax, matched against the
/// canonical root-relative id form. User patterns MERGE over these (union,
/// not replace: setting `blob_exclude` must never silently drop the secret
/// defaults); full off is `store_source_blobs = false`.
pub const SECRET_BLOB_EXCLUDE_DEFAULTS: &[&str] = &[
    ".env",
    ".env.*",
    "*.env",
    "*.pem",
    "*.key",
    "*.pfx",
    "*.p12",
    "*.jks",
    "*.kdbx",
    "*secret*",
    "*credential*",
    "*.token",
    "id_rsa*",
    "id_dsa*",
];

/// Where the Macrame store lives.
#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    /// Relative to the project root, or absolute. The default is the path
    /// `analyze` hardcoded before this was configurable.
    pub path: String,
    /// §1.9 (DR-30: default-on-with-notice). File bytes are content-addressed
    /// into the blob store on every analyze/update_file write-through, with
    /// the digest at `extra.coderadar.source_blob` on the file's module
    /// concept. `false` is the kill-switch: graph coverage continues, no
    /// bytes are stored. True by default — also on paths that never push
    /// config (bare `analyze`), which is what default-on means.
    pub store_source_blobs: bool,
    /// Extra gitignore patterns (besides the secret defaults above) whose
    /// files get graph coverage WITHOUT a blob. Merged (union) with the
    /// defaults at `set_config`, never replacing them.
    pub blob_exclude: Vec<String>,
}
impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: ".coderadar/store/coderadar.db".to_string(),
            store_source_blobs: true,
            blob_exclude: SECRET_BLOB_EXCLUDE_DEFAULTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolutionConfig {
    pub min_confidence: f32,
}
impl Default for ResolutionConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImportGraphConfig {
    pub max_import_depth: usize,
    pub include_same_package: bool,
    pub max_wildcard_hops: u8,
}
impl Default for ImportGraphConfig {
    fn default() -> Self {
        Self {
            max_import_depth: 3,
            include_same_package: true,
            max_wildcard_hops: 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SignatureConfig {
    pub min_score: f32,
    pub name_weight: f32,
    pub arity_weight: f32,
    pub proximity_weight: f32,
    /// Pattern from CodeGraph's name-matcher.ts: when a name is defined more
    /// than this many times, fuzzy resolution strategies decline to prevent
    /// near-certain-wrong edges and O(K²) blowup (vendored themes, SDK copies).
    /// Precise strategies (qualified-name, import-based) still run unaffected.
    pub ambiguous_name_ceiling: usize,
}
impl Default for SignatureConfig {
    fn default() -> Self {
        Self {
            min_score: 0.5,
            name_weight: 0.4,
            arity_weight: 0.3,
            proximity_weight: 0.3,
            ambiguous_name_ceiling: 500,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MutationConfig {
    pub enabled: bool,
    pub default_dry_run: bool,
    pub max_files_per_plan: usize,
    pub max_edits_per_plan: usize,
    pub max_body_tokens: usize,
    pub backup_retention_hours: u64,
    pub post_verify: bool,
    pub max_repair_attempts: u32,
    pub require_clean_git: bool,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}
impl Default for MutationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_dry_run: true,
            max_files_per_plan: 100,
            max_edits_per_plan: 500,
            max_body_tokens: 4000,
            backup_retention_hours: 24,
            post_verify: true,
            max_repair_attempts: 3,
            require_clean_git: false,
            // An empty allow list means "anywhere inside the project root".
            // A populated one is a strict whitelist, so the default must not
            // be one: `.coderadar.toml` is now read, and a shipped whitelist
            // would become the effective policy for every project that does
            // not set its own, refusing writes to any layout unlike
            // src/lib/tests.
            allow: vec![],
            // `.harness/` and `.codegraph/` were earlier names for the store
            // directory and are gone; `.coderadar/` is the one in use.
            deny: vec![
                ".git/".into(),
                ".coderadar/".into(),
                "/migrations/".into(),
                "/*.lock".into(),
                "/generated/".into(),
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct QueryConfig {
    pub max_depth: usize,
    pub default_top_k: usize,
    pub cache_ttl_seconds: u64,
    pub cache_max_size: usize,
    pub use_rust_graph_for_traversal: bool,
}
impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            max_depth: 5,
            default_top_k: 10,
            cache_ttl_seconds: 300,
            cache_max_size: 256,
            use_rust_graph_for_traversal: true,
        }
    }
}

/// Analysis-engine refinements (Stage 4).
#[derive(Clone, Debug, Default)]
pub struct AnalysisConfig {
    /// Refine cyclomatic complexity with CFG math and emit
    /// `intra-dead-statements` findings when bodies parse.
    pub use_cfg_metrics: bool,
}
