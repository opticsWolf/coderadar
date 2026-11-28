//! Unit 4: the Rust CLI front door.

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

/// A CLI failure that carries its own exit code.
///
/// `DisplayHelp` and `DisplayVersion` are successes (code 0, message on
/// stdout); usage errors exit 2; runtime errors exit 1 — the same contract as
/// the Python CLI.
#[derive(Debug)]
pub struct Failure {
    /// The text to print.
    pub message: String,
    /// Process exit code.
    pub code: u8,
}

impl Failure {
    fn usage(message: String) -> Self {
        Self { message, code: 2 }
    }

    fn display(message: String) -> Self {
        Self { message, code: 0 }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { message, code: 1 }
    }
}
use serde_json::{Value, json};

use crate::{annotations_ops as notes, port, render, state};

#[derive(Parser)]
#[command(
    name = "coderadar",
    version = concat!(env!("CARGO_PKG_VERSION"), " (spec v3.6)"),
    about = "Live semantic graph of your codebase"
)]
struct Cli {
    /// Project root (defaults to the nearest `.coderadar` marker)
    #[arg(short = 'C', long)]
    project: Option<PathBuf>,
    /// Output format: text for people, json for scripts
    #[arg(long, global = true, default_value = "text", value_parser = ["text", "json"])]
    format: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a store and index the project
    Init {
        path: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Index freshness and graph statistics
    Status {
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Keyword search over names, signatures and docstrings
    Search {
        query: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 10)]
        top_k: i64,
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// One entity's full record
    Node {
        id: String,
        #[arg(long)]
        neighbors: bool,
    },
    /// Direct callers of an entity
    Callers { id: String },
    /// Direct callees of an entity
    Callees { id: String },
    /// Transitive callers (blast radius)
    Affected {
        id: String,
        #[arg(long, default_value_t = 5)]
        max_depth: i64,
    },
    /// Source and call paths for named symbols
    Explore {
        /// Symbols to explore
        symbols: Vec<String>,
        /// Free text naming the symbols
        #[arg(short = 'q', long)]
        query: Option<String>,
        #[arg(long, default_value = "both")]
        direction: String,
        #[arg(long, default_value_t = 8)]
        max_files: i64,
    },
    /// Breadth-first graph traversal
    Traverse {
        id: String,
        #[arg(long, default_value = "both")]
        direction: String,
        /// Comma-separated edge kinds: calls, imports, extends
        #[arg(long, value_delimiter = ',')]
        edge_kinds: Vec<String>,
        #[arg(long, default_value_t = 3)]
        max_depth: i64,
    },
    /// Run a Pest query over the projection
    Query { text: String },
    /// Functions unreachable from any entry point
    DeadCode {
        #[arg(long, default_value_t = 0.6)]
        min_confidence: f64,
        #[arg(long)]
        include_test_reachable: bool,
        #[arg(long, default_value_t = 100)]
        max_findings: i64,
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Code-smell findings
    Smells {
        #[arg(long)]
        entity_id: Option<String>,
        #[arg(long)]
        rule_id: Option<String>,
        #[arg(long, default_value = "normal")]
        strictness: String,
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Duplicate-code groups
    Clones {
        #[arg(long, default_value_t = 10)]
        min_lines: i64,
        #[arg(long, default_value_t = 0.8)]
        min_similarity: f64,
        #[arg(long, default_value_t = 100)]
        max_groups: i64,
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Scaffolding debt and (optionally) secrets
    Scaffolding {
        #[arg(long)]
        include_secrets: bool,
        #[arg(long, default_value_t = 100)]
        max_findings: i64,
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Raw blob bytes by content digest
    BlobGet { digest: String },
    /// Archive cold blobs and old ledger rows
    /// Archive cold blobs and old ledger rows (cutoff defaults to `[retention]`)
    Archive { cutoff: Option<String> },
    /// Files modified after the index was built
    StaleFiles { paths: Vec<String> },
    /// Line-numbered source for one entity
    ReadSource { id: String },
    /// Entities as they were at a recorded instant
    AsOf {
        /// Timestamp (omit when addressing a commit with `--at`)
        timestamp: Option<String>,
        /// Symbols to look up as of that instant
        symbols: Vec<String>,
        /// Free text naming the symbols
        #[arg(short = 'q', long)]
        query: Option<String>,
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        valid: bool,
    },
    /// Byte coverage across recorded generations
    VerifyHistory {
        #[arg(long = "from", alias = "from-ts")]
        from_ts: Option<String>,
        #[arg(long = "to", alias = "to-ts")]
        to_ts: Option<String>,
        #[arg(long, default_value_t = 25)]
        generations: i64,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long, default_value_t = 10)]
        offenders: i64,
        #[arg(long)]
        max_generations: Option<i64>,
    },
    /// Every content version of a file
    HistoryOf { file: String },
    /// Entity changes between two instants
    ChangesBetween {
        #[arg(long)]
        t1: Option<String>,
        #[arg(long)]
        t2: Option<String>,
        /// Deprecated positional t1/t2
        positional: Vec<String>,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long)]
        valid: bool,
    },
    /// Ledger lineages
    Branches,
    /// The store's own ingest state
    IngestStatus {
        #[arg(long = "store", alias = "store-path")]
        store_path: Option<PathBuf>,
    },
    /// Ingest git history (dry-run by default)
    IngestHistory {
        /// Git repository to ingest
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Deprecated positional repository
        repo_pos: Option<PathBuf>,
        #[arg(long, default_value = "main")]
        branch: String,
        /// Exclusive window start (a sha)
        #[arg(long = "from", alias = "from-sha")]
        from_sha: Option<String>,
        /// Backfill window end (a sha)
        #[arg(long = "to", alias = "to-sha")]
        to_sha: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        lineage: Option<String>,
        #[arg(long, default_value = "main")]
        parent_ref: String,
        /// Store file to write
        #[arg(long, alias = "store-path")]
        store: Option<PathBuf>,
    },
    /// Heal merge commits skipped hollow by an older trunk walk
    ReingestMergedCommits {
        repo: PathBuf,
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, alias = "store-path")]
        store: Option<PathBuf>,
    },
    /// Rebuild a lineage's commit index
    BackfillCommitIndex {
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long)]
        store_path: Option<PathBuf>,
    },
    /// Rebuild commit messages from git objects
    BackfillCommitMessages {
        repo: PathBuf,
        #[arg(long)]
        store_path: Option<PathBuf>,
    },
    /// Compute embeddings for every indexed entity
    ComputeEmbeddings {
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 32)]
        batch_size: i64,
        #[arg(long)]
        recompute: bool,
    },
    /// Semantic search over entity embeddings
    SearchSimilar {
        query: String,
        #[arg(long, default_value_t = 10)]
        top_k: i64,
    },
    /// Hybrid keyword + vector search
    SearchHybrid {
        query: String,
        #[arg(long, default_value_t = 10)]
        top_k: i64,
        #[arg(long, default_value = "")]
        query_text: String,
    },
    /// Replace a function/method body (dry-run by default)
    ReplaceBody {
        id: String,
        body: String,
        #[arg(long)]
        expected_hash: Option<String>,
        #[arg(long)]
        apply: bool,
    },
    /// Update a signature (dry-run by default)
    UpdateSignature {
        id: String,
        signature: String,
        #[arg(long)]
        inject_defaults: bool,
        #[arg(long)]
        apply: bool,
    },
    /// Rename an entity and its references (dry-run by default)
    Rename {
        id: String,
        new_name: String,
        #[arg(long)]
        apply: bool,
    },
    /// Insert a new entity (dry-run by default)
    CreateEntity {
        file: PathBuf,
        language: String,
        kind: String,
        name: String,
        body: String,
        #[arg(long)]
        decorator: Vec<String>,
        #[arg(long, default_value = "end")]
        anchor: String,
        #[arg(long)]
        signature: Option<String>,
        #[arg(long)]
        apply: bool,
    },
    /// Sync one file into the graph
    UpdateFile {
        file: PathBuf,
        #[arg(long)]
        content: Option<String>,
    },
    /// Mint a checklist annotation
    Annotate { about: String, body: String },
    /// Mint a freeform comment
    Comment { about: String, body: String },
    /// Annotations anchored to a concept
    AnnotationsOn { target: String },
    /// Comments anchored to a concept
    CommentsOn { target: String },
    /// Unchecked checklist items
    OpenItems {
        /// Prefix on the annotated id (`a.py` covers `a.py::f`)
        #[arg(long)]
        scope: Option<String>,
        /// Deprecated positional scope
        scope_pos: Option<String>,
    },
    /// Export all note rows as JSON
    ExportAnnotations {
        /// File to write (default: print the JSON document)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Store file to read (default: the loaded project's store)
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// Replay an annotation export
    ImportAnnotations {
        path: PathBuf,
        /// Store file to write (default: the loaded project's store)
        #[arg(long)]
        store: Option<PathBuf>,
        /// An id that already exists refuses, or is skipped
        #[arg(long, default_value = "skip", value_parser = ["error", "skip"])]
        on_conflict: String,
    },
    /// Additional parity commands (captured verbatim)
    #[command(external_subcommand)]
    External(Vec<String>),
    /// Model Context Protocol server
    Mcp {
        #[command(subcommand)]
        action: McpCommand,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve JSON-RPC on stdin/stdout
    Serve {
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

fn run_external(args: Vec<String>, project: &Path) -> Result<(), String> {
    let Some(name) = args.first().cloned() else {
        return Err("missing parity command".to_string());
    };
    let tail = &args[1..];
    if tail.iter().any(|value| value == "-h" || value == "--help")
        && let Some(usage) = crate::usage::for_command(&name)
    {
        println!("{usage}");
        return Ok(());
    }
    let external_flag = |name: &str| -> Option<String> {
        tail.iter()
            .position(|value| value == name)
            .and_then(|index| tail.get(index + 1))
            .cloned()
    };
    let external_flags = |name: &str| -> Vec<String> {
        tail.iter()
            .enumerate()
            .filter(|(_, value)| *value == name)
            .filter_map(|(index, _)| tail.get(index + 1).cloned())
            .collect()
    };
    match name.as_str() {
        "shell" => {
            ensure_loaded(project)?;
            println!("CodeRadar Shell");
            println!("Type 'help' for commands, 'exit' to quit.");
            flush_stdout();
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let line = line.map_err(|e| e.to_string())?;
                let trimmed = line.trim();
                match trimmed {
                    "exit" | "quit" => break,
                    "help" => {
                        println!("Commands: query <pest>, traverse <id>, callers <id>, stats, exit")
                    }
                    "stats" => {
                        let snap =
                            state::projection().ok_or_else(|| "no graph loaded".to_string())?;
                        println!("{}", pretty(&core_indexer::api::stats_full(&snap))?);
                    }
                    _ if trimmed.starts_with("query ") => {
                        let rows = port::query(trimmed[6..].to_string())?;
                        if rows.is_empty() {
                            println!("No results");
                        }
                        for row in rows {
                            println!("{}", pretty(&row)?);
                        }
                    }
                    _ if trimmed.starts_with("traverse ") => {
                        let value = port::traverse(
                            trimmed[9..].trim().to_string(),
                            "both".to_string(),
                            None,
                            3,
                        )?;
                        println!("{}", pretty(&value)?);
                    }
                    _ if trimmed.starts_with("callers ") => {
                        let rows = port::callers(trimmed[8..].trim().to_string())?;
                        if rows.is_empty() {
                            println!("No callers found for {}", trimmed[8..].trim());
                        }
                        for row in rows {
                            println!("{}", pretty(&row)?);
                        }
                    }
                    "" => {}
                    other => println!("Unknown command: {other}"),
                }
            }
        }
        "watch" => {
            ensure_loaded(project)?;
            let root = find_root(project);
            let (config_debounce, max_bytes) = watch_config();
            let debounce = external_flag("--debounce")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(config_debounce);
            if tail.iter().any(|value| value == "--git") {
                let repo = root.to_string_lossy().into_owned();
                println!("Watching git refs (commits, checkouts, merges)");
                println!("Press Ctrl+C to stop");
                flush_stdout();
                let mut last = core_indexer::api::git_head(&repo).ok();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(500.max(debounce * 5)));
                    let head = core_indexer::api::git_head(&repo).ok();
                    if head != last {
                        match port::sync_git(
                            repo.clone(),
                            None,
                            "main".to_string(),
                            true,
                            "watch".to_string(),
                        ) {
                            Ok(report) => println!("{}", pretty(&report)?),
                            Err(error) => println!("sync failed: {error}"),
                        }
                        last = head;
                    }
                }
            }
            let mut paths: Vec<std::path::PathBuf> = Vec::new();
            let mut skip_next = false;
            for value in tail.iter() {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if value == "--debounce" {
                    skip_next = true;
                    continue;
                }
                if !value.starts_with('-') && value != "--git" {
                    paths.push(root.join(value));
                }
            }
            let paths = if paths.is_empty() {
                vec![root.join("src"), root.join("tests")]
            } else {
                paths
            };
            println!(
                "Watching: {}",
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            flush_stdout();
            println!("Press Ctrl+C to stop");
            flush_stdout();
            let mut seen = watch_mtimes(&paths, max_bytes);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(debounce.max(50)));
                let current = watch_mtimes(&paths, max_bytes);
                let mut changed: Vec<String> = current
                    .iter()
                    .filter(|(path, stamp)| seen.get(*path) != Some(stamp))
                    .map(|(path, _)| path.clone())
                    .collect();
                changed.sort();
                for path in changed {
                    let rel = std::path::Path::new(&path)
                        .strip_prefix(&root)
                        .unwrap_or(std::path::Path::new(&path))
                        .to_string_lossy()
                        .replace('\\', "/");
                    match port::update_file(rel.clone(), None, None) {
                        Ok(report) => {
                            println!("updated {rel}: {}", pretty(&report)?);
                            flush_stdout();
                            let _ = port::refresh_framework_file(
                                serde_json::json!(root.to_string_lossy()),
                                rel,
                                None,
                            );
                        }
                        Err(error) => println!("update failed for {rel}: {error}"),
                    }
                }
                seen = current;
            }
        }
        "lsp" => {
            let value = core_indexer::api::lsp_cli(tail, &project.to_string_lossy())?;
            println!("{}", pretty(&value)?);
        }
        "context" => {
            ensure_loaded(project)?;
            let value = core_indexer::api::graphrag_cli(tail)?;
            println!("{}", pretty(&value)?);
        }
        "visualize" => {
            ensure_loaded(project)?;
            let viz_type = tail
                .first()
                .ok_or_else(|| "missing visualization type".to_string())?
                .clone();
            let format = external_flag("--format").unwrap_or_else(|| "mermaid".to_string());
            if !matches!(format.as_str(), "mermaid" | "graphviz" | "dot") {
                return Err(format!(
                    "Unknown format: {format} (supported: mermaid, graphviz, dot)"
                ));
            }
            let output = external_flag("--output").or_else(|| external_flag("-o"));
            let mut args = Vec::new();
            let mut skip = false;
            for value in tail.iter().skip(1) {
                if skip {
                    skip = false;
                    continue;
                }
                if matches!(value.as_str(), "--format" | "--output" | "-o") {
                    skip = true;
                    continue;
                }
                if !value.starts_with('-') {
                    args.push(value.clone());
                }
            }
            let snap = state::projection().ok_or_else(|| "no graph loaded".to_string())?;
            let text = crate::visualize::render(&viz_type, &format, &args, &snap)?;
            if let Some(path) = output {
                std::fs::write(&path, &text).map_err(|e| e.to_string())?;
                println!("Written to {path}");
            } else {
                println!("{text}");
            }
        }
        "rebuild-store" => {
            let repo = external_flag("--repo").unwrap_or_else(|| ".".to_string());
            let store = external_flag("--store");
            let branches = external_flag("--branches").map(|value| {
                value
                    .split(',')
                    .map(|part| part.trim().to_string())
                    .collect::<Vec<String>>()
            });
            let parent_ref = external_flag("--parent-ref").unwrap_or_else(|| "main".to_string());
            let export = external_flag("--export");
            let on_conflict = external_flag("--on-conflict").unwrap_or_else(|| "error".to_string());
            let embed = tail.iter().any(|value| value == "--embed");
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            let result = port::rebuild_op(
                repo,
                store,
                branches,
                parent_ref,
                export,
                on_conflict,
                embed,
            )?;
            if format == "json" {
                println!("{}", pretty(&result)?);
            } else {
                println!("{}", port::rebuild(result)?);
            }
        }
        "docs" => match tail.first().map(String::as_str) {
            None => {
                for (name, description) in crate::assets::DOC_NAMES
                    .iter()
                    .zip(crate::assets::DOC_DESCRIPTIONS)
                {
                    println!("{name}  {description}");
                }
            }
            Some(name) => {
                let text = crate::assets::doc_text(name).ok_or_else(|| {
                    format!(
                        "unknown document {name:?}; available: {}",
                        crate::assets::DOC_NAMES.join(", ")
                    )
                })?;
                print!("{text}");
            }
        },
        "git" => match tail.first().map(String::as_str) {
            Some("blame") => {
                let file = tail.get(1).ok_or_else(|| "missing file".to_string())?;
                let repo = external_flag("--repo").unwrap_or_else(|| ".".to_string());
                let rows = core_indexer::api::git_blame(&repo, file)
                    .map_err(|e| format!("Could not check the worktree: {e}"))?;
                let array = rows.as_array().cloned().unwrap_or_default();
                if array.is_empty() {
                    println!("No blame data (git feature may be disabled)");
                } else {
                    println!("Blame: {file}");
                    println!("  Line  Author  Commit");
                    for row in array {
                        let line = row.get("line").and_then(Value::as_u64).unwrap_or(0);
                        let author = row.get("author").and_then(Value::as_str).unwrap_or("");
                        let commit = row.get("commit").and_then(Value::as_str).unwrap_or("");
                        println!("  {line:>4}  {author}  {}", &commit[..commit.len().min(8)]);
                    }
                }
            }
            Some("is-clean") => {
                let repo = tail.get(1).cloned().unwrap_or_else(|| ".".to_string());
                let clean = core_indexer::api::git_worktree_clean(&repo)
                    .map_err(|e| format!("Could not check the worktree: {e}"))?;
                if clean {
                    println!("Worktree clean");
                } else {
                    println!("Worktree has uncommitted changes");
                }
            }
            Some("diff") => {
                let repo = tail.get(1).cloned().unwrap_or_else(|| ".".to_string());
                let old = external_flag("--old");
                let new = external_flag("--new");
                let files =
                    core_indexer::api::git_changed_files(&repo, old.as_deref(), new.as_deref())
                        .map_err(|e| format!("Could not diff revisions: {e}"))?;
                if files.is_empty() {
                    println!("No changed files (or git feature disabled)");
                } else {
                    println!("{} changed files:", files.len());
                    for file in files {
                        println!("  {file}");
                    }
                }
            }
            _ => return Err("usage: git blame|is-clean|diff".to_string()),
        },
        "skills" => match tail.first().map(String::as_str) {
            Some("list") => {
                println!("Bundled skills");
                for name in crate::assets::SKILL_NAMES {
                    let description = crate::assets::skill_description(name).unwrap_or("");
                    println!("{name}  {description}");
                }
                println!("Install with `coderadar skills install`.");
            }
            Some("show") => {
                let name = tail
                    .get(1)
                    .ok_or_else(|| "missing skill name".to_string())?;
                let text = crate::assets::skill_text(name).ok_or_else(|| {
                    format!(
                        "unknown skill {name:?}; available: {}",
                        crate::assets::SKILL_NAMES.join(", ")
                    )
                })?;
                print!("{text}");
            }
            Some("install") => {
                let project = tail.iter().any(|value| value == "--project");
                let force = tail.iter().any(|value| value == "--force");
                let destinations = external_flags("--dest");
                let selected = external_flags("--skill");
                if !destinations.is_empty() && project {
                    return Err("--dest and --project are mutually exclusive".to_string());
                }
                let home = std::env::var("HOME").unwrap_or_default();
                let defaults = if project {
                    vec![
                        std::path::PathBuf::from(".claude/skills"),
                        std::path::PathBuf::from(".opencode/skills"),
                    ]
                } else {
                    vec![
                        std::path::PathBuf::from(&home).join(".claude/skills"),
                        std::path::PathBuf::from(&home).join(".config/opencode/skills"),
                    ]
                };
                let targets: Vec<std::path::PathBuf> = if destinations.is_empty() {
                    defaults
                } else {
                    destinations
                        .into_iter()
                        .map(std::path::PathBuf::from)
                        .collect()
                };
                let names: Vec<String> = if selected.is_empty() {
                    crate::assets::SKILL_NAMES
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect()
                } else {
                    selected
                };
                let mut skipped = 0;
                for target in targets {
                    for name in &names {
                        let text = crate::assets::skill_text(name).ok_or_else(|| {
                            format!(
                                "unknown skill {name:?}; available: {}",
                                crate::assets::SKILL_NAMES.join(", ")
                            )
                        })?;
                        let dir = target.join(name);
                        let marker = dir.join("SKILL.md");
                        let existed = marker.is_file();
                        if existed && !force {
                            skipped += 1;
                            println!("skipped    {}", dir.display());
                            continue;
                        }
                        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                        std::fs::write(&marker, text).map_err(|e| e.to_string())?;
                        println!(
                            "{:<9} {}",
                            if existed { "updated" } else { "installed" },
                            dir.display()
                        );
                    }
                }
                if skipped > 0 {
                    println!("{skipped} skill(s) already present; use --force to overwrite.");
                }
            }
            _ => return Err("usage: skills list|show NAME|install".to_string()),
        },
        "get-smells" => {
            ensure_loaded(project)?;
            let rows = port::get_smells(
                external_flag("--entity-id"),
                external_flag("--rule-id"),
                external_flag("--strictness").unwrap_or_else(|| "normal".to_string()),
            )?;
            print_value(
                &Value::Array(rows),
                &external_flag("--format").unwrap_or_else(|| "text".to_string()),
                |value| render::rows(rows_as_array(value)),
            );
        }
        "find-clones" => {
            ensure_loaded(project)?;
            let rows = port::find_clones(
                external_flag("--min-lines")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(10),
                external_flag("--min-similarity")
                    .and_then(|value| value.parse::<f64>().ok())
                    .unwrap_or(0.8),
                external_flag("--max-groups")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(100),
            )?;
            print_value(
                &Value::Array(rows),
                &external_flag("--format").unwrap_or_else(|| "text".to_string()),
                |value| render::rows(rows_as_array(value)),
            );
        }
        "find-scaffolding" => {
            ensure_loaded(project)?;
            let rows = port::find_scaffolding(
                tail.iter().any(|value| value == "--include-secrets"),
                external_flag("--max-findings")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(100),
            )?;
            print_value(
                &Value::Array(rows),
                &external_flag("--format").unwrap_or_else(|| "text".to_string()),
                |value| render::rows(rows_as_array(value)),
            );
        }
        "exclude" => {
            let root = find_root(project);
            match tail.first().map(String::as_str) {
                Some("list") => {
                    println!("Effective excludes (baseline +  exclude + .gitignore):");
                    for pattern in core_indexer::api::default_excludes() {
                        println!("  baseline  {pattern}");
                    }
                    for pattern in config_excludes(&root) {
                        println!("  config    {pattern}");
                    }
                    let gitignore = root.join(".gitignore");
                    match std::fs::read_to_string(&gitignore) {
                        Ok(text) => {
                            for line in text.lines() {
                                let trimmed = line.trim();
                                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                                    println!("  gitignore {trimmed}");
                                }
                            }
                        }
                        Err(_) => println!("  gitignore  (no .gitignore)"),
                    }
                }
                Some("add") => {
                    let pattern = tail
                        .get(1)
                        .ok_or_else(|| "missing exclude pattern".to_string())?;
                    let mut patterns = config_excludes(&root);
                    if patterns.contains(pattern) {
                        println!("Already excluded: {pattern}");
                    } else {
                        patterns.push(pattern.clone());
                        rewrite_excludes(&root, patterns)?;
                        println!("OK  Excluded {pattern} — re-analyze to apply.");
                    }
                }
                Some("remove") => {
                    let pattern = tail
                        .get(1)
                        .ok_or_else(|| "missing exclude pattern".to_string())?;
                    let current = config_excludes(&root);
                    if !current.contains(pattern) {
                        println!("Not excluded: {pattern}");
                    } else {
                        let patterns: Vec<String> = current
                            .into_iter()
                            .filter(|value| value != pattern)
                            .collect();
                        rewrite_excludes(&root, patterns)?;
                        println!("OK  No longer excluded: {pattern}.");
                    }
                }
                _ => return Err("usage: exclude list|add PATTERN|remove PATTERN".to_string()),
            }
        }
        "load-snapshot" => {
            let snapshot = tail
                .first()
                .ok_or_else(|| "missing snapshot path".to_string())?;
            let root = external_flag("--root")
                .map(PathBuf::from)
                .unwrap_or_else(|| find_root(project));
            state::load_from(std::path::Path::new(snapshot), &root)?;
            let snap = state::projection()
                .ok_or_else(|| "snapshot did not produce a graph".to_string())?;
            println!(
                "Snapshot loaded: {}",
                render::json_text(&core_indexer::api::stats_full(&snap))
            );
        }
        "store-repair" => {
            let db = external_flag("--db").map(PathBuf::from).unwrap_or_else(|| {
                find_root(project)
                    .join(".coderadar")
                    .join("store")
                    .join("coderadar.db")
            });
            let shown = db.to_string_lossy().into_owned();
            let result = if !db.exists() {
                json!({ "missing": shown.clone() })
            } else if tail.iter().any(|value| value == "--delete") {
                fs::remove_file(&db)
                    .map_err(|error| format!("failed to delete {shown}: {error}"))?;
                json!({ "deleted": shown.clone() })
            } else {
                core_indexer::api::store_repair(&shown)?
            };
            let text = port::store_repair(shown, result)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            if format == "json" {
                println!("{}", text);
            } else {
                println!("{text}");
            }
        }
        "history" => {
            ensure_loaded(project)?;
            let file = tail.first().ok_or_else(|| "missing file".to_string())?;
            let result = port::history_of(file.clone(), None)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&result, &format, render::json_text)?;
        }
        "revise-annotation" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing annotation id".to_string())?;
            let body = tail
                .get(1)
                .ok_or_else(|| "missing revision body".to_string())?;
            let value = notes::revise_annotation(id, body)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&value, &format, render::json_text)?;
        }
        "retire-annotation" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing annotation id".to_string())?;
            let value = notes::retire_annotation(id)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&value, &format, render::json_text)?;
        }
        "comments-on" => {
            ensure_loaded(project)?;
            let target = tail
                .first()
                .ok_or_else(|| "missing target id".to_string())?;
            let value = notes::comments_on(target)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&value, &format, render::json_text)?;
        }
        "annotation-history" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing annotation id".to_string())?;
            let value = notes::annotation_history(id)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&value, &format, render::json_text)?;
        }
        "comment-history" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing comment id".to_string())?;
            let value = notes::comment_history(id)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&value, &format, render::json_text)?;
        }
        "module-children" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing module id".to_string())?;
            let value = port::module_children(id.clone())?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&value, &format, render::json_text)?;
        }
        "resolve" => {
            ensure_loaded(project)?;
            let name = tail.first().ok_or_else(|| "missing name".to_string())?;
            let limit = tail
                .iter()
                .position(|v| v == "--limit")
                .and_then(|i| tail.get(i + 1))
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(5);
            let value = port::resolve(name.clone(), limit)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&value, &format, render::json_text)?;
        }
        "diagnose" => {
            ensure_loaded(project)?;
            let mut unresolved = tail.iter().any(|v| v == "--unresolved");
            let mut low_confidence = tail.iter().any(|v| v == "--low-confidence");
            if !unresolved && !low_confidence {
                unresolved = true;
                low_confidence = true;
            }
            let result = port::diagnose(unresolved, low_confidence)?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&result, &format, |_| {
                port::render_diagnose(result.clone()).unwrap_or_default()
            })?;
        }
        "resolve-framework" => {
            ensure_loaded(project)?;
            let name = tail
                .first()
                .ok_or_else(|| "missing reference name".to_string())?;
            let limit = tail
                .get(1)
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(5);
            let candidates = port::framework_candidates_op(name.clone(), limit)?;
            let result = port::framework_resolve_reference(name.clone(), candidates, limit)?;
            println!("{}", pretty(&result)?);
        }
        "reindex" => {
            let root = find_root(project);
            let result = port::reindex_op(&root)?;
            if external_flag("--format").as_deref() == Some("json") {
                println!("{}", pretty(&result)?);
            } else {
                println!("{}", render::reindex(&result));
            }
        }
        "sync-git" => {
            ensure_loaded(project)?;
            let repo = tail
                .first()
                .ok_or_else(|| "missing repository path".to_string())?
                .clone();
            let flag = |name: &str| -> Option<String> {
                tail.iter()
                    .position(|v| v == name)
                    .and_then(|i| tail.get(i + 1))
                    .cloned()
            };
            let result = port::sync_git(
                repo,
                flag("--store"),
                flag("--parent-ref").unwrap_or_else(|| "main".to_string()),
                !tail.iter().any(|v| v == "--no-catch-up"),
                flag("--source").unwrap_or_else(|| "watch".to_string()),
            )?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&result, &format, render::json_text)?;
        }
        "migrate-to-commit-time" => {
            let repo = tail
                .first()
                .ok_or_else(|| "missing repository path".to_string())?
                .clone();
            let store = tail
                .get(1)
                .ok_or_else(|| "missing destination store path".to_string())?
                .clone();
            let flag = |name: &str| -> Option<String> {
                tail.iter()
                    .position(|v| v == name)
                    .and_then(|i| tail.get(i + 1))
                    .cloned()
            };
            let branches = flag("--branches").map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });
            let result = port::migrate_to_commit_time_op(
                repo,
                store,
                branches,
                flag("--parent-ref").unwrap_or_else(|| "main".to_string()),
                !tail.iter().any(|v| v == "--no-embed"),
                !tail.iter().any(|v| v == "--no-verify"),
            )?;
            let format = external_flag("--format").unwrap_or_else(|| "text".to_string());
            emit(&result, &format, render::json_text)?;
        }
        other => return Err(format!("unknown command `{other}`")),
    }
    Ok(())
}

fn config_path(root: &Path) -> PathBuf {
    root.join(".coderadar.toml")
}

fn config_excludes(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(config_path(root)) else {
        return Vec::new();
    };
    let mut in_project = false;
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.starts_with('[') {
            in_project = stripped == "[project]";
            continue;
        }
        if in_project
            && stripped.starts_with("exclude")
            && let Some((_, value)) = stripped.split_once('=')
            && let Ok(rows) = serde_json::from_str::<Vec<String>>(value.trim())
        {
            return rows;
        }
    }
    Vec::new()
}

fn rewrite_excludes(root: &Path, patterns: Vec<String>) -> Result<(), String> {
    let path = config_path(root);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut unique: Vec<String> = patterns.into_iter().collect();
    unique.sort();
    unique.dedup();
    let mut parts = Vec::new();
    for pattern in &unique {
        parts.push(serde_json::to_string(pattern).map_err(|e| e.to_string())?);
    }
    let value = format!("[{}]", parts.join(", "));
    let mut in_project = false;
    let mut done = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.starts_with('[') {
            if in_project && !done {
                out.push(format!("exclude = {value}"));
                done = true;
            }
            in_project = stripped == "[project]";
            out.push(line.to_string());
            continue;
        }
        if in_project && stripped.starts_with("exclude") && stripped.contains('=') {
            if !done {
                out.push(format!("exclude = {value}"));
                done = true;
            }
            continue;
        }
        out.push(line.to_string());
    }
    if in_project && !done {
        out.push(format!("exclude = {value}"));
        done = true;
    }
    if !done {
        out.push("[project]".to_string());
        out.push(format!("exclude = {value}"));
    }
    std::fs::write(
        &path,
        format!(
            "{}
",
            out.join(
                "
"
            )
        ),
    )
    .map_err(|e| e.to_string())
}

fn watch_config() -> (u64, u64) {
    core_indexer::api::watch_settings()
}

fn watch_mtimes(
    roots: &[std::path::PathBuf],
    max_bytes: u64,
) -> std::collections::BTreeMap<String, std::time::SystemTime> {
    fn walk(
        path: &std::path::Path,
        max_bytes: u64,
        found: &mut std::collections::BTreeMap<String, std::time::SystemTime>,
    ) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if path.is_dir() {
                if matches!(name, ".git" | ".coderadar" | "target" | "__pycache__") {
                    continue;
                }
                walk(&path, max_bytes, found);
            } else if path.is_file()
                && let Ok(meta) = path.metadata()
                && meta.len() <= max_bytes
                && let Ok(modified) = meta.modified()
            {
                found.insert(path.to_string_lossy().into_owned(), modified);
            }
        }
    }
    let mut found = std::collections::BTreeMap::new();
    for root in roots {
        walk(root, max_bytes, &mut found);
    }
    found
}

fn flush_stdout() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn find_root(start: &Path) -> PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".coderadar").is_dir() || dir.join(".coderadar.toml").is_file() {
            return dir;
        }
        if !dir.pop() {
            return start.to_path_buf();
        }
    }
}

fn root(cli: &Cli) -> PathBuf {
    let start = cli
        .project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    find_root(&start)
}

fn ensure_loaded(root: &Path) -> Result<(), String> {
    if state::projection().is_none() {
        state::load(root)?;
    }
    Ok(())
}

/// Print one result as text or as pretty JSON.
fn emit(value: &Value, format: &str, text: impl FnOnce(&Value) -> String) -> Result<(), String> {
    if format == "json" {
        println!("{}", pretty(value)?);
    } else {
        println!("{}", text(value));
    }
    Ok(())
}

fn print_value(value: &Value, format: &str, text: impl FnOnce(&Value) -> String) {
    if format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    } else {
        println!("{}", text(value));
    }
}

fn status_value(root: &Path) -> Value {
    let config = root.join(".coderadar.toml");
    let store = state::default_store_path(root);
    let loaded = state::projection();
    let (stats, store_fresh) = match &loaded {
        Some(snap) => {
            let fresh = std::fs::metadata(&store)
                .and_then(|m| m.modified())
                .ok()
                .is_some();
            (Some(core_indexer::api::stats_full(snap)), fresh)
        }
        None => (None, false),
    };
    let age_seconds = std::fs::metadata(&store)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0);
    serde_json::json!({
        "root": root.to_string_lossy(),
        "config": config.to_string_lossy(),
        "store": store.to_string_lossy(),
        "loaded": loaded.is_some(),
        "store_fresh": store_fresh,
        "age_seconds": age_seconds,
        "stats": stats,
    })
}

/// Parse and execute one CLI invocation, printing the result.
///
/// # Errors
/// Returns the operation's error message for a non-zero exit.
pub fn run(args: Vec<String>) -> Result<(), Failure> {
    let cli = match Cli::try_parse_from(std::iter::once("coderadar-rs".to_string()).chain(args)) {
        Ok(cli) => cli,
        Err(error) => {
            use clap::error::ErrorKind;
            return Err(match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    Failure::display(error.to_string())
                }
                _ => Failure::usage(error.to_string()),
            });
        }
    };
    let project = root(&cli);
    let _activation = core_indexer::api::activate_config(&project.to_string_lossy());

    match cli.command {
        Command::Init { path, force } => {
            let target = path.unwrap_or_else(|| project.clone());
            let target = target.to_string_lossy().to_string();
            let _activation = core_indexer::api::activate_config(&target);
            let result = port::analyze_with_routes(&target, true, &[])?;
            state::clear();
            state::load(Path::new(&target))?;
            let _ = force;
            println!("{}", render::reindex(&result));
        }
        Command::Status { format } => {
            let _ = ensure_loaded(&project);
            let value = status_value(&project);
            print_value(&value, &format, render::status);
        }
        Command::Search {
            query,
            kind,
            top_k,
            format,
        } => {
            ensure_loaded(&project)?;
            let rows = port::search(query.clone(), kind.clone(), top_k)?;
            let text = |v: &Value| render::search(&query, kind.as_deref(), rows_as_array(v));
            if format == "json" {
                println!("{}", pretty(&Value::Array(rows))?);
            } else {
                println!("{}", text(&serde_json::Value::Array(rows)));
            }
        }
        Command::Node { id, neighbors } => {
            ensure_loaded(&project)?;
            let entity = port::node(id, neighbors)?;
            if cli.format == "json" {
                println!("{}", pretty(&entity)?);
            } else if let Some(Value::String(strategy)) = entity.get("_strategy") {
                println!("{strategy}");
            } else {
                println!("{}", render::node(&entity));
            }
        }
        Command::Callers { id } => {
            ensure_loaded(&project)?;
            let rows = port::callers(id.clone())?;
            emit(&Value::Array(rows.clone()), &cli.format, |_| {
                render::callers(&id, &rows)
            })?;
        }
        Command::Callees { id } => {
            ensure_loaded(&project)?;
            let rows = port::callees(id.clone())?;
            emit(&Value::Array(rows.clone()), &cli.format, |_| {
                render::callees(&id, &rows)
            })?;
        }
        Command::Affected { id, max_depth } => {
            ensure_loaded(&project)?;
            let result = port::affected(id.clone(), max_depth)?;
            emit(&result, &cli.format, |value| render::affected(&id, value))?;
        }
        Command::Explore {
            symbols,
            query,
            direction,
            max_files,
        } => {
            ensure_loaded(&project)?;
            let result = port::explore(
                query.unwrap_or_default(),
                if symbols.is_empty() {
                    None
                } else {
                    Some(symbols)
                },
                direction,
                max_files,
            )?;
            emit(&result, &cli.format, render::explore)?;
        }
        Command::Traverse {
            id,
            direction,
            edge_kinds,
            max_depth,
        } => {
            ensure_loaded(&project)?;
            let result = port::traverse(
                id,
                direction,
                if edge_kinds.is_empty() {
                    None
                } else {
                    Some(edge_kinds)
                },
                max_depth,
            )?;
            emit(&result, &cli.format, render::traverse)?;
        }
        Command::Query { text } => {
            ensure_loaded(&project)?;
            let rows = port::query(text)?;
            emit(&Value::Array(rows.clone()), &cli.format, |_| {
                render::query(&rows)
            })?;
        }
        Command::DeadCode {
            min_confidence,
            include_test_reachable,
            max_findings,
            format,
        } => {
            ensure_loaded(&project)?;
            let rows = port::dead_code(min_confidence, include_test_reachable, max_findings)?;
            print_value(&Value::Array(rows), &format, |v| {
                render::rows(rows_as_array(v))
            });
        }
        Command::Smells {
            entity_id,
            rule_id,
            strictness,
            format,
        } => {
            ensure_loaded(&project)?;
            let rows = port::get_smells(entity_id, rule_id, strictness)?;
            print_value(&Value::Array(rows), &format, |v| {
                render::rows(rows_as_array(v))
            });
        }
        Command::Clones {
            min_lines,
            min_similarity,
            max_groups,
            format,
        } => {
            ensure_loaded(&project)?;
            let rows = port::find_clones(min_lines, min_similarity, max_groups)?;
            print_value(&Value::Array(rows), &format, |v| {
                render::rows(rows_as_array(v))
            });
        }
        Command::Scaffolding {
            include_secrets,
            max_findings,
            format,
        } => {
            ensure_loaded(&project)?;
            let rows = port::find_scaffolding(include_secrets, max_findings)?;
            print_value(&Value::Array(rows), &format, |v| {
                render::rows(rows_as_array(v))
            });
        }
        Command::BlobGet { digest } => {
            let value = port::blob_get(digest)?.unwrap_or(Value::Null);
            emit(&value, &cli.format, render::blob)?;
        }
        Command::Archive { cutoff } => {
            let value = port::archive(cutoff)?;
            emit(&value, &cli.format, render::json_text)?;
        }
        Command::StaleFiles { paths } => {
            let rows = port::stale_files(paths)?;
            emit(&Value::Array(rows.clone()), &cli.format, |_| {
                render::rows(&rows)
            })?;
        }
        Command::ReadSource { id } => {
            ensure_loaded(&project)?;
            let entity = port::find_entity(id)?
                .ok_or_else(|| "NotFound: entity does not exist".to_string())?;
            let source = port::read_source(entity)?
                .ok_or_else(|| "ContentUnavailable: no source for that entity".to_string())?;
            emit(&Value::String(source.clone()), &cli.format, |_| {
                source.clone()
            })?;
        }
        Command::AsOf {
            timestamp,
            symbols,
            query,
            branch,
            strict,
            at,
            valid,
        } => {
            ensure_loaded(&project)?;
            let result = port::as_of(
                timestamp.unwrap_or_default(),
                query.unwrap_or_default(),
                if symbols.is_empty() {
                    None
                } else {
                    Some(symbols)
                },
                branch,
                strict,
                at,
                valid,
                None,
            )?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::VerifyHistory {
            from_ts,
            to_ts,
            generations,
            all,
            branch,
            offenders,
            max_generations,
        } => {
            ensure_loaded(&project)?;
            let result = port::verify_history(
                from_ts,
                to_ts,
                generations,
                all,
                branch,
                offenders,
                max_generations,
                None,
            )?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::HistoryOf { file } => {
            ensure_loaded(&project)?;
            let result = port::history_of(file, None)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::ChangesBetween {
            t1,
            t2,
            positional,
            scope,
            branch,
            valid,
        } => {
            ensure_loaded(&project)?;
            let t1 = t1
                .or_else(|| positional.first().cloned())
                .ok_or_else(|| "missing --t1".to_string())?;
            let t2 = t2
                .or_else(|| positional.get(1).cloned())
                .ok_or_else(|| "missing --t2".to_string())?;
            let result = port::changes_between(t1, t2, scope, branch, valid, None)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::Branches => {
            ensure_loaded(&project)?;
            let rows = port::branches(None)?;
            emit(&Value::Array(rows.clone()), &cli.format, |_| {
                render::rows(&rows)
            })?;
        }
        Command::IngestStatus { store_path } => {
            ensure_loaded(&project)?;
            let result = port::ingest_status(store_path.map(|p| p.to_string_lossy().to_string()))?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::IngestHistory {
            repo,
            repo_pos,
            branch,
            from_sha,
            to_sha,
            dry_run,
            lineage,
            parent_ref,
            store,
        } => {
            ensure_loaded(&project)?;
            let repo = repo
                .or(repo_pos)
                .ok_or_else(|| "missing --repo".to_string())?;
            let result = port::ingest_history(
                repo.to_string_lossy().to_string(),
                branch,
                from_sha,
                dry_run,
                lineage,
                parent_ref,
                to_sha,
                store.map(|p| p.to_string_lossy().to_string()),
            )?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::ReingestMergedCommits {
            repo,
            branch,
            dry_run,
            store,
        } => {
            ensure_loaded(&project)?;
            let result = port::reingest_merged_commits(
                repo.to_string_lossy().to_string(),
                branch,
                dry_run,
                store.map(|p| p.to_string_lossy().to_string()),
            )?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::BackfillCommitIndex { branch, store_path } => {
            ensure_loaded(&project)?;
            let result = port::backfill_commit_index(
                branch,
                store_path.map(|p| p.to_string_lossy().to_string()),
            )?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::BackfillCommitMessages { repo, store_path } => {
            ensure_loaded(&project)?;
            let result = port::backfill_commit_messages(
                repo.to_string_lossy().to_string(),
                store_path.map(|p| p.to_string_lossy().to_string()),
            )?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::ComputeEmbeddings {
            model,
            batch_size,
            recompute,
        } => {
            ensure_loaded(&project)?;
            let result = port::compute_embeddings_op(model, batch_size, recompute)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::SearchSimilar { query, top_k } => {
            ensure_loaded(&project)?;
            let result = port::search_similar_op(Value::String(query), top_k)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::SearchHybrid {
            query,
            top_k,
            query_text,
        } => {
            ensure_loaded(&project)?;
            let text = if query_text.is_empty() {
                query.clone()
            } else {
                query_text
            };
            let result = port::search_hybrid_op(Value::String(text), top_k, query)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::ReplaceBody {
            id,
            body,
            expected_hash,
            apply,
        } => {
            ensure_loaded(&project)?;
            let result = port::replace_body(id, body, expected_hash, !apply, None)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::UpdateSignature {
            id,
            signature,
            inject_defaults,
            apply,
        } => {
            ensure_loaded(&project)?;
            let result = port::update_signature(id, signature, inject_defaults, !apply, None)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::Rename {
            id,
            new_name,
            apply,
        } => {
            ensure_loaded(&project)?;
            let result = port::rename(id, new_name, !apply, None)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::CreateEntity {
            file,
            language,
            kind,
            name,
            body,
            decorator,
            anchor,
            signature,
            apply,
        } => {
            ensure_loaded(&project)?;
            let result = port::create_entity(
                file.to_string_lossy().to_string(),
                language,
                kind,
                name,
                body,
                if decorator.is_empty() {
                    None
                } else {
                    Some(decorator)
                },
                anchor,
                signature,
                !apply,
                None,
            )?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::UpdateFile { file, content } => {
            ensure_loaded(&project)?;
            let result = port::update_file(file.to_string_lossy().to_string(), content, None)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::Annotate { about, body } => {
            ensure_loaded(&project)?;
            let result = notes::annotate(&about, &body)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::Comment { about, body } => {
            ensure_loaded(&project)?;
            let result = notes::comment(&about, &body)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::AnnotationsOn { target } => {
            ensure_loaded(&project)?;
            let result = notes::annotations_on(&target)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::CommentsOn { target } => {
            ensure_loaded(&project)?;
            let result = notes::comments_on(&target)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::OpenItems { scope, scope_pos } => {
            ensure_loaded(&project)?;
            let result = notes::open_items(scope.or(scope_pos).as_deref())?;
            emit(&result, &cli.format, render::open_items)?;
        }
        Command::ExportAnnotations { out, store } => {
            if let Some(store) = store {
                state::load_from(&store, &project)?;
            } else {
                ensure_loaded(&project)?;
            }
            let result = notes::export_annotations()?;
            match out {
                Some(path) => {
                    let text = pretty(&result)?;
                    std::fs::write(&path, text).map_err(|e| e.to_string())?;
                    println!("Exported to {}", path.display());
                }
                None => println!("{}", pretty(&result)?),
            }
        }
        Command::ImportAnnotations {
            path,
            store,
            on_conflict,
        } => {
            if let Some(store) = store {
                state::load_from(&store, &project)?;
            } else {
                ensure_loaded(&project)?;
            }
            let payload = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let result = notes::import_annotations(&payload, &on_conflict)?;
            emit(&result, &cli.format, render::json_text)?;
        }
        Command::External(args) => run_external(args, &project)?,
        Command::Mcp { action } => match action {
            McpCommand::Serve { path } => {
                let project = path.unwrap_or(project);
                ensure_loaded(&project)?;
                crate::mcp::serve()?;
            }
        },
    }
    Ok(())
}

fn rows_as_array(value: &Value) -> &[Value] {
    match value {
        Value::Array(rows) => rows,
        _ => &[],
    }
}

fn pretty(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|e| e.to_string())
}
