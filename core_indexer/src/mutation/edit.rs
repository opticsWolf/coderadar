// CodeRadar v3.6 — Mutation: Rope-Based Multi-Edit Application (§11.3)
// Applies edits in descending offset order to keep positions stable.

use ropey::Rope;

use crate::types::ByteSpan;

use super::{MutationEdit, MutationError};

/// Apply a list of MutationEdits to source text using a rope.
/// Edits are sorted by descending span.start so earlier edits don't shift later spans.
/// ByteSpans are byte offsets; ropey uses char indices — we convert via byte_to_char.
pub fn apply_edits_to_file(source: &str, edits: &[MutationEdit]) -> Result<String, MutationError> {
    let rope = Rope::from_str(source);
    let mut ordered: Vec<&MutationEdit> = edits.iter().collect();
    ordered.sort_by(|a, b| b.span.start.cmp(&a.span.start)); // descending byte offsets

    let mut result = rope.clone();
    for edit in ordered {
        let (s, e) = rope_clamped_char_bounds(&result, edit.span)?;
        result.remove(s..e);
        result.insert(s, &edit.replacement);
    }

    Ok(result.to_string())
}

/// Convert a ByteSpan (byte offsets) to Rope char indices, clamping to bounds.
fn rope_clamped_char_bounds(rope: &Rope, span: ByteSpan) -> Result<(usize, usize), MutationError> {
    let len_bytes = rope.len_bytes();
    let len_chars = rope.len_chars();
    let start = span.start.min(len_bytes);
    let end = span.end.min(len_bytes);

    if start > len_bytes || end > len_bytes || start > end {
        return Err(MutationError::ParseFailed(
            "ByteSpan out of rope bounds".into(),
        ));
    }
    // Convert byte offsets → char indices (ropey is char-indexed)
    let char_start = rope.byte_to_char(start);
    let char_end = rope.byte_to_char(end);
    Ok((char_start.min(len_chars), char_end.min(len_chars)))
}

/// Lines of context around a change in `unified_diff`.
const DIFF_CONTEXT: usize = 3;

/// Render a unified diff between two versions of one file.
///
/// Positional line-by-line comparison — what this replaced — reports every
/// line after an insertion as changed. This trims the common prefix and
/// suffix and emits what is left as a single hunk, which is exact: the
/// output applies cleanly with `patch`.
pub fn unified_diff(path: &str, old_source: &str, new_source: &str) -> String {
    let old_lines: Vec<&str> = old_source.lines().collect();
    let new_lines: Vec<&str> = new_source.lines().collect();

    if old_lines == new_lines {
        return String::new();
    }

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let ctx_start = prefix.saturating_sub(DIFF_CONTEXT);
    let old_change_end = old_lines.len() - suffix;
    let new_change_end = new_lines.len() - suffix;
    let old_ctx_end = (old_change_end + DIFF_CONTEXT).min(old_lines.len());
    let new_ctx_end = (new_change_end + DIFF_CONTEXT).min(new_lines.len());

    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n", path));
    out.push_str(&format!("+++ b/{}\n", path));
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        ctx_start + 1,
        old_ctx_end - ctx_start,
        ctx_start + 1,
        new_ctx_end - ctx_start,
    ));
    for line in &old_lines[ctx_start..prefix] {
        out.push_str(&format!(" {}\n", line));
    }
    for line in &old_lines[prefix..old_change_end] {
        out.push_str(&format!("-{}\n", line));
    }
    for line in &new_lines[prefix..new_change_end] {
        out.push_str(&format!("+{}\n", line));
    }
    for line in &old_lines[old_change_end..old_ctx_end] {
        out.push_str(&format!(" {}\n", line));
    }
    out
}

/// Render the diff a plan's edits would produce, file by file.
///
/// The plan carries byte spans and replacements; the MCP tool description
/// promises "a diff preview for review" and the Python layer renders the
/// result inside a ```diff fence. This reads each touched file, applies that
/// file's edits in memory, and diffs the two versions — nothing is written.
///
/// A file that cannot be read or whose spans do not apply falls back to a
/// one-line description of the edit rather than an empty preview.
pub fn diff_preview_for_edits(edits: &[MutationEdit]) -> String {
    let mut files: Vec<&str> = edits.iter().map(|e| e.file.as_str()).collect();
    files.sort();
    files.dedup();

    let mut out = String::new();
    for file in files {
        let for_file: Vec<MutationEdit> = edits
            .iter()
            .filter(|e| e.file == file)
            .cloned()
            .collect();
        match std::fs::read_to_string(file) {
            Ok(original) => match apply_edits_to_file(&original, &for_file) {
                Ok(updated) => out.push_str(&unified_diff(file, &original, &updated)),
                Err(e) => out.push_str(&format!("# {}: edits do not apply ({:?})\n", file, e)),
            },
            Err(e) => {
                out.push_str(&format!("# {}: cannot be read ({})\n", file, e));
                for edit in &for_file {
                    out.push_str(&format!(
                        "# replace {} bytes at {}..{}\n",
                        edit.span.end.saturating_sub(edit.span.start),
                        edit.span.start,
                        edit.span.end,
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::MutationEdit;
    use crate::types::ByteSpan;

    #[test]
    fn test_apply_single_edit() {
        // "def foo():\n    pass\n" = 20 chars
        // chars 0..10 = "def foo():", char 10 = '\n'
        let source = "def foo():\n    pass\n";
        let edit = MutationEdit {
            file: "test.py".into(),
            span: ByteSpan { start: 0, end: 10 },  // just "def foo():", no newline
            replacement: "def bar():".into(),
            expected_hash: "".into(),
        };
        let result = apply_edits_to_file(source, &[edit]).unwrap();
        assert_eq!(result, "def bar():\n    pass\n");
    }

    #[test]
    fn test_apply_multiple_edits_descending_order() {
        // "first line\nsecond line\nthird line\n" = 34 chars
        // "third line\n" is at chars 23..34
        // "first line" is at chars 0..10 (no newline)
        let source = "first line\nsecond line\nthird line\n";
        let edit1 = MutationEdit {
            file: "test.py".into(),
            span: ByteSpan { start: 23, end: 34 }, // "third line\n"
            replacement: "THIRD\n".into(),
            expected_hash: "".into(),
        };
        let edit2 = MutationEdit {
            file: "test.py".into(),
            span: ByteSpan { start: 0, end: 10 },  // "first line"
            replacement: "FIRST".into(),
            expected_hash: "".into(),
        };
        let result = apply_edits_to_file(source, &[edit1, edit2]).unwrap();
        assert_eq!(result, "FIRST\nsecond line\nTHIRD\n");
    }

    #[test]
    fn test_apply_empty_edits_noop() {
        let source = "unchanged";
        let result = apply_edits_to_file(source, &[]).unwrap();
        assert_eq!(result, source);
    }

    #[test]
    fn unified_diff_has_headers_and_a_hunk() {
        let diff = unified_diff("m.py", "old\nsame\n", "new\nsame\n");
        assert!(diff.starts_with("--- a/m.py\n+++ b/m.py\n"), "{}", diff);
        assert!(diff.contains("@@ -1,2 +1,2 @@"), "{}", diff);
        assert!(diff.contains("-old\n"), "{}", diff);
        assert!(diff.contains("+new\n"), "{}", diff);
        assert!(diff.contains(" same\n"), "{}", diff);
    }

    #[test]
    fn unified_diff_of_identical_sources_is_empty() {
        assert!(unified_diff("m.py", "same\n", "same\n").is_empty());
    }

    #[test]
    fn an_inserted_line_does_not_rewrite_the_rest_of_the_file() {
        // The positional diff this replaced paired line i with line i, so it
        // reported every line after an insertion as changed.
        let old_source = "a\nb\nc\nd\ne\nf\n";
        let new_source = "a\nb\nc\nINSERTED\nd\ne\nf\n";
        let diff = unified_diff("m.py", old_source, new_source);
        assert!(diff.contains("+INSERTED\n"), "{}", diff);
        assert!(!diff.contains("-d\n"), "d was not touched: {}", diff);
        assert!(!diff.contains("-e\n"), "e was not touched: {}", diff);
    }

    #[test]
    fn diff_preview_reads_the_file_and_applies_the_edits() {
        let dir = std::env::temp_dir()
            .join(format!("coderadar_diff_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.py");
        std::fs::write(&file, "def f():\n    return 1\n").unwrap();

        let path = file.to_string_lossy().to_string();
        let start = "def f():\n    ".len();
        let edit = MutationEdit {
            file: path.clone(),
            span: ByteSpan { start, end: start + "return 1".len() },
            replacement: "return 2".into(),
            expected_hash: "".into(),
        };
        let preview = diff_preview_for_edits(&[edit]);
        assert!(preview.contains("-    return 1\n"), "{}", preview);
        assert!(preview.contains("+    return 2\n"), "{}", preview);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn diff_preview_says_so_when_a_file_is_missing() {
        let edit = MutationEdit {
            file: "no/such/file.py".into(),
            span: ByteSpan { start: 0, end: 4 },
            replacement: "x".into(),
            expected_hash: "".into(),
        };
        let preview = diff_preview_for_edits(&[edit]);
        assert!(preview.contains("cannot be read"), "{}", preview);
    }

    #[test]
    fn test_apply_edit_with_multibyte_chars_before_span() {
        // Multi-byte UTF-8 chars BEFORE the edit point shift byte offsets
        // away from char indices. "é" is 2 bytes, "→" is 3 bytes.
        // Source: "# é → café\ndef target():\n    pass\n"
        // Byte offset of "target" is NOT its char index.
        let source = "# é → café\ndef target():\n    pass\n";
        // Compute the byte offset of "target" by searching bytes
        let target_byte = source.find("target").unwrap();
        assert!(target_byte > source[..target_byte].chars().count(),
            "byte offset should exceed char count with multibyte chars");
        let edit = MutationEdit {
            file: "test.py".into(),
            span: ByteSpan { start: target_byte, end: target_byte + "target".len() },
            replacement: "renamed".into(),
            expected_hash: "".into(),
        };
        let result = apply_edits_to_file(source, &[edit]).unwrap();
        assert_eq!(result, "# é → café\ndef renamed():\n    pass\n");
    }
}
