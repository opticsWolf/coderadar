//! Unit 4: the Rust CLI front door.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::Value;

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
            (Some(core_indexer::api::stats(snap)), fresh)
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
