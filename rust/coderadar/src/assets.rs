//! Bundled docs and agent skills, embedded so the Rust binary is standalone.

const QUERY_LANGUAGE: &str = include_str!("../../../py_agent/src/coderadar/docs/query-language.md");
const CLI_SKILL: &str =
    include_str!("../../../py_agent/src/coderadar/skills/coderadar-cli/SKILL.md");
const MCP_SKILL: &str =
    include_str!("../../../py_agent/src/coderadar/skills/coderadar-mcp/SKILL.md");

pub const DOC_NAMES: &[&str] = &["query-language"];
pub const DOC_DESCRIPTIONS: &[&str] =
    &["Query language and field reference (generated from the grammar)."];
pub const SKILL_NAMES: &[&str] = &["coderadar-cli", "coderadar-mcp"];

/// Verbatim text of a bundled reference document.
pub fn doc_text(name: &str) -> Option<&'static str> {
    match name {
        "query-language" => Some(QUERY_LANGUAGE),
        _ => None,
    }
}

/// Verbatim text of a bundled skill pack.
pub fn skill_text(name: &str) -> Option<&'static str> {
    match name {
        "coderadar-cli" => Some(CLI_SKILL),
        "coderadar-mcp" => Some(MCP_SKILL),
        _ => None,
    }
}

/// The `description:` field from a skill's YAML frontmatter.
pub fn skill_description(name: &str) -> Option<&'static str> {
    let text = skill_text(name)?;
    for line in text.lines().skip(1) {
        if line.trim() == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("description:") {
            let value = value.trim();
            return Some(
                value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(value),
            );
        }
    }
    None
}

/// The instructions block the MCP server advertises on `initialize`.
pub const SERVER_INSTRUCTIONS: &str = r#"# CodeRadar — live semantic graph of your codebase

A pre-built graph of every symbol, call edge and file in the workspace
(41 languages). Use it before and while editing, not only for questions: one
call returns verbatim source plus who calls it and what it affects.

## Start with coderadar_explore

Any structural or flow question — "how does X work?", "the flow from X to
Y", reading a symbol before editing it — is one `coderadar_explore` call
naming the symbol(s). It returns line-numbered source grouped by file (safe
to Edit from), the call paths between the symbols and a blast-radius summary.
Need more? Call it again with more specific names.

## Other read tools

- `coderadar_search` — find symbols by keyword when you don't know the name
- `coderadar_search_similar` — semantic search (embeddings via `coderadar_compute_embeddings`, computed on first use)
- `coderadar_search_hybrid` — RRF-fused keyword + semantic search, with per-arm rank evidence
- `coderadar_node` — one entity's details and neighbours; `coderadar_module_children` — a module's contents
- `coderadar_affected` — transitive callers (blast radius), centrality-ranked; `coderadar_callers` / `coderadar_callees` — direct 1-hop neighbours
- `coderadar_traverse` — any edge kind, upstream or downstream
- `coderadar_query` — structured queries, e.g. `functions where caller_count == 0` (full reference: resource `docs://query-language`)
- `coderadar_as_of` — the graph at a past timestamp, on either axis (`valid=true` reads what was true then, the default reads what the store had recorded); `at=` addresses a commit (sha, branch, `{branch}#{n}`) instead of an instant
- `coderadar_verify_history` — per-generation byte coverage: whether the past can be reconstructed
- `coderadar_ingest_status` — what the store records about its own ingest: per-lineage cursor, horizon, tip, generation, commits vs completions (read-only; never the writer)
- `coderadar_changes_between` — entity diff between two instants from recorded hashes (no byte reads); `valid=true` diffs the repository's own states
- `coderadar_annotations_on` / `coderadar_comments_on` — annotation/comment versions on a concept (the why); `coderadar_annotation_history` / `coderadar_comment_history` — one row's chain; `coderadar_open_items` — the unchecked task items under a scope, aggregated across annotations
- `coderadar_resolve` — framework references: routes (`/users/:id`), `*Model` / `*View` names
- `coderadar_get_smells`, `coderadar_dead_code`, `coderadar_find_clones`, `coderadar_find_scaffolding` — quality findings. Dead code is ranked evidence, not proof: check `coderadar_affected` before deleting.
- `coderadar_diagnose` — graph self-health (unresolved targets, ambiguous bases); empty reads as clean.

## Editing

`coderadar_replace_body`, `coderadar_update_signature`, `coderadar_rename`
and `coderadar_create_entity` edit the file and the graph together. They
dry-run by default: review the diff, then apply with dry_run=False.
After editing with Read/Edit instead, sync with `coderadar_update_file` (one
file) or `coderadar_reindex` (many files). `coderadar_status` shows what is
served and how fresh the index is.

## Anti-patterns

- **Don't re-verify results with grep.** They come from a full parse.
- **Don't grep or Read first** to find indexed code, and don't reconstruct a flow by hand: name the endpoints and explore.
- **Files flagged "⚠ changed on disk after index sync"** are stale: Read those. Every other file is fresh.
- **No index for a project?** Use built-in tools. Indexing is the user's decision: mention `coderadar init`, don't run it.
- **"Indexing in progress" is not an error.** Retry in a few seconds rather than falling back to grep.

## Skill packs

`coderadar-mcp` and `coderadar-cli` ship inside this package and are
readable as the resources `skills://coderadar-mcp` and
`skills://coderadar-cli`. Run `coderadar skills show <name>` to print either
file, or `coderadar skills install` to copy both into Claude Code / OpenCode
discovery directories.

## One project at a time

The server serves one project. Every tool takes an optional `project_path`:
a path inside the served project is accepted, another project is refused
with the reason. `coderadar_set_project` switches wholesale (config re-read,
background re-index, earlier event ids invalid).

The same graph ops exist on the shell under the same names, hyphenated
(`coderadar explore`, `coderadar get-smells`, `coderadar status`, ...).
Project switching is the one exception: the CLI spells it `-C/--project`,
because one CLI process serves one root.
"#;
