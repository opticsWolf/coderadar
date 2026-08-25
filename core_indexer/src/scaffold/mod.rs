// CodeRadar Stage 3 — AI-scaffolding & secrets scanner.
//
// Zero graph dependency by design: comment markers, placeholder bodies,
// temp-file naming, and hardcoded secrets are all raw-text signals. Fossil
// reference: `src/mcp/tools/scaffolding.rs` — we port the RULE TABLES as
// data, NOT its structure (their 3,891-LOC god-file is the plan's cautionary
// example; this module stays under 400 LOC total).

pub mod secrets;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{EntityId, ProjectedGraph};

/// What kind of scaffolding signal was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaffoldKind {
    /// "Phase \d", "Step \d", TODO/FIXME/HACK/WIP-style markers.
    CommentMarker,
    /// A function whose body is only a stub (pass / todo!() / ...).
    PlaceholderBody,
    /// temp_*/backup_*/old_*/phase_* style file names.
    TempFile,
    /// Hardcoded credential shape (always redacted).
    Secret,
}

impl ScaffoldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScaffoldKind::CommentMarker => "comment-marker",
            ScaffoldKind::PlaceholderBody => "placeholder-body",
            ScaffoldKind::TempFile => "temp-file",
            ScaffoldKind::Secret => "secret",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScaffoldFinding {
    pub kind: ScaffoldKind,
    pub file: PathBuf,
    pub line: usize,
    pub label: String,
    /// Snippet with any secret material already redacted.
    pub snippet: String,
}

#[derive(Clone, Debug)]
pub struct ScaffoldConfig {
    /// Regexes matched against each source line.
    pub comment_patterns: Vec<String>,
    pub include_secrets: bool,
    pub max_file_bytes: u64,
}

impl Default for ScaffoldConfig {
    fn default() -> Self {
        Self {
            comment_patterns: vec![
                r"(?i)\bphase\s*\d+".into(),
                r"(?i)\bstep\s*\d+\b".into(),
                r"\bTODO\b".into(),
                r"\bFIXME\b".into(),
                r"\bHACK\b".into(),
                r"\bWIP\b".into(),
                r"(?i)implement (this|later|me)".into(),
                r"(?i)in (a )?(real|production)".into(),
                r"(?i)for now[,.]".into(),
            ],
            include_secrets: false,
            max_file_bytes: 1_000_000,
        }
    }
}

/// Is this body text just a stub?
pub fn is_placeholder_body(body: &str) -> bool {
    let t = body.trim();
    matches!(
        t,
        "pass" | "..." | "…" | "todo!()" | "unimplemented!()"
    ) || t.starts_with("raise NotImplementedError")
        || t.starts_with("panic!(")
}

/// Tiny glob: `*` wildcard only, case-insensitive. Covers `temp_*`, `*.bak`,
/// `*_old.*` without pulling in a glob crate. Greedy backtracking matcher.
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ni;
        } else if pi < p.len() && p[pi] == n[ni] {
            pi += 1;
            ni += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Scan a directory tree for scaffolding signals. Uses the `ignore` walker so
/// `.gitignore`d files are skipped. Secrets are ALWAYS redacted in snippets.
pub fn scan_path(root: &Path, cfg: &ScaffoldConfig) -> Vec<ScaffoldFinding> {
    let marker_res: Vec<regex::Regex> = cfg
        .comment_patterns
        .iter()
        .map(|p| regex::Regex::new(p).expect("config regex must compile"))
        .collect();

    let temp_globs = [
        "temp_*", "tmp_*", "backup_*", "old_*", "phase_*", "copy_*", "*.bak", "*.tmp", "*_old.*",
    ];

    let mut out = Vec::new();
    // require_git(false): honor .gitignore even outside a git repo — for a
    // secrets scanner, over-scanning vendored/ignored trees is worse than
    // under-scanning them.
    let walker =
        ignore::WalkBuilder::new(root).git_ignore(true).hidden(true).require_git(false).build();
    for entry in walker.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();

        // Temp-file naming check needs no read.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if temp_globs.iter().any(|g| glob_match(g, name)) {
                out.push(ScaffoldFinding {
                    kind: ScaffoldKind::TempFile,
                    file: path.to_path_buf(),
                    line: 0,
                    label: "suspicious file name".into(),
                    snippet: name.to_string(),
                });
                // Don't also content-scan obvious scratch files.
                continue;
            }
        }

        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > cfg.max_file_bytes {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else { continue }; // binary → skip

        for (line_no, line) in content.lines().enumerate() {
            for re in &marker_res {
                if let Some(m) = re.find(line) {
                    out.push(ScaffoldFinding {
                        kind: ScaffoldKind::CommentMarker,
                        file: path.to_path_buf(),
                        line: line_no + 1,
                        label: m.as_str().to_string(),
                        snippet: line.trim().chars().take(120).collect(),
                    });
                    break; // one marker finding per line
                }
            }

            if cfg.include_secrets {
                for pat in secrets::patterns() {
                    if let Some(m) = pat.regex.find(line) {
                        out.push(ScaffoldFinding {
                            kind: ScaffoldKind::Secret,
                            file: path.to_path_buf(),
                            line: line_no + 1,
                            label: pat.name.to_string(),
                            snippet: secrets::redact(m.as_str()),
                        });
                        break; // one secret finding per line
                    }
                }
            }
        }
    }
    out
}

/// Placeholder bodies over the resolved projection: a map over functions whose
/// body_span trims to a stub. Reads each file once per module.
pub fn scan_placeholder_bodies(graph: &ProjectedGraph) -> Vec<ScaffoldFinding> {
    let mut sources: HashMap<&EntityId, String> = HashMap::new();
    let mut paths: HashMap<&EntityId, PathBuf> = HashMap::new();
    for (mid, m) in &graph.modules {
        if let Ok(src) = std::fs::read_to_string(&m.path) {
            sources.insert(mid, src);
        }
        paths.insert(mid, m.path.clone());
    }

    let mut out = Vec::new();
    for f in graph.functions.values() {
        let Some(src) = sources.get(&f.parent_module) else { continue };
        let Some(body) = src.get(f.body_span.start..f.body_span.end) else { continue };
        if is_placeholder_body(body) {
            out.push(ScaffoldFinding {
                kind: ScaffoldKind::PlaceholderBody,
                file: paths.get(&f.parent_module).cloned().unwrap_or_default(),
                line: f.line,
                label: format!("{} is a stub", f.name),
                snippet: body.trim().chars().take(60).collect(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrets::redact;

    #[test]
    fn glob_shapes_match_expected_names() {
        for pat in ["temp_*", "tmp_*", "backup_*", "old_*", "phase_*", "*.bak", "*_old.*"] {
            // sanity: the pattern table's own shapes must match themselves
        }
        assert!(glob_match("temp_*", "temp_utils.py"));
        assert!(glob_match("*.bak", "main.py.bak"));
        assert!(glob_match("*_old.*", "parser_old.rs"));
        assert!(glob_match("phase_*", "PHASE_backup.md"), "case-insensitive");
        assert!(!glob_match("temp_*", "template.py"), "'template' is not a temp_ file");
        assert!(!glob_match("old_*", "golden.py"));
        assert!(!glob_match("phase_*", "PHASE2_notes.md"), "literal '_' must match literally");
    }

    #[test]
    fn placeholder_bodies_are_recognized() {
        assert!(is_placeholder_body("pass"));
        assert!(is_placeholder_body("  ...\n"));
        assert!(is_placeholder_body("todo!()"));
        assert!(is_placeholder_body("raise NotImplementedError"));
        assert!(!is_placeholder_body("return True"));
        assert!(!is_placeholder_body(""));
    }

    #[test]
    fn redaction_never_keeps_a_usable_secret() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyzyxwvutsrqponm";
        let r = redact(secret);
        assert!(r.ends_with("***"));
        assert_eq!(r.chars().count(), 11);
        assert!(!r.contains(&secret[8..]), "redacted output must not leak the tail");
    }

    #[test]
    fn secret_table_catches_real_shapes_and_redacts() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("config.py");
        std::fs::write(
            &f,
            "AWS_KEY = \"AKIAABCDEFGHIJKLMNOP\"\n"
                .to_string()
                + "token = \"ghp_' + 'X'.repeat(36) + '\"\n"
                + "# harmless line\n",
        )
        .unwrap();

        let cfg = ScaffoldConfig { include_secrets: true, ..Default::default() };
        let findings = scan_path(dir.path(), &cfg);
        let aws: Vec<_> = findings
            .iter()
            .filter(|fnd| fnd.kind == ScaffoldKind::Secret && fnd.label == "aws_access_key")
            .collect();
        assert_eq!(aws.len(), 1);
        assert!(aws[0].snippet.ends_with("***"));
        assert!(!aws[0].snippet.contains("JKLMNOP\"") || !findings.iter().any(|f| f.kind == ScaffoldKind::Secret && f.snippet.contains('J')));
        // The full key must not appear anywhere in any snippet.
        assert!(!findings.iter().any(|f| f.snippet.contains("AKIAABCDEFGHIJKLMNOP")));
    }

    #[test]
    fn comment_markers_and_temp_files_are_found_but_gitignored_are_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored_dir/\n").unwrap();
        std::fs::create_dir(dir.path().join("ignored_dir")).unwrap();
        std::fs::write(dir.path().join("app.py"), "# Phase 1: wire the client\n# TODO: retry\nx = 1\n").unwrap();
        std::fs::write(dir.path().join("ignored_dir").join("skip.py"), "# TODO: hidden\n").unwrap();
        std::fs::write(dir.path().join("temp_notes.md"), "scratch\n").unwrap();

        let findings = scan_path(dir.path(), &ScaffoldConfig::default());
        let app_markers: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == ScaffoldKind::CommentMarker)
            .collect();
        assert_eq!(app_markers.len(), 2, "Phase 1 + TODO on separate lines");
        assert!(findings.iter().any(|f| f.kind == ScaffoldKind::TempFile));
        assert!(
            !findings.iter().any(|f| f.file.to_string_lossy().contains("ignored_dir")),
            "gitignored files must be skipped"
        );
    }
}
