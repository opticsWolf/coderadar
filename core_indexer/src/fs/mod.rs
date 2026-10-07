// CodeRadar v3.6 — Filesystem Module
pub mod git;
pub mod watcher;

use std::path::{Path, PathBuf};

/// `std::fs::canonicalize` returns verbatim paths on Windows (`\\?\D:\…`),
/// which defeat `starts_with`/`strip_prefix` against regular paths and leak
/// into every user-visible string.
pub fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    p
}

/// A path as a user should see it (plan §5.3): relative to `root` when it is
/// inside it, forward slashes, no `\\?\` prefix.
///
/// Every finding type used to invent its own spelling — scaffolding reported
/// root-relative paths, secrets reported the raw walker path with the Windows
/// verbatim prefix — so the same file appeared under two names in one report.
pub fn display_path(root: &Path, path: &Path) -> String {
    // Cross-platform (§5.1): Windows spellings must strip on Linux CI too.
    // `Path::strip_prefix` is platform-native — on Linux `C:\proj` is a
    // single filename, not a drive + dirs — so normalize to forward
    // slashes first and strip lexically with a separator boundary.
    let root_s = strip_verbatim_prefix(root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    let path_s = strip_verbatim_prefix(path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    // The filesystem root strips to a relative spelling, not an absolute one.
    if root_s == "/" {
        return path_s.trim_start_matches('/').to_string();
    }
    let root_trim = root_s.trim_end_matches('/');
    if root_trim.is_empty() {
        return path_s;
    }
    if path_s == root_trim {
        return String::new();
    }
    if let Some(rest) = path_s.strip_prefix(&format!("{root_trim}/")) {
        return rest.to_string();
    }
    // Windows drive letters are case-insensitive (`C:/` vs `c:/`); the
    // rest of the path keeps exact case. POSIX stays case-sensitive.
    if root_trim.len() >= 2
        && root_trim.as_bytes()[1] == b':'
        && root_trim.as_bytes()[0].is_ascii_alphabetic()
    {
        if path_s.eq_ignore_ascii_case(root_trim) {
            return String::new();
        }
        if path_s.len() > root_trim.len()
            && path_s[root_trim.len()..].starts_with('/')
            && path_s[..root_trim.len()].eq_ignore_ascii_case(root_trim)
        {
            return path_s[root_trim.len() + 1..].to_string();
        }
    }
    path_s
}

#[cfg(test)]
mod display_path_tests {
    use super::*;

    #[test]
    fn strips_the_root_and_normalises_separators() {
        let root = Path::new("C:\\proj");
        assert_eq!(
            display_path(root, Path::new("C:\\proj\\src\\app.py")),
            "src/app.py"
        );
    }

    #[test]
    fn a_path_outside_the_root_is_kept_whole() {
        assert_eq!(
            display_path(Path::new("C:\\proj"), Path::new("D:\\elsewhere\\x.py")),
            "D:/elsewhere/x.py"
        );
    }

    #[test]
    fn verbatim_prefixes_never_reach_the_output() {
        let root = Path::new(r"\\?\C:\proj");
        assert_eq!(
            display_path(root, Path::new(r"\\?\C:\proj\pkg\m.py")),
            "pkg/m.py"
        );
    }
}
