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
    let root = strip_verbatim_prefix(root.to_path_buf());
    let path = strip_verbatim_prefix(path.to_path_buf());
    let relative = path.strip_prefix(&root).unwrap_or(path.as_path());
    relative.to_string_lossy().replace('\\', "/")
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
