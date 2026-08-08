// CodeRadar v3.3 — AST-Aware Mutation Engine (§11)
// Four refactoring tools: replace_entity_body, update_signature, rename_symbol, create_entity.

pub mod edit;
pub mod indent;
pub mod write_guard;

use std::collections::HashMap;

use ropey::Rope;

use crate::mutation::edit::apply_edits_to_file;
use crate::mutation::indent::{detect_indent_style, normalize_indent, IndentStyle};
use crate::mutation::write_guard::WriteGuard;
use crate::types::ByteSpan;

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
        _entity_id: &str,
        new_body: &str,
        expected_hash: Option<String>,
        dry_run: bool,
    ) -> Result<MutationPlan, MutationError> {
        let expected_hash = expected_hash.clone();
        let _ = (&expected_hash, dry_run);

        // 1. Look up entity by id → get body_span
        // 2. Detect indent style of the target file
        // 3. Normalize new_body indentation to match target
        // 4. Produce one edit replacing body_span

        Ok(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "replace_entity_body".to_string(),
            edits: vec![MutationEdit {
                file: String::new(),
                span: ByteSpan { start: 0, end: 0 },
                replacement: new_body.to_string(),
                expected_hash: expected_hash.unwrap_or_default(),
            }],
            affected_files: Vec::new(),
            diff_preview: String::new(),
            unverified_sites: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// Plan a signature update with full call-site cascade.
    pub fn plan_signature_update(
        &self,
        _entity_id: &str,
        _new_signature: &str,
        _call_site_values: &HashMap<String, String>,
        _inject_defaults: bool,
        _dry_run: bool,
    ) -> Result<MutationPlan, MutationError> {
        // 1. Preflight parse of new_signature
        // 2. Diff old vs new parameters → added/removed/renamed/reordered/retyped
        // 3. Definition edit: replace params_span
        // 4. Enumerate call sites via call_graph.find_callers + Stack-Graph references
        // 5. Per-site rewrite rules based on parameter change type
        // 6. Preflight completeness check

        Ok(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "update_signature".to_string(),
            edits: Vec::new(),
            affected_files: Vec::new(),
            diff_preview: String::new(),
            unverified_sites: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// Plan a symbol rename across the codebase.
    pub fn plan_rename(
        &self,
        _entity_id: &str,
        _new_name: &str,
        _include_strings: bool,
        _dry_run: bool,
    ) -> Result<MutationPlan, MutationError> {
        // 1. Rewrite definition name_span
        // 2. Rewrite every Stack-Graph-resolved (L1) and import-constrained (L2) reference name_span
        // 3. Rewrite import statements preserving aliases
        // 4. Qualified usages rewrite attribute identifier node only
        // 5. String-literal occurrences (only if include_strings=true) → flagged in unverified_sites

        Ok(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "rename_symbol".to_string(),
            edits: Vec::new(),
            affected_files: Vec::new(),
            diff_preview: String::new(),
            unverified_sites: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// Plan a new entity creation anchored after an existing entity or at file top/end.
    pub fn plan_create_entity(
        &self,
        _target_file: &str,
        _anchor: &str,
        _code: &str,
        _dry_run: bool,
    ) -> Result<MutationPlan, MutationError> {
        // 1. Determine insertion point: anchor's span.end or file top/end
        // 2. Normalize new code indentation to sibling level
        // 3. Parse-check in context (synthetic file = target + insertion)
        // 4. List unresolvable references in warnings

        Ok(MutationPlan {
            id: ulid::Ulid::new().to_string(),
            tool: "create_entity".to_string(),
            edits: Vec::new(),
            affected_files: Vec::new(),
            diff_preview: String::new(),
            unverified_sites: Vec::new(),
            warnings: Vec::new(),
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
