// CodeRadar v3.6 — AST-Aware Mutation Engine (§11)
// Four refactoring tools: replace_entity_body, update_signature, rename_symbol, create_entity.
//
// Each plan_* method takes the current CodeGraph for entity/span lookup and
// computes byte-accurate edits with content-hash guards. apply() performs
// stale-write rejection, backup, atomic write, post-write parse verification,
// and automatic rollback on tainted updates.

pub mod edit;
pub mod indent;
pub mod write_guard;

use std::collections::HashMap;

use crate::mutation::edit::apply_edits_to_file;
use crate::mutation::indent::{detect_indent_style, normalize_indent};
use crate::mutation::write_guard::WriteGuard;
use crate::types::{ByteSpan, ParseQuality, ProjectedGraph, ResolvedCall};

/// Mutation plan — produced by the planner, consumed by apply().
#[derive(Clone, Debug)]
pub struct MutationPlan {
    pub id: String,
    pub tool: String,
    pub edits: Vec<MutationEdit>,
    pub affected_files: Vec<String>,
    pub diff_preview: String,
    pub unverified_sites: Vec<UnverifiedSite>,
    pub warnings: Vec<String>,
}

/// A single edit to a single file.
#[derive(Clone, Debug)]
pub struct MutationEdit {
    pub file: String,
    pub span: ByteSpan,
    pub replacement: String,
    pub expected_hash: String,
}

/// A call site that could not be auto-edited (needs LLM/manual review).
#[derive(Clone, Debug)]
pub struct UnverifiedSite {
    pub file: String,
    pub line: u32,
    pub snippet: String,
    pub reason: String,
}

/// Result of applying a mutation plan.
#[derive(Clone, Debug)]
pub struct MutationResult {
    pub status: MutationStatus,
    pub files_written: Vec<String>,
    pub syntax_errors: Vec<SyntaxDiagnostic>,
    pub reindex: ReindexSummary,
    pub backup_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutationStatus {
    Applied,
    RolledBack,
    RejectedStale,
    /// Refused by `MutationConfig` before any file was read or written.
    RejectedPolicy,
}

#[derive(Clone, Debug)]
pub struct SyntaxDiagnostic {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub offending_span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct ReindexSummary {
    pub files: usize,
    pub entities_updated: usize,
    pub edges_updated: usize,
    pub duration_ms: u64,
}

// ── Mutation Engine ────────────────────────────────────────────────────────

/// Resolve a module ID (e.g. `.\src\foo.py::module`) to its file path (`.\src\foo.py`).
fn module_file_path(projection: &ProjectedGraph, module_id: &str) -> String {
    if let Some(module) = projection.modules.get(module_id) {
        return module.path.to_string_lossy().to_string();
    }
    // Fallback: strip the `::module` suffix
    module_id.trim_end_matches("::module").to_string()
}

/// Read a project file by module-derived path / resolve a canonical
/// id-form path for disk IO (F14): centralized in module_resolution so
/// analysis passes share the same root-aware resolution as the engine.
use crate::graph::module_resolution::{disk_path_for, read_project_file};

/// Confirm that `span` in `source` still holds the identifier the index recorded.///
/// Spans are captured at index time. The stale-write hash carried on every edit
/// is computed from the same read the span was resolved against, so it proves
/// the file did not change between *plan* and *apply* — it says nothing about
/// whether the graph still matches disk. This is the check that does.
/// Heuristic: does this body text open with a Python docstring (a triple-
/// quoted string as its first token)? Deliberately simple — it only decides
/// whether to warn that documentation may be lost, never to rewrite code.
fn has_leading_docstring(body: &str) -> bool {
    let t = body.trim_start();
    t.starts_with("\"\"\"") || t.starts_with("'''")
}

fn span_holds_name(source: &[u8], span: ByteSpan, name: &str) -> bool {
    source
        .get(span.start..span.end)
        .is_some_and(|bytes| bytes == name.as_bytes())
}

/// Convert a 1-indexed line + 0-indexed byte column to an absolute byte offset.
fn line_col_to_byte(source: &[u8], line: usize, col: usize) -> Option<usize> {
    let mut line_start = 0usize;
    let mut current_line = 1usize;
    if line > 1 {
        for (i, &b) in source.iter().enumerate() {
            if b == b'\n' {
                current_line += 1;
                if current_line == line {
                    line_start = i + 1;
                    break;
                }
            }
        }
        if current_line != line {
            return None; // line out of range
        }
    }
    let pos = line_start + col;
    (pos <= source.len()).then_some(pos)
}

/// Textual call-site backstop (v0.8 P2-5).
///
/// The graph knows the call sites that were *resolved* at index time. What
/// resolution cannot see — macro bodies, unsupported syntax, anything the
/// cascade gave up on — is exactly where a rename or signature change breaks
/// silently. This walks every indexed file for the textual shape of a call:
/// a word-boundary `name` immediately followed by `(`. Deliberately dumb —
/// no syntax, no expansion: a hit may be a comment, a string, or a same-name
/// call to a different entity, and the report says so.
fn textual_call_sites(projection: &ProjectedGraph, name: &str) -> Vec<(String, u32, String)> {
    let needle = format!("{}(", name);
    let mut out: Vec<(String, u32, String)> = Vec::new();
    let mut visited: Vec<String> = Vec::new();
    for module in projection.modules.values() {
        let file = module.path.to_string_lossy().to_string();
        if !visited.iter().any(|f| f == &file) {
            visited.push(file.clone());
        } else {
            continue;
        }
        let source = read_project_file(&file);
        for (idx, line) in source.lines().enumerate() {
            // One report per line is enough — the snippet carries the context.
            for (pos, _) in line.match_indices(&needle) {
                // Word boundary before the name: the previous character must
                // be non-identifier. A non-ASCII previous byte is treated as
                // identifier material (conservative: Rust/Python identifiers
                // may be unicode, so `éname(` is one token, not a call to
                // `name`).
                let boundary = match pos {
                    0 => true,
                    p => {
                        let b = line.as_bytes()[p - 1];
                        b < 0x80 && !b.is_ascii_alphanumeric() && b != b'_'
                    }
                };
                if boundary {
                    out.push((file.clone(), (idx + 1) as u32, line.trim().to_string()));
                    break;
                }
            }
            if out.len() >= 15 {
                return out; // cap: a report is a triage list, not a census
            }
        }
    }
    out
}

/// (file, line) of every graph call site targeting one of `targets` — the
/// "already covered" set for the textual backstop.
fn structural_call_site_lines(
    projection: &ProjectedGraph,
    targets: &[String],
) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    for target in targets {
        let Some(callers) = projection.callers_by_callee.get(target) else {
            continue;
        };
        for caller_id in callers {
            let Some(caller_fn) = projection.functions.get(caller_id) else {
                continue;
            };
            let file = module_file_path(projection, &caller_fn.parent_module);
            for (i, rc) in caller_fn.resolved_calls.iter().enumerate() {
                let targets_entity = matches!(
                    rc,
                    ResolvedCall::Function(id)
                    | ResolvedCall::Method { method: id, .. }
                    | ResolvedCall::Constructor(id)
                        if id == target
                );
                if !targets_entity {
                    continue;
                }
                let Some(call) = caller_fn.calls.get(i) else {
                    continue;
                };
                out.push((file.clone(), call.line as u32));
            }
        }
    }
    out
}

/// Append textual occurrences of `name(` that no structural site already
/// covers (same file, line within ±1) as unverified sites. `covered` should
/// include the entity's definition line plus every structural reference the
/// planner already handled.
fn push_textual_backstop(
    projection: &ProjectedGraph,
    name: &str,
    covered: &[(String, u32)],
    unverified: &mut Vec<UnverifiedSite>,
) {
    for (file, line, snippet) in textual_call_sites(projection, name) {
        if covered
            .iter()
            .any(|(f, l)| f == &file && line.saturating_sub(1) <= *l && *l <= line + 1)
        {
            continue;
        }
        unverified.push(UnverifiedSite {
            file,
            line,
            snippet,
            reason:
                "Textual occurrence — may be inside a macro body, comment or string; check manually"
                    .into(),
        });
    }
}

/// xxh3_64 hex digest of a byte slice (used for stale-write rejection).
fn span_hash(bytes: &[u8]) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes))
}

/// Hash the content of `bytes` within `span` (clamped to bounds).
fn hash_span(bytes: &[u8], span: ByteSpan) -> String {
    let len = bytes.len();
    let start = span.start.min(len);
    let end = span.end.min(len).max(start);
    span_hash(&bytes[start..end])
}

/// Parse `source` with the tree-sitter grammar for `lang`, if available.
/// Returns None if the language has no grammar or parsing fails.
fn parse_has_error(lang: crate::types::Language, source: &[u8]) -> Option<bool> {
    let ts_lang = crate::graph::CodeGraph::ts_language(&lang)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_lang).ok()?;
    let tree = parser.parse(source, None)?;
    Some(tree.root_node().has_error())
}

/// Post-write parse verification: report a diagnostic only if the mutation
/// *introduced* a syntax error (after has_error && !before has_error).
fn verify_parse_introduced_error(file_path: &str, original: &[u8]) -> Option<SyntaxDiagnostic> {
    let lang = crate::types::Language::from_extension(
        std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("py"),
    );
    let before = parse_has_error(lang, original);
    let after_bytes = std::fs::read(disk_path_for(file_path)).ok()?;
    let after = parse_has_error(lang, &after_bytes);

    match (before, after) {
        (Some(_), Some(true)) if before == Some(false) => Some(SyntaxDiagnostic {
            file: file_path.to_string(),
            line: 0,
            column: 0,
            message: "post-write parse check failed — mutation introduced a syntax error".into(),
            offending_span: ByteSpan { start: 0, end: 0 },
        }),
        _ => None,
    }
}

/// Shared process-wide WriteGuard — suppresses watcher events for files the
/// mutation engine wrote. The watcher consults the same instance so mutation
/// writes don't trigger double-indexing.
pub static WRITE_GUARD: std::sync::OnceLock<std::sync::Arc<WriteGuard>> =
    std::sync::OnceLock::new();

/// Get (or lazily create) the shared WriteGuard instance.
pub fn shared_write_guard() -> std::sync::Arc<WriteGuard> {
    WRITE_GUARD
        .get_or_init(|| std::sync::Arc::new(WriteGuard::new()))
        .clone()
}

/// Does `rel` — a project-relative path with forward slashes — match `pattern`?
///
/// Patterns come from `MutationConfig` and use the three shapes its defaults
/// carry: a leading directory (`src/`), an interior path fragment
/// (`/migrations/`), and an extension glob (`/*.lock`).
fn path_matches(rel: &str, pattern: &str) -> bool {
    if let Some(ext) = pattern.strip_prefix("/*") {
        return !ext.is_empty() && rel.ends_with(ext);
    }
    // Interior-fragment patterns (`/migrations/`) match at any depth —
    // that is their documented purpose, so both forms stay.
    if let Some(fragment) = pattern.strip_prefix('/') {
        if fragment.is_empty() {
            return false;
        }
        return rel.starts_with(fragment) || rel.contains(&format!("/{}", fragment));
    }
    // Leading-directory patterns (`src/`) are ROOT-ANCHORED (F2 fix): the
    // old `contains("/src/")` fallback admitted `py_agent/src/x.py`
    // through an allow list of `src/` and applied a live mutation to a
    // production file. A trailing slash already bounds the match
    // (`src/` can't prefix `srcfoo/`); a bare name (`src`) matches the
    // dir itself or anything under it, on a `/` boundary.
    if pattern.is_empty() {
        return false;
    }
    if pattern.ends_with('/') {
        return rel.starts_with(pattern);
    }
    rel == pattern || rel.starts_with(&format!("{}/", pattern))
}

/// Resolve `path` to an absolute, symlink-free form.
///
/// The file may not exist yet (`create_entity`), so fall back to canonicalizing
/// the parent directory and re-attaching the file name — enough to defeat
/// `..` traversal, which is the point.
fn canonicalize_target(path: &str, project_root: Option<&std::path::Path>) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    // F14: plan targets are canonical root-relative ids (`.\d.py`) — they
    // resolve against the project root, not the process CWD (the agent may
    // stand anywhere; resolving via CWD put every canonical id outside
    // the root and every plan died at the gate).
    let abs = match (p.is_absolute(), project_root) {
        (false, Some(root)) => root.join(p),
        _ => p.to_path_buf(),
    };
    if let Ok(c) = std::fs::canonicalize(&abs) {
        return c;
    }
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => match std::fs::canonicalize(parent) {
            Ok(c) => c.join(name),
            Err(_) => abs,
        },
        _ => abs,
    }
}

pub struct MutationEngine {
    pub write_guard: std::sync::Arc<WriteGuard>,
    pub config: crate::graph::MutationConfig,
    /// Root the mutation is confined to. `None` disables the containment check
    /// — set it from the indexed root so a plan cannot reach outside the
    /// project it was planned against.
    pub project_root: Option<std::path::PathBuf>,
}

impl MutationEngine {
    pub fn new(config: crate::graph::MutationConfig) -> Self {
        Self {
            write_guard: shared_write_guard(),
            config,
            project_root: None,
        }
    }

    /// Confine writes to `root`.
    pub fn with_project_root(mut self, root: impl AsRef<std::path::Path>) -> Self {
        self.project_root = std::fs::canonicalize(root.as_ref())
            .ok()
            .or_else(|| Some(root.as_ref().to_path_buf()));
        self
    }

    /// Gate a plan against `MutationConfig` before anything is read or written.
    ///
    /// The trust boundary is the FFI, not the Python caller: `apply_mutation`
    /// accepts an arbitrary JSON plan — any file, any byte span, and an
    /// `expected_hash` that defaults to empty, which the stale check skips. So
    /// the policy has to be enforced here, where every path arrives.
    ///
    /// Returns the reason the plan is refused, or `Ok(())`.
    fn check_policy(&self, plan: &MutationPlan) -> Result<(), String> {
        if !self.config.enabled {
            return Err("mutation engine is disabled by configuration".into());
        }

        if plan.edits.len() > self.config.max_edits_per_plan {
            return Err(format!(
                "plan carries {} edits, over the configured limit of {}",
                plan.edits.len(),
                self.config.max_edits_per_plan
            ));
        }

        if self.config.require_clean_git {
            let repo = self
                .project_root
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            match crate::fs::git::is_worktree_clean(&repo.to_string_lossy()) {
                Ok(true) => {}
                Ok(false) => {
                    return Err("worktree has uncommitted changes and \
                                require_clean_git is set"
                        .into())
                }
                Err(e) => {
                    return Err(format!(
                        "require_clean_git is set but the worktree could not be \
                         checked: {:?}",
                        e
                    ))
                }
            }
        }

        for edit in &plan.edits {
            // An empty hash tells apply() to skip the stale check. Only
            // create_entity legitimately has nothing to compare against.
            if edit.expected_hash.is_empty() && plan.tool != "create_entity" {
                return Err(format!(
                    "edit on {} carries no expected_hash — refusing to write \
                     without a stale-content guard",
                    edit.file
                ));
            }

            let target = canonicalize_target(&edit.file, self.project_root.as_deref());

            let rel = match &self.project_root {
                Some(root) => match target.strip_prefix(root) {
                    Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
                    Err(_) => {
                        return Err(format!(
                            "{} is outside the project root {}",
                            target.display(),
                            root.display()
                        ))
                    }
                },
                None => target.to_string_lossy().replace('\\', "/"),
            };

            if let Some(pattern) = self.config.deny.iter().find(|p| path_matches(&rel, p)) {
                return Err(format!("{} is deny-listed by \"{}\"", rel, pattern));
            }

            if !self.config.allow.is_empty()
                && !self.config.allow.iter().any(|p| path_matches(&rel, p))
            {
                return Err(format!(
                    "{} is outside the configured allow list {:?}",
                    rel, self.config.allow
                ));
            }
        }

        Ok(())
    }

    /// Gate a freshly built plan against `MutationConfig` (F2 follow-up).
    ///
    /// `apply()` has always refused forbidden targets, but the plan
    /// (dry-run) phase previewed them as if allowed. Refusing here means
    /// the agent sees `PolicyViolation` before any diff preview.
    fn gate_plan_policy(&self, plan: MutationPlan) -> Result<MutationPlan, MutationError> {
        if let Err(reason) = self.check_policy(&plan) {
            let path = plan
                .edits
                .first()
                .map(|e| e.file.clone())
                .unwrap_or_default();
            return Err(MutationError::PolicyViolation { path, reason });
        }
        Ok(plan)
    }

    /// Rebase a recorded params span against current file content (F4 fix).
    ///
    /// Concept spans go stale whenever disk moves under the graph; the old
    /// code trusted `params_span` blindly and spliced the new signature
    /// wherever it pointed — into method bodies, over the next def line.
    /// The fast path validates in place; otherwise the span is re-resolved
    /// from the def line (nearest `name (` in a small window) with a
    /// warning. When even that fails, refuse with `StaleIndex` — reindex
    /// and re-plan — instead of writing garbage.
    fn rebased_params_span(
        &self,
        source: &str,
        entity_name: &str,
        def_line: usize,
        file: &str,
        recorded: ByteSpan,
        warnings: &mut Vec<String>,
    ) -> Result<ByteSpan, MutationError> {
        if params_span_valid(source, entity_name, recorded) {
            return Ok(recorded);
        }
        // Byte-exact line starts (CRLF-safe: never assume 1-byte terminators).
        let mut line_starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let n_lines = line_starts.len();
        let line_text = |idx: usize| -> &str {
            let s = line_starts[idx];
            let e = line_starts.get(idx + 1).copied().unwrap_or(source.len());
            source[s..e].trim_end_matches(|c| c == '\r' || c == '\n')
        };
        let def_idx = def_line.saturating_sub(1).min(n_lines - 1);
        let lo = def_idx.saturating_sub(3);
        let hi = (def_idx + 8).min(n_lines);
        // Search outward from the def line: 0, -1, +1, -2, +2, …
        let mut order = vec![def_idx];
        let mut d = 1usize;
        while (def_idx >= d && def_idx - d >= lo) || def_idx + d < hi {
            if def_idx >= d && def_idx - d >= lo {
                order.push(def_idx - d);
            }
            if def_idx + d < hi {
                order.push(def_idx + d);
            }
            d += 1;
        }
        for idx in order {
            let line = line_text(idx);
            let Some(rel) = find_name_paren(line, entity_name) else {
                continue;
            };
            let open = line_starts[idx] + rel;
            let Some(close) = match_paren_end(source, open) else {
                continue;
            };
            let candidate = ByteSpan {
                start: open,
                end: close,
            };
            if params_span_valid(source, entity_name, candidate) {
                warnings.push(format!(
                    "Recorded params span for `{}` was stale (pointed at byte {}); re-resolved from the def line — consider `codegraph_update_file` before relying on call-site edits.",
                    entity_name, recorded.start
                ));
                return Ok(candidate);
            }
        }
        Err(MutationError::StaleIndex {
            file: file.to_string(),
            expected: format!("def-line params for {}", entity_name),
            span: recorded,
        })
    }

    /// Re-base a replacement body for splicing at a `body_span` that starts
    /// at the first body token. See the call site in `plan_body_replacement`
    /// for the full contract and rationale.
    fn normalize_body_for_splice(
        &self,
        new_body: &str,
        file_source: &str,
        body_span: &ByteSpan,
    ) -> String {
        let len = file_source.len();
        let start = body_span.start.min(len);

        // Whitespace between the line start and the span start = the column
        // the body lives at. If the span starts mid-line (inline body such as
        // `def f(): return 1`) there is no column to inherit — passthrough is
        // the only honest behavior there.
        let line_start = file_source[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let prefix = &file_source[line_start..start];
        if prefix.is_empty() || !prefix.chars().all(|c| c == ' ' || c == '\t') {
            return new_body.to_string();
        }
        let body_column: &str = prefix;

        // Incoming base = smallest leading whitespace over non-blank lines.
        let incoming_base = new_body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
            .min()
            .unwrap_or(0);

        let mut out_lines: Vec<String> = Vec::new();
        for (i, raw) in new_body.lines().enumerate() {
            // Normalize CRLF replacements down to LF; the writer joins with
            // \n and the file's own trailing newline stays untouched.
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            if raw.trim().is_empty() {
                out_lines.push(String::new());
                continue;
            }
            let stripped: String = raw.chars().skip(incoming_base).collect();
            if i == 0 {
                // First line inherits whatever the prefix already provides.
                out_lines.push(stripped);
            } else {
                out_lines.push(format!("{}{}", body_column, stripped));
            }
        }
        // `.lines()` already dropped one trailing newline; the span ends
        // before the file's own, so nothing further to trim here.
        out_lines.join("\n")
    }

    /// Brace-language body splice (F3 fix).
    ///
    /// For `{ … }`-delimited languages tree-sitter's `body` node spans the
    /// braces themselves, so the indent-language splice overwrote them with
    /// bare body text — every brace-language apply died on syntax error and
    /// rolled back. Reconstruct the full block instead: strip the incoming
    /// base indent, re-indent to the original body column (or the closing
    /// column plus one unit when the old body is empty), and re-emit the
    /// braces around it. Returns `None` when the span isn't brace-delimited
    /// after all, so the caller falls back to the indent-language path.
    fn brace_splice_body(
        &self,
        new_body: &str,
        file_source: &str,
        body_span: &ByteSpan,
    ) -> Option<String> {
        let src = file_source.get(body_span.start..body_span.end.min(file_source.len()))?;
        let open = src.find('{')?;
        let close = src.rfind('}')?;
        if close <= open {
            return None;
        }
        // The braces must delimit the span (a block body), not merely occur
        // inside it.
        if !src[..open].trim().is_empty() || !src[close + 1..].trim().is_empty() {
            return None;
        }
        let inner = &src[open + 1..close];

        // Closing column = indent of the line holding `}` (in span coords).
        let close_line_start = src[..close].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let close_prefix = &src[close_line_start..close];
        let closing_indent = if close_prefix.chars().all(|c| c == ' ' || c == '\t') {
            close_prefix
        } else {
            ""
        };
        // Body column = indent of the first content line; when the old body
        // is empty (`{}`), one unit deeper than the closing column.
        let body_indent = inner
            .lines()
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .find(|l| !l.trim().is_empty())
            .map(|l| l[..l.len() - l.trim_start().len()].to_string())
            .unwrap_or_else(|| {
                let style = detect_indent_style(file_source);
                if style.unit == '\t' {
                    format!("{}\t", closing_indent)
                } else {
                    format!("{}{}", closing_indent, " ".repeat(style.width))
                }
            });

        // Strip the incoming base indent (same rule as the indent path).
        let incoming_base = new_body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
            .min()
            .unwrap_or(0);

        let mut out = String::from("{\n");
        for raw in new_body.lines() {
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            if raw.trim().is_empty() {
                out.push('\n');
                continue;
            }
            let stripped: String = raw.chars().skip(incoming_base).collect();
            out.push_str(&body_indent);
            out.push_str(&stripped);
            out.push('\n');
        }
        out.push_str(closing_indent);
        out.push('}');
        Some(out)
    }

    /// Plan a body replacement — replaces the function/method body only.
    /// Signature, docstring, and decorators are untouched.
    pub fn plan_body_replacement(
        &self,
        entity_id: &str,
        new_body: &str,
        expected_hash: Option<String>,
        dry_run: bool,
        projection: &ProjectedGraph,
    ) -> Result<MutationPlan, MutationError> {
        // 1. Look up entity by id → get body_span
        let fn_entity = projection
            .functions
            .get(entity_id)
            .ok_or_else(|| MutationError::EntityNotFound(entity_id.to_string()))?;

        let body_span = fn_entity.body_span;

        // Detect indent style from the file (spaces vs tabs, width).
        let file_path = module_file_path(projection, &fn_entity.parent_module);
        let file_source = read_project_file(&file_path);

        // Stale-write guard: hash the current body span content so apply() can
        // reject the edit if the file changed between planning and applying.
        let computed_hash = hash_span(file_source.as_bytes(), body_span);
        let edit_expected_hash = expected_hash.unwrap_or(computed_hash);

        // Body spans start at the first body token (or inline `{`), so the
        // leading indentation is NOT part of the span: the file's own body
        // indent stays in the prefix and the replacement's FIRST line is
        // spliced in right after it. Continuation lines, however, arrive
        // without any prefix — they must carry the body column themselves.
        // The splice geometry is therefore asymmetric:
        //
        //   line 1        → stripped of its incoming base indent (inherits
        //                    whatever the prefix already provides)
        //   lines 2+      → stripped to their relative indent, then given
        //                    the source's body column
        //   trailing \n   → dropped (the span ends before the file's own
        //                    newline; keeping it would add a blank line)
        //
        // Both natural spellings now produce identical files: an unindented
        // body ("return a + 1") and a source-copied body ("    return a + 1")
        // — CODERADAR_BUGS_QUIRKS #1 follow-up. Verbatim passthrough remains
        // for inline bodies where the span starts mid-line.
        //
        // Brace-delimited languages (F3) take a different road: their body
        // span INCLUDES the `{ … }`, so the path above ate the braces and
        // every apply rolled back. `brace_splice_body` reconstructs the
        // whole block; it returns None when the span isn't brace-delimited
        // after all, and then the indent path below still applies.
        let lang = crate::types::Language::from_extension(
            std::path::Path::new(&file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("py"),
        );
        let normalized_body = if lang.uses_braces() {
            self.brace_splice_body(new_body, &file_source, &body_span)
                .unwrap_or_else(|| {
                    self.normalize_body_for_splice(new_body, &file_source, &body_span)
                })
        } else {
            self.normalize_body_for_splice(new_body, &file_source, &body_span)
        };

        let mut warnings = if fn_entity.parse_quality != ParseQuality::Clean {
            vec!["Entity parse quality is not Clean — body_span may be approximate".into()]
        } else {
            Vec::new()
        };

        // BUGS_QUIRKS #4: "body" is everything under the signature line, so
        // replacing it used to silently delete an existing docstring. Dropping
        // documentation may be intended — but it must never be silent.
        if !file_source.is_empty() {
            if let Some(old_body) =
                file_source.get(body_span.start..body_span.end.min(file_source.len()))
            {
                if has_leading_docstring(old_body) && !has_leading_docstring(&normalized_body) {
                    warnings.push(
                        "The existing docstring is NOT present in the replacement body — "
                            .to_string()
                            + "re-include it, or apply knowing the documentation will be removed.",
                    );
                }
            }
        }

        let plan_id = ulid::Ulid::new().to_string();

        let edits = vec![MutationEdit {
            file: module_file_path(projection, &fn_entity.parent_module),
            span: body_span,
            replacement: normalized_body,
            expected_hash: edit_expected_hash,
        }];

        // A real diff, not "replace 412 bytes at 1830..2242": the MCP
        // tool promises a preview for review and renders it in a ```diff
        // fence, so the preview has to be one.
        let preview = if dry_run {
            crate::mutation::edit::diff_preview_for_edits(&edits)
        } else {
            String::new()
        };

        self.gate_plan_policy(MutationPlan {
            id: plan_id,
            tool: "replace_entity_body".to_string(),
            affected_files: vec![module_file_path(projection, &fn_entity.parent_module)],
            diff_preview: preview,
            edits,
            unverified_sites: Vec::new(),
            warnings,
        })
    }

    /// Plan a signature update with full call-site cascade.
    pub fn plan_signature_update(
        &self,
        entity_id: &str,
        new_signature: &str,
        call_site_values: &HashMap<String, String>,
        inject_defaults: bool,
        dry_run: bool,
        projection: &ProjectedGraph,
    ) -> Result<MutationPlan, MutationError> {
        let fn_entity = projection
            .functions
            .get(entity_id)
            .ok_or_else(|| MutationError::EntityNotFound(entity_id.to_string()))?;

        let params_span = fn_entity.params_span;

        let def_file = module_file_path(projection, &fn_entity.parent_module);
        let def_source = read_project_file(&def_file);

        // Warnings live for the whole plan: the rebase below may add one.
        let mut warnings = Vec::new();

        // F4: never trust the recorded span against live disk. A graph that
        // lags the file spliced new signatures into method bodies and over
        // the next def line. Validate, re-resolve, or refuse — in that order.
        let params_span = self.rebased_params_span(
            &def_source,
            &fn_entity.name,
            fn_entity.line,
            &def_file,
            params_span,
            &mut warnings,
        )?;

        // The MCP tool asks the agent for a whole `def f(a, b) -> str:` line,
        // but `params_span` is exactly the parenthesised parameter list. The
        // whole line used to be written into that span, producing
        // `def greetdef greet(name, punctuation)::` — a syntax error that
        // `apply` caught and rolled back, so the tool reliably did nothing.
        let (header_span, replacement) = signature_header(&def_source, params_span, new_signature)?;

        // Stale-write guard: hash what is there now.
        let def_expected_hash = hash_span(def_source.as_bytes(), header_span);

        let mut edits = Vec::new();
        let mut unverified = Vec::new();

        // A different name in the new signature is a rename, and renames have
        // their own plan because they have to reach call sites. Say so rather
        // than half-doing it.
        if let Some(new_name) = signature_name(new_signature) {
            if new_name != fn_entity.name {
                warnings.push(format!(
                    "New signature names `{}` but this entity is `{}` — the \n                     name was left alone. Use rename to change it.",
                    new_name, fn_entity.name
                ));
            }
        }

        // 2. Definition edit: replace the header from `(` to just before `:`
        edits.push(MutationEdit {
            file: def_file,
            span: header_span,
            replacement,
            expected_hash: def_expected_hash,
        });

        // 3. Enumerate call sites and generate per-site edits
        let callers = projection
            .callers_by_callee
            .get(entity_id)
            .cloned()
            .unwrap_or_default();

        if callers.len() > self.config.max_files_per_plan {
            return Err(MutationError::TooManyFiles(callers.len()));
        }

        for caller_id in &callers {
            if let Some(caller_fn) = projection.functions.get(caller_id) {
                let caller_file = module_file_path(projection, &caller_fn.parent_module);
                for (i, call) in caller_fn.resolved_calls.iter().enumerate() {
                    // Check if this call targets the entity being modified
                    let target_matches = match call {
                        ResolvedCall::Function(id)
                        | ResolvedCall::Method { method: id, .. }
                        | ResolvedCall::Constructor(id) => id == entity_id,
                        _ => false,
                    };

                    if !target_matches {
                        continue;
                    }

                    // resolved_calls is parallel to calls — use index i for line/col
                    let line = caller_fn.calls.get(i).map(|c| c.line as u32).unwrap_or(0);

                    if let Some(kv_value) = call_site_values.get(caller_id) {
                        // No arg span is stored in the graph — surface for manual review
                        unverified.push(UnverifiedSite {
                            file: caller_file.clone(),
                            line,
                            snippet: format!("call to {} — new args: {}", entity_id, kv_value),
                            reason: "Call-site arg span unavailable; apply arg change manually"
                                .into(),
                        });
                    } else if inject_defaults {
                        warnings.push(format!(
                            "Call site in {} line {} — inject default args manually",
                            caller_file, line
                        ));
                    } else {
                        unverified.push(UnverifiedSite {
                            file: caller_file.clone(),
                            line,
                            snippet: format!("call to {} in {}", entity_id, caller_fn.id),
                            reason: "Call site needs manual update — no values provided".into(),
                        });
                    }
                }
            }
        }

        // 4. Textual backstop (v0.8 P2-5): a call site resolution did not
        //    record (macro body, unsupported syntax) still reads `name(` —
        //    report it unverified instead of leaving it to break silently.
        {
            let mut covered = structural_call_site_lines(
                projection,
                std::slice::from_ref(&entity_id.to_string()),
            );
            covered.push((
                module_file_path(projection, &fn_entity.parent_module),
                fn_entity.line as u32,
            ));
            push_textual_backstop(projection, &fn_entity.name, &covered, &mut unverified);
        }

        let plan_id = ulid::Ulid::new().to_string();

        // A real diff, not "replace 412 bytes at 1830..2242": the MCP
        // tool promises a preview for review and renders it in a ```diff
        // fence, so the preview has to be one.
        let preview = if dry_run {
            crate::mutation::edit::diff_preview_for_edits(&edits)
        } else {
            String::new()
        };

        self.gate_plan_policy(MutationPlan {
            id: plan_id,
            tool: "update_signature".to_string(),
            edits,
            affected_files: {
                let mut files: Vec<String> = callers
                    .iter()
                    .filter_map(|id| projection.functions.get(id))
                    .map(|f| module_file_path(projection, &f.parent_module))
                    .collect();
                files.push(module_file_path(projection, &fn_entity.parent_module));
                files.sort();
                files.dedup();
                files
            },
            diff_preview: preview,
            unverified_sites: unverified,
            warnings,
        })
    }

    /// Plan a symbol rename across the codebase.
    ///
    /// Dispatches on entity kind up front. Functions and classes are referenced
    /// in different shapes — call sites versus base-class lists — so each gets
    /// its own plan path.
    pub fn plan_rename(
        &self,
        entity_id: &str,
        new_name: &str,
        include_strings: bool,
        dry_run: bool,
        projection: &ProjectedGraph,
    ) -> Result<MutationPlan, MutationError> {
        if projection.functions.contains_key(entity_id) {
            self.plan_rename_function(entity_id, new_name, include_strings, dry_run, projection)
        } else if projection.classes.contains_key(entity_id) {
            self.plan_rename_class(entity_id, new_name, include_strings, dry_run, projection)
        } else {
            Err(MutationError::EntityNotFound(entity_id.to_string()))
        }
    }

    /// Verify a reference recorded at `(line, col)` and turn it into an edit.
    ///
    /// `name` is what the index says sits there. If the bytes disagree the file
    /// has moved under the index, and we record an unverified site rather than
    /// overwrite whatever now occupies that position.
    #[allow(clippy::too_many_arguments)]
    fn push_reference_edit(
        source: &[u8],
        file: &str,
        line: usize,
        col: usize,
        name: &str,
        new_name: &str,
        edits: &mut Vec<MutationEdit>,
        affected: &mut Vec<String>,
        unverified: &mut Vec<UnverifiedSite>,
    ) {
        let verified = line_col_to_byte(source, line, col)
            .map(|start| ByteSpan {
                start,
                end: (start + name.len()).min(source.len()),
            })
            .filter(|span| span_holds_name(source, *span, name));

        match verified {
            Some(span) => {
                edits.push(MutationEdit {
                    file: file.to_string(),
                    span,
                    replacement: new_name.to_string(),
                    expected_hash: hash_span(source, span),
                });
                if !affected.iter().any(|f| f == file) {
                    affected.push(file.to_string());
                }
            }
            None => unverified.push(UnverifiedSite {
                file: file.to_string(),
                line: line as u32,
                snippet: name.to_string(),
                reason: format!(
                    "Reference no longer holds \"{}\" at line {} col {} — \
                     file changed since indexing; reindex and retry",
                    name, line, col
                ),
            }),
        }
    }

    /// Rewrite every call site whose resolved target is one of `targets`.
    ///
    /// A class is reachable under more than one id — as itself and as its
    /// synthesized constructor — so the caller passes every spelling.
    fn collect_call_site_edits(
        &self,
        targets: &[String],
        new_name: &str,
        projection: &ProjectedGraph,
        edits: &mut Vec<MutationEdit>,
        affected: &mut Vec<String>,
        unverified: &mut Vec<UnverifiedSite>,
    ) -> Result<usize, MutationError> {
        let mut callers: Vec<String> = targets
            .iter()
            .filter_map(|t| projection.callers_by_callee.get(t.as_str()))
            .flat_map(|set| set.iter().cloned())
            .collect();
        callers.sort();
        callers.dedup();

        if callers.len() > self.config.max_files_per_plan {
            return Err(MutationError::TooManyFiles(callers.len()));
        }

        for caller_id in &callers {
            let Some(caller_fn) = projection.functions.get(caller_id) else {
                continue;
            };
            let caller_file = module_file_path(projection, &caller_fn.parent_module);
            let caller_source = read_project_file(&caller_file).into_bytes();

            for (i, rc) in caller_fn.resolved_calls.iter().enumerate() {
                let targets_entity = match rc {
                    ResolvedCall::Function(id)
                    | ResolvedCall::Method { method: id, .. }
                    | ResolvedCall::Constructor(id) => targets.iter().any(|t| t == id),
                    _ => false,
                };
                if !targets_entity {
                    continue;
                }

                // resolved_calls is parallel to calls — use index i
                let Some(call) = caller_fn.calls.get(i) else {
                    continue;
                };

                if call.path.is_empty() {
                    Self::push_reference_edit(
                        &caller_source,
                        &caller_file,
                        call.line,
                        call.col,
                        &call.name,
                        new_name,
                        edits,
                        affected,
                        unverified,
                    );
                } else {
                    // Method/attribute call `obj.foo()` — needs manual review
                    unverified.push(UnverifiedSite {
                        file: caller_file.clone(),
                        line: call.line as u32,
                        snippet: format!("{}.{}", call.path.join("."), call.name),
                        reason: "Method/attribute call-site rename needs manual review".into(),
                    });
                }
            }
        }

        Ok(callers.len())
    }

    /// Rewrite the `from X import <old>` bindings that name this entity
    /// (R2-17): a rename that stops at the definition + call sites leaves
    /// every import binding dangling — `from app import combine` still
    /// says `combine` after the definition became `combine_r2`, and the
    /// next resolve lands on `external::`. Each module's explicit-name
    /// imports are checked independently, so re-export chains heal link
    /// by link in one plan: the import's source module is resolved,
    /// `find_symbol_in_module` (cycle guard + shadow rules) must land on
    /// exactly this entity, and the bound-name span is located with
    /// tree-sitter — every identifier after the `import`/`export` keyword
    /// that is not an `as` alias. Star imports need nothing (runtime
    /// name-agnostic); `__all__` string literals stay for review.
    fn collect_import_binding_edits(
        &self,
        entity_id: &str,
        old_name: &str,
        new_name: &str,
        projection: &ProjectedGraph,
        edits: &mut Vec<MutationEdit>,
        affected: &mut Vec<String>,
        unverified: &mut Vec<UnverifiedSite>,
    ) -> Result<usize, MutationError> {
        use crate::graph::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};
        use crate::types::{ImportKind, ImportResolution, SymbolId};

        // (importer module id, import id) pairs confirmed to bind us.
        let mut confirmed: Vec<(String, String)> = Vec::new();
        for (mod_id, module) in &projection.modules {
            for import_id in &module.imports {
                let Some(import) = projection.imports.get(import_id) else {
                    continue;
                };
                let names: &Vec<(String, Option<String>)> = match &import.kind {
                    ImportKind::FromImport { names, .. } => names,
                    ImportKind::RelativeImport { names, .. } => names,
                    // ModuleImport/Side bind modules, StarImport is
                    // name-agnostic at runtime — nothing to rewrite.
                    _ => continue,
                };
                if !names.iter().any(|(orig, _)| orig == old_name) {
                    continue;
                }
                let bound = match &import.resolution {
                    ImportResolution::Symbol(SymbolId::Function(id))
                    | ImportResolution::Symbol(SymbolId::Class(id)) => id == entity_id,
                    ImportResolution::Module(src) => {
                        find_symbol_in_module(projection, src, old_name).as_deref()
                            == Some(entity_id)
                    }
                    ImportResolution::Wildcard { module, .. } => {
                        find_symbol_in_module(projection, module, old_name).as_deref()
                            == Some(entity_id)
                    }
                    // Unresolved/External/Dynamic/Symbol(Module|Import):
                    // fall back to the dotted source name, fail closed.
                    _ => {
                        let dotted: Option<&str> = match &import.kind {
                            ImportKind::FromImport { module, .. } => Some(module.as_str()),
                            ImportKind::RelativeImport { module, .. } => module.as_deref(),
                            _ => None,
                        };
                        dotted
                            .and_then(|d| find_module_by_dotted_name(projection, d, mod_id))
                            .is_some_and(|src| {
                                find_symbol_in_module(projection, &src, old_name).as_deref()
                                    == Some(entity_id)
                            })
                    }
                };
                if bound {
                    confirmed.push((mod_id.clone(), import_id.clone()));
                }
            }
        }

        // Cap like call sites: a widely re-exported name fans out the same way.
        let mut files: Vec<String> = confirmed
            .iter()
            .filter_map(|(m, _)| projection.modules.get(m))
            .map(|m| m.path.to_string_lossy().to_string())
            .collect();
        files.sort();
        files.dedup();
        if files.len() > self.config.max_files_per_plan {
            return Err(MutationError::TooManyFiles(files.len()));
        }

        // Group by importer file: parse once, locate each binding in it.
        let mut by_file: Vec<(String, crate::types::Language, Vec<String>)> = Vec::new();
        for (mod_id, import_id) in &confirmed {
            let Some(module) = projection.modules.get(mod_id) else {
                continue;
            };
            let file = module.path.to_string_lossy().to_string();
            match by_file.iter_mut().find(|(f, _, _)| f == &file) {
                Some((_, _, ids)) => ids.push(import_id.clone()),
                None => {
                    by_file.push((file, module.language, vec![import_id.clone()]));
                }
            }
        }

        for (file, language, import_ids) in &by_file {
            let source = read_project_file(file).into_bytes();
            let tree: Option<tree_sitter::Tree> = crate::graph::CodeGraph::ts_language(language)
                .and_then(|ts_lang| {
                    let mut parser = tree_sitter::Parser::new();
                    parser.set_language(&ts_lang).ok()?;
                    parser.parse(&source, None)
                });
            for import_id in import_ids {
                let Some(import) = projection.imports.get(import_id) else {
                    continue;
                };
                let Some(stmt_tree) = tree.as_ref() else {
                    unverified.push(UnverifiedSite {
                        file: file.clone(),
                        line: import.line as u32,
                        snippet: format!("from-import binding of \"{old_name}\""),
                        reason: "No tree-sitter grammar: import binding needs manual review".into(),
                    });
                    continue;
                };
                let Some(stmt) = Self::import_statement_at_line(stmt_tree.root_node(), import.line)
                else {
                    unverified.push(UnverifiedSite {
                        file: file.clone(),
                        line: import.line as u32,
                        snippet: format!("from-import binding of \"{old_name}\""),
                        reason:
                            "Import statement moved since indexing: binding needs manual review"
                                .into(),
                    });
                    continue;
                };
                let spans = Self::import_binding_spans(&source, stmt, old_name);
                if spans.is_empty() {
                    unverified.push(UnverifiedSite {
                        file: file.clone(),
                        line: import.line as u32,
                        snippet: format!("from-import binding of \"{old_name}\""),
                        reason: "Bound name not located in import statement: needs manual review"
                            .into(),
                    });
                    continue;
                }
                for span in spans {
                    if !span_holds_name(&source, span, old_name) {
                        unverified.push(UnverifiedSite {
                            file: file.clone(),
                            line: import.line as u32,
                            snippet: old_name.to_string(),
                            reason: format!(
                                "Reference no longer holds \"{old_name}\" — file changed since                                  indexing; reindex and retry"
                            ),
                        });
                        continue;
                    }
                    edits.push(MutationEdit {
                        file: file.clone(),
                        span,
                        replacement: new_name.to_string(),
                        expected_hash: hash_span(&source, span),
                    });
                    if !affected.iter().any(|f| f == file) {
                        affected.push(file.clone());
                    }
                }
            }
        }

        Ok(files.len())
    }

    /// The import statement starting on `line` (1-based), located
    /// grammar-agnostically: the first `import`/`export` keyword leaf on
    /// that row, whose parent is its statement in every grammar's import
    /// forms. (An outermost-starter search returns the whole program root
    /// for line-1 imports — and the binding walk then eats same-named
    /// call sites as duplicates. The keyword's parent can't overreach.)
    fn import_statement_at_line(
        root: tree_sitter::Node<'_>,
        line: usize,
    ) -> Option<tree_sitter::Node<'_>> {
        let target = line.saturating_sub(1);
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if n.child_count() == 0 {
                let kind = n.kind();
                if (kind == "import" || kind == "export") && n.start_position().row == target {
                    return match n.parent() {
                        Some(p) if p.start_position().row == target => Some(p),
                        // Degenerate: walk it anyway, find nothing, report.
                        _ => Some(n),
                    };
                }
            } else {
                for i in (0..n.child_count()).rev() {
                    if let Some(c) = n.child(i as u32) {
                        stack.push(c);
                    }
                }
            }
        }
        None
    }

    /// Bound-name spans in one import statement: every identifier after the
    /// `import`/`export` keyword whose previous significant token is not
    /// `as` — so `from x import combine as c` rewrites the bound `combine`
    /// and keeps the alias, while `from x import c as combine` (a local
    /// alias, still valid after the rename) is left alone.
    fn import_binding_spans(
        source: &[u8],
        stmt: tree_sitter::Node<'_>,
        old_name: &str,
    ) -> Vec<ByteSpan> {
        fn walk(
            n: tree_sitter::Node<'_>,
            source: &[u8],
            old_name: &str,
            seen_kw: &mut bool,
            prev_as: &mut bool,
            out: &mut Vec<ByteSpan>,
        ) {
            if n.child_count() == 0 {
                let text = n.utf8_text(source).unwrap_or("");
                if text == "import" || text == "export" {
                    *seen_kw = true;
                    *prev_as = false;
                } else if *seen_kw {
                    if n.is_named() {
                        if text == old_name && !*prev_as {
                            out.push(ByteSpan {
                                start: n.start_byte(),
                                end: n.end_byte(),
                            });
                        }
                        *prev_as = false;
                    } else {
                        *prev_as = text == "as";
                    }
                }
                return;
            }
            let mut cursor = n.walk();
            for child in n.children(&mut cursor) {
                walk(child, source, old_name, seen_kw, prev_as, out);
            }
        }
        let mut out = Vec::new();
        let mut seen_kw = false;
        let mut prev_as = false;
        walk(stmt, source, old_name, &mut seen_kw, &mut prev_as, &mut out);
        out
    }

    fn plan_rename_function(
        &self,
        entity_id: &str,
        new_name: &str,
        include_strings: bool,
        dry_run: bool,
        projection: &ProjectedGraph,
    ) -> Result<MutationPlan, MutationError> {
        let fn_entity = projection
            .functions
            .get(entity_id)
            .ok_or_else(|| MutationError::EntityNotFound(entity_id.to_string()))?;

        let name_span = fn_entity.name_span;
        let mut edits = Vec::new();
        let mut affected = Vec::new();
        let mut unverified = Vec::new();

        // 1. Definition: rewrite name_span
        let def_file = module_file_path(projection, &fn_entity.parent_module);
        let def_source = read_project_file(&def_file).into_bytes();
        // The definition anchors the whole rename — if the index no longer
        // agrees with disk here, every span in this plan is suspect. Fail the
        // plan rather than renaming call sites against a definition we would
        // have mangled.
        if !span_holds_name(&def_source, name_span, &fn_entity.name) {
            return Err(MutationError::StaleIndex {
                file: def_file,
                expected: fn_entity.name.clone(),
                span: name_span,
            });
        }
        edits.push(MutationEdit {
            file: def_file.clone(),
            span: name_span,
            replacement: new_name.to_string(),
            expected_hash: hash_span(&def_source, name_span),
        });
        affected.push(def_file);

        // 2. Caller side: rewrite every verified reference
        let _ = self.collect_call_site_edits(
            std::slice::from_ref(&entity_id.to_string()),
            new_name,
            projection,
            &mut edits,
            &mut affected,
            &mut unverified,
        )?;

        // 2b. Import side (R2-17): rewrite the `from X import <old>`
        // bindings that name this entity, link by link down any
        // re-export chain — otherwise the renamed definition strands
        // every importer on `external::`.
        let _ = self.collect_import_binding_edits(
            entity_id,
            &fn_entity.name,
            new_name,
            projection,
            &mut edits,
            &mut affected,
            &mut unverified,
        )?;

        // 3. String-literal occurrences (only if include_strings=true)
        if include_strings {
            unverified.push(UnverifiedSite {
                file: module_file_path(projection, &fn_entity.parent_module),
                line: 0,
                snippet: format!("string-literal references to \"{}\"", fn_entity.name),
                reason: "String-literal rename requires manual review".into(),
            });
        }

        // 3b. Textual backstop (v0.8 P2-5): occurrences of `name(` the graph
        //     did not resolve as a call to this entity.
        {
            let mut covered = structural_call_site_lines(
                projection,
                std::slice::from_ref(&entity_id.to_string()),
            );
            covered.push((
                module_file_path(projection, &fn_entity.parent_module),
                fn_entity.line as u32,
            ));
            push_textual_backstop(projection, &fn_entity.name, &covered, &mut unverified);
        }

        // A real diff, not "replace 412 bytes at 1830..2242": the MCP
        // tool promises a preview for review and renders it in a ```diff
        // fence, so the preview has to be one.
        let preview = if dry_run {
            crate::mutation::edit::diff_preview_for_edits(&edits)
        } else {
            String::new()
        };

        self.gate_plan_policy(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "rename_symbol".to_string(),
            edits,
            affected_files: affected,
            diff_preview: preview,
            unverified_sites: unverified,
            warnings: Vec::new(),
        })
    }

    /// Rename a class: its definition, every subclass's base list, and every
    /// construction site.
    fn plan_rename_class(
        &self,
        entity_id: &str,
        new_name: &str,
        include_strings: bool,
        dry_run: bool,
        projection: &ProjectedGraph,
    ) -> Result<MutationPlan, MutationError> {
        let cls = projection
            .classes
            .get(entity_id)
            .ok_or_else(|| MutationError::EntityNotFound(entity_id.to_string()))?;

        let mut edits = Vec::new();
        let mut affected = Vec::new();
        let mut unverified = Vec::new();

        // 1. Definition: rewrite name_span
        let def_file = module_file_path(projection, &cls.parent_module);
        let def_source = read_project_file(&def_file).into_bytes();
        if !span_holds_name(&def_source, cls.name_span, &cls.name) {
            return Err(MutationError::StaleIndex {
                file: def_file,
                expected: cls.name.clone(),
                span: cls.name_span,
            });
        }
        edits.push(MutationEdit {
            file: def_file.clone(),
            span: cls.name_span,
            replacement: new_name.to_string(),
            expected_hash: hash_span(&def_source, cls.name_span),
        });
        affected.push(def_file);

        // 2. Subclasses: `class Sub(Old)` → `class Sub(New)`
        let subclasses = projection
            .subclasses
            .get(entity_id)
            .cloned()
            .unwrap_or_default();

        for sub_id in &subclasses {
            let Some(sub) = projection.classes.get(sub_id.as_str()) else {
                continue;
            };
            let sub_file = module_file_path(projection, &sub.parent_module);
            let sub_source = read_project_file(&sub_file).into_bytes();

            for base in sub.bases.iter().filter(|b| b.name == cls.name) {
                if base.path.is_empty() {
                    Self::push_reference_edit(
                        &sub_source,
                        &sub_file,
                        base.line,
                        base.col,
                        &base.name,
                        new_name,
                        &mut edits,
                        &mut affected,
                        &mut unverified,
                    );
                } else {
                    // Qualified base `mod.Old` — the reference spans more than
                    // the bare name; leave it to review.
                    unverified.push(UnverifiedSite {
                        file: sub_file.clone(),
                        line: base.line as u32,
                        snippet: format!("{}.{}", base.path.join("."), base.name),
                        reason: "Qualified base-class reference needs manual review".into(),
                    });
                }
            }
        }

        // 3. Construction sites. A `Old()` call resolves under the class id or
        // under its constructor id depending on the cascade layer that matched,
        // so ask for both.
        let targets = vec![entity_id.to_string(), format!("{}.__init__", entity_id)];
        let caller_count = self.collect_call_site_edits(
            &targets,
            new_name,
            projection,
            &mut edits,
            &mut affected,
            &mut unverified,
        )?;

        // 3b. Import side (R2-17): `from X import Old` bindings name the
        // class the same way they name functions — heal the chain.
        let _ = self.collect_import_binding_edits(
            entity_id,
            &cls.name,
            new_name,
            projection,
            &mut edits,
            &mut affected,
            &mut unverified,
        )?;

        let mut warnings = Vec::new();
        if caller_count == 0 {
            // The cascade currently classifies `Old()` as an External call
            // (resolve_calls.rs edge target `external::Old`), so construction
            // sites produce no callers_by_callee entry to walk. Say so rather
            // than let the definition be renamed out from under them.
            warnings.push(format!(
                "No construction sites resolved for \"{}\" — `{}(...)` calls are not \
                 rewritten by this plan; check them manually",
                cls.name, cls.name
            ));
        }

        // 4. String-literal occurrences (only if include_strings=true)
        if include_strings {
            unverified.push(UnverifiedSite {
                file: module_file_path(projection, &cls.parent_module),
                line: 0,
                snippet: format!("string-literal references to \"{}\"", cls.name),
                reason: "String-literal rename requires manual review".into(),
            });
        }

        // 4b. Textual backstop (v0.8 P2-5): `Old(` occurrences the cascade
        //     did not classify as construction sites (the known gap: `Old()`
        //     resolves as External and produces no callers entry).
        {
            let mut covered: Vec<(String, u32)> = vec![(
                module_file_path(projection, &cls.parent_module),
                cls.line as u32,
            )];
            for sub_id in &subclasses {
                let Some(sub) = projection.classes.get(sub_id.as_str()) else {
                    continue;
                };
                let sub_file = module_file_path(projection, &sub.parent_module);
                for base in sub.bases.iter().filter(|b| b.name == cls.name) {
                    covered.push((sub_file.clone(), base.line as u32));
                }
            }
            covered.extend(structural_call_site_lines(projection, &targets));
            push_textual_backstop(projection, &cls.name, &covered, &mut unverified);
        }

        // A real diff, not "replace 412 bytes at 1830..2242": the MCP
        // tool promises a preview for review and renders it in a ```diff
        // fence, so the preview has to be one.
        let preview = if dry_run {
            crate::mutation::edit::diff_preview_for_edits(&edits)
        } else {
            String::new()
        };

        self.gate_plan_policy(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "rename_symbol".to_string(),
            edits,
            affected_files: affected,
            diff_preview: preview,
            unverified_sites: unverified,
            warnings,
        })
    }

    /// Plan a new entity creation anchored after an existing entity or at file top/end.
    pub fn plan_create_entity(
        &self,
        target_file: &str,
        anchor: &str,
        code: &str,
        dry_run: bool,
        projection: &ProjectedGraph,
    ) -> Result<MutationPlan, MutationError> {
        // 1. Determine insertion point + replacement (with newline normalization)
        // F14: agent-supplied target resolves against the indexed root.
        let file_bytes = std::fs::read(disk_path_for(target_file)).unwrap_or_default();
        let file_len = file_bytes.len();
        let code_trimmed = code.trim_matches(['\n', '\r']);

        let (insert_span, replacement) = if anchor == "top" || anchor.is_empty() {
            // Insert at file top (after a UTF-8 BOM if present)
            let start = if file_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                3
            } else {
                0
            };
            (
                ByteSpan { start, end: start },
                format!("{}\n", code_trimmed),
            )
        } else if anchor == "end" {
            // Insert at file end, blank-line separated from existing content
            let repl = if file_len == 0 {
                format!("{}\n", code_trimmed)
            } else {
                format!("\n{}\n", code_trimmed)
            };
            (
                ByteSpan {
                    start: file_len,
                    end: file_len,
                },
                repl,
            )
        } else {
            // Anchor after a specific entity
            let span = if let Some(fn_ent) = projection.functions.get(anchor) {
                ByteSpan {
                    start: fn_ent.span.end,
                    end: fn_ent.span.end,
                }
            } else if let Some(cls_ent) = projection.classes.get(anchor) {
                ByteSpan {
                    start: cls_ent.span.end,
                    end: cls_ent.span.end,
                }
            } else {
                return Err(MutationError::EntityNotFound(anchor.to_string()));
            };
            (span, format!("\n{}\n", code_trimmed))
        };

        let plan_id = ulid::Ulid::new().to_string();

        let edits = vec![MutationEdit {
            file: target_file.to_string(),
            span: insert_span,
            replacement,
            expected_hash: String::new(),
        }];

        // A real diff, not "replace 412 bytes at 1830..2242": the MCP
        // tool promises a preview for review and renders it in a ```diff
        // fence, so the preview has to be one.
        let preview = if dry_run {
            crate::mutation::edit::diff_preview_for_edits(&edits)
        } else {
            String::new()
        };

        self.gate_plan_policy(MutationPlan {
            id: plan_id,
            tool: "create_entity".to_string(),
            affected_files: vec![target_file.to_string()],
            diff_preview: preview,
            edits,
            unverified_sites: Vec::new(),
            warnings: vec!["Preflight parse-check deferred to Python layer".into()],
        })
    }

    /// Apply a mutation plan: stale-check → backup → write → post-verify → rollback.
    pub fn apply(&mut self, plan: &MutationPlan) -> MutationResult {
        use std::io::Write;

        // ── Phase 0: Policy ─────────────────────────────────────────────
        // Before anything is read or written. See check_policy for why the
        // gate lives here rather than in the Python caller.
        if let Err(reason) = self.check_policy(plan) {
            return MutationResult {
                status: MutationStatus::RejectedPolicy,
                files_written: vec![],
                syntax_errors: vec![SyntaxDiagnostic {
                    file: plan
                        .edits
                        .first()
                        .map(|e| e.file.clone())
                        .unwrap_or_default(),
                    line: 0,
                    column: 0,
                    message: format!("policy rejected the plan — {}", reason),
                    offending_span: ByteSpan { start: 0, end: 0 },
                }],
                reindex: ReindexSummary {
                    files: 0,
                    entities_updated: 0,
                    edges_updated: 0,
                    duration_ms: 0,
                },
                backup_path: None,
            };
        }

        // Group edits by file path (all edits carry real byte spans)
        let mut by_file: HashMap<String, Vec<MutationEdit>> = HashMap::new();
        for edit in &plan.edits {
            by_file
                .entry(edit.file.clone())
                .or_default()
                .push(edit.clone());
        }

        let mut syntax_errors: Vec<SyntaxDiagnostic> = Vec::new();
        let mut originals: HashMap<String, String> = HashMap::new();

        // ── Phase 1: Stale-write rejection ──────────────────────────────
        // Every non-empty expected_hash must match the current content at its
        // span. Any mismatch → reject the entire plan (nothing is written).
        for (file_path, edits) in &by_file {
            // F14: keys are canonical root-relative ids; resolve for disk.
            let disk_path = if std::path::Path::new(file_path).is_absolute() {
                std::path::PathBuf::from(file_path)
            } else {
                crate::indexed_root().join(file_path)
            };
            let original = match std::fs::read_to_string(&disk_path)
                .or_else(|_| std::fs::read_to_string(file_path))
            {
                Ok(s) => s,
                Err(e) => {
                    syntax_errors.push(SyntaxDiagnostic {
                        file: file_path.clone(),
                        line: 0,
                        column: 0,
                        message: format!("read failed: {}", e),
                        offending_span: ByteSpan { start: 0, end: 0 },
                    });
                    return MutationResult {
                        status: MutationStatus::RolledBack,
                        files_written: vec![],
                        syntax_errors,
                        reindex: ReindexSummary {
                            files: 0,
                            entities_updated: 0,
                            edges_updated: 0,
                            duration_ms: 0,
                        },
                        backup_path: None,
                    };
                }
            };
            for edit in edits {
                if edit.expected_hash.is_empty() {
                    continue;
                }
                let actual = hash_span(original.as_bytes(), edit.span);
                if actual != edit.expected_hash {
                    syntax_errors.push(SyntaxDiagnostic {
                        file: file_path.clone(), line: 0, column: 0,
                        message: format!(
                            "stale edit rejected — content changed since planning (expected {}, found {})",
                            edit.expected_hash, actual
                        ),
                        offending_span: edit.span,
                    });
                    return MutationResult {
                        status: MutationStatus::RejectedStale,
                        files_written: vec![],
                        syntax_errors,
                        reindex: ReindexSummary {
                            files: 0,
                            entities_updated: 0,
                            edges_updated: 0,
                            duration_ms: 0,
                        },
                        backup_path: None,
                    };
                }
            }
            originals.insert(file_path.clone(), original);
        }

        // ── Phase 2: Backup every target file ────────────────────────────
        let mut backups: Vec<(String, String)> = Vec::new(); // (file, backup)
        for file_path in by_file.keys() {
            let backup_path = format!("{}.coderadar-bak", file_path);
            if let Err(e) = std::fs::copy(disk_path_for(file_path), disk_path_for(&backup_path)) {
                for (_, bp) in &backups {
                    let _ = std::fs::remove_file(disk_path_for(bp));
                }
                syntax_errors.push(SyntaxDiagnostic {
                    file: file_path.clone(),
                    line: 0,
                    column: 0,
                    message: format!("backup failed: {}", e),
                    offending_span: ByteSpan { start: 0, end: 0 },
                });
                return MutationResult {
                    status: MutationStatus::RolledBack,
                    files_written: vec![],
                    syntax_errors,
                    reindex: ReindexSummary {
                        files: 0,
                        entities_updated: 0,
                        edges_updated: 0,
                        duration_ms: 0,
                    },
                    backup_path: None,
                };
            }
            backups.push((file_path.clone(), backup_path));
        }

        // ── Phase 3: Write (atomic temp + rename) ────────────────────────
        let mut files_written: Vec<String> = Vec::new();
        for (file_path, edits) in &by_file {
            let original = originals.get(file_path).cloned().unwrap_or_default();
            let new_content = match apply_edits_to_file(&original, &edits) {
                Ok(s) => s,
                Err(e) => {
                    rollback_all(&backups);
                    syntax_errors.push(SyntaxDiagnostic {
                        file: file_path.clone(),
                        line: 0,
                        column: 0,
                        message: format!("apply failed: {:?}", e),
                        offending_span: ByteSpan { start: 0, end: 0 },
                    });
                    return MutationResult {
                        status: MutationStatus::RolledBack,
                        files_written: vec![],
                        syntax_errors,
                        reindex: ReindexSummary {
                            files: 0,
                            entities_updated: 0,
                            edges_updated: 0,
                            duration_ms: 0,
                        },
                        backup_path: None,
                    };
                }
            };

            let tmp_path = format!("{}.coderadar-tmp", file_path);
            let write_ok = std::fs::File::create(disk_path_for(&tmp_path))
                .and_then(|mut f| f.write_all(new_content.as_bytes()))
                .and_then(|_| std::fs::rename(disk_path_for(&tmp_path), disk_path_for(file_path)))
                .is_ok();

            if write_ok {
                files_written.push(file_path.clone());
                // Suppress watcher events for this file for 5s — the mutation
                // engine wrote it, so the watcher shouldn't re-index it.
                self.write_guard.suppress(
                    std::path::PathBuf::from(file_path),
                    span_hash(new_content.as_bytes()),
                    5,
                );
            } else {
                let _ = std::fs::remove_file(disk_path_for(&tmp_path));
                rollback_all(&backups);
                syntax_errors.push(SyntaxDiagnostic {
                    file: file_path.clone(),
                    line: 0,
                    column: 0,
                    message: "atomic write failed".into(),
                    offending_span: ByteSpan { start: 0, end: 0 },
                });
                return MutationResult {
                    status: MutationStatus::RolledBack,
                    files_written: vec![],
                    syntax_errors,
                    reindex: ReindexSummary {
                        files: 0,
                        entities_updated: 0,
                        edges_updated: 0,
                        duration_ms: 0,
                    },
                    backup_path: None,
                };
            }
        }

        // ── Phase 4: Post-write parse verification ───────────────────────
        let mut tainted: Vec<SyntaxDiagnostic> = Vec::new();
        for file_path in &files_written {
            let original_bytes = originals
                .get(file_path)
                .map(|s| s.as_bytes())
                .unwrap_or(&[]);
            if let Some(diag) = verify_parse_introduced_error(file_path, original_bytes) {
                tainted.push(diag);
            }
        }

        if !tainted.is_empty() {
            // Tainted update → automatic rollback of every written file.
            rollback_all(&backups);
            syntax_errors.extend(tainted);
            return MutationResult {
                status: MutationStatus::RolledBack,
                files_written: vec![],
                syntax_errors,
                reindex: ReindexSummary {
                    files: 0,
                    entities_updated: 0,
                    edges_updated: 0,
                    duration_ms: 0,
                },
                backup_path: backups.first().map(|(_, bp)| bp.clone()),
            };
        }

        // ── Phase 5: Success — clean up backups ──────────────────────────
        for (_, backup_path) in &backups {
            let _ = std::fs::remove_file(disk_path_for(backup_path));
        }

        MutationResult {
            status: MutationStatus::Applied,
            files_written,
            syntax_errors: Vec::new(),
            reindex: ReindexSummary {
                files: by_file.len(),
                entities_updated: 0,
                edges_updated: 0,
                duration_ms: 0,
            },
            backup_path: None,
        }
    }
}

/// Restore every backup (rollback) and remove the backup files.
fn rollback_all(backups: &[(String, String)]) {
    for (file_path, backup_path) in backups {
        let _ = std::fs::copy(disk_path_for(backup_path), disk_path_for(file_path));
        let _ = std::fs::remove_file(disk_path_for(backup_path));
    }
}

/// The header a signature update rewrites, and the text to put in it.
///
/// `params_span` covers exactly `(a, b)`. A Python header can carry a return
/// annotation between that and the colon, so the span that a whole-signature
/// replacement owns runs from the opening paren to just before the header's
/// colon. Anything the caller did not include — a return type they dropped —
/// is dropped on purpose: they passed a complete signature.
/// Byte offset just past the `)` matching the `(` at `open` (F4 helper).
///
/// Bracket-aware (`(`, `[`, `{` nest jointly) and quote-aware, so defaults
/// like `x=(1, 2)`, annotations like `x: dict[str, int]` and string
/// defaults like `x=")"` don't end the scan early. Returns `None` when
/// the parens never balance.
fn match_paren_end(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                // Skip string literals (triple-quoted included).
                let q = bytes[i];
                let triple = i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q;
                i += if triple { 3 } else { 1 };
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if triple
                        && i + 2 < bytes.len()
                        && bytes[i] == q
                        && bytes[i + 1] == q
                        && bytes[i + 2] == q
                    {
                        i += 3;
                        break;
                    }
                    if !triple && bytes[i] == q {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte offset of the `(` in a `name<…>? (` occurrence on `line`.
///
/// Allows optional generic arguments between the name and the paren
/// (`fn foo<T>(x: T)`), with word boundaries on the name. Returns the
/// offset of the paren itself.
fn find_name_paren(line: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(pos) = line[from..].find(name) {
        let s = from + pos;
        let mut e = s + name.len();
        if s > 0 && is_ident_byte(bytes[s - 1]) {
            from = e;
            continue;
        }
        while e < line.len() && (bytes[e] == b' ' || bytes[e] == b'\t') {
            e += 1;
        }
        // Optional `<…>` generic arguments (`fn foo<T>(x: T)`).
        if bytes.get(e) == Some(&b'<') {
            let mut depth = 0i32;
            let mut j = e;
            let mut closed = None;
            while j < line.len() {
                match bytes[j] {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            closed = Some(j + 1);
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            let Some(after) = closed else {
                from = s + name.len();
                continue;
            };
            e = after;
            while e < line.len() && (bytes[e] == b' ' || bytes[e] == b'\t') {
                e += 1;
            }
        }
        if bytes.get(e) == Some(&b'(') {
            return Some(e);
        }
        from = s + name.len();
    }
    None
}

/// Whether a recorded params span is trustworthy against current disk (F4).
///
/// Demands: the span opens with `(`, the parens balance, the trailer looks
/// like a header end (`:`, `->`, `{`, `;`, newline/EOF, `where`-style word),
/// and the entity's own name precedes the paren on the same line (rules out
/// spans that drifted into unrelated code — the F4 mangler).
fn params_span_valid(source: &str, name: &str, span: ByteSpan) -> bool {
    let end = span.end.min(source.len());
    if span.start >= end {
        return false;
    }
    if source.as_bytes().get(span.start) != Some(&b'(') {
        return false;
    }
    let paren_end = match match_paren_end(source, span.start) {
        Some(e) => e,
        None => return false,
    };
    // Trailer sanity.
    let after: String = source[paren_end..].chars().take(32).collect();
    let t = after.trim_start();
    let trailer_ok = t.is_empty()
        || t.starts_with(':')
        || t.starts_with('-')
        || t.starts_with('{')
        || t.starts_with(';')
        || t.starts_with('\n')
        || t.starts_with('\r')
        || t.chars().next().is_some_and(|c| c.is_alphabetic());
    if !trailer_ok {
        return false;
    }
    // Same-line anchor: the def line names its own function. The prefix
    // ends right AT the paren, so check it ends with `name` (or
    // `name<…>` generics) on a word boundary — a span that drifted into
    // unrelated code (the F4 mangler) fails here.
    let line_start = source[..span.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let prefix = &source[line_start..span.start];
    if !prefix_ends_with_name_call(prefix, name) {
        return false;
    }
    true
}

/// Whether `prefix` (line text up to, but excluding, the paren) ends with
/// `name` or `name<…>` on a word boundary.
fn prefix_ends_with_name_call(prefix: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut core = prefix.trim_end();
    if core.ends_with('>') {
        // Strip one balanced `<…>` generic group from the right.
        let bytes = core.as_bytes();
        let mut depth = 0i32;
        let mut i = bytes.len();
        let mut ok = false;
        while i > 0 {
            i -= 1;
            match bytes[i] {
                b'>' => depth += 1,
                b'<' => {
                    depth -= 1;
                    if depth == 0 {
                        core = core[..i].trim_end();
                        ok = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !ok {
            return false;
        }
    }
    core.len() >= name.len() && core.ends_with(name) && {
        let s = core.len() - name.len();
        s == 0 || !is_ident_byte(core.as_bytes()[s - 1])
    }
}

fn signature_header(
    source: &str,
    params_span: ByteSpan,
    new_signature: &str,
) -> Result<(ByteSpan, String), MutationError> {
    let open = new_signature.find('(').ok_or_else(|| {
        MutationError::ParseFailed(format!(
            "New signature has no parameter list: {:?}",
            new_signature
        ))
    })?;
    let mut replacement = new_signature[open..].trim_end().to_string();
    if replacement.ends_with(':') {
        replacement.pop();
        replacement = replacement.trim_end().to_string();
    }

    let end = header_colon(source, params_span.end).unwrap_or(params_span.end);
    Ok((
        ByteSpan {
            start: params_span.start,
            end,
        },
        replacement,
    ))
}

/// Byte offset of the colon that ends a `def` header, searching from `from`.
///
/// Bracket-aware, so a `-> dict[str, int]:` does not end at the wrong place,
/// and it stops at the newline rather than running into the body.
fn header_colon(source: &str, from: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth: i32 = 0;
    for i in from..bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b':' if depth <= 0 => return Some(i),
            b'\n' => return None,
            _ => {}
        }
    }
    None
}

/// The identifier a supplied signature names, if it names one.
fn signature_name(new_signature: &str) -> Option<&str> {
    let open = new_signature.find('(')?;
    let head = new_signature[..open].trim_end();
    let name = head.rsplit(|c: char| c.is_whitespace()).next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[derive(Debug)]
pub enum MutationError {
    EntityNotFound(String),
    /// A span recorded at index time no longer holds the identifier it named —
    /// the index is stale relative to disk. Reindex and re-plan.
    StaleIndex {
        file: String,
        expected: String,
        span: ByteSpan,
    },
    ParseFailed(String),
    PolicyViolation {
        path: String,
        reason: String,
    },
    HashMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    TooManyFiles(usize),
    TooManyEdits(usize),
    SyntaxDiagnostic(Vec<SyntaxDiagnostic>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::MutationConfig;
    use crate::mutation::indent::IndentStyle;
    use crate::types::ByteSpan;
    use std::path::Path;
    use std::sync::Arc;

    /// `params_span` covers `(a, b)`, but the MCP tool asks the agent for a
    /// whole `def f(a, b) -> str:` line. Writing one into the other produced
    /// `def greetdef greet(name, punctuation)::` — a syntax error that
    /// `apply` caught and rolled back, so update_signature reliably did
    /// nothing at all.
    mod signature_header_tests {
        use super::*;

        fn span_of(source: &str, params: &str) -> ByteSpan {
            let start = source.find(params).expect("params not in source");
            ByteSpan {
                start,
                end: start + params.len(),
            }
        }

        #[test]
        fn the_replacement_is_the_parameter_list_not_the_whole_line() {
            let source = "def greet(name):\n    return name\n";
            let params = span_of(source, "(name)");
            let (span, replacement) =
                signature_header(source, params, "def greet(name, punctuation):").unwrap();

            assert_eq!(replacement, "(name, punctuation)");
            assert_eq!(&source[span.start..span.end], "(name)");
        }

        #[test]
        fn a_return_annotation_is_part_of_the_header() {
            // params_span stops at `)`, so the old span could not have
            // reached `-> int` and a new annotation would have landed after
            // the colon.
            let source = "def greet(name) -> str:\n    return name\n";
            let params = span_of(source, "(name)");
            let (span, replacement) =
                signature_header(source, params, "def greet(name) -> int:").unwrap();

            assert_eq!(replacement, "(name) -> int");
            assert_eq!(&source[span.start..span.end], "(name) -> str");
        }

        #[test]
        fn a_bracketed_return_type_does_not_end_the_header_early() {
            let source = "def load(p) -> dict[str, int]:\n    return {}\n";
            let params = span_of(source, "(p)");
            let (span, _) = signature_header(source, params, "def load(p, q):").unwrap();

            assert_eq!(&source[span.start..span.end], "(p) -> dict[str, int]");
        }

        #[test]
        fn a_bare_parameter_list_is_accepted_too() {
            let source = "def greet(name):\n    return name\n";
            let params = span_of(source, "(name)");
            let (_, replacement) = signature_header(source, params, "(a, b)").unwrap();
            assert_eq!(replacement, "(a, b)");
        }

        #[test]
        fn a_signature_without_parameters_is_refused() {
            let source = "def greet(name):\n    return name\n";
            let params = span_of(source, "(name)");
            assert!(signature_header(source, params, "def greet").is_err());
        }

        #[test]
        fn a_missing_colon_falls_back_to_the_parameter_span() {
            // Truncated or unparsable source must not make the span run off
            // into the body.
            let source = "def greet(name)\n";
            let params = span_of(source, "(name)");
            let (span, _) = signature_header(source, params, "def greet(a):").unwrap();
            assert_eq!(span, params);
        }

        #[test]
        fn the_name_is_read_back_out_of_a_supplied_signature() {
            assert_eq!(signature_name("def greet(a, b):"), Some("greet"));
            assert_eq!(signature_name("async def greet(a):"), Some("greet"));
            assert_eq!(signature_name("(a, b)"), None);
            assert_eq!(signature_name("no parens here"), None);
        }
    }

    fn engine() -> MutationEngine {
        MutationEngine::new(MutationConfig::default())
    }

    fn plan(edits: Vec<MutationEdit>) -> MutationPlan {
        MutationPlan {
            id: "t".into(),
            tool: "create_entity".into(),
            affected_files: edits.iter().map(|e| e.file.clone()).collect(),
            edits,
            diff_preview: String::new(),
            unverified_sites: vec![],
            warnings: vec![],
        }
    }

    // ── Policy gate (plan §0.5) ────────────────────────────────────────
    //
    // apply_mutation deserializes an arbitrary JSON plan from Python: any
    // file, any byte span, and an expected_hash that defaults to empty — which
    // apply() reads as "skip the stale check". These pin the gate that stops
    // that being a write primitive for anything on disk.

    /// A hashed edit on a normal file inside the root, so the other tests are
    /// known to be rejecting for the reason they name.
    fn hashed_plan(file: &str, span: ByteSpan, source: &str) -> MutationPlan {
        MutationPlan {
            id: "t".into(),
            tool: "rename_symbol".into(),
            affected_files: vec![file.to_string()],
            edits: vec![MutationEdit {
                file: file.to_string(),
                span,
                replacement: "x".into(),
                expected_hash: hash_span(source.as_bytes(), span),
            }],
            diff_preview: String::new(),
            unverified_sites: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn test_policy_allows_a_hashed_edit_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.py");
        std::fs::write(&path, "value = 1\n").unwrap();

        let mut eng = engine().with_project_root(dir.path());
        let plan = hashed_plan(
            &path.to_string_lossy(),
            ByteSpan { start: 0, end: 5 },
            "value = 1\n",
        );
        assert_eq!(eng.apply(&plan).status, MutationStatus::Applied);
    }

    #[test]
    fn test_policy_rejects_writes_outside_the_project_root() {
        let project = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let path = elsewhere.path().join("victim.py");
        std::fs::write(&path, "secret = 1\n").unwrap();

        let mut eng = engine().with_project_root(project.path());
        let plan = hashed_plan(
            &path.to_string_lossy(),
            ByteSpan { start: 0, end: 6 },
            "secret = 1\n",
        );

        let result = eng.apply(&plan);
        assert_eq!(result.status, MutationStatus::RejectedPolicy);
        assert!(result.syntax_errors[0]
            .message
            .contains("outside the project root"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret = 1\n");
    }

    #[test]
    fn test_policy_rejects_deny_listed_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let path = dir.path().join(".git").join("config");
        std::fs::write(&path, "[core]\n").unwrap();

        let mut eng = engine().with_project_root(dir.path());
        let plan = hashed_plan(
            &path.to_string_lossy(),
            ByteSpan { start: 0, end: 6 },
            "[core]\n",
        );

        let result = eng.apply(&plan);
        assert_eq!(result.status, MutationStatus::RejectedPolicy);
        assert!(result.syntax_errors[0].message.contains("deny-listed"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[core]\n");
    }

    #[test]
    fn leading_dir_patterns_are_root_anchored_f2() {
        // F2: allow=`src/` admitted `py_agent/src/x.py` via the
        // `contains("/src/")` fallback and mutated a production file.
        assert!(path_matches("src/x.py", "src/"));
        assert!(path_matches("src/a/b.py", "src/"));
        assert!(!path_matches("py_agent/src/x.py", "src/"));
        assert!(!path_matches(
            ".venv/Lib/site-packages/foo/src/x.py",
            "src/"
        ));
        assert!(!path_matches("srcfoo/x.py", "src/"));
        // Bare names match on a `/` boundary.
        assert!(path_matches("src/x.py", "src"));
        assert!(!path_matches("srcfoo/x.py", "src"));
    }

    #[test]
    fn interior_fragments_still_match_at_depth() {
        // `/migrations/` keeps the anywhere-match — that is its purpose.
        assert!(path_matches("migrations/001.py", "/migrations/"));
        assert!(path_matches("app/migrations/001.py", "/migrations/"));
        assert!(!path_matches("app/migration/001.py", "/migrations/"));
        // Extension globs unchanged.
        assert!(path_matches("a/b.lock", "/*.lock"));
        assert!(!path_matches("a/b.locked", "/*.lock"));
    }

    #[test]
    fn brace_splice_reconstructs_block_f3() {
        // F3: the old splice ate `{ … }` and every brace-language apply
        // rolled back. The reconstruction must keep braces + indents.
        let eng = engine();
        let src =
            "    pub fn total_cents(&self) -> i64 {\n        self.entries.iter().sum()\n    }\n";
        let span = ByteSpan {
            start: src.find('{').unwrap(),
            end: src.rfind('}').unwrap() + 1,
        };
        let out = eng
            .brace_splice_body("self.entries.iter().map(|e| e.cents).sum()", src, &span)
            .expect("brace span must splice");
        assert_eq!(
            out,
            "{\n        self.entries.iter().map(|e| e.cents).sum()\n    }"
        );
        // Multi-line input keeps relative indent under the body column.
        let out2 = eng
            .brace_splice_body("let a = 1;\nlet b = 2;", src, &span)
            .expect("brace span must splice");
        assert_eq!(out2, "{\n        let a = 1;\n        let b = 2;\n    }");
        // Non-brace spans fall back to the indent path.
        let py = "def f():\n    return 1\n";
        assert!(eng
            .brace_splice_body("return 2", py, &ByteSpan { start: 9, end: 21 })
            .is_none());
    }

    #[test]
    fn test_policy_rejects_edits_with_no_stale_guard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.py");
        std::fs::write(&path, "value = 1\n").unwrap();

        let mut eng = engine().with_project_root(dir.path());
        let mut plan = hashed_plan(
            &path.to_string_lossy(),
            ByteSpan { start: 0, end: 5 },
            "value = 1\n",
        );
        plan.edits[0].expected_hash = String::new();

        let result = eng.apply(&plan);
        assert_eq!(result.status, MutationStatus::RejectedPolicy);
        assert!(result.syntax_errors[0].message.contains("no expected_hash"));
    }

    #[test]
    fn test_policy_allows_create_entity_without_a_hash() {
        // create_entity inserts into empty space — there is nothing to hash.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.py");
        std::fs::write(&path, "").unwrap();
        let file = path.to_string_lossy().to_string();

        let mut eng = engine().with_project_root(dir.path());
        let created = plan(vec![MutationEdit {
            file,
            span: ByteSpan { start: 0, end: 0 },
            replacement: "x = 1\n".into(),
            expected_hash: String::new(),
        }]);
        assert_eq!(eng.apply(&created).status, MutationStatus::Applied);
    }

    #[test]
    fn test_policy_rejects_plans_over_the_edit_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.py");
        std::fs::write(&path, "value = 1\n").unwrap();

        let mut config = MutationConfig::default();
        config.max_edits_per_plan = 1;
        let mut eng = MutationEngine::new(config).with_project_root(dir.path());

        let mut over = hashed_plan(
            &path.to_string_lossy(),
            ByteSpan { start: 0, end: 5 },
            "value = 1\n",
        );
        over.edits.push(over.edits[0].clone());

        let result = eng.apply(&over);
        assert_eq!(result.status, MutationStatus::RejectedPolicy);
        assert!(result.syntax_errors[0]
            .message
            .contains("over the configured limit"));
    }

    #[test]
    fn test_policy_rejects_traversal_out_of_the_root() {
        let project = tempfile::tempdir().unwrap();
        let outside = project.path().parent().unwrap().join("escape.py");
        std::fs::write(&outside, "x = 1\n").unwrap();

        let escaping = project.path().join("..").join("escape.py");
        let mut eng = engine().with_project_root(project.path());
        let plan = hashed_plan(
            &escaping.to_string_lossy(),
            ByteSpan { start: 0, end: 1 },
            "x = 1\n",
        );

        assert_eq!(eng.apply(&plan).status, MutationStatus::RejectedPolicy);
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn test_policy_rejects_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.py");
        std::fs::write(&path, "value = 1\n").unwrap();

        let mut config = MutationConfig::default();
        config.enabled = false;
        let mut eng = MutationEngine::new(config).with_project_root(dir.path());
        let plan = hashed_plan(
            &path.to_string_lossy(),
            ByteSpan { start: 0, end: 5 },
            "value = 1\n",
        );

        assert_eq!(eng.apply(&plan).status, MutationStatus::RejectedPolicy);
    }

    #[test]
    fn test_allow_list_is_a_whitelist_when_populated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("ok.py"), "a = 1\n").unwrap();
        std::fs::write(dir.path().join("other.py"), "a = 1\n").unwrap();

        let mut config = MutationConfig::default();
        config.allow = vec!["src/".into()];

        let inside = dir.path().join("src").join("ok.py");
        let mut eng = MutationEngine::new(config.clone()).with_project_root(dir.path());
        let ok = hashed_plan(
            &inside.to_string_lossy(),
            ByteSpan { start: 0, end: 1 },
            "a = 1\n",
        );
        assert_eq!(eng.apply(&ok).status, MutationStatus::Applied);

        let outside = dir.path().join("other.py");
        let mut eng = MutationEngine::new(config).with_project_root(dir.path());
        let refused = hashed_plan(
            &outside.to_string_lossy(),
            ByteSpan { start: 0, end: 1 },
            "a = 1\n",
        );
        let result = eng.apply(&refused);
        assert_eq!(result.status, MutationStatus::RejectedPolicy);
        assert!(result.syntax_errors[0].message.contains("allow list"));
    }

    #[test]
    fn test_path_matches_pattern_shapes() {
        assert!(path_matches("src/a.py", "src/"));
        // F2: leading-dir patterns are root-anchored — `pkg/src/a.py`
        // must NOT match `src/` (it used to, and mutated prod files).
        assert!(!path_matches("pkg/src/a.py", "src/"));
        assert!(!path_matches("mysrc/a.py", "src/"));

        assert!(path_matches("app/migrations/0001.py", "/migrations/"));
        assert!(path_matches("migrations/0001.py", "/migrations/"));

        assert!(path_matches("uv.lock", "/*.lock"));
        assert!(path_matches("sub/poetry.lock", "/*.lock"));
        assert!(!path_matches("locked.py", "/*.lock"));
    }

    #[test]
    fn test_has_leading_docstring_variants() {
        assert!(super::has_leading_docstring(
            "\"\"\"doc.\"\"\"\n    return 1"
        ));
        assert!(super::has_leading_docstring("  '''doc'''\n    return 1"));
        assert!(!super::has_leading_docstring("    return 1"));
        assert!(!super::has_leading_docstring(
            "x = \"\"\"not a docstring\"\"\"\n"
        ));
        assert!(!super::has_leading_docstring(""));
    }

    #[test]
    fn test_apply_inserts_at_top_with_zero_span() {
        // span 0..0 is a legitimate "insert at top" — must NOT be skipped
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "import os\n").unwrap();
        let file = path.to_string_lossy().to_string();

        let mut eng = engine();
        let p = plan(vec![MutationEdit {
            file: file.clone(),
            span: ByteSpan { start: 0, end: 0 },
            replacement: "def f():\n    pass\n".into(),
            expected_hash: String::new(),
        }]);
        let result = eng.apply(&p);
        assert_eq!(
            result.status,
            MutationStatus::Applied,
            "{:#?}",
            result.syntax_errors
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "def f():\n    pass\nimport os\n"
        );
    }

    #[test]
    fn test_apply_inserts_at_end() {
        // end anchor: insert at file length, adding a newline separator
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "import os").unwrap(); // no trailing newline
        let file = path.to_string_lossy().to_string();
        let len = "import os".len();

        let mut eng = engine();
        let p = plan(vec![MutationEdit {
            file: file.clone(),
            span: ByteSpan {
                start: len,
                end: len,
            },
            replacement: "\ndef g():\n    pass\n".into(),
            expected_hash: String::new(),
        }]);
        let result = eng.apply(&p);
        assert_eq!(result.status, MutationStatus::Applied);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "import os\ndef g():\n    pass\n"
        );
    }

    #[test]
    fn test_apply_body_replacement_keeps_indentation_and_parses() {
        // Regression for CODERADAR_BUGS_QUIRKS.md #1: replacing a Python
        // function body with naturally-indented code must not shift any
        // line's indentation — the written file has to parse.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        let src = "def f():\n    \"\"\"old doc.\"\"\"\n    return 1\n";
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        // Span covers the body from the first body token through the end:
        // the docstring line minus its indent, plus the indented return line.
        let body_start = src.find("\"").unwrap();
        let span = ByteSpan {
            start: body_start,
            end: src.len(),
        };
        let replacement =
            "\"\"\"Merge HTML metadata.\"\"\"\n    merged = {**a, **b}\n    return merged\n";

        let mut eng = engine();
        let p = plan(vec![MutationEdit {
            file: file.clone(),
            span,
            replacement: normalize_indent(replacement, "", &IndentStyle::spaces(4), &[]),
            expected_hash: String::new(),
        }]);
        let result = eng.apply(&p);
        assert_eq!(
            result.status,
            MutationStatus::Applied,
            "{:#?}",
            result.syntax_errors
        );

        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            written,
            "def f():\n    \"\"\"Merge HTML metadata.\"\"\"\n    merged = {**a, **b}\n    return merged\n",
            "every line must keep its indentation"
        );
        assert_eq!(
            parse_has_error(
                crate::types::Language::from_extension("py"),
                written.as_bytes()
            ),
            Some(false),
            "written file must parse — G0-A gate"
        );
    }

    #[test]
    fn test_body_replacement_pre_indented_no_double_indent() {
        // CODERADAR_BUGS_QUIRKS #1 follow-up (v0.7.19): the body span starts
        // AFTER the file's own indent, so a source-copied (pre-indented)
        // replacement must not be spliced on top of it — that doubled the
        // indent and the replacement's trailing newline added a blank line.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        let src = "def f(a):\n    return a\n";
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        let mut g = crate::smells::engine::tests::empty_graph();
        let mut f = crate::graph::deadcode::tests::func("m.py::f", "f", "m.py::module");
        let bstart = src.find("return a").unwrap();
        f.body_span = ByteSpan {
            start: bstart,
            end: bstart + 8,
        }; // exactly "return a"
        g.functions.insert("m.py::f".into(), Arc::new(f));
        g.modules.insert(
            "m.py::module".into(),
            Arc::new(crate::types::Module {
                id: "m.py::module".into(),
                name: "m".into(),
                path: path.clone(),
                language: crate::types::Language::Python,
                package: None,
                exports: vec![],
                star_exports: None,
                classes: vec![],
                functions: vec![],
                imports: vec![],
                constants: vec![],
                type_aliases: vec![],
                parse_quality: crate::types::ParseQuality::Clean,
                content_hash: 0,
                embedding: Default::default(),
                file_version: 0,
            }),
        );

        let mut eng = MutationEngine::new(MutationConfig::default());
        for body in ["    return a + 1\n", "return a + 1\n"] {
            std::fs::write(&path, src).unwrap();
            let plan = eng
                .plan_body_replacement("m.py::f", body, None, true, &g)
                .unwrap();
            assert!(
                plan.diff_preview.contains("+    return a + 1\n"),
                "preview must show single indent for body {:?}: {}",
                body,
                plan.diff_preview
            );
            assert!(
                !plan.diff_preview.contains("+\n"),
                "no stray blank line for body {:?}: {}",
                body,
                plan.diff_preview
            );
            let p2 = eng.apply(&plan);
            assert_eq!(p2.status, MutationStatus::Applied);
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "def f(a):\n    return a + 1\n",
                "body {:?} written incorrectly",
                body
            );
        }
    }

    #[test]
    fn test_body_replacement_multiline_continuations_get_body_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        let src = "def f(a):\n    if a:\n        return 1\n    return 2\n";
        std::fs::write(&path, src).unwrap();

        // body span covers from 'if' through final '2'
        let start = src.find("if a:").unwrap();
        let end = src.find("return 2").unwrap() + 8;

        let mut g = crate::smells::engine::tests::empty_graph();
        let mut f = crate::graph::deadcode::tests::func("m.py::f", "f", "m.py::module");
        f.body_span = ByteSpan { start, end };
        g.functions.insert("m.py::f".into(), Arc::new(f));
        g.modules.insert(
            "m.py::module".into(),
            Arc::new(crate::types::Module {
                id: "m.py::module".into(),
                name: "m".into(),
                path: path.clone(),
                language: crate::types::Language::Python,
                package: None,
                exports: vec![],
                star_exports: None,
                classes: vec![],
                functions: vec![],
                imports: vec![],
                constants: vec![],
                type_aliases: vec![],
                parse_quality: crate::types::ParseQuality::Clean,
                content_hash: 0,
                embedding: Default::default(),
                file_version: 0,
            }),
        );

        let mut eng = MutationEngine::new(MutationConfig::default());
        let plan = eng
            .plan_body_replacement(
                "m.py::f",
                "    total = a or 0\n    if total:\n        return total\n    return -1\n",
                None,
                true,
                &g,
            )
            .unwrap();
        let p2 = eng.apply(&plan);
        assert_eq!(p2.status, MutationStatus::Applied);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "def f(a):\n    total = a or 0\n    if total:\n        return total\n    return -1\n",
            "continuation lines land at the body column; first line inherits the prefix"
        );
    }

    #[test]
    fn test_apply_rejects_stale_edit() {
        let src = b"def foo():\n    return 1\n";
        let start = src.windows(8).position(|w| w == b"return 1").unwrap();
        let span = ByteSpan {
            start,
            end: start + 8,
        };
        let hash = hash_span(src, span);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        // Simulate the file changing after the plan was built
        std::fs::write(&file, b"def foo():\n    return 999\n").unwrap();

        let mut eng = engine();
        let p = plan(vec![MutationEdit {
            file: file.clone(),
            span,
            replacement: "return 2".into(),
            expected_hash: hash,
        }]);
        let result = eng.apply(&p);
        assert_eq!(result.status, MutationStatus::RejectedStale);
        assert!(!result.syntax_errors.is_empty());
        // File must be untouched
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "def foo():\n    return 999\n"
        );
    }

    #[test]
    fn test_apply_rolls_back_tainted_update() {
        let src = b"def foo():\n    return 1\n";
        let span = ByteSpan {
            start: 0,
            end: src.len(),
        };
        let hash = hash_span(src, span);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        let mut eng = engine();
        // Replacement is syntactically broken → post-verify must roll back
        let p = plan(vec![MutationEdit {
            file: file.clone(),
            span,
            replacement: "def foo(:\n  broken".into(),
            expected_hash: hash,
        }]);
        let result = eng.apply(&p);
        assert_eq!(
            result.status,
            MutationStatus::RolledBack,
            "{:#?}",
            result.syntax_errors
        );
        // File restored to original
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "def foo():\n    return 1\n"
        );
        // No leftover backup file
        assert!(!Path::new(&format!("{}.coderadar-bak", file)).exists());
    }

    #[test]
    fn test_apply_succeeds_when_hash_matches() {
        let src = b"def foo():\n    return 1\n";
        let start = src.windows(8).position(|w| w == b"return 1").unwrap();
        let span = ByteSpan {
            start,
            end: start + 8,
        };
        let hash = hash_span(src, span);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        let mut eng = engine();
        let p = plan(vec![MutationEdit {
            file: file.clone(),
            span,
            replacement: "return 2".into(),
            expected_hash: hash,
        }]);
        let result = eng.apply(&p);
        assert_eq!(
            result.status,
            MutationStatus::Applied,
            "{:#?}",
            result.syntax_errors
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "def foo():\n    return 2\n"
        );
    }

    // ── Textual call-site backstop (v0.8 P2-5) ─────────────────────────

    mod textual_backstop_tests {
        use super::*;
        use crate::graph::{CodeGraph, GraphConfig};
        use crate::types::Language;

        #[test]
        fn stale_graph_signature_update_rebases_instead_of_mangling_f4() {
            // F4: with disk 3 lines ahead of the graph, the old code
            // spliced the new signature into another method's body (and
            // could eat the next def line). Two consecutive methods, like
            // the demo_billing.py fixture.
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("c.py");
            let src = concat!(
                "class Invoice:\n",
                "    def first(self, x):\n",
                "        return x * 2\n\n",
                "    def second(self, y):\n",
                "        return y + 1\n",
            );
            std::fs::write(&file, src).unwrap();
            let file_str = file.to_string_lossy().to_string();
            let graph = CodeGraph::new(GraphConfig::default());
            graph.index_file(src, &file_str, &Language::Python).unwrap();
            let projection = (*graph.snapshot()).clone();
            // Disk moves under the graph: 3 comment lines on top.
            let shifted = format!("# a\n# b\n# c\n{}", src);
            std::fs::write(&file, &shifted).unwrap();

            let entity_id = format!("{}::Invoice.second", file_str);
            let plan = engine()
                .plan_signature_update(
                    &entity_id,
                    "def second(self, y, z=0):",
                    &HashMap::new(),
                    false,
                    true,
                    &projection,
                )
                .expect("stale span must rebase, not refuse");
            // Exactly one definition edit, on the real def line…
            assert_eq!(plan.edits.len(), 1);
            let edit = &plan.edits[0];
            assert_eq!(&shifted[edit.span.start..edit.span.end], "(self, y)");
            assert_eq!(edit.replacement, "(self, y, z=0)");
            // …and the plan says so via a stale-span warning.
            assert!(
                plan.warnings.iter().any(|w| w.contains("stale")),
                "{:?}",
                plan.warnings
            );
            // The spliced file keeps both methods intact.
            let applied =
                crate::mutation::edit::apply_edits_to_file(&shifted, &plan.edits).unwrap();
            assert!(applied.contains("def first(self, x):"));
            assert!(applied.contains("def second(self, y, z=0):"));
            assert!(applied.contains("return x * 2"));
            assert!(applied.contains("return y + 1"));
        }

        #[test]
        fn unresolvable_stale_span_refuses_with_stale_index() {
            // Span points at garbage with no `name (` nearby: refuse.
            let eng = engine();
            let source = "x = 1\nfoo(2)\n";
            let err = eng
                .rebased_params_span(
                    source,
                    "target_fn",
                    1,
                    "a.py",
                    ByteSpan { start: 9, end: 12 },
                    &mut Vec::new(),
                )
                .expect_err("must refuse");
            assert!(matches!(err, MutationError::StaleIndex { .. }));
        }

        // line 6: resolved by the graph (call inside a function);
        // line 8: module-level — no enclosing function, so the cascade
        // records no call site; only the textual scan can see it.
        const SOURCE: &str = concat!(
            "def target_fn():\n    return 1\n\n\n",
            "def direct_caller():\n    return target_fn()\n\n",
            "target_fn()\n",
        );

        fn indexed_projection(dir: &std::path::Path) -> (ProjectedGraph, String) {
            let file = dir.join("a.py");
            std::fs::write(&file, SOURCE).unwrap();
            let file_str = file.to_string_lossy().to_string();
            let graph = CodeGraph::new(GraphConfig::default());
            graph
                .index_file(SOURCE, &file_str, &Language::Python)
                .unwrap();
            let mut projection = (*graph.snapshot()).clone();
            graph.resolve_all_calls(&mut projection);
            (projection, file_str)
        }

        fn textual_sites(plan: &MutationPlan) -> Vec<&UnverifiedSite> {
            plan.unverified_sites
                .iter()
                .filter(|s| s.reason.starts_with("Textual occurrence"))
                .collect()
        }

        #[test]
        fn rename_reports_only_the_unresolved_textual_call() {
            let dir = tempfile::tempdir().unwrap();
            let (projection, file) = indexed_projection(dir.path());
            let entity_id = format!("{}::target_fn", file);

            let plan = engine()
                .plan_rename(&entity_id, "new_target", false, true, &projection)
                .unwrap();

            // The resolved call site (line 6) became an edit, not a report.
            assert!(plan
                .edits
                .iter()
                .any(|e| e.file == file && e.replacement == "new_target"));
            // The module-level call (line 8) is the one textual report.
            let sites = textual_sites(&plan);
            assert_eq!(sites.len(), 1, "{:?}", plan.unverified_sites);
            assert_eq!(sites[0].file, file);
            assert_eq!(sites[0].line, 8);
            assert!(sites[0].snippet.contains("target_fn()"));
            // Definition (line 1) and the resolved site (line 6) stay quiet.
            assert!(!sites.iter().any(|s| s.line == 1 || s.line == 6));
        }

        /// Index a re-export chain on disk: helpers defines `combine`,
        /// app/__init__ re-exports it, main imports it from app and calls it.
        fn indexed_reexport_chain(
            dir: &std::path::Path,
        ) -> (ProjectedGraph, String, String, String) {
            let app = dir.join("app");
            std::fs::create_dir_all(&app).unwrap();
            let helpers = app.join("helpers.py");
            let init = app.join("__init__.py");
            let main = dir.join("main.py");
            std::fs::write(&helpers, "def combine(items):\n    return items\n").unwrap();
            std::fs::write(&init, "from .helpers import combine\n").unwrap();
            std::fs::write(
                &main,
                "from app import combine\ndef run(items):\n    return combine(items)\n",
            )
            .unwrap();
            let graph = CodeGraph::new(GraphConfig::default());
            for p in [&helpers, &init, &main] {
                let src = std::fs::read_to_string(p).unwrap();
                graph
                    .index_file(&src, &p.to_string_lossy(), &Language::Python)
                    .unwrap();
            }
            let mut projection = (*graph.snapshot()).clone();
            graph.resolve_imports(&mut projection);
            graph.resolve_all_calls(&mut projection);
            (
                projection,
                helpers.to_string_lossy().to_string(),
                init.to_string_lossy().to_string(),
                main.to_string_lossy().to_string(),
            )
        }

        fn fn_id_named(projection: &ProjectedGraph, name: &str, file_frag: &str) -> String {
            projection
                .functions
                .keys()
                .find(|id| id.contains(file_frag) && id.ends_with(&format!("::{name}")))
                .cloned()
                .expect("fixture function missing")
        }

        fn applied_for(plan: &MutationPlan, file: &str, src: &str) -> String {
            let edits: Vec<MutationEdit> = plan
                .edits
                .iter()
                .filter(|e| e.file == file)
                .cloned()
                .collect();
            crate::mutation::edit::apply_edits_to_file(src, &edits).unwrap()
        }

        #[test]
        fn rename_rewrites_import_bindings_through_reexport_chain() {
            // R2-17: rename stopped at def + call sites, stranding
            // `from app import combine` on the old name (next resolve:
            // external::). The chain must heal link by link in one plan.
            let dir = tempfile::tempdir().unwrap();
            let (projection, helpers, init, main) = indexed_reexport_chain(dir.path());
            let comb = fn_id_named(&projection, "combine", "helpers");
            let plan = engine()
                .plan_rename(&comb, "combine_r2", false, true, &projection)
                .expect("rename plans");
            let src_h = std::fs::read_to_string(&helpers).unwrap();
            let src_i = std::fs::read_to_string(&init).unwrap();
            let src_m = std::fs::read_to_string(&main).unwrap();
            assert!(applied_for(&plan, &helpers, &src_h).contains("def combine_r2(items):"));
            assert!(
                applied_for(&plan, &init, &src_i).contains("from .helpers import combine_r2"),
                "init re-export must track the rename"
            );
            let applied_main = applied_for(&plan, &main, &src_m);
            assert!(
                applied_main.contains("from app import combine_r2"),
                "importer binding must track the rename: {applied_main}"
            );
            assert!(applied_main.contains("return combine_r2(items)"));
            assert!(
                !plan
                    .unverified_sites
                    .iter()
                    .any(|s| s.snippet.contains("binding")),
                "bindings verified, not reported: {:?}",
                plan.unverified_sites
            );
            // Round-trip: the rewritten tree re-indexes with run resolved
            // to the renamed definition — no external::, no dangling import.
            std::fs::write(&helpers, applied_for(&plan, &helpers, &src_h)).unwrap();
            std::fs::write(&init, applied_for(&plan, &init, &src_i)).unwrap();
            std::fs::write(&main, applied_main.clone()).unwrap();
            let graph2 = CodeGraph::new(GraphConfig::default());
            for (p, s) in [
                (&helpers, std::fs::read_to_string(&helpers).unwrap()),
                (&init, std::fs::read_to_string(&init).unwrap()),
                (&main, applied_main),
            ] {
                graph2.index_file(&s, p, &Language::Python).unwrap();
            }
            let mut proj2 = (*graph2.snapshot()).clone();
            graph2.resolve_imports(&mut proj2);
            graph2.resolve_all_calls(&mut proj2);
            let run2 = fn_id_named(&proj2, "run", "main");
            let new_comb = format!("{}::combine_r2", helpers);
            let callees = proj2
                .callees_by_caller
                .get(&run2)
                .cloned()
                .unwrap_or_default();
            assert!(
                callees.contains(&new_comb),
                "round-trip callees: {callees:?}"
            );
            assert!(
                !callees.iter().any(|c| c.starts_with("external::")),
                "no external fallback after chain rename: {callees:?}"
            );
        }

        #[test]
        fn rename_rewrites_bound_name_but_keeps_alias() {
            // `from h import combine as c`: the bound `combine` must track
            // the rename, the local alias `c` stays valid and untouched.
            let dir = tempfile::tempdir().unwrap();
            let h = dir.path().join("h.py");
            let u = dir.path().join("u.py");
            std::fs::write(&h, "def combine(items):\n    return items\n").unwrap();
            std::fs::write(&u, "from h import combine as c\n").unwrap();
            let graph = CodeGraph::new(GraphConfig::default());
            for p in [&h, &u] {
                let src = std::fs::read_to_string(p).unwrap();
                graph
                    .index_file(&src, &p.to_string_lossy(), &Language::Python)
                    .unwrap();
            }
            let mut projection = (*graph.snapshot()).clone();
            graph.resolve_imports(&mut projection);
            graph.resolve_all_calls(&mut projection);
            let comb = fn_id_named(&projection, "combine", "h.py");
            let plan = engine()
                .plan_rename(&comb, "combine_r2", false, true, &projection)
                .expect("rename plans");
            let u_str = u.to_string_lossy().to_string();
            let applied = applied_for(&plan, &u_str, &std::fs::read_to_string(&u).unwrap());
            assert!(
                applied.contains("from h import combine_r2 as c"),
                "bound name tracks, alias kept: {applied}"
            );
        }

        #[test]
        fn rename_skips_same_named_import_from_another_module() {
            // Two modules each define `combine`; only the renamed one's
            // importers are rewritten (find_symbol_in_module shadow rule).
            let dir = tempfile::tempdir().unwrap();
            let h = dir.path().join("h.py");
            let other = dir.path().join("other.py");
            let u = dir.path().join("u.py");
            std::fs::write(&h, "def combine(items):\n    return items\n").unwrap();
            std::fs::write(&other, "def combine(items):\n    return None\n").unwrap();
            std::fs::write(&u, "from other import combine\n").unwrap();
            let graph = CodeGraph::new(GraphConfig::default());
            for p in [&h, &other, &u] {
                let src = std::fs::read_to_string(p).unwrap();
                graph
                    .index_file(&src, &p.to_string_lossy(), &Language::Python)
                    .unwrap();
            }
            let mut projection = (*graph.snapshot()).clone();
            graph.resolve_imports(&mut projection);
            graph.resolve_all_calls(&mut projection);
            let comb = fn_id_named(&projection, "combine", "h.py");
            let plan = engine()
                .plan_rename(&comb, "combine_r2", false, true, &projection)
                .expect("rename plans");
            let u_str = u.to_string_lossy().to_string();
            assert!(
                plan.edits.iter().all(|e| e.file != u_str),
                "foreign binding untouched: {:?}",
                plan.edits
            );
            assert_eq!(
                std::fs::read_to_string(&u).unwrap(),
                "from other import combine\n"
            );
        }

        #[test]
        fn class_rename_rewrites_import_bindings() {
            // R2-17 hook on the class path: `from pkg import Widget`
            // tracks `Widget -> Gadget` the same way functions do.
            let dir = tempfile::tempdir().unwrap();
            let pkg = dir.path().join("pkg");
            std::fs::create_dir_all(&pkg).unwrap();
            let m = pkg.join("m.py");
            let init = pkg.join("__init__.py");
            let main = dir.path().join("main2.py");
            std::fs::write(&m, "class Widget:\n    pass\n").unwrap();
            std::fs::write(&init, "from .m import Widget\n").unwrap();
            std::fs::write(&main, "from pkg import Widget\n").unwrap();
            let graph = CodeGraph::new(GraphConfig::default());
            for p in [&m, &init, &main] {
                let src = std::fs::read_to_string(p).unwrap();
                graph
                    .index_file(&src, &p.to_string_lossy(), &Language::Python)
                    .unwrap();
            }
            let mut projection = (*graph.snapshot()).clone();
            graph.resolve_imports(&mut projection);
            graph.resolve_all_calls(&mut projection);
            let widget = projection
                .classes
                .keys()
                .find(|id| id.ends_with("::Widget"))
                .cloned()
                .expect("fixture class missing");
            let plan = engine()
                .plan_rename(&widget, "Gadget", false, true, &projection)
                .expect("rename plans");
            let init_str = init.to_string_lossy().to_string();
            let main_str = main.to_string_lossy().to_string();
            assert!(
                applied_for(&plan, &init_str, &std::fs::read_to_string(&init).unwrap())
                    .contains("from .m import Gadget"),
                "{:?}",
                plan.edits
            );
            assert!(
                applied_for(&plan, &main_str, &std::fs::read_to_string(&main).unwrap())
                    .contains("from pkg import Gadget"),
                "{:?}",
                plan.edits
            );
        }

        #[test]
        fn signature_update_reports_only_the_unresolved_textual_call() {
            let dir = tempfile::tempdir().unwrap();
            let (projection, file) = indexed_projection(dir.path());
            let entity_id = format!("{}::target_fn", file);

            let plan = engine()
                .plan_signature_update(
                    &entity_id,
                    "def target_fn(p: int):",
                    &HashMap::new(),
                    false,
                    true,
                    &projection,
                )
                .unwrap();

            let sites = textual_sites(&plan);
            assert_eq!(sites.len(), 1, "{:?}", plan.unverified_sites);
            assert_eq!(sites[0].file, file);
            assert_eq!(sites[0].line, 8);
        }

        #[test]
        fn word_boundary_is_respected_and_strings_are_reported() {
            // The scanner's contract on a throwaway file: word-boundary
            // `name(` only — and it is deliberately blind to strings and
            // syntax, so those are reported too (the reason string says so).
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("b.py");
            let src = "mytarget_fn()\ntarget_fn()\n_ = target_fn(1)\n\"target_fn()\"\n";
            std::fs::write(&file, src).unwrap();
            let file_str = file.to_string_lossy().to_string();

            let graph = CodeGraph::new(GraphConfig::default());
            graph.index_file(src, &file_str, &Language::Python).unwrap();
            let projection = (*graph.snapshot()).clone();

            let sites = textual_call_sites(&projection, "target_fn");
            let lines: Vec<u32> = sites.iter().map(|(_, l, _)| *l).collect();
            assert_eq!(lines, vec![2, 3, 4], "{:?}", sites);
        }

        // The spec's fixture, literally: a direct call (a graph site) plus
        // a call inside a macro token body. tree-sitter never visits the
        // macro body, so only the textual scan can see line 10.
        const SOURCE_RS: &str = concat!(
            "fn target_fn() -> i32 {\n    1\n}\n\n",
            "fn direct_caller() -> i32 {\n    target_fn()\n}\n\n",
            "macro_rules! m {\n    () => { target_fn() };\n}\n",
        );

        fn indexed_rs_projection(dir: &std::path::Path) -> (ProjectedGraph, String) {
            let file = dir.join("mac.rs");
            std::fs::write(&file, SOURCE_RS).unwrap();
            let file_str = file.to_string_lossy().to_string();
            let graph = CodeGraph::new(GraphConfig::default());
            graph
                .index_file(SOURCE_RS, &file_str, &Language::Rust)
                .unwrap();
            let mut projection = (*graph.snapshot()).clone();
            graph.resolve_all_calls(&mut projection);
            (projection, file_str)
        }

        #[test]
        fn rename_reports_the_call_inside_a_macro_body() {
            let dir = tempfile::tempdir().unwrap();
            let (projection, file) = indexed_rs_projection(dir.path());
            let entity_id = format!("{}::target_fn", file);

            let plan = engine()
                .plan_rename(&entity_id, "new_target", false, true, &projection)
                .unwrap();

            // The direct call (line 6) is a graph site: rewritten, not reported.
            assert!(plan
                .edits
                .iter()
                .any(|e| e.file == file && e.replacement == "new_target"));
            // The macro-body call (line 10) is textually visible but
            // structurally unresolvable: the one textual report.
            let sites = textual_sites(&plan);
            assert_eq!(sites.len(), 1, "{:?}", plan.unverified_sites);
            assert_eq!(sites[0].file, file);
            assert_eq!(sites[0].line, 10);
            assert!(sites[0].snippet.contains("target_fn()"));
            // Definition (line 1) and the resolved site (line 6) stay quiet.
            assert!(!sites.iter().any(|s| s.line == 1 || s.line == 6));
        }
    }
}
