//! Unit 4: the Rust CLI front door.

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::{annotations_ops as notes, port, render, state};

#[derive(Parser)]
#[command(
    name = "coderadar-rs",
    version,
    about = "Live semantic graph of your codebase"
)]
struct Cli {
    /// Project root (defaults to the nearest `.coderadar` marker)
    #[arg(short = 'C', long)]
    project: Option<PathBuf>,
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
    Explore { query: String },
    /// Breadth-first graph traversal
    Traverse {
        id: String,
        #[arg(long, default_value = "both")]
        direction: String,
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
    Archive { cutoff: String },
    /// Files modified after the index was built
    StaleFiles { paths: Vec<String> },
    /// Line-numbered source for one entity
    ReadSource { id: String },
    /// Entities as they were at a recorded instant
    AsOf {
        timestamp: String,
        #[arg(default_value = "")]
        query: String,
        #[arg(long)]
        symbols: Vec<String>,
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
        #[arg(long)]
        from_ts: Option<String>,
        #[arg(long)]
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
        t1: String,
        t2: String,
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
        #[arg(long)]
        store_path: Option<PathBuf>,
    },
    /// Ingest git history (dry-run by default)
    IngestHistory {
        repo: PathBuf,
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long)]
        from_sha: Option<String>,
        #[arg(long)]
        to_sha: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        lineage: Option<String>,
        #[arg(long, default_value = "main")]
        parent_ref: String,
    },
    /// Heal merge commits skipped hollow by an older trunk walk
    ReingestMergedCommits {
        repo: PathBuf,
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long)]
        dry_run: bool,
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
    OpenItems { scope: Option<String> },
    /// Export all note rows as JSON
    ExportAnnotations,
    /// Replay an annotation export
    ImportAnnotations { path: PathBuf },
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
            let debounce = external_flag("--debounce")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(100);
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
            let mut seen = watch_mtimes(&paths);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(debounce.max(50)));
                let current = watch_mtimes(&paths);
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
            println!("{}", port::store_repair(shown, result)?);
        }
        "history" => {
            ensure_loaded(project)?;
            let file = tail.first().ok_or_else(|| "missing file".to_string())?;
            let result = port::history_of(file.clone(), None)?;
            println!("{}", render::json_text(&result));
        }
        "revise-annotation" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing annotation id".to_string())?;
            let body = tail
                .get(1)
                .ok_or_else(|| "missing revision body".to_string())?;
            println!("{}", pretty(&notes::revise_annotation(id, body)?)?);
        }
        "retire-annotation" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing annotation id".to_string())?;
            println!("{}", pretty(&notes::retire_annotation(id)?)?);
        }
        "comments-on" => {
            ensure_loaded(project)?;
            let target = tail
                .first()
                .ok_or_else(|| "missing target id".to_string())?;
            println!("{}", pretty(&notes::comments_on(target)?)?);
        }
        "annotation-history" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing annotation id".to_string())?;
            println!("{}", pretty(&notes::annotation_history(id)?)?);
        }
        "comment-history" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing comment id".to_string())?;
            println!("{}", pretty(&notes::comment_history(id)?)?);
        }
        "module-children" => {
            ensure_loaded(project)?;
            let id = tail
                .first()
                .ok_or_else(|| "missing module id".to_string())?;
            println!("{}", pretty(&port::module_children(id.clone())?)?);
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
            println!("{}", pretty(&port::resolve(name.clone(), limit)?)?);
        }
        "diagnose" => {
            ensure_loaded(project)?;
            let result = port::diagnose(
                tail.iter().any(|v| v == "--unresolved"),
                tail.iter().any(|v| v == "--low-confidence"),
            )?;
            println!("{}", port::render_diagnose(result)?);
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
            state::load(&root)?;
            println!("{}", render::reindex(&result));
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
            println!("{}", pretty(&result)?);
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
            println!("{}", pretty(&result)?);
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

fn watch_mtimes(
    roots: &[std::path::PathBuf],
) -> std::collections::BTreeMap<String, std::time::SystemTime> {
    fn walk(
        path: &std::path::Path,
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
                walk(&path, found);
            } else if path.is_file()
                && let Ok(modified) = path.metadata().and_then(|meta| meta.modified())
            {
                found.insert(path.to_string_lossy().into_owned(), modified);
            }
        }
    }
    let mut found = std::collections::BTreeMap::new();
    for root in roots {
        walk(root, &mut found);
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
    serde_json::json!({
        "root": root.to_string_lossy(),
        "config": config.to_string_lossy(),
        "store": store.to_string_lossy(),
        "loaded": loaded.is_some(),
        "store_fresh": store_fresh,
        "stats": stats,
    })
}

/// Parse and execute one CLI invocation, printing the result.
///
/// # Errors
/// Returns the operation's error message for a non-zero exit.
pub fn run(args: Vec<String>) -> Result<(), String> {
    let cli = Cli::try_parse_from(std::iter::once("coderadar-rs".to_string()).chain(args))
        .map_err(|e| e.to_string())?;
    let project = root(&cli);

    match cli.command {
        Command::Init { path, force } => {
            let target = path.unwrap_or_else(|| project.clone());
            let target = target.to_string_lossy().to_string();
            let result = core_indexer::api::analyze(&target, true, &[])?;
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
            if let Some(Value::String(strategy)) = entity.get("_strategy") {
                println!("{strategy}");
            } else {
                println!("{}", render::node(&entity));
            }
        }
        Command::Callers { id } => {
            ensure_loaded(&project)?;
            let rows = port::callers(id.clone())?;
            println!("{}", render::callers(&id, &rows));
        }
        Command::Callees { id } => {
            ensure_loaded(&project)?;
            let rows = port::callees(id.clone())?;
            println!("{}", render::callees(&id, &rows));
        }
        Command::Affected { id, max_depth } => {
            ensure_loaded(&project)?;
            let result = port::affected(id.clone(), max_depth)?;
            println!("{}", render::affected(&id, &result));
        }
        Command::Explore { query } => {
            ensure_loaded(&project)?;
            let result = port::explore(query, None, "both".into(), 8)?;
            println!("{}", render::explore(&result));
        }
        Command::Traverse {
            id,
            direction,
            max_depth,
        } => {
            ensure_loaded(&project)?;
            let result = port::traverse(id, direction, None, max_depth)?;
            println!("{}", render::traverse(&result));
        }
        Command::Query { text } => {
            ensure_loaded(&project)?;
            let rows = port::query(text)?;
            println!("{}", render::query(&rows));
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
            println!("{}", render::blob(&value));
        }
        Command::Archive { cutoff } => {
            let value = port::archive(Some(cutoff))?;
            println!("{}", render::json_text(&value));
        }
        Command::StaleFiles { paths } => {
            let rows = port::stale_files(paths)?;
            println!("{}", render::rows(&rows));
        }
        Command::ReadSource { id } => {
            ensure_loaded(&project)?;
            let entity = port::find_entity(id)?
                .ok_or_else(|| "NotFound: entity does not exist".to_string())?;
            let source = port::read_source(entity)?
                .ok_or_else(|| "ContentUnavailable: no source for that entity".to_string())?;
            println!("{source}");
        }
        Command::AsOf {
            timestamp,
            query,
            symbols,
            branch,
            strict,
            at,
            valid,
        } => {
            ensure_loaded(&project)?;
            let result = port::as_of(
                timestamp,
                query,
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
            println!("{}", render::json_text(&result));
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
            println!("{}", render::json_text(&result));
        }
        Command::HistoryOf { file } => {
            ensure_loaded(&project)?;
            let result = port::history_of(file, None)?;
            println!("{}", render::json_text(&result));
        }
        Command::ChangesBetween {
            t1,
            t2,
            scope,
            branch,
            valid,
        } => {
            ensure_loaded(&project)?;
            let result = port::changes_between(t1, t2, scope, branch, valid, None)?;
            println!("{}", render::json_text(&result));
        }
        Command::Branches => {
            ensure_loaded(&project)?;
            let rows = port::branches(None)?;
            println!("{}", render::rows(&rows));
        }
        Command::IngestStatus { store_path } => {
            ensure_loaded(&project)?;
            let result = port::ingest_status(store_path.map(|p| p.to_string_lossy().to_string()))?;
            println!("{}", render::json_text(&result));
        }
        Command::IngestHistory {
            repo,
            branch,
            from_sha,
            to_sha,
            dry_run,
            lineage,
            parent_ref,
        } => {
            ensure_loaded(&project)?;
            let result = port::ingest_history(
                repo.to_string_lossy().to_string(),
                branch,
                from_sha,
                dry_run,
                lineage,
                parent_ref,
                to_sha,
                None,
            )?;
            println!("{}", render::json_text(&result));
        }
        Command::ReingestMergedCommits {
            repo,
            branch,
            dry_run,
        } => {
            ensure_loaded(&project)?;
            let result = port::reingest_merged_commits(
                repo.to_string_lossy().to_string(),
                branch,
                dry_run,
                None,
            )?;
            println!("{}", render::json_text(&result));
        }
        Command::BackfillCommitIndex { branch, store_path } => {
            ensure_loaded(&project)?;
            let result = port::backfill_commit_index(
                branch,
                store_path.map(|p| p.to_string_lossy().to_string()),
            )?;
            println!("{}", render::json_text(&result));
        }
        Command::BackfillCommitMessages { repo, store_path } => {
            ensure_loaded(&project)?;
            let result = port::backfill_commit_messages(
                repo.to_string_lossy().to_string(),
                store_path.map(|p| p.to_string_lossy().to_string()),
            )?;
            println!("{}", render::json_text(&result));
        }
        Command::ComputeEmbeddings {
            model,
            batch_size,
            recompute,
        } => {
            ensure_loaded(&project)?;
            let result = port::compute_embeddings_op(model, batch_size, recompute)?;
            println!("{}", render::json_text(&result));
        }
        Command::SearchSimilar { query, top_k } => {
            ensure_loaded(&project)?;
            let result = port::search_similar_op(Value::String(query), top_k)?;
            println!("{}", render::json_text(&result));
        }
        Command::SearchHybrid {
            query,
            top_k,
            query_text,
        } => {
            ensure_loaded(&project)?;
            let result = port::search_hybrid_op(Value::String(query), top_k, query_text)?;
            println!("{}", render::json_text(&result));
        }
        Command::ReplaceBody {
            id,
            body,
            expected_hash,
            apply,
        } => {
            ensure_loaded(&project)?;
            let result = port::replace_body(id, body, expected_hash, !apply, None)?;
            println!("{}", render::json_text(&result));
        }
        Command::UpdateSignature {
            id,
            signature,
            inject_defaults,
            apply,
        } => {
            ensure_loaded(&project)?;
            let result = port::update_signature(id, signature, inject_defaults, !apply, None)?;
            println!("{}", render::json_text(&result));
        }
        Command::Rename {
            id,
            new_name,
            apply,
        } => {
            ensure_loaded(&project)?;
            let result = port::rename(id, new_name, !apply, None)?;
            println!("{}", render::json_text(&result));
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
            println!("{}", render::json_text(&result));
        }
        Command::UpdateFile { file, content } => {
            ensure_loaded(&project)?;
            let result = port::update_file(file.to_string_lossy().to_string(), content, None)?;
            println!("{}", render::json_text(&result));
        }
        Command::Annotate { about, body } => {
            ensure_loaded(&project)?;
            let result = notes::annotate(&about, &body)?;
            println!("{}", pretty(&result)?);
        }
        Command::Comment { about, body } => {
            ensure_loaded(&project)?;
            let result = notes::comment(&about, &body)?;
            println!("{}", pretty(&result)?);
        }
        Command::AnnotationsOn { target } => {
            ensure_loaded(&project)?;
            let result = notes::annotations_on(&target)?;
            println!("{}", pretty(&result)?);
        }
        Command::CommentsOn { target } => {
            ensure_loaded(&project)?;
            let result = notes::comments_on(&target)?;
            println!("{}", pretty(&result)?);
        }
        Command::OpenItems { scope } => {
            ensure_loaded(&project)?;
            let result = notes::open_items(scope.as_deref())?;
            if let Ok(text) = serde_json::to_string_pretty(&result) {
                let _ = text;
            }
            println!("{}", render::open_items(&result));
        }
        Command::ExportAnnotations => {
            ensure_loaded(&project)?;
            let result = notes::export_annotations()?;
            println!("{}", pretty(&result)?);
        }
        Command::ImportAnnotations { path } => {
            ensure_loaded(&project)?;
            let payload = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let result = notes::import_annotations(&payload, "skip")?;
            println!("{}", pretty(&result)?);
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
