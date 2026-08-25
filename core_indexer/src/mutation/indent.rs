// CodeRadar v3.6 — Mutation: Indent Normalization (§11.4)
// Normalizes LLM-pasted code to the target indentation level before rope application.

use crate::types::ByteSpan;

/// Detected indentation style for a file.
#[derive(Clone, Debug, PartialEq)]
pub struct IndentStyle {
    pub unit: char, // ' ' or '\t'
    pub width: usize,
}

impl IndentStyle {
    pub fn spaces(width: usize) -> Self {
        Self {
            unit: ' ',
            width: width.max(1),
        }
    }

    pub fn tabs() -> Self {
        Self { unit: '\t', width: 1 }
    }
}

/// Detect the file's dominant indentation convention.
/// Tabs win if tab-indented lines outnumber space-indented ones;
/// otherwise spaces with the most common leading-run width.
pub fn detect_indent_style(source: &str) -> IndentStyle {
    let mut tab_count = 0usize;
    let mut space_counts: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();

    for line in source.lines() {
        if line.is_empty() {
            continue;
        }
        let leading = line.chars().take_while(|c| c.is_whitespace()).collect::<Vec<_>>();
        if leading.iter().any(|c| *c == '\t') {
            tab_count += 1;
        } else if !leading.is_empty() && leading.iter().all(|c| *c == ' ') {
            *space_counts.entry(leading.len()).or_default() += 1;
        }
    }

    if tab_count > space_counts.values().sum() {
        IndentStyle::tabs()
    } else {
        // Pick the most common width. On ties, prefer the smaller width
        // (base indentation, e.g. 4-space over 8-space continuation).
        let width = space_counts
            .into_iter()
            .max_by(|(w1, c1), (w2, c2)| c1.cmp(c2).then_with(|| w2.cmp(w1)))
            .map(|(w, _)| w)
            .unwrap_or(4);
        IndentStyle::spaces(width)
    }
}

/// Normalize new code's indentation to match the target level.
///
/// Steps:
/// 0. An EMPTY target means the caller supplies final formatting verbatim:
///    the input is returned unchanged. Historically an empty target stripped
///    every line to the incoming minimum, which silently dedented
///    naturally-indented bodies by one level and corrupted the file
///    (CODERADAR_BUGS_QUIRKS.md #1). Least-destruction wins: when there is
///    no target level to normalize TO, do not touch the caller's text.
/// 1. Find incoming_base = minimum leading whitespace across non-empty lines.
/// 2. Per line: target + (line with incoming_base stripped), preserving relative depth.
/// 3. Convert leading whitespace to match IndentStyle (tabs <-> spaces).
/// 4. Lines inside verbatim_spans (multi-line string literals) are preserved verbatim.
/// 5. Empty lines stay empty (no trailing whitespace introduced).
pub fn normalize_indent(
    new_code: &str,
    target_indent: &str,
    style: &IndentStyle,
    verbatim_spans: &[ByteSpan],
) -> String {
    // 0. Empty target = verbatim passthrough (see doc above).
    if target_indent.is_empty() {
        return new_code.to_string();
    }

    // 1. Compute incoming base indent
    let incoming_base = new_code
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
        .min()
        .unwrap_or(0);

    let target_base_len = target_indent.len();
    let mut result = String::new();

    for (line_idx, line) in new_code.lines().enumerate() {
        if line.trim().is_empty() {
            result.push('\n');
            continue;
        }

        // Check if this line falls within a verbatim span
        let is_verbatim = verbatim_spans.iter().any(|span| {
            let line_start = new_code
                .lines()
                .take(line_idx)
                .map(|l| l.len() + 1)
                .sum::<usize>();
            line_start >= span.start && line_start < span.end
        });

        if is_verbatim {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 2. Strip incoming base, apply target indent
        let leading_count = line
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        let relative = leading_count.saturating_sub(incoming_base);
        let trimmed = line.trim_start();

        // 3. Build new leading whitespace in target style
        // `relative` is already a whitespace char count — do NOT multiply by width.
        let new_leading = match style.unit {
            ' ' => " ".repeat(target_base_len + relative),
            '\t' => {
                let tabs = (target_base_len + relative) / style.width.max(1);
                "\t".repeat(tabs)
            }
            _ => " ".repeat(target_base_len + relative),
        };

        result.push_str(&new_leading);
        result.push_str(trimmed);
        result.push('\n');
    }

    // Remove trailing newline if original didn't have one
    if !new_code.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_indent_spaces() {
        let source = "    def foo():\n        pass\n";
        let style = detect_indent_style(source);
        assert_eq!(style.unit, ' ');
        assert_eq!(style.width, 4);
    }

    #[test]
    fn test_normalize_column_zero_paste() {
        let new_code = "def foo():\n    return True\n";
        let target = "        "; // 8 spaces
        let style = IndentStyle::spaces(4);
        let result = normalize_indent(new_code, target, &style, &[]);
        // Should re-indent to target level
        assert!(result.starts_with("        def foo()"));
    }

    #[test]
    fn test_normalize_preserves_verbatim() {
        let new_code = "def foo():\n    '''\n    verbatim\n    '''\n    pass\n";
        let target = "    ";
        let style = IndentStyle::spaces(4);
        let span = ByteSpan { start: 14, end: 37 }; // rough span covering the string
        let result = normalize_indent(new_code, target, &style, &[span]);
        // The triple-quoted interior should be preserved
        assert!(result.contains("    verbatim"));
    }

    #[test]
    fn test_empty_target_is_verbatim_no_dedent() {
        // Regression: CODERADAR_BUGS_QUIRKS.md #1 — an empty target used to
        // strip every line to the incoming minimum, dedenting a naturally-
        // indented body by one level and producing `return` outside function.
        let new_body = "    \"\"\"Merge HTML metadata.\"\"\"\n    merged = {**a, **b}\n    return merged\n";
        assert_eq!(
            normalize_indent(new_body, "", &IndentStyle::spaces(4), &[]),
            new_body,
            "empty target must be a verbatim passthrough"
        );
    }
}
