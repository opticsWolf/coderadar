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

use ropey::Rope;

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

pub struct MutationEngine {
    pub write_guard: std::sync::Arc<WriteGuard>,
    pub config: crate::graph::MutationConfig,
}

impl MutationEngine {
    pub fn new(config: crate::graph::MutationConfig) -> Self {
        Self {
            write_guard: shared_write_guard(),
            config,
        }
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
    pub fn plan_rename(
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
        let def_hash = hash_span(&def_source, name_span);
        edits.push(MutationEdit {
            file: def_file.clone(),
            span: name_span,
            replacement: new_name.to_string(),
            expected_hash: def_hash,
        });
        affected.push(def_file);

        // 2. Caller side: rewrite every resolved reference name_span
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
                let caller_source = std::fs::read(&caller_file).unwrap_or_default();

                for (i, rc) in caller_fn.resolved_calls.iter().enumerate() {
                    let targets_entity = match rc {
                        ResolvedCall::Function(id)
                        | ResolvedCall::Method { method: id, .. }
                        | ResolvedCall::Constructor(id) => id == entity_id,
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
                        // Simple call `foo()` — name starts at (line, col)
                        if let Some(start) = line_col_to_byte(&caller_source, call.line, call.col) {
                            let end = (start + call.name.len()).min(caller_source.len());
                            let span = ByteSpan { start, end };
                            edits.push(MutationEdit {
                                file: caller_file.clone(),
                                span,
                                replacement: new_name.to_string(),
                                expected_hash: hash_span(&caller_source, span),
                            });
                            if !affected.contains(&caller_file) {
                                affected.push(caller_file.clone());
                            }
                        }
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
        }

        // 3. String-literal occurrences (only if include_strings=true)
        if include_strings {
            unverified.push(UnverifiedSite {
                file: module_file_path(projection, &fn_entity.parent_module),
                line: 0,
                snippet: format!("string-literal references to \"{}\"", fn_entity.name),
                reason: "String-literal rename requires manual review".into(),
            });
        }

        // 4. Also check classes
        if let Some(cls) = projection.classes.get(entity_id) {
            let cls_file = module_file_path(projection, &cls.parent_module);
            let cls_source = std::fs::read(&cls_file).unwrap_or_default();
            edits.push(MutationEdit {
                file: cls_file.clone(),
                span: cls.name_span,
                replacement: new_name.to_string(),
                expected_hash: hash_span(&cls_source, cls.name_span),
            });
            if !affected.contains(&cls_file) {
                affected.push(cls_file);
            }
        }

        let plan_id = ulid::Ulid::new().to_string();

        Ok(MutationPlan {
            id: plan_id,
            tool: "rename_symbol".to_string(),
            edits,
            affected_files: affected,
            diff_preview: if dry_run {
                format!("rename \"{}\" → \"{}\" in {} file(s)",
                    fn_entity.name, new_name, callers.len() + 1)
            } else {
                String::new()
            },
            unverified_sites: unverified,
            warnings: Vec::new(),
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
