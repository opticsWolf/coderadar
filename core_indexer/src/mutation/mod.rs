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
    module_id
        .trim_end_matches("::module")
        .to_string()
}

/// Confirm that `span` in `source` still holds the identifier the index recorded.
///
/// Spans are captured at index time. The stale-write hash carried on every edit
/// is computed from the same read the span was resolved against, so it proves
/// the file did not change between *plan* and *apply* — it says nothing about
/// whether the graph still matches disk. This is the check that does.
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
fn verify_parse_introduced_error(
    file_path: &str, original: &[u8],
) -> Option<SyntaxDiagnostic> {
    let lang = crate::types::Language::from_extension(
        std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("py")
    );
    let before = parse_has_error(lang, original);
    let after_bytes = std::fs::read(file_path).ok()?;
    let after = parse_has_error(lang, &after_bytes);

    match (before, after) {
        (Some(_), Some(true)) if before == Some(false) => Some(SyntaxDiagnostic {
            file: file_path.to_string(),
            line: 0, column: 0,
            message: "post-write parse check failed — mutation introduced a syntax error".into(),
            offending_span: ByteSpan { start: 0, end: 0 },
        }),
        _ => None,
    }
}

/// Shared process-wide WriteGuard — suppresses watcher events for files the
/// mutation engine wrote. The watcher consults the same instance so mutation
/// writes don't trigger double-indexing.
pub static WRITE_GUARD: std::sync::OnceLock<std::sync::Arc<WriteGuard>> = std::sync::OnceLock::new();

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
    let fragment = pattern.strip_prefix('/').unwrap_or(pattern);
    if fragment.is_empty() {
        return false;
    }
    rel.starts_with(fragment) || rel.contains(&format!("/{}", fragment))
}

/// Resolve `path` to an absolute, symlink-free form.
///
/// The file may not exist yet (`create_entity`), so fall back to canonicalizing
/// the parent directory and re-attaching the file name — enough to defeat
/// `..` traversal, which is the point.
fn canonicalize_target(path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => match std::fs::canonicalize(parent) {
            Ok(c) => c.join(name),
            Err(_) => p.to_path_buf(),
        },
        _ => p.to_path_buf(),
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

            let target = canonicalize_target(&edit.file);

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
        let file_source = std::fs::read_to_string(&file_path).unwrap_or_default();
        let indent = detect_indent_style(&file_source);

        // Stale-write guard: hash the current body span content so apply() can
        // reject the edit if the file changed between planning and applying.
        let computed_hash = hash_span(file_source.as_bytes(), body_span);
        let edit_expected_hash = expected_hash.unwrap_or(computed_hash);

        // Body spans start at the first body token (or inline `{`), so the
        // leading indentation is NOT part of the span — use an empty target.
        // normalize_indent preserves the replacement's relative indentation.
        let normalized_body = normalize_indent(new_body, "", &indent, &[]);

        let plan_id = ulid::Ulid::new().to_string();

        Ok(MutationPlan {
            id: plan_id,
            tool: "replace_entity_body".to_string(),
            edits: vec![MutationEdit {
                file: module_file_path(projection, &fn_entity.parent_module),
                span: body_span,
                replacement: normalized_body,
                expected_hash: edit_expected_hash,
            }],
            affected_files: vec![module_file_path(projection, &fn_entity.parent_module)],
            diff_preview: if dry_run {
                format!("replace {} bytes at {}..{} in {}",
                    body_span.end - body_span.start,
                    body_span.start, body_span.end,
                    fn_entity.parent_module)
            } else {
                String::new()
            },
            unverified_sites: Vec::new(),
            warnings: if fn_entity.parse_quality != ParseQuality::Clean {
                vec!["Entity parse quality is not Clean — body_span may be approximate".into()]
            } else {
                Vec::new()
            },
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
        let old_params = &fn_entity.parameters;

        // Stale-write guard: hash the current params span content.
        let def_file = module_file_path(projection, &fn_entity.parent_module);
        let def_source = std::fs::read_to_string(&def_file).unwrap_or_default();
        let def_expected_hash = hash_span(def_source.as_bytes(), params_span);

        // 1. Diff old vs new parameters
        let new_param_names: Vec<&str> = new_signature
            .split(',')
            .map(|s| s.trim().split(':').next().unwrap_or("").trim())
            .filter(|s| !s.is_empty() && *s != "self" && *s != "cls")
            .collect();

        let old_names: Vec<&str> = old_params
            .iter()
            .map(|p| p.name.as_str())
            .collect();

        let mut edits = Vec::new();
        let mut warnings = Vec::new();
        let mut unverified = Vec::new();

        // 2. Definition edit: replace params_span
        edits.push(MutationEdit {
            file: def_file,
            span: params_span,
            replacement: new_signature.to_string(),
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
                        ResolvedCall::Function(id) | ResolvedCall::Method { method: id, .. } | ResolvedCall::Constructor(id) => {
                            id == entity_id
                        }
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
                            reason: "Call-site arg span unavailable; apply arg change manually".into(),
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

        let plan_id = ulid::Ulid::new().to_string();

        Ok(MutationPlan {
            id: plan_id,
            tool: "update_signature".to_string(),
            edits,
            affected_files: {
                let mut files: Vec<String> = callers.iter()
                    .filter_map(|id| projection.functions.get(id))
                    .map(|f| module_file_path(projection, &f.parent_module))
                    .collect();
                files.push(module_file_path(projection, &fn_entity.parent_module));
                files.sort();
                files.dedup();
                files
            },
            diff_preview: if dry_run {
                format!("{} param(s) changed, {} call site(s) affected",
                    old_names.len().abs_diff(new_param_names.len()),
                    callers.len())
            } else {
                String::new()
            },
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
            let caller_source = std::fs::read(&caller_file).unwrap_or_default();

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
        let def_source = std::fs::read(&def_file).unwrap_or_default();
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
        let caller_count = self.collect_call_site_edits(
            std::slice::from_ref(&entity_id.to_string()),
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

        Ok(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "rename_symbol".to_string(),
            edits,
            affected_files: affected,
            diff_preview: if dry_run {
                format!("rename \"{}\" → \"{}\" in {} file(s)",
                    fn_entity.name, new_name, caller_count + 1)
            } else {
                String::new()
            },
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
        let def_source = std::fs::read(&def_file).unwrap_or_default();
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
            let sub_source = std::fs::read(&sub_file).unwrap_or_default();

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

        Ok(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "rename_symbol".to_string(),
            edits,
            affected_files: affected,
            diff_preview: if dry_run {
                format!(
                    "rename class \"{}\" → \"{}\": {} subclass(es), {} construction site(s)",
                    cls.name, new_name, subclasses.len(), caller_count
                )
            } else {
                String::new()
            },
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
        let file_bytes = std::fs::read(target_file).unwrap_or_default();
        let file_len = file_bytes.len();
        let code_trimmed = code.trim_matches(['\n', '\r']);

        let (insert_span, replacement) = if anchor == "top" || anchor.is_empty() {
            // Insert at file top (after a UTF-8 BOM if present)
            let start = if file_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
            (ByteSpan { start, end: start }, format!("{}\n", code_trimmed))
        } else if anchor == "end" {
            // Insert at file end, blank-line separated from existing content
            let repl = if file_len == 0 {
                format!("{}\n", code_trimmed)
            } else {
                format!("\n{}\n", code_trimmed)
            };
            (ByteSpan { start: file_len, end: file_len }, repl)
        } else {
            // Anchor after a specific entity
            let span = if let Some(fn_ent) = projection.functions.get(anchor) {
                ByteSpan { start: fn_ent.span.end, end: fn_ent.span.end }
            } else if let Some(cls_ent) = projection.classes.get(anchor) {
                ByteSpan { start: cls_ent.span.end, end: cls_ent.span.end }
            } else {
                return Err(MutationError::EntityNotFound(anchor.to_string()));
            };
            (span, format!("\n{}\n", code_trimmed))
        };

        let plan_id = ulid::Ulid::new().to_string();

        Ok(MutationPlan {
            id: plan_id,
            tool: "create_entity".to_string(),
            edits: vec![MutationEdit {
                file: target_file.to_string(),
                span: insert_span,
                replacement,
                expected_hash: String::new(),
            }],
            affected_files: vec![target_file.to_string()],
            diff_preview: if dry_run {
                format!("insert {} bytes after \"{}\" in {}", code.len(), anchor, target_file)
            } else {
                String::new()
            },
            unverified_sites: Vec::new(),
            warnings: vec![
                "Preflight parse-check deferred to Python layer".into(),
            ],
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
            by_file.entry(edit.file.clone())
                .or_default()
                .push(edit.clone());
        }

        let mut syntax_errors: Vec<SyntaxDiagnostic> = Vec::new();
        let mut originals: HashMap<String, String> = HashMap::new();

        // ── Phase 1: Stale-write rejection ──────────────────────────────
        // Every non-empty expected_hash must match the current content at its
        // span. Any mismatch → reject the entire plan (nothing is written).
        for (file_path, edits) in &by_file {
            let original = match std::fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(e) => {
                    syntax_errors.push(SyntaxDiagnostic {
                        file: file_path.clone(), line: 0, column: 0,
                        message: format!("read failed: {}", e),
                        offending_span: ByteSpan { start: 0, end: 0 },
                    });
                    return MutationResult {
                        status: MutationStatus::RolledBack,
                        files_written: vec![], syntax_errors,
                        reindex: ReindexSummary { files: 0, entities_updated: 0, edges_updated: 0, duration_ms: 0 },
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
                        files_written: vec![], syntax_errors,
                        reindex: ReindexSummary { files: 0, entities_updated: 0, edges_updated: 0, duration_ms: 0 },
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
            if let Err(e) = std::fs::copy(file_path, &backup_path) {
                for (_, bp) in &backups { let _ = std::fs::remove_file(bp); }
                syntax_errors.push(SyntaxDiagnostic {
                    file: file_path.clone(), line: 0, column: 0,
                    message: format!("backup failed: {}", e),
                    offending_span: ByteSpan { start: 0, end: 0 },
                });
                return MutationResult {
                    status: MutationStatus::RolledBack,
                    files_written: vec![], syntax_errors,
                    reindex: ReindexSummary { files: 0, entities_updated: 0, edges_updated: 0, duration_ms: 0 },
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
                        file: file_path.clone(), line: 0, column: 0,
                        message: format!("apply failed: {:?}", e),
                        offending_span: ByteSpan { start: 0, end: 0 },
                    });
                    return MutationResult {
                        status: MutationStatus::RolledBack,
                        files_written: vec![], syntax_errors,
                        reindex: ReindexSummary { files: 0, entities_updated: 0, edges_updated: 0, duration_ms: 0 },
                        backup_path: None,
                    };
                }
            };

            let tmp_path = format!("{}.coderadar-tmp", file_path);
            let write_ok = std::fs::File::create(&tmp_path)
                .and_then(|mut f| f.write_all(new_content.as_bytes()))
                .and_then(|_| std::fs::rename(&tmp_path, file_path))
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
                let _ = std::fs::remove_file(&tmp_path);
                rollback_all(&backups);
                syntax_errors.push(SyntaxDiagnostic {
                    file: file_path.clone(), line: 0, column: 0,
                    message: "atomic write failed".into(),
                    offending_span: ByteSpan { start: 0, end: 0 },
                });
                return MutationResult {
                    status: MutationStatus::RolledBack,
                    files_written: vec![], syntax_errors,
                    reindex: ReindexSummary { files: 0, entities_updated: 0, edges_updated: 0, duration_ms: 0 },
                    backup_path: None,
                };
            }
        }

        // ── Phase 4: Post-write parse verification ───────────────────────
        let mut tainted: Vec<SyntaxDiagnostic> = Vec::new();
        for file_path in &files_written {
            let original_bytes = originals.get(file_path).map(|s| s.as_bytes()).unwrap_or(&[]);
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
                reindex: ReindexSummary { files: 0, entities_updated: 0, edges_updated: 0, duration_ms: 0 },
                backup_path: backups.first().map(|(_, bp)| bp.clone()),
            };
        }

        // ── Phase 5: Success — clean up backups ──────────────────────────
        for (_, backup_path) in &backups {
            let _ = std::fs::remove_file(backup_path);
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
        let _ = std::fs::copy(backup_path, file_path);
        let _ = std::fs::remove_file(backup_path);
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
    PolicyViolation { path: String, reason: String },
    HashMismatch { file: String, expected: String, actual: String },
    TooManyFiles(usize),
    TooManyEdits(usize),
    SyntaxDiagnostic(Vec<SyntaxDiagnostic>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::MutationConfig;
    use crate::types::ByteSpan;
    use std::path::Path;

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
        assert!(result.syntax_errors[0].message.contains("outside the project root"));
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
        assert!(result.syntax_errors[0].message.contains("over the configured limit"));
    }

    #[test]
    fn test_policy_rejects_traversal_out_of_the_root() {
        let project = tempfile::tempdir().unwrap();
        let outside = project.path().parent().unwrap().join("escape.py");
        std::fs::write(&outside, "x = 1\n").unwrap();

        let escaping = project.path().join("..").join("escape.py");
        let mut eng = engine().with_project_root(project.path());
        let plan = hashed_plan(&escaping.to_string_lossy(), ByteSpan { start: 0, end: 1 }, "x = 1\n");

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
        let ok = hashed_plan(&inside.to_string_lossy(), ByteSpan { start: 0, end: 1 }, "a = 1\n");
        assert_eq!(eng.apply(&ok).status, MutationStatus::Applied);

        let outside = dir.path().join("other.py");
        let mut eng = MutationEngine::new(config).with_project_root(dir.path());
        let refused = hashed_plan(&outside.to_string_lossy(), ByteSpan { start: 0, end: 1 }, "a = 1\n");
        let result = eng.apply(&refused);
        assert_eq!(result.status, MutationStatus::RejectedPolicy);
        assert!(result.syntax_errors[0].message.contains("allow list"));
    }

    #[test]
    fn test_path_matches_pattern_shapes() {
        assert!(path_matches("src/a.py", "src/"));
        assert!(path_matches("pkg/src/a.py", "src/"));
        assert!(!path_matches("mysrc/a.py", "src/"));

        assert!(path_matches("app/migrations/0001.py", "/migrations/"));
        assert!(path_matches("migrations/0001.py", "/migrations/"));

        assert!(path_matches("uv.lock", "/*.lock"));
        assert!(path_matches("sub/poetry.lock", "/*.lock"));
        assert!(!path_matches("locked.py", "/*.lock"));
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
        assert_eq!(result.status, MutationStatus::Applied, "{:#?}", result.syntax_errors);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "def f():\n    pass\nimport os\n");
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
            span: ByteSpan { start: len, end: len },
            replacement: "\ndef g():\n    pass\n".into(),
            expected_hash: String::new(),
        }]);
        let result = eng.apply(&p);
        assert_eq!(result.status, MutationStatus::Applied);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "import os\ndef g():\n    pass\n");
    }

    #[test]
    fn test_apply_rejects_stale_edit() {
        let src = b"def foo():\n    return 1\n";
        let start = src.windows(8).position(|w| w == b"return 1").unwrap();
        let span = ByteSpan { start, end: start + 8 };
        let hash = hash_span(src, span);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        // Simulate the file changing after the plan was built
        std::fs::write(&file, b"def foo():\n    return 999\n").unwrap();

        let mut eng = engine();
        let p = plan(vec![MutationEdit {
            file: file.clone(), span, replacement: "return 2".into(), expected_hash: hash,
        }]);
        let result = eng.apply(&p);
        assert_eq!(result.status, MutationStatus::RejectedStale);
        assert!(!result.syntax_errors.is_empty());
        // File must be untouched
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "def foo():\n    return 999\n");
    }

    #[test]
    fn test_apply_rolls_back_tainted_update() {
        let src = b"def foo():\n    return 1\n";
        let span = ByteSpan { start: 0, end: src.len() };
        let hash = hash_span(src, span);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        let mut eng = engine();
        // Replacement is syntactically broken → post-verify must roll back
        let p = plan(vec![MutationEdit {
            file: file.clone(), span,
            replacement: "def foo(:\n  broken".into(),
            expected_hash: hash,
        }]);
        let result = eng.apply(&p);
        assert_eq!(result.status, MutationStatus::RolledBack, "{:#?}", result.syntax_errors);
        // File restored to original
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "def foo():\n    return 1\n");
        // No leftover backup file
        assert!(!Path::new(&format!("{}.coderadar-bak", file)).exists());
    }

    #[test]
    fn test_apply_succeeds_when_hash_matches() {
        let src = b"def foo():\n    return 1\n";
        let start = src.windows(8).position(|w| w == b"return 1").unwrap();
        let span = ByteSpan { start, end: start + 8 };
        let hash = hash_span(src, span);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, src).unwrap();
        let file = path.to_string_lossy().to_string();

        let mut eng = engine();
        let p = plan(vec![MutationEdit {
            file: file.clone(), span, replacement: "return 2".into(), expected_hash: hash,
        }]);
        let result = eng.apply(&p);
        assert_eq!(result.status, MutationStatus::Applied, "{:#?}", result.syntax_errors);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "def foo():\n    return 2\n");
    }
}
