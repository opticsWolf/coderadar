// CodeRadar v3.6 — AST-Aware Mutation Engine (§11)
// Four refactoring tools: replace_entity_body, update_signature, rename_symbol, create_entity.
//
// Each plan_* method takes the current CodeGraph for entity/span lookup.
// Actual file edits are applied via the Python layer (ropey + tree-sitter verify).

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

pub struct MutationEngine {
    pub write_guard: WriteGuard,
    pub config: crate::graph::MutationConfig,
}

impl MutationEngine {
    pub fn new(config: crate::graph::MutationConfig) -> Self {
        Self {
            write_guard: WriteGuard::new(),
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
                expected_hash: expected_hash.unwrap_or_else(|| format!("{:x}", fn_entity.body_hash)),
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
            file: module_file_path(projection, &fn_entity.parent_module),
            span: params_span,
            replacement: new_signature.to_string(),
            expected_hash: format!("{:x}", fn_entity.signature_hash),
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
        edits.push(MutationEdit {
            file: def_file.clone(),
            span: name_span,
            replacement: new_name.to_string(),
            expected_hash: format!("{:x}", fn_entity.signature_hash),
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
                            edits.push(MutationEdit {
                                file: caller_file.clone(),
                                span: ByteSpan { start, end },
                                replacement: new_name.to_string(),
                                expected_hash: String::new(),
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
            edits.push(MutationEdit {
                file: module_file_path(projection, &cls.parent_module),
                span: cls.name_span,
                replacement: new_name.to_string(),
                expected_hash: format!("{:x}", cls.content_hash),
            });
            let fp = module_file_path(projection, &cls.parent_module);
            if !affected.contains(&fp) {
                affected.push(fp);
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

    /// Apply a mutation plan: group edits by file → read → rope-apply → atomic write.
    pub fn apply(&mut self, plan: &MutationPlan) -> MutationResult {
        use std::collections::HashMap;
        use std::io::Write;

        // Group edits by file path (all edits carry real byte spans)
        let mut by_file: HashMap<String, Vec<MutationEdit>> = HashMap::new();
        for edit in &plan.edits {
            by_file.entry(edit.file.clone())
                .or_default()
                .push(edit.clone());
        }

        let mut files_written: Vec<String> = Vec::new();
        let mut syntax_errors: Vec<SyntaxDiagnostic> = Vec::new();

        for (file_path, edits) in &by_file {
            // Read original
            let original = match std::fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(e) => {
                    syntax_errors.push(SyntaxDiagnostic {
                        file: file_path.clone(),
                        line: 0, column: 0,
                        message: format!("read failed: {}", e),
                        offending_span: ByteSpan { start: 0, end: 0 },
                    });
                    continue;
                }
            };

            // Apply edits via rope
            let new_content = match apply_edits_to_file(&original, &edits) {
                Ok(s) => s,
                Err(e) => {
                    syntax_errors.push(SyntaxDiagnostic {
                        file: file_path.clone(),
                        line: 0, column: 0,
                        message: format!("apply failed: {:?}", e),
                        offending_span: ByteSpan { start: 0, end: 0 },
                    });
                    continue;
                }
            };

            // Atomic write: temp file + rename
            let tmp_path = format!("{}.coderadar-tmp", file_path);
            let write_ok = std::fs::File::create(&tmp_path)
                .and_then(|mut f| f.write_all(new_content.as_bytes()))
                .and_then(|_| std::fs::rename(&tmp_path, file_path))
                .is_ok();

            if write_ok {
                files_written.push(file_path.clone());
            } else {
                let _ = std::fs::remove_file(&tmp_path);
                syntax_errors.push(SyntaxDiagnostic {
                    file: file_path.clone(),
                    line: 0, column: 0,
                    message: "atomic write failed".into(),
                    offending_span: ByteSpan { start: 0, end: 0 },
                });
            }
        }

        let status = if syntax_errors.is_empty() {
            MutationStatus::Applied
        } else if !files_written.is_empty() {
            MutationStatus::Applied // partial success — report errors separately
        } else {
            MutationStatus::RolledBack
        };

        MutationResult {
            status,
            files_written,
            syntax_errors,
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
}
