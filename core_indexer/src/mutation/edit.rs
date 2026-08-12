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

/// Compute a simple line-based diff between old and new source.
pub fn compute_diff_preview(old_source: &str, new_source: &str) -> String {
    let old_lines: Vec<&str> = old_source.lines().collect();
    let new_lines: Vec<&str> = new_source.lines().collect();

    let mut diff = String::new();
    let max_len = old_lines.len().max(new_lines.len());

    for i in 0..max_len {
        let old_line = old_lines.get(i).unwrap_or(&"");
        let new_line = new_lines.get(i).unwrap_or(&"");

        if old_line != new_line {
            diff.push_str(&format!("- {}\n", old_line));
            diff.push_str(&format!("+ {}\n", new_line));
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::{MutationEdit, MutationError};
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
    fn test_compute_diff_preview() {
        let diff = compute_diff_preview("old\nsame\n", "new\nsame\n");
        assert!(diff.contains("- old"));
        assert!(diff.contains("+ new"));
    }

    #[test]
    fn test_compute_diff_preview_no_changes() {
        let diff = compute_diff_preview("same\n", "same\n");
        assert!(diff.is_empty());
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
