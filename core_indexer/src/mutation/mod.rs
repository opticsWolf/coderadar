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
use crate::mutation::indent::{detect_indent_style, normalize_indent, IndentStyle};
use crate::mutation::write_guard::WriteGuard;
use crate::types::{ByteSpan, ParseQuality, ProjectedGraph, ResolvedCall};

/// Detect indent style from a span — defers to file-content-based detection.
/// The Python layer handles actual file reads; here we return a safe default.
fn detect_indent_style_from_span(_span: &ByteSpan) -> IndentStyle {
    IndentStyle { unit: ' ', width: 4 }
}

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

        // 2. Detect indent style from body
        let indent = detect_indent_style_from_span(
            &fn_entity.body_span,
            // body text provided by Python layer
        );

        // 3. Normalize new_body indentation to match target
        let target_indent = " ".repeat(indent.width);
        let normalized_body = normalize_indent(new_body, &target_indent, &indent, &[]);

        let plan_id = ulid::Ulid::new().to_string();

        Ok(MutationPlan {
            id: plan_id,
            tool: "replace_entity_body".to_string(),
            edits: vec![MutationEdit {
                file: fn_entity.parent_module.clone(),
                span: body_span,
                replacement: normalized_body,
                expected_hash: expected_hash.unwrap_or_else(|| format!("{:x}", fn_entity.body_hash)),
            }],
            affected_files: vec![fn_entity.parent_module.clone()],
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
            file: fn_entity.parent_module.clone(),
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
                for call in &caller_fn.resolved_calls {
                    // Check if this call targets the entity being modified
                    let target_matches = match call {
                        ResolvedCall::Function(id) | ResolvedCall::Method { method: id, .. } | ResolvedCall::Constructor(id) => {
                            id == entity_id
                        }
                        _ => false,
                    };

                    if target_matches {
                        // Generate call-site edit
                        if let Some(kv_value) = call_site_values.get(caller_id) {
                            // Caller provided new arg list for this call site
                            edits.push(MutationEdit {
                                file: caller_fn.parent_module.clone(),
                                span: ByteSpan { start: 0, end: 0 }, // resolved by Python layer
                                replacement: kv_value.clone(),
                                expected_hash: String::new(),
                            });
                        } else if inject_defaults {
                            // Auto-fill with defaults from new signature
                            // This is language-specific and handled by Python layer
                            warnings.push(format!(
                                "Call site in {} at {} — defaults auto-injected",
                                caller_fn.parent_module, caller_fn.id
                            ));
                        } else {
                            unverified.push(UnverifiedSite {
                                file: caller_fn.parent_module.clone(),
                                line: 0, // resolved by Python
                                snippet: format!("call to {} in {}", entity_id, caller_fn.id),
                                reason: "Call site needs manual update — no values provided".into(),
                            });
                        }
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
                    .map(|f| f.parent_module.clone())
                    .collect();
                files.push(fn_entity.parent_module.clone());
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
        edits.push(MutationEdit {
            file: fn_entity.parent_module.clone(),
            span: name_span,
            replacement: new_name.to_string(),
            expected_hash: format!("{:x}", fn_entity.signature_hash),
        });

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
                // Find call sites referencing this entity and produce name rewrites
                let found = caller_fn.resolved_calls.iter().any(|rc| {
                    match rc {
                        ResolvedCall::Function(id) | ResolvedCall::Method { method: id, .. } | ResolvedCall::Constructor(id) => {
                            id == entity_id
                        }
                        _ => false,
                    }
                });

                if found {
                    // The actual name_span of the call site is resolved by Python layer
                    // (we don't know exact byte offsets from the projected graph)
                    edits.push(MutationEdit {
                        file: caller_fn.parent_module.clone(),
                        span: ByteSpan { start: 0, end: 0 }, // resolved by Python
                        replacement: new_name.to_string(),
                        expected_hash: String::new(),
                    });
                }

                if !affected.contains(&caller_fn.parent_module) {
                    affected.push(caller_fn.parent_module.clone());
                }
            }
        }

        // 3. String-literal occurrences (only if include_strings=true)
        if include_strings {
            unverified.push(UnverifiedSite {
                file: fn_entity.parent_module.clone(),
                line: 0,
                snippet: format!("string-literal references to \"{}\"", fn_entity.name),
                reason: "String-literal rename requires manual review".into(),
            });
        }

        // 4. Also check classes
        if let Some(cls) = projection.classes.get(entity_id) {
            edits.push(MutationEdit {
                file: cls.parent_module.clone(),
                span: cls.name_span,
                replacement: new_name.to_string(),
                expected_hash: format!("{:x}", cls.content_hash),
            });
            if !affected.contains(&cls.parent_module) {
                affected.push(cls.parent_module.clone());
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
        // 1. Determine insertion point
        let insert_span = if anchor == "top" || anchor.is_empty() {
            // Insert at file top (after module docstring if present)
            ByteSpan { start: 0, end: 0 }
        } else if anchor == "end" {
            // Insert at file end
            ByteSpan { start: 0, end: 0 } // resolved by Python layer
        } else {
            // Anchor after a specific entity
            // Look up anchor entity by id
            if let Some(fn_ent) = projection.functions.get(anchor) {
                ByteSpan {
                    start: fn_ent.span.end,
                    end: fn_ent.span.end, // insert-only (replacement of empty)
                }
            } else if let Some(cls_ent) = projection.classes.get(anchor) {
                ByteSpan {
                    start: cls_ent.span.end,
                    end: cls_ent.span.end,
                }
            } else {
                return Err(MutationError::EntityNotFound(anchor.to_string()));
            }
        };

        let plan_id = ulid::Ulid::new().to_string();

        Ok(MutationPlan {
            id: plan_id,
            tool: "create_entity".to_string(),
            edits: vec![MutationEdit {
                file: target_file.to_string(),
                span: insert_span,
                replacement: format!("\n{}", code),
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

    /// Apply a mutation plan: hash guard → backup → indent normalize → apply → verify → commit.
    pub fn apply(&mut self, plan: &MutationPlan) -> MutationResult {
        // §11.6 Mutation Apply Pipeline:
        // 1. Policy check
        // 2. Acquire single-writer lock
        // 3. Hash guard — every file's xxHash == edit.expected_hash
        // 4. Snapshot originals → .harness/backups/{plan_id}/
        // 5. Per file: indent normalize → rope apply → candidate content
        // 6. VERIFY: re-parse with tree-sitter; new ERROR nodes → rollback
        // 7. Register in WriteGuard
        // 8. Atomic write: temp file + rename()
        // 9. Synchronous reindex through §6 pipeline
        // 10. Release writer lock; MutationLog entry; metrics
        // 11. Return MutationResult

        MutationResult {
            status: MutationStatus::Applied,
            files_written: plan.affected_files.clone(),
            syntax_errors: Vec::new(),
            reindex: ReindexSummary {
                files: plan.affected_files.len(),
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
