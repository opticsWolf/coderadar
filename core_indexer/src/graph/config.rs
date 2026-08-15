
// ── Graph Config (§15) ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct GraphConfig {
    pub resolution: ResolutionConfig,
    pub stack_graph: StackGraphConfig,
    pub import_graph: ImportGraphConfig,
    pub signature: SignatureConfig,
    pub memory: MemoryConfig,
    pub mutation: MutationConfig,
    pub query: QueryConfig,
    pub git: GitConfig,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            resolution: ResolutionConfig::default(),
            stack_graph: StackGraphConfig::default(),
            import_graph: ImportGraphConfig::default(),
            signature: SignatureConfig::default(),
            memory: MemoryConfig::default(),
            mutation: MutationConfig::default(),
            query: QueryConfig::default(),
            git: GitConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolutionConfig {
    pub min_confidence: f32,
}
impl Default for ResolutionConfig {
    fn default() -> Self {
        Self { min_confidence: 0.3 }
    }
}

#[derive(Clone, Debug)]
pub struct StackGraphConfig {
    pub rules_dir: String,
    pub max_path_depth: usize,
    pub incremental: bool,
}
impl Default for StackGraphConfig {
    fn default() -> Self {
        Self { rules_dir: String::new(), max_path_depth: 10, incremental: true }
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
        Self { max_import_depth: 3, include_same_package: true, max_wildcard_hops: 3 }
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
        Self { min_score: 0.5, name_weight: 0.4, arity_weight: 0.3, proximity_weight: 0.3, ambiguous_name_ceiling: 500 }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryConfig {
    pub stack_graph_mb: usize,
    pub call_graph_mb: usize,
    pub resolution_cache_mb: usize,
    pub projected_graph_mb: usize,
    pub spill_compression: String,
}
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            stack_graph_mb: 60, call_graph_mb: 40, resolution_cache_mb: 20,
            projected_graph_mb: 200, spill_compression: "zstd".to_string(),
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
            enabled: true, default_dry_run: true, max_files_per_plan: 100,
            max_edits_per_plan: 500, max_body_tokens: 4000,
            backup_retention_hours: 24, post_verify: true, max_repair_attempts: 3,
            require_clean_git: false,
            // An empty allow list means "anywhere inside the project root".
            // A populated one is a strict whitelist, so the default cannot be
            // one: nothing reads .coderadar.toml yet (plan §3), which would
            // make a shipped whitelist the effective policy for every layout
            // and refuse writes to any project not shaped like src/lib/tests.
            allow: vec![],
            deny: vec![".git/".into(), ".harness/".into(), ".codegraph/".into(),
                       ".coderadar/".into(),
                       "/migrations/".into(), "/*.lock".into(), "/generated/".into()],
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
        Self { max_depth: 5, default_top_k: 10, cache_ttl_seconds: 300,
               cache_max_size: 256, use_rust_graph_for_traversal: true }
    }
}

#[derive(Clone, Debug)]
pub struct GitConfig {
    pub enabled: bool,
    pub reindex_on_branch_switch: bool,
}
impl Default for GitConfig {
    fn default() -> Self {
        Self { enabled: true, reindex_on_branch_switch: true }
    }
}
